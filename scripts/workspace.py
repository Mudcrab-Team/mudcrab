#!/usr/bin/env python3
"""Repository-scoped build storage and asset references (Python 3.9+, no dependencies)."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time

GIB = 1024**3
VERSION = 1
SLOTS = 2
RESERVE_GIB = 40
BUILD_BUDGET_GIB = 190  # raised 2026-11-15 by lead (slots regrow fast with many owners; RESERVE_GIB floor unchanged)
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}\Z")
# Automatic build cleanup. A bad or zero environment value turns that limit off.
SLOT_INCREMENTAL_GIB = 8      # MUDCRAB_SLOT_INCREMENTAL_GIB: trim a slot's target/*/incremental above this size
TTL_MIN_HOURS = 1             # prune --isolated-ttl-hours refuses anything shorter
LABEL_CAP_GIB = 12            # MUDCRAB_LABEL_CAP_GIB: warn when one label's builds exceed this size
KEEP_DAYS = 7.0               # default length of a --keep pin
MIN_IDLE_SECONDS = 1800       # settle and superseding leave builds used within this time
LSOF_TIMEOUT_SECONDS = 120
RETIRE_AFTER_SUCCESS = {"check", "clippy", "fmt", "test", "nextest"}  # `doc` output is the result
AUTOMATIC_REASONS = {"on-success", "superseded"}  # these never delete a build that holds result artifacts
KEEP_LABELS_FILE = "keep-labels"
KEEPS_BUILD_OUTPUT = {"--no-run", "--open", "--timings", "--message-format", "--build-plan"}
MANAGED_BUILD = re.compile(r"slot-[01]|isolated-[0-9a-f]{16}-[A-Za-z0-9_.-]+")
ISOLATED_PARTS = re.compile(r"isolated-(?P<owner>[0-9a-f]{16})-(?P<label>[A-Za-z0-9_.-]+?)-(?P<head>[0-9a-f]{12})")
RETIRING = re.compile(r"\.retiring-(?:slot-[01]|isolated-[0-9a-f]{16}-[A-Za-z0-9_.-]+)-[0-9]+")
TRIMMING = re.compile(r"\.trim-incremental-[0-9]+")


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()


def write_json(path: Path, data: dict) -> None:
    temporary = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
        temporary.replace(path)
    except BaseException:
        # A full disk can fail after the file exists. Do not leave a partial file behind.
        try:
            temporary.unlink(missing_ok=True)
        except OSError:
            pass
        raise


def read_json(path: Path) -> dict:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"Expected a JSON object: {path}")
    return value


def valid_name(value: str) -> str:
    if not NAME.fullmatch(value) or value in {".", ".."}:
        raise ValueError("Names must use 1-80 letters, digits, dots, underscores or hyphens")
    return value


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def env_number(name: str, default: float) -> float:
    """A nonnegative finite number from the environment; anything else (or 0) disables the limit."""
    raw = os.environ.get(name)
    if raw is None or raw.strip() == "":
        return float(default)
    try:
        value = float(raw)
    except ValueError:
        return 0.0
    return value if math.isfinite(value) and value >= 0 else 0.0


def inventory(root: Path, seen: set | None = None, device: int | None = None) -> dict:
    """Metadata only; skip symlinks and count hard-linked bytes once across roots.

    With `device`, an entry on another device (a mount point or another volume) is counted in
    `devices` and not entered."""
    seen = set() if seen is None else seen
    result = {"path": str(root), "files": 0, "logical_bytes": 0, "devices": 0,
              "allocated_bytes": 0, "symlinks": 0, "errors": []}
    if not root.exists() and not root.is_symlink():
        return result
    pending = [root]
    while pending:
        path = pending.pop()
        try:
            if device is not None and os.lstat(path).st_dev != device:
                result["devices"] += 1
            elif path.is_symlink():
                result["symlinks"] += 1
            elif path.is_dir():
                pending.extend(path.iterdir())
            elif path.is_file():
                info = path.stat()
                result["files"] += 1
                inode = (info.st_dev, info.st_ino) if info.st_ino else str(path)
                if inode not in seen:
                    seen.add(inode)
                    result["logical_bytes"] += info.st_size
                    result["allocated_bytes"] += getattr(info, "st_blocks", 0) * 512
        except OSError as exc:
            result["errors"].append(f"{path}: {exc}")
    return result


@contextmanager
def lease(path: Path, wait: float = 0.0):
    """Process lease; locks live outside the directories we may retire.

    It does not block unless `wait` seconds are given: then it retries until the time is up."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a+b") as stream:
        deadline = time.monotonic() + wait
        while True:
            try:
                if os.name == "nt":
                    import msvcrt
                    stream.seek(0, 2)
                    if stream.tell() == 0:
                        stream.write(b"0")
                        stream.flush()
                    stream.seek(0)
                    msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.05)
        try:
            yield
        finally:
            if os.name == "nt":
                stream.seek(0)
                msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(stream, fcntl.LOCK_UN)


@contextmanager
def try_lease(path: Path):
    """Yield True when the lease was taken, False when another process holds it."""
    manager = lease(path)
    try:
        manager.__enter__()
    except OSError:
        yield False
        return
    try:
        yield True
    finally:
        manager.__exit__(None, None, None)


class Workspace:
    def __init__(self, repo: Path, root: Path | None = None):
        self.repo = Path(git(repo, "rev-parse", "--show-toplevel")).resolve()
        self.common = Path(git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")).resolve()
        # A linked checkout's .git is a file; all linked checkouts resolve this same root.
        self.root = (root or Path(os.environ.get("MUDCRAB_WORKSPACE_ROOT",
                                               str(self.common.parent / ".mudcrab-local")))).resolve()
        if (self.root == self.repo or self.root == self.common or self.root in self.repo.parents
                or self.common in self.root.parents):
            raise ValueError("Storage root must be a dedicated directory, not a checkout or Git directory")
        self.owner = hashlib.sha256(str(self.repo).encode()).hexdigest()[:16]
        self.warned: set[str] = set()

    def paths(self) -> dict:
        return {"checkout": str(self.repo), "storage": str(self.root),
                **{name: str(self.root / name) for name in ("builds", "packs", "scratch", "evidence")}}

    def setup(self) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        if (self.root / "locks").is_symlink():
            raise ValueError("Managed lock directory cannot be a symlink")
        with lease(self.root / "locks" / "registry.lock", wait=10.0):
            marker = self.root / "workspace.json"
            expected = {"version": VERSION, "git_common_dir": str(self.common)}
            if marker.is_symlink():
                raise ValueError(f"Storage marker cannot be a symlink: {marker}")
            if marker.exists() and read_json(marker) != expected:
                raise ValueError(f"Storage belongs to another repository or version: {marker}")
            for name in ("builds", "packs", "scratch", "evidence", "bin"):
                path = self.root / name
                if path.is_symlink():
                    raise ValueError(f"Managed directories cannot be symlinks: {path}")
                path.mkdir(exist_ok=True)
            if not marker.exists():
                write_json(marker, expected)

    def build_usage(self) -> int:
        report = inventory(self.root / "builds")
        if report["errors"] or report["symlinks"]:
            raise ValueError(f"Cannot safely account for build storage: {report}")
        return report["logical_bytes"]

    def preflight(self, needed_gib: float = 8, output: Path | None = None) -> dict:
        reserve = float(os.environ.get("MUDCRAB_RESERVE_GIB", RESERVE_GIB))
        budget = float(os.environ.get("MUDCRAB_BUILD_BUDGET_GIB", BUILD_BUDGET_GIB))
        if not all(math.isfinite(n) and n >= 0 for n in (needed_gib, reserve, budget)):
            raise ValueError("Storage budgets must be finite, nonnegative numbers")
        destination = (output or self.root).resolve()
        while not destination.exists():
            destination = destination.parent
        free = shutil.disk_usage(destination).free
        used = self.build_usage()
        report = {"free_gib": round(free / GIB, 2), "build_gib": round(used / GIB, 2),
                  "needed_gib": needed_gib, "reserve_gib": reserve, "build_budget_gib": budget}
        if free < (needed_gib + reserve) * GIB:
            raise ValueError(f"Insufficient free space: {report}")
        if output is None and used + needed_gib * GIB > budget * GIB:
            raise ValueError(f"Build budget exceeded; audit and prune idle managed builds: {report}")
        return report

    def build_key(self, env: dict) -> str:
        compiler = subprocess.check_output(["rustc", "-vV"], cwd=self.repo, env=env, text=True)
        inputs = {"compiler": compiler, "environment": {
            k: v for k, v in env.items()
            if k.startswith(("CARGO_", "RUST", "CC", "CXX")) or k in {"SDKROOT", "MACOSX_DEPLOYMENT_TARGET"}
        }}
        for name in ("Cargo.lock", "Cargo.toml", ".cargo/config.toml", "rust-toolchain.toml"):
            path = self.repo / name
            if path.exists():
                inputs[name] = digest(path)
        return hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()[:16]

    def checkout_identity(self) -> str:
        """Path and root inode. A different directory moved to this path is another checkout."""
        return f"{self.repo}#{os.stat(self.repo).st_ino}"

    def registry_only_packages(self) -> set[str]:
        """Package names whose every source is immutable (registry or git) for this checkout.

        Read from files only. `cargo metadata` could rewrite Cargo.lock before a `--locked` run.
        A name that this cannot prove is not returned, so its fingerprints are removed."""
        external: set[str] = set()
        local: set[str] = set()
        lock = self.repo / "Cargo.lock"
        if lock.is_file():
            for block in lock.read_text(encoding="utf-8").split("[[package]]")[1:]:
                name = re.search(r'^name = "([^"]+)"', block, re.MULTILINE)
                if name:
                    (external if re.search(r"^source = ", block, re.MULTILINE) else local).add(name[1])
        # A new or newly patched path crate can be missing from a stale lock file. Read every
        # listed manifest, and follow `path` values into directories that Git ignores. Matching
        # any `name` or `path` over-selects, which only costs a rebuild.
        listed = git(self.repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard",
                     "--", "Cargo.toml", "*/Cargo.toml")
        pending = [self.repo / relative for relative in filter(None, listed.split("\0"))]
        seen: set[Path] = set()
        while pending:
            manifest = pending.pop().resolve()
            if manifest in seen or not manifest.is_file():
                continue
            seen.add(manifest)
            text = manifest.read_text(encoding="utf-8")
            local.update(re.findall(r"""\bname["']?\s*=\s*["']([^"'\n]+)["']""", text))
            pending.extend(manifest.parent / value / "Cargo.toml"
                           for value in re.findall(r"""\bpath["']?\s*=\s*["']([^"'\n]+)["']""", text))
        return external - local

    def forget_path_crates(self, slot: Path) -> int:
        """Remove the slot's Cargo fingerprints for every package that is not from a registry or git.

        Cargo gives a path package the same fingerprint directory in every checkout, and judges
        freshness by mtime. Without this, it can report another checkout's output as Fresh.
        Cargo then rebuilds these packages and the packages that depend on them."""
        target = slot / "target"
        stores = []
        for profile in [*target.glob("*"), *target.glob("*/*")]:  # <profile> and <triple>/<profile>
            if profile.relative_to(target).parts[0] in {"doc", "package", "tmp"}:
                continue
            store = profile / ".fingerprint"
            # Fail closed on a layout this does not know, such as Cargo's newer build-dir layout.
            if (any(profile.glob("build/*/*/fingerprint")) or store.is_symlink()
                    or (not store.is_dir() and ((profile / "deps").is_dir() or (profile / "build").is_dir()))):
                raise ValueError(f"Unknown Cargo layout, so {slot.name} cannot be made safe for this checkout: "
                                 f"{profile}. Use --isolated LABEL and report this")
            if store.is_dir():
                stores.append(store)
        if not stores:
            return 0
        keep = self.registry_only_packages()
        removed = 0
        for store in stores:
            for entry in store.iterdir():
                unit = re.fullmatch(r"(.+)-[0-9a-f]{16}", entry.name)
                if unit and unit[1] not in keep:
                    shutil.rmtree(entry)  # A failure stops the build: the output is still unsafe.
                    removed += 1
        return removed

    def candidates(self, label: str | None, key: str) -> list[Path]:
        if label:
            head = git(self.repo, "rev-parse", "HEAD")[:12]
            return [self.root / "builds" / f"isolated-{self.owner}-{valid_name(label)}-{head}"]
        slots = [self.root / "builds" / f"slot-{i}" for i in range(SLOTS)]
        def preference(path):
            marker = path / "build.json"
            previous = read_json(marker) if marker.exists() else {}
            # A matching key avoids the largest rebuild. This checkout's slot then avoids the path crates.
            return previous.get("key") != key, previous.get("fresh_for") != self.checkout_identity()
        return sorted(slots, key=preference)

    @staticmethod
    def retires_on_success(args: list[str]) -> bool:
        """Only commands whose output is not a deliverable may retire their isolated build."""
        if not args or args[0] not in RETIRE_AFTER_SUCCESS:
            return False
        options = args[:args.index("--")] if "--" in args else args
        if any(a.split("=", 1)[0] in KEEPS_BUILD_OUTPUT for a in options):
            return False
        return args[0] != "nextest" or args[1:2] == ["run"]

    def cargo(self, args: list[str], label: str | None, needed_gib: float,
              keep: bool = False, keep_days: float = KEEP_DAYS) -> int:
        if not args or args[0] not in {"build", "check", "clippy", "test", "nextest", "run", "fmt", "doc"}:
            raise ValueError("Use cargo build/check/clippy/test/nextest/run/fmt/doc through this entrypoint")
        cargo_args = args[:args.index("--")] if "--" in args else args
        overrides = ("--target-dir", "--config", "--manifest-path", "--artifact-dir", "--out-dir")
        if any(a in overrides or any(a.startswith(option + "=") for option in overrides)
               for a in cargo_args):
            raise ValueError("Run from the intended checkout; target-dir/config/manifest overrides bypass storage ownership")
        return self.run_task(["cargo", *args], label, needed_gib, retire=self.retires_on_success(args),
                             keep=keep, keep_days=keep_days)

    def run_task(self, command: list[str], label: str | None, needed_gib: float,
                 retire: bool = False, keep: bool = False, keep_days: float = KEEP_DAYS) -> int:
        if not command:
            raise ValueError("Missing command")
        if keep and not label:
            raise ValueError("--keep pins an isolated build; add --isolated LABEL")
        if keep and not (math.isfinite(keep_days) and keep_days > 0):
            raise ValueError("--keep-days must be a finite number above zero")
        env = os.environ.copy()
        # Ignore inherited per-worktree output paths. This command owns output placement.
        env.pop("CARGO_TARGET_DIR", None)
        env.pop("CARGO_BUILD_TARGET_DIR", None)
        env.pop("CARGO_BUILD_BUILD_DIR", None)
        env.setdefault("CARGO_PROFILE_DEV_DEBUG", "1")
        if label:
            env["CARGO_INCREMENTAL"] = "0"
        key = self.build_key(env)
        self.cleanup_step("recover interrupted deletions", self.recover_interrupted)
        # Serialize capacity checks + selection, but keep the build lease for the entire command.
        selected = None
        # Another launch can hold this lock while it scans build usage. Wait for it.
        with lease(self.root / "locks" / "allocation.lock", wait=30.0):
            self.preflight(needed_gib)
            for path in self.candidates(label, key):
                # A short wait lets a dry run's momentary probe of the same lock finish.
                lock = lease(self.root / "locks" / f"{path.name}.lock", wait=0.3)
                try:
                    lock.__enter__()
                except OSError:
                    continue
                selected = (path, lock)
                break
            if selected is None:
                raise ValueError("Build slots are busy. Reuse the running build or retry after it finishes; do not create another target directory")
            path, lock = selected
            existed = True  # Becomes false only after the first check below finds no directory.
            try:
                if (path.is_symlink() or (path / "build.json").is_symlink()
                        or (path.exists() and not (path / "build.json").is_file())):
                    raise ValueError(f"Refusing unowned build directory: {path}")
                existed = path.exists()
                previous = read_json(path / "build.json") if existed else {}
                if existed and previous.get("version") != VERSION:
                    raise ValueError(f"Unsupported build ownership marker: {path}")
                path.mkdir(exist_ok=True)
                marker = {"version": VERSION, "key": key,
                          "owner": self.owner if label else "shared", "checkout": str(self.repo),
                          "commit": git(self.repo, "rev-parse", "HEAD"), "last_used": time.time()}
                # A build, run, exec or keep-output command leaves a result. The mark stays for the
                # life of the directory, so a later successful check or test cannot retire it.
                # Only an explicit false (written for a directory that checks and tests created)
                # means "no result". A directory from an older script has no key and counts as a result.
                if label:
                    marker["deliverable"] = (not retire) or (existed and previous.get("deliverable") is not False)
                elif command[:2] == ["cargo", "fmt"]:  # Builds nothing, so the slot's owner does not change.
                    if "fresh_for" in previous:
                        marker["fresh_for"] = previous["fresh_for"]
                else:
                    # A new key, not "checkout": an older script drops it, so the next run removes again.
                    marker["fresh_for"] = self.checkout_identity()
                    if existed and previous.get("fresh_for") != marker["fresh_for"]:
                        removed = self.forget_path_crates(path)
                        if removed:
                            print(f"workspace: {path.name} last served another checkout; removed {removed} "
                                  "path-package fingerprints so Cargo rebuilds them", file=sys.stderr)
                write_json(path / "build.json", marker)
            except BaseException:
                if not existed and path.is_dir() and not path.is_symlink():
                    try:
                        path.rmdir()  # Only an empty directory that this call created.
                    except OSError:
                        pass
                lock.__exit__(None, None, None)
                raise
        try:
            if keep:
                self.pin(path.name, "--keep on the command line", keep_days)
            env["CARGO_TARGET_DIR"] = str(path / "target")
            env["CARGO_BUILD_BUILD_DIR"] = env["CARGO_TARGET_DIR"]
            env["MUDCRAB_BUILD_LEASE"] = str(path)
            env["MUDCRAB_STORAGE"] = str(self.root)
            print(f"CARGO_TARGET_DIR={env['CARGO_TARGET_DIR']}", file=sys.stderr, flush=True)
            result = subprocess.run(command, cwd=self.repo, env=env).returncode
            try:
                write_json(path / "build.json", {**read_json(path / "build.json"), "last_used": time.time()})
            except (OSError, ValueError) as exc:  # Keep the build and the child's exit status.
                print(f"workspace: could not update the build marker: {exc}", file=sys.stderr)
            try:
                if self.build_usage() > float(os.environ.get("MUDCRAB_BUILD_BUDGET_GIB", BUILD_BUDGET_GIB)) * GIB:
                    print("Build storage exceeds budget; prune idle builds before the next build.", file=sys.stderr)
            except ValueError as exc:  # A symlink in builds/ must not turn a finished command into a failure.
                print(f"workspace: build storage accounting skipped: {exc}", file=sys.stderr)
            # Cleanup runs while this command still holds the lease. It can only add a stderr
            # note: it never changes the exit status of the command the caller asked for.
            self.cleanup_step("cleanup", self.after_command, path, label, result, retire)
            return result
        finally:
            lock.__exit__(None, None, None)

    @staticmethod
    def cleanup_step(what: str, function, *args) -> None:
        try:
            function(*args)
        except Exception as exc:  # KeyboardInterrupt and SystemExit still propagate.
            print(f"workspace: {what} skipped: {exc}", file=sys.stderr)

    def after_command(self, path: Path, label: str | None, result: int, retire: bool) -> None:
        if not label:
            row = self.trim_slot_incremental(path, leased=True)
            if row["status"] == "trimmed":
                print(f"workspace: trimmed {path.name} incremental state ({row['bytes']} bytes)", file=sys.stderr)
            elif row["status"].startswith("held:"):
                print(f"workspace: trim held for {path.name}: {row['status']}", file=sys.stderr)
            return
        self.warn_label_cap(path)
        if result != 0:
            return  # Keep a failing build to inspect. Nothing is retired or superseded.
        if retire:
            # Only a passing check or test replaces older heads. A build, run or exec never does:
            # keeping two heads side by side is how one compares them.
            self.supersede(path)
            row = self.retire(path, "on-success", leased=True, owner_only=True)
            if row["status"] != "removed":
                print(f"workspace: kept {path.name}: {row['status']}", file=sys.stderr)

    def warn_label_cap(self, current: Path) -> None:
        cap = env_number("MUDCRAB_LABEL_CAP_GIB", LABEL_CAP_GIB) * GIB
        parts = ISOLATED_PARTS.fullmatch(current.name)
        if cap <= 0 or not parts:
            return
        total = 0
        for sibling in (self.root / "builds").iterdir():
            other = ISOLATED_PARTS.fullmatch(sibling.name)
            if other and other["owner"] == parts["owner"] and other["label"] == parts["label"] and not sibling.is_symlink():
                total += inventory(sibling)["logical_bytes"]
        if total > cap:
            print(f"workspace: builds for label '{parts['label']}' use {total / GIB:.2f} GiB, which exceeds the "
                  f"{cap / GIB:.2f} GiB cap (MUDCRAB_LABEL_CAP_GIB). Nothing was deleted. Retire old builds "
                  "with 'settle --apply' or 'prune --apply'.", file=sys.stderr)

    def supersede(self, current: Path) -> list[dict]:
        """Retire older heads of the same checkout and label after a successful command."""
        parts = ISOLATED_PARTS.fullmatch(current.name)
        if not parts:
            return []
        rows = []
        for sibling in sorted((self.root / "builds").iterdir()):
            other = ISOLATED_PARTS.fullmatch(sibling.name)
            if sibling == current or not other or other["owner"] != parts["owner"] or other["label"] != parts["label"]:
                continue
            rows.append(self.retire(sibling, "superseded", owner_only=True, min_idle_seconds=MIN_IDLE_SECONDS))
        return rows

    def audit(self) -> dict:
        raw = subprocess.check_output(["git", "-C", str(self.repo), "worktree", "list", "--porcelain", "-z"])
        roots = [Path(os.fsdecode(item[9:])) for item in raw.split(b"\0") if item.startswith(b"worktree ")]
        seen: set = set()
        reports = []
        for root in [self.root, *roots]:
            if root == self.root:
                children = [root / name for name in ("builds", "packs", "scratch", "evidence")]
            elif root.is_dir():
                children = [p for p in root.iterdir() if p.name in {
                    "target", "modern_assets", "Data", "data", "game_data", "extracted",
                    "extracted_data", "transformed_data", "mod_cache", "dump"
                } or ".assets-cache" in p.name or ".staging-" in p.name or ".pack-" in p.name]
            else:
                continue
            reports.extend(inventory(path, seen) for path in children if path.exists() or path.is_symlink())
        return {"paths": self.paths(), "artifacts": reports,
                "note": "Metadata accounting deduplicates hard links; APFS clones/snapshots prevent exact reclaim estimates. No content-duplicate detection."}

    # ---- Guarded deletion. Every path below fails closed: any doubt keeps the directory. ----

    def pin(self, name: str, reason: str, days: float = KEEP_DAYS, now: float | None = None) -> Path:
        if not MANAGED_BUILD.fullmatch(name) or not (math.isfinite(days) and days > 0):
            raise ValueError("A pin needs a managed build name and a finite duration above zero")
        pins = self.root / "pins"
        if pins.is_symlink():
            raise ValueError(f"Managed pin directory cannot be a symlink: {pins}")
        pins.mkdir(exist_ok=True)
        now = time.time() if now is None else now
        target = pins / f"{name}.json"
        write_json(target, {"reason": reason, "created": now, "until": now + days * 86400,
                            "created_by": self.owner})
        return target

    def pin_hold(self, name: str) -> str | None:
        pins = self.root / "pins"
        target = pins / f"{name}.json"
        if pins.is_symlink() or target.is_symlink():
            return "held: pin check failed"
        if not target.exists():
            return None
        try:
            until = read_json(target).get("until")
        except (OSError, ValueError):
            return "held: pin check failed"
        if isinstance(until, bool) or not isinstance(until, (int, float)) or not math.isfinite(until):
            return "held: pinned"  # An unreadable expiry is never treated as expired.
        return "held: pinned" if until > time.time() else None

    def warn_once(self, key: str, message: str) -> None:
        if key not in self.warned:
            self.warned.add(key)
            print(message, file=sys.stderr)

    def receipt_hold(self, name: str) -> str | None:
        """Hold a build named by any evidence/*/build-receipt.json whose state is not 'final'."""
        evidence = self.root / "evidence"
        try:
            receipts = sorted(evidence.glob("*/build-receipt.json"))
        except OSError:
            return "held: receipt check failed"
        for receipt in receipts:
            try:
                data = read_json(receipt)
                builds = data.get("builds", [])
                if not isinstance(builds, list):
                    raise ValueError("builds is not a list")
            except (OSError, ValueError) as exc:
                self.warn_once(str(receipt), f"workspace: receipt check failed: {receipt} ({type(exc).__name__}). "
                                             "No build is retired until it is fixed.")
                return "held: receipt check failed"
            if any(Path(str(item)).name == name for item in builds) and data.get("state") != "final":
                return "held: named in a pending receipt"
        return None

    def keep_labels(self) -> set[str] | None:
        """Labels that are never retired: MUDCRAB_KEEP_LABELS plus <root>/keep-labels. None if unreadable."""
        labels = {item.strip().casefold() for item in os.environ.get("MUDCRAB_KEEP_LABELS", "").split(",")
                  if item.strip()}
        keep_file = self.root / KEEP_LABELS_FILE
        failure = f"workspace: keep list check failed: {keep_file}. No build is retired until it is fixed."
        if keep_file.is_symlink():
            self.warn_once(str(keep_file), failure)
            return None
        if keep_file.exists():
            try:
                text = keep_file.read_text(encoding="utf-8")
            except (OSError, ValueError):
                self.warn_once(str(keep_file), failure)
                return None
            for line in text.splitlines():
                labels.update(item.casefold() for item in re.split(r"[,\s]+", line.split("#", 1)[0]) if item)
        return labels

    def policy_hold(self, name: str) -> str | None:
        parts = ISOLATED_PARTS.fullmatch(name)
        label = parts["label"] if parts else (name.split("-", 2)[2] if name.startswith("isolated-") else None)
        keep = self.keep_labels()
        if keep is None:
            return "held: keep list check failed"
        if label and label.casefold() in keep:
            return "held: label is in the keep list"
        return self.pin_hold(name) or self.receipt_hold(name)

    @staticmethod
    def consumer_hold(path: Path) -> str | None:
        # Directly launched executables may outlive Cargo. Fail closed if we cannot check
        # those consumers; never infer idle from the lease alone.
        lsof = shutil.which("lsof")
        if not lsof:
            return "held: lsof unavailable"
        try:
            opened = subprocess.run([lsof, "-nP", "+D", str(path)], capture_output=True, text=True,
                                    timeout=LSOF_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            return "held: consumer check timed out"
        except OSError:
            return "held: consumer check failed"
        if opened.returncode != 1 or opened.stdout or opened.stderr:
            return "held: open files or consumer check failed"
        return None

    def owned_build(self, path: Path, owner_only: bool, allow_slot: bool) -> tuple[dict | None, str | None]:
        builds = self.root / "builds"
        if builds.is_symlink() or path.parent != builds:
            return None, "held: outside the managed builds directory"
        if not MANAGED_BUILD.fullmatch(path.name):
            return None, "held: not a managed build name"
        if path.is_symlink():
            return None, "held: symlink"
        if not path.is_dir():
            return None, "held: missing"
        if os.path.ismount(path):
            return None, "held: mount point"
        if path.name.startswith("slot-") and not allow_slot:
            return None, "held: whole slots are never retired automatically"
        marker = path / "build.json"
        if marker.is_symlink() or not marker.is_file():
            return None, "held: not tool-owned (no build.json)"
        try:
            metadata = read_json(marker)
        except (OSError, ValueError):
            return None, "held: unreadable build.json"
        if metadata.get("version") != VERSION:
            return None, "held: unsupported build marker"
        last_used = metadata.get("last_used")
        if isinstance(last_used, bool) or not isinstance(last_used, (int, float)) or not math.isfinite(last_used):
            return None, "held: bad last_used in build.json"
        if owner_only and metadata.get("owner") != self.owner:
            return None, "held: another owner"
        return metadata, None

    def device(self) -> int:
        return os.lstat(self.root / "builds").st_dev

    def append_receipt(self, path: Path, reason: str, status: str, size: int, checks: list[str]) -> None:
        line = json.dumps({"time": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "path": str(path), "bytes": size,
                           "reason": reason, "status": status, "checks": checks}, separators=(",", ":"))
        with (self.root / "evidence" / "cleanup-receipts.jsonl").open("a", encoding="utf-8") as stream:
            stream.write(line + "\n")

    def delete_leftover(self, path: Path, reason: str, size: int, checks: list[str], row: dict) -> bool:
        """Delete a directory that was already renamed out of its build name."""
        try:
            with try_lease(self.root / "locks" / f"{path.name}.lock") as taken:
                if not taken:
                    row["status"] = "held: active lease"
                    return False
                try:
                    shutil.rmtree(path)
                except Exception as exc:  # A crash (BaseException) leaves the directory for recovery.
                    if not (isinstance(exc, FileNotFoundError) and not path.exists()):
                        row["status"] = "held: removal incomplete"
                        self.safe_receipt(path, reason, "removal incomplete", size, checks)
                        print(f"workspace: removal incomplete, will retry later: {path}: {exc}", file=sys.stderr)
                        return False
                (self.root / "locks" / f"{path.name}.lock").unlink(missing_ok=True)
        except OSError as exc:
            row["status"] = f"held: removal failed: {exc}"
            return False
        return True

    def safe_receipt(self, *args) -> None:
        try:
            self.append_receipt(*args)
        except OSError as exc:
            print(f"workspace: receipt line not written: {exc}", file=sys.stderr)

    def retire(self, path: Path, reason: str, *, leased: bool = False, min_idle_seconds: float = 0.0,
               owner_only: bool = False, allow_slot: bool = False) -> dict:
        """Remove one tool-owned build directory, or say why not. Never raises for a hold."""
        row = {"path": str(path), "status": "candidate"}
        _, hold = self.owned_build(path, owner_only, allow_slot)
        if hold:
            row["status"] = hold
            return row
        if leased:
            return self.retire_leased(path, reason, min_idle_seconds, owner_only, allow_slot, row)
        with try_lease(self.root / "locks" / f"{path.name}.lock") as taken:
            if not taken:
                row["status"] = "held: active lease"
                return row
            return self.retire_leased(path, reason, min_idle_seconds, owner_only, allow_slot, row)

    def retire_leased(self, path, reason, min_idle_seconds, owner_only, allow_slot, row) -> dict:
        # A build could have changed between discovery and lease acquisition: check again.
        metadata, hold = self.owned_build(path, owner_only, allow_slot)
        checks = ["managed-name", "not-symlink", "build-marker", "lease"]
        if hold is None and min_idle_seconds > 0:
            last_used = metadata.get("last_used")
            if (isinstance(last_used, bool) or not isinstance(last_used, (int, float))
                    or time.time() - last_used < min_idle_seconds):
                hold = "held: recently used"
            checks.append("idle")
        if hold is None and reason in AUTOMATIC_REASONS and metadata.get("deliverable") is not False:
            hold = "held: holds result artifacts"
        checks.append("no-result-artifacts")
        hold = hold or self.policy_hold(path.name)
        checks += ["keep-list", "unpinned", "no-pending-receipt"]
        hold = hold or self.consumer_hold(path)
        checks.append("no-open-files")
        scan = None
        if hold is None:
            scan = inventory(path, device=self.device())
            if scan["devices"]:
                hold = "held: contains another filesystem"
            elif scan["errors"] or scan["symlinks"]:
                hold = "held: scan errors or symlinks"
            checks += ["tree-scan", "same-device"]
        if hold:
            row["status"] = hold
            return row
        stamp = time.time_ns()
        retiring = path.with_name(f".retiring-{path.name}-{stamp}")
        receipt = {"build": metadata, "inventory": scan, "retired_at": time.time(), "reason": reason,
                   "leftovers": [retiring.name],
                   "checks": checks, "free_before": shutil.disk_usage(self.root).free}
        receipt_path = self.root / "evidence" / f"retired-{path.name}-{stamp}.json"
        try:
            write_json(receipt_path, receipt)
            path.rename(retiring)
        except OSError as exc:
            receipt_path.unlink(missing_ok=True)  # Nothing was removed; do not leave a false receipt.
            row["status"] = f"held: could not prepare removal: {exc}"
            return row
        if not self.delete_leftover(retiring, reason, scan["logical_bytes"], checks, row):
            return row
        self.safe_receipt(path, reason, "removed", scan["logical_bytes"], checks)
        print(f"workspace: removed {path.name} ({reason}, {scan['logical_bytes'] / 2**20:.1f} MiB)", file=sys.stderr)
        try:
            write_json(receipt_path, {**receipt, "free_after": shutil.disk_usage(self.root).free})
            (self.root / "pins" / f"{path.name}.json").unlink(missing_ok=True)  # Only an expired or absent pin gets here.
        except OSError:
            pass
        row["status"] = "removed"
        return row

    def trim_slot_incremental(self, slot: Path, leased: bool) -> dict:
        """Remove target/<profile>/incremental of an idle slot that exceeds its cap. Nothing else."""
        row = {"path": str(slot), "status": "disabled", "bytes": 0}
        cap = env_number("MUDCRAB_SLOT_INCREMENTAL_GIB", SLOT_INCREMENTAL_GIB) * GIB
        if cap <= 0:
            return row
        if not slot.name.startswith("slot-"):
            row["status"] = "held: not a slot"
            return row
        _, hold = self.owned_build(slot, False, True)
        if hold:
            row["status"] = hold
            return row
        if leased:
            return self.trim_leased(slot, cap, row)
        with try_lease(self.root / "locks" / f"{slot.name}.lock") as taken:
            if not taken:
                row["status"] = "held: active lease"
                return row
            return self.trim_leased(slot, cap, row)

    def trim_leased(self, slot: Path, cap: float, row: dict) -> dict:
        row["status"] = "unchanged"
        target = slot / "target"
        if target.is_symlink() or not target.is_dir():
            return row
        found, total = [], 0
        for profile in sorted(target.iterdir()):
            if profile.is_symlink() or not profile.is_dir():
                continue
            incremental = profile / "incremental"
            if incremental.is_symlink():
                row["status"] = "held: symlink"
                return row
            if not incremental.is_dir():
                continue
            scan = inventory(incremental, device=self.device())
            if scan["devices"]:
                row["status"] = "held: contains another filesystem"
                return row
            if scan["errors"] or scan["symlinks"]:
                row["status"] = "held: scan errors or symlinks"
                return row
            found.append(incremental)
            total += scan["logical_bytes"]
        if not found or total <= cap:
            return row
        for incremental in found:
            hold = self.consumer_hold(incremental)
            if hold:
                row["status"] = hold
                return row
        checks = ["slot-name", "lease", "no-symlinks", "no-open-files", "over-cap"]
        stamp = time.time_ns()
        planned = [p.with_name(f".trim-incremental-{stamp}") for p in found]
        receipt = self.root / "evidence" / f"trimmed-{slot.name}-{stamp}.json"
        try:
            write_json(receipt, {"slot": slot.name, "directories": [str(p) for p in found], "bytes": total,
                                 "leftovers": [p.relative_to(self.root / "builds").as_posix() for p in planned],
                                 "checks": checks, "cap_bytes": cap, "trimmed_at": time.time()})
        except OSError as exc:
            row["status"] = f"held: could not write receipt: {exc}"
            return row
        leftovers = []
        failed = False
        try:
            for incremental, leftover in zip(found, planned):
                incremental.rename(leftover)  # Cargo recreates `incremental` on the next build.
                leftovers.append(leftover)
        except OSError as exc:
            failed = True
            print(f"workspace: trim rename failed: {exc}", file=sys.stderr)
            if not leftovers:
                receipt.unlink(missing_ok=True)  # Nothing was removed; do not leave a false receipt.
                row["status"] = f"held: could not rename: {exc}"
                return row
        for leftover in leftovers:
            try:
                shutil.rmtree(leftover)
            except Exception as exc:
                failed = True
                print(f"workspace: trim incomplete, will retry later: {leftover}: {exc}", file=sys.stderr)
        row.update(status="held: removal incomplete" if failed else "trimmed", bytes=total)
        self.safe_receipt(slot, "slot-incremental", "removal incomplete" if failed else "trimmed", total, checks)
        return row

    def has_recovery_evidence(self, leftover: Path) -> bool:
        evidence = self.root / "evidence"
        if leftover.name.startswith(".retiring-"):
            marker = leftover / "build.json"
            if marker.is_file() and not marker.is_symlink():
                return True
            receipt = evidence / f"retired-{leftover.name[len('.retiring-'):]}.json"
        else:
            stamp = leftover.name[len(".trim-incremental-"):]
            receipt = evidence / f"trimmed-{leftover.parent.parent.parent.name}-{stamp}.json"
        try:
            if receipt.is_symlink() or not receipt.is_file():
                return False
            recorded = read_json(receipt).get("leftovers")
            return isinstance(recorded, list) and leftover.relative_to(self.root / "builds").as_posix() in recorded
        except (OSError, ValueError):
            return False

    def recover_interrupted(self) -> list[dict]:
        """Finish deletions that a crash interrupted: `.retiring-*` builds and `.trim-incremental-*` state."""
        rows = []
        builds = self.root / "builds"
        if builds.is_symlink() or not builds.is_dir():
            return rows
        leftovers = [p for p in sorted(builds.iterdir()) if RETIRING.fullmatch(p.name)]
        for slot in (builds / "slot-0", builds / "slot-1"):
            target = slot / "target"
            if slot.is_symlink() or target.is_symlink() or not target.is_dir():
                continue
            for profile in sorted(target.iterdir()):
                if profile.is_symlink() or not profile.is_dir():
                    continue
                leftovers += [p for p in sorted(profile.iterdir()) if TRIMMING.fullmatch(p.name)]
        for leftover in leftovers:
            row = {"path": str(leftover), "status": "candidate"}
            rows.append(row)
            if leftover.is_symlink() or not leftover.is_dir():
                row["status"] = "held: symlink or not a directory"
                continue
            if not self.has_recovery_evidence(leftover):
                row["status"] = "held: no build.json or receipt shows that the tool made it"
                continue
            hold = self.consumer_hold(leftover)
            scan = inventory(leftover, device=self.device()) if hold is None else None
            if hold is None and scan["devices"]:
                hold = "held: contains another filesystem"
            elif hold is None and (scan["errors"] or scan["symlinks"]):
                hold = "held: scan errors or symlinks"
            if hold:
                row["status"] = hold
                continue
            if self.delete_leftover(leftover, "recovered", scan["logical_bytes"], ["no-open-files", "tree-scan"], row):
                self.safe_receipt(leftover, "recovered", "removed", scan["logical_bytes"], ["no-open-files", "tree-scan"])
                row["status"] = "removed"
        return rows

    def settle(self, apply: bool) -> list[dict]:
        """This checkout's isolated builds. Dry run unless apply is set."""
        return self.prune(0, apply, owner_only=True, isolated_only=True, reason="settle",
                          min_idle_seconds=MIN_IDLE_SECONDS)

    def prune(self, days: float | None, apply: bool, owner_only: bool = False, isolated_only: bool = False,
              reason: str | None = None, min_idle_seconds: float = 0.0,
              isolated_ttl_hours: float | None = None) -> list[dict]:
        """List, and with apply remove, recognized builds unused for `days` (default 7).

        `isolated_ttl_hours` gives isolated builds a shorter age. It is never applied unless asked for."""
        if days is not None and (not math.isfinite(days) or days < 0):
            raise ValueError("Age must be finite and nonnegative")
        if isolated_ttl_hours is not None and not (math.isfinite(isolated_ttl_hours)
                                                   and isolated_ttl_hours >= TTL_MIN_HOURS):
            raise ValueError(f"The isolated TTL must be a finite number of at least {TTL_MIN_HOURS} hour")
        results = self.recover_interrupted() if apply else []
        for path in sorted((self.root / "builds").iterdir()):
            marker = path / "build.json"
            if path.is_symlink() or not marker.is_file() or marker.is_symlink():
                continue
            try:
                metadata = read_json(marker)
            except (OSError, ValueError):
                continue
            if metadata.get("version") != VERSION or (owner_only and metadata.get("owner") != self.owner):
                continue
            if not MANAGED_BUILD.fullmatch(path.name):
                continue
            isolated = path.name.startswith("isolated-")
            if isolated_only and not isolated:
                continue
            by_ttl = isolated and isolated_ttl_hours is not None
            if by_ttl:  # An explicit longer age wins over a shorter TTL.
                age = max(isolated_ttl_hours * 3600, (days or 0) * 86400)
            else:
                age = (days if days is not None else 7) * 86400
            last_used = metadata.get("last_used")
            if (isinstance(last_used, bool) or not isinstance(last_used, (int, float))
                    or not math.isfinite(last_used) or time.time() - last_used < age):
                continue
            if not apply:
                results.append({"path": str(path), "status": self.dry_run_status(path, last_used, min_idle_seconds)})
                continue
            results.append(self.retire(path, reason or ("ttl" if by_ttl else "prune"), owner_only=owner_only,
                                       min_idle_seconds=max(age, min_idle_seconds), allow_slot=True))
        return results

    def dry_run_status(self, path: Path, last_used: float, min_idle_seconds: float) -> str:
        """What --apply would say for the cheap checks. It takes no lock and removes nothing."""
        hold = self.policy_hold(path.name)
        if hold is None and min_idle_seconds > 0 and time.time() - last_used < min_idle_seconds:
            hold = "held: recently used"
        lock = self.root / "locks" / f"{path.name}.lock"
        if hold is None and lock.exists():
            with try_lease(lock) as taken:
                if not taken:
                    hold = "held: active lease"
        return hold or "candidate"

    def register_pack(self, name: str, path: Path) -> None:
        valid_name(name)
        path = path.resolve(strict=True)
        hashes = {}
        for filename, gate in (("conversion-manifest.json", "complete"), ("integration-report.json", "passed")):
            manifest = path / filename
            if read_json(manifest).get(gate) is not True:
                raise ValueError(f"Pack has not passed its {gate} gate: {manifest}")
            hashes[filename] = digest(manifest)
        with lease(self.root / "locks" / "registry.lock", wait=10.0):
            registry_path = self.root / "packs.json"
            registry = read_json(registry_path) if registry_path.exists() else {}
            entry = {"path": str(path), "manifests": hashes}
            if name in registry and registry[name] != entry:
                raise ValueError("Pack names are immutable; register changed inputs/settings under a new name")
            registry[name] = entry
            write_json(registry_path, registry)

    def pack_path(self, name: str) -> Path:
        entry = read_json(self.root / "packs.json")[valid_name(name)]
        path = Path(entry["path"])
        for filename, expected in entry["manifests"].items():
            if digest(path / filename) != expected:
                raise ValueError(f"Registered pack changed: {path}; register a new immutable pack")
        return path

    def install(self, git_hook: bool = False) -> None:
        """Install a small bootstrap so hooks also work on older branches."""
        source = Path(__file__).resolve()
        policy = self.repo / "AGENTS.md"
        guide = self.repo / "docs/contributing/storage.md"
        # Copy workspace.py last, so a missing source file cannot leave a half-updated install.
        for origin, name in ((policy, "AGENTS.md"), (guide, "storage.md"), (source, "workspace.py")):
            destination = self.root / "bin" / name
            if not origin.is_file():
                print(f"workspace: install skipped {name}: {origin} does not exist", file=sys.stderr)
                continue
            if origin != destination:
                temporary = destination.with_suffix(".tmp")
                shutil.copyfile(origin, temporary)
                temporary.replace(destination)
        # Older branches do not yet have the tracked .gitignore entry. Keep the installed
        # shared directory out of their status without hiding the backfilled agent rules.
        if self.root == self.common.parent / ".mudcrab-local":
            exclude = self.common / "info" / "exclude"
            exclude.parent.mkdir(exist_ok=True)
            existing = exclude.read_text(encoding="utf-8") if exclude.exists() else ""
            if "/.mudcrab-local/" not in existing.splitlines():
                exclude.write_text(existing.rstrip("\n") + "\n/.mudcrab-local/\n", encoding="utf-8")
        if git_hook:
            self.install_git_hook()

    def install_git_hook(self) -> None:
        configured = subprocess.run(["git", "-C", str(self.repo), "config", "--get", "core.hooksPath"],
                                    capture_output=True, text=True)
        if configured.returncode != 1:
            raise ValueError("core.hooksPath is configured or could not be checked; chain shared setup through your existing hook manager")
        hook = self.common / "hooks" / "post-checkout"
        marker = "# mudcrab-workspace-post-checkout-v1"
        if hook.is_symlink() or (hook.exists() and marker not in hook.read_text(encoding="utf-8").splitlines()):
            raise ValueError(f"Existing checkout hook preserved; chain shared setup manually: {hook}")
        hook.parent.mkdir(exist_ok=True)
        command = shlex.join(["python3", str(self.root / "bin/workspace.py"), "--root", str(self.root), "setup"])
        temporary = hook.with_suffix(".tmp")
        temporary.write_text(f"#!/bin/sh\n{marker}\nexec {command}\n", encoding="utf-8")
        temporary.chmod(0o755)
        temporary.replace(hook)

    def bootstrap_guidance(self) -> None:
        policy = self.root / "bin" / "AGENTS.md"
        target = self.repo / "AGENTS.md"
        if not target.exists() and policy.is_file():
            try:
                with target.open("x", encoding="utf-8") as stream:
                    stream.write(policy.read_text(encoding="utf-8"))
            except FileExistsError:
                pass


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--root", type=Path, help="Override shared storage; use the same value in every worktree")
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("paths", "setup", "audit"):
        sub.add_parser(name)
    settle = sub.add_parser("settle", help="List this checkout's retirable isolated builds; --apply removes them")
    settle.add_argument("--apply", action="store_true")
    install = sub.add_parser("install")
    install.add_argument("--git-hook", action="store_true", help="Install setup as post-checkout when no other hook manager owns it")
    preflight = sub.add_parser("preflight")
    preflight.add_argument("--needed-gib", type=float, required=True)
    preflight.add_argument("--output", type=Path, help="Check conversion destination free space instead of build budget")
    for name in ("cargo", "exec"):
        command = sub.add_parser(name)
        command.add_argument("--isolated", metavar="LABEL")
        command.add_argument("--needed-gib", type=float, default=8)
        command.add_argument("--keep", action="store_true", help="Pin the isolated build; it is never retired automatically while pinned")
        command.add_argument("--keep-days", type=float, default=KEEP_DAYS)
        command.add_argument("args", nargs=argparse.REMAINDER)
    prune = sub.add_parser("prune")
    prune.add_argument("--older-than-days", type=float, default=None,
                       help="Age for every build (default 7 days)")
    prune.add_argument("--isolated-ttl-hours", type=float, default=None,
                       help=f"Shorter age for isolated builds only (at least {TTL_MIN_HOURS} hour). Never applied by default")
    prune.add_argument("--apply", action="store_true")
    register = sub.add_parser("register-pack")
    register.add_argument("name")
    register.add_argument("path", type=Path)
    pack = sub.add_parser("pack-path")
    pack.add_argument("name")
    args = parser.parse_args(argv)
    try:
        workspace = Workspace(args.repo, args.root)
        if args.command in {"paths", "audit"}:
            result = workspace.paths() if args.command == "paths" else workspace.audit()
        else:
            workspace.setup()
            if args.command in {"cargo", "exec"}:
                cargo_args = args.args[1:] if args.args[:1] == ["--"] else args.args
                run = workspace.cargo if args.command == "cargo" else workspace.run_task
                return run(cargo_args, args.isolated, args.needed_gib, keep=args.keep, keep_days=args.keep_days)
            if args.command == "setup":
                workspace.bootstrap_guidance()
                result = {**workspace.paths(), "instructions": "Read AGENTS.md. Use workspace.py cargo; reuse registered packs. No per-worktree target directories."}
            elif args.command == "install":
                workspace.install(args.git_hook)
                result = {"bootstrap": str(workspace.root / "bin" / "workspace.py"), "checkout_hook": args.git_hook}
            elif args.command == "preflight":
                result = workspace.preflight(args.needed_gib, args.output)
            elif args.command == "settle":
                result = workspace.settle(args.apply)
            elif args.command == "prune":
                result = workspace.prune(args.older_than_days, args.apply, isolated_ttl_hours=args.isolated_ttl_hours)
            elif args.command == "register-pack":
                workspace.register_pack(args.name, args.path)
                result = {"name": args.name, "path": str(args.path.resolve())}
            elif args.command == "pack-path":
                print(workspace.pack_path(args.name))
                return 0
        print(json.dumps(result, indent=2))
        if args.command == "audit" and any(row["errors"] for row in result["artifacts"]):
            return 2
        return 0
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
        print(f"workspace: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
