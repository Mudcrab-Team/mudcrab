#!/usr/bin/env python3
"""Merge `origin/main` into the current branch and resolve only the known mechanical conflicts.

The script never rebases and never forces. It makes a merge commit, or stops and leaves the
merge in progress for a person. For every conflicted path it applies one known rule:

* A generated inventory file that `main` deleted is deleted.
* `tools/world_data_inventory/reviewed-surface.json` takes `main`'s side, then the committed
  surface is regenerated with the inventory tool and checked.
* `Cargo.toml` workspace members, a sorted one-entry-per-line registry, take the union of
  both sides, sorted and de-duplicated, then the member paths are validated.

Any other path is a real conflict. The script prints one line per real conflict and stops.
It never guesses.

After a merge it runs the fast gates: the silent-failure ratchet, the unwrap ratchet against
a clean detached checkout of `origin/main` outside `.mudcrab-local`, the retail claims
check, and the world-data inventory check. A failed gate is reported and exits nonzero.

Usage:
  python3 scripts/sync_branch.py                 # fetch, merge, resolve known conflicts, gate
  python3 scripts/sync_branch.py --dry-run       # print what the merge would do
  python3 scripts/sync_branch.py --no-fetch      # use the current origin/main

Exit codes: 0 = merged and gates passed (or dry run completed), 1 = real conflicts remain.
2 = refused or a gate/tool failed. A 1 leaves the merge in progress.
"""

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import json
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

# Generated files that `main` removed. They are derived and gitignored; a branch that still
# edits one gets a modify/delete conflict. Delete the branch copy.
GENERATED_DELETED_BY_MAIN = frozenset({
    "docs/data-inventory/inventory.json",
    "docs/data-inventory/report.md",
    "tools/world_data_inventory/reviewed-sources.json",
})

# The committed review surface is derived from the tree. Take `main`'s copy, then regenerate
# it from the merged tree, exactly as docs/data-inventory/README.md says.
INVENTORY_REGENERATED = frozenset({
    "tools/world_data_inventory/reviewed-surface.json",
})

# Human-authored inventory files. Taking `main`'s side would silently drop a classification,
# so these always stop.
INVENTORY_HUMAN = (
    "tools/world_data_inventory/dispositions.json",
    "tools/world_data_inventory/dispositions.schema.json",
    "tools/world_data_inventory/store.schema.json",
    "tools/world_data_inventory/table.schema.json",
    "tools/world_data_inventory/stores/",
    "tools/world_data_inventory/tables/",
)

DEFAULT_BASE = "origin/main"


@dataclass(frozen=True)
class Registry:
    """A sorted one-entry-per-line block inside a file."""

    path: str
    start: str
    end: str
    item: str
    indent: str


# Keep this table small. Add a registry only when its whole conflict is a sorted list and a
# validator can prove the result. Do not add an append-only list that permits no reordering.
REGISTRIES = {
    registry.path: registry
    for registry in (
        Registry(
            path="Cargo.toml",
            start=r"^members = \[$",
            end=r"^\]$",
            item=r'^"[^"]+"\s*,?$',
            indent="    ",
        ),
    )
}


class SyncError(Exception):
    """A refusal. The message is printed without a traceback."""


def git(root, *args, check=True):
    """Run git in `root` and return its text output."""
    done = subprocess.run(
        ["git", *args],
        cwd=str(root),
        text=True,
        encoding="utf-8",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if check and done.returncode != 0:
        detail = (done.stderr or done.stdout or "").strip()
        raise SyncError(f"git {' '.join(args)} failed: {detail or 'no output'}")
    return done


def top_level():
    done = git(Path.cwd(), "rev-parse", "--show-toplevel", check=False)
    if done.returncode != 0:
        raise SyncError("not inside a git work tree")
    return Path(done.stdout.strip())


def branch_name(root):
    done = git(root, "symbolic-ref", "--quiet", "--short", "HEAD", check=False)
    if done.returncode != 0:
        raise SyncError("HEAD is detached; check out the branch to sync")
    return done.stdout.strip()


def merge_in_progress(root):
    done = git(root, "rev-parse", "--quiet", "--verify", "MERGE_HEAD", check=False)
    return done.returncode == 0


def ensure_clean(root):
    """Refuse staged or modified tracked files. Untracked files are allowed; git stops if one is in the way."""
    done = git(root, "status", "--porcelain", "-z")
    dirty = []
    for record in done.stdout.split("\0"):
        if not record or record.startswith("??"):
            continue
        dirty.append(record[3:])
    if dirty:
        raise SyncError(
            "working tree is not clean; commit or stash these paths first: "
            + ", ".join(sorted(dirty))
        )


def fetch_main(root):
    done = git(root, "fetch", "--quiet", "origin", "main", check=False)
    if done.returncode != 0:
        detail = (done.stderr or done.stdout or "").strip()
        raise SyncError(f"cannot fetch origin/main: {detail or 'no output'}")


def tree_has(root, ref, path):
    return git(root, "cat-file", "-e", f"{ref}:{path}", check=False).returncode == 0


def unmerged_stages(root, path):
    """The stage numbers present for one unmerged path: 2 = ours, 3 = theirs."""
    done = git(root, "ls-files", "-u", "-z", "--", path)
    stages = {}
    for record in done.stdout.split("\0"):
        if not record:
            continue
        # A record is "<mode> <oid> <stage>\t<path>".
        meta, _, _ = record.partition("\t")
        fields = meta.split()
        if len(fields) == 3:
            stages[int(fields[2])] = fields[1]
    return stages


def conflicted_paths(root):
    done = git(root, "diff", "--name-only", "--diff-filter=U", "-z")
    return [name for name in done.stdout.split("\0") if name]


def short_reason(root, path):
    """A short reason for a real conflict, for the final list."""
    stages = unmerged_stages(root, path)
    if 2 not in stages and 3 in stages:
        return "deleted on this branch but changed on main"
    if 3 not in stages and 2 in stages:
        return "changed on this branch but deleted on main"
    if path in GENERATED_DELETED_BY_MAIN:
        return "generated file changed on both sides; main still has it"
    if path in INVENTORY_REGENERATED:
        return "review surface is not a two-sided text conflict"
    if path.endswith(".snap"):
        return "snapshot changed on both sides; regenerate it from its own test"
    if path.startswith("crates/") and path.endswith(".rs"):
        return "code changed on both sides"
    if path.startswith("docs/"):
        return "documentation changed on both sides"
    if any(path == name or path.startswith(name) for name in INVENTORY_HUMAN):
        return "human inventory disposition; review the classification by hand"
    return "changed on both sides; no mechanical rule"


def delete_generated(root, path):
    git(root, "rm", "-f", "--ignore-unmatch", "--", path)


def resolve_inventory(root, path, inventory_command):
    check_surface_columns(root, path)
    git(root, "checkout", "--theirs", "--", path)
    run_tool(root, inventory_command + ["--review-sources"], "inventory --review-sources")
    run_tool(root, inventory_command + ["--check"], "inventory --check")
    git(root, "add", "--", path)


def check_surface_columns(root, path):
    """Refuse a surface conflict whose reviewed columns differ on the two sides."""
    stages = unmerged_stages(root, path)
    if 2 not in stages or 3 not in stages:
        raise SyncError(f"cannot regenerate {path}: it is not a two-sided text conflict")
    ours = surface_columns(root, path, 2)
    theirs = surface_columns(root, path, 3)
    if ours != theirs:
        raise SyncError(
            f"{path}: reviewed columns differ on each side. Resolve the source conflicts, then run "
            "inventory.py --review-sources --extraction <extraction.json> and --check; "
            "taking main's copy would drop the branch's columns.")


def surface_columns(root, path, stage):
    """The reviewed emitted columns of one clean side of the conflict."""
    done = git(root, "show", f":{stage}:{path}")
    try:
        document = json.loads(done.stdout)
    except json.JSONDecodeError as error:
        raise SyncError(f"{path}: cannot read stage {stage} as JSON: {error}") from None
    if not isinstance(document, dict):
        raise SyncError(f"{path}: stage {stage} is not a JSON object")
    columns = document.get("columns", [])
    if not isinstance(columns, list):
        raise SyncError(f"{path}: columns in stage {stage} are not a list")
    return columns


def conflict_marker(line):
    return (
        line.startswith("<<<<<<< ")
        or line.startswith("||||||| ")
        or line.startswith("=======")
        or line.startswith(">>>>>>> ")
    )


def registry_members(text, path, registry):
    """The member paths in one clean registry block. Raises when the block is malformed."""
    lines = text.splitlines()
    starts = [index for index, line in enumerate(lines) if re.match(registry.start, line)]
    if len(starts) != 1:
        raise SyncError(f"{path}: expected one registry block start, found {len(starts)}")
    start = starts[0]
    ends = [index for index in range(start + 1, len(lines)) if re.match(registry.end, lines[index])]
    if not ends:
        raise SyncError(f"{path}: registry block has no end line")
    members = []
    for line in lines[start + 1 : ends[0]]:
        if not line.strip():
            continue
        if conflict_marker(line):
            raise SyncError(f"{path}: conflict marker in a registry side")
        match = re.match(r'^"([^"]+)"\s*,?$', line.strip())
        if not match:
            raise SyncError(f"{path}: registry line is not a member path: {line!r}")
        members.append(match.group(1))
    return members


def side_members(root, path, stage, registry):
    done = git(root, "show", f":{stage}:{path}")
    return set(registry_members(done.stdout, path, registry))


def resolve_registry(root, path, registry):
    """Replace the conflict hunks inside one sorted registry block with the sorted union."""
    stages = unmerged_stages(root, path)
    if 1 in stages:
        base = side_members(root, path, 1, registry)
        ours = side_members(root, path, 2, registry)
        theirs = side_members(root, path, 3, registry)
        removed = sorted((base - ours) | (base - theirs))
        if removed:
            raise SyncError(
                f"{path}: registry member removed on one side ({', '.join(removed)}); "
                "a union would add it back. Resolve this path by hand.")
    target = root / path
    try:
        original = target.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise SyncError(f"cannot read {path}: {error}") from None
    lines = original.splitlines()
    starts = [index for index, line in enumerate(lines) if re.match(registry.start, line)]
    if len(starts) != 1:
        raise SyncError(f"{path}: expected one registry block start, found {len(starts)}")
    start = starts[0]
    ends = [index for index in range(start + 1, len(lines)) if re.match(registry.end, lines[index])]
    if not ends:
        raise SyncError(f"{path}: registry block has no end line")
    end = ends[0]
    for index, line in enumerate(lines):
        if conflict_marker(line) and not (start < index < end):
            raise SyncError(f"{path}: conflict outside the registry block; resolve it by hand")

    items = []
    seen = False
    index = start + 1
    while index < end:
        line = lines[index]
        if line.startswith("<<<<<<< "):
            seen = True
            index += 1
            side = "ours"
            sides = {"ours": [], "base": [], "theirs": []}
            while index < end and not lines[index].startswith(">>>>>>> "):
                line = lines[index]
                if line.startswith("||||||| "):
                    side = "base"
                elif line.startswith("======="):
                    side = "theirs"
                elif line.strip():
                    sides[side].append(line)
                index += 1
            if index >= end:
                raise SyncError(f"{path}: unterminated conflict in the registry block")
            for side in ("ours", "theirs"):
                for line in sides[side]:
                    if not re.match(registry.item, line.strip()):
                        raise SyncError(f"{path}: registry line is not a member path: {line!r}")
                    items.append(line.strip())
            index += 1
            continue
        if conflict_marker(line):
            raise SyncError(f"{path}: unexpected conflict marker in the registry block")
        if line.strip():
            if not re.match(registry.item, line.strip()):
                raise SyncError(f"{path}: registry line is not a member path: {line!r}")
            items.append(line.strip())
        index += 1

    if not seen:
        raise SyncError(f"{path}: no conflict inside the registry block")

    normalized = []
    for item in items:
        if not item.endswith(","):
            item += ","
        normalized.append(item)
    unique = sorted(set(normalized), key=lambda text: text.strip().rstrip(","))

    resolved = list(lines[: start + 1])
    resolved.extend(registry.indent + item for item in unique)
    resolved.extend(lines[end:])
    text = "\n".join(resolved) + ("\n" if original.endswith("\n") else "")
    if any(conflict_marker(line) for line in text.splitlines()):
        raise SyncError(f"{path}: conflict markers remain after the registry union")

    validate_registry(root, registry, unique)
    with open(target, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)
    git(root, "add", "--", path)


def validate_registry(root, registry, items):
    """Prove the registry format after the union. Cargo workspace members must exist."""
    if registry.path != "Cargo.toml":
        raise SyncError(f"{registry.path}: no validator is registered")
    seen = set()
    for item in items:
        match = re.match(r'^"([^"]+)"\s*,?$', item.strip())
        if not match:
            raise SyncError(f"{registry.path}: invalid member line: {item!r}")
        member = match.group(1)
        if not member or member.startswith("/") or ".." in Path(member).parts:
            raise SyncError(f"{registry.path}: member path is not relative: {member!r}")
        if member in seen:
            raise SyncError(f"{registry.path}: duplicate member: {member}")
        seen.add(member)
        manifest = root / member / "Cargo.toml"
        if not manifest.is_file():
            raise SyncError(f"{registry.path}: member has no Cargo.toml: {member}")
    ordered = sorted(seen)
    actual = [re.match(r'^"([^"]+)"', item.strip()).group(1) for item in items]
    if actual != ordered:
        raise SyncError(f"{registry.path}: member paths are not sorted")


def run_tool(root, command, label):
    """Run one repo tool and raise with its output when it fails."""
    if not command:
        raise SyncError(f"{label}: empty command")
    done = subprocess.run(
        command,
        cwd=str(root),
        text=True,
        encoding="utf-8",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    if done.returncode != 0:
        output = done.stdout.strip()
        raise SyncError(f"{label} failed (exit {done.returncode}):\n{output or 'no output'}")


@contextmanager
def base_checkout(root, base):
    """A clean detached checkout of `base`, outside `.mudcrab-local`."""
    parent = Path(tempfile.mkdtemp(prefix="mudcrab-sync-base-"))
    resolved = parent.resolve()
    if ".mudcrab-local" in resolved.parts:
        shutil.rmtree(parent, ignore_errors=True)
        raise SyncError(
            f"temporary directory {resolved} is inside .mudcrab-local; set TMPDIR outside it"
        )
    tree = parent / "tree"
    try:
        git(root, "worktree", "add", "--detach", "--quiet", str(tree), base)
        yield tree
    finally:
        git(root, "worktree", "remove", "--force", str(tree), check=False)
        shutil.rmtree(parent, ignore_errors=True)
        git(root, "worktree", "prune", check=False)


def python_command():
    return [sys.executable, "-B"]


def gate_commands(base_ref, base_tree, inventory_command):
    return [
        (
            "silent-failure ratchet",
            python_command()
            + ["tools/silent_failure_ratchet/silent_failure_ratchet.py", "--base-root", str(base_tree)],
        ),
        (
            "unwrap ratchet",
            python_command()
            + ["tools/unwrap_ratchet/unwrap_ratchet.py", "--base-root", str(base_tree)],
        ),
        (
            "retail claims",
            python_command()
            + ["scripts/check_retail_claims.py", "--repo", ".", "--base", base_ref, "--head", "HEAD"],
        ),
        ("world-data inventory", inventory_command + ["--check"]),
    ]


def run_gates(root, base_ref, inventory_command):
    failures = []
    with base_checkout(root, base_ref) as base_tree:
        for name, command in gate_commands(base_ref, base_tree, inventory_command):
            print(f"sync-branch: gate: {name}")
            done = subprocess.run(
                command,
                cwd=str(root),
                text=True,
                encoding="utf-8",
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
            )
            output = done.stdout.strip()
            if output:
                print(output)
            if done.returncode != 0:
                failures.append(name)
    if failures:
        raise SyncError(
            "fast gate(s) failed: " + ", ".join(failures)
            + ". The merge commit is in place; fix the gate failure before you push."
        )
    print("sync-branch: fast gates passed")


def describe_rule(root, path, base):
    """The dry-run description of the rule for one conflicted path."""
    if path in GENERATED_DELETED_BY_MAIN and not tree_has(root, base, path):
        return "delete the generated file"
    if path in INVENTORY_REGENERATED:
        return "take main's side and regenerate reviewed-surface.json"
    if path in REGISTRIES:
        return "union the sorted registry block and validate it"
    return "STOP: " + short_reason(root, path)


def predicted_conflicts(root, base):
    """Use merge-tree to predict conflicts without touching the working tree."""
    done = git(root, "merge-tree", "--write-tree", "--no-messages", "--name-only", base, "HEAD", check=False)
    if done.returncode not in (0, 1):
        detail = (done.stderr or done.stdout or "").strip()
        raise SyncError(f"git merge-tree failed: {detail or 'no output'}")
    lines = done.stdout.splitlines()
    paths = [line for line in lines[1:] if line.strip()] if lines else []
    if done.returncode == 1 and not paths:
        raise SyncError("git merge-tree reported a conflict but named no paths")
    return paths


def dry_run(root, base):
    ensure_clean(root)
    if merge_in_progress(root):
        raise SyncError("a merge is already in progress; finish or abort it first")
    branch_name(root)
    if git(root, "merge-base", "--is-ancestor", base, "HEAD", check=False).returncode == 0:
        print(f"sync-branch: dry run: HEAD already contains {base}; the merge would be a no-op")
        return
    paths = predicted_conflicts(root, base)
    if not paths:
        print(f"sync-branch: dry run: merging {base} would make a clean merge commit")
    else:
        print(f"sync-branch: dry run: merging {base} would conflict in {len(paths)} path(s)")
        known = 0
        blocked = 0
        for path in paths:
            description = describe_rule(root, path, base)
            if description.startswith("STOP: "):
                blocked += 1
            else:
                known += 1
            print(f"  {path}: {description}")
        if blocked:
            print(f"sync-branch: dry run: {known} mechanical path(s); {blocked} path(s) would stop for a person")
        else:
            print(f"sync-branch: dry run: {known} mechanical path(s); no real conflict")
    print("sync-branch: dry run: gates would run after the merge:")
    for name, _ in gate_commands(
        base, Path("/tmp/base"), [sys.executable, "-B", "tools/world_data_inventory/inventory.py"]
    ):
        print(f"  {name}")


def merge(root, base):
    branch = branch_name(root)
    if git(root, "merge-base", "--is-ancestor", base, "HEAD", check=False).returncode == 0:
        print(f"sync-branch: HEAD already contains {base}; no merge commit")
        return False
    print(f"sync-branch: merging {base} into {branch}")
    done = git(
        root,
        "merge",
        "--no-ff",
        "--no-edit",
        "-m",
        f"Merge {base} into {branch}",
        base,
        check=False,
    )
    if done.returncode == 0:
        print("sync-branch: clean merge commit")
        return False
    if done.returncode not in (1,) or not merge_in_progress(root):
        detail = (done.stderr or done.stdout or "").strip()
        raise SyncError(f"git merge failed: {detail or 'no output'}")
    return True


def resolve(root, base, inventory_command):
    """Resolve every known class. Return the paths that need a person."""
    remaining = []
    deferred = []
    for path in conflicted_paths(root):
        stages = unmerged_stages(root, path)
        if (
            path in GENERATED_DELETED_BY_MAIN
            and 3 not in stages
            and 2 in stages
            and not tree_has(root, base, path)
        ):
            print(f"sync-branch: delete generated file that main removed: {path}")
            delete_generated(root, path)
            continue
        if path in INVENTORY_REGENERATED:
            # Regenerate only after every source conflict is resolved. The inventory tool scans
            # the working tree, and a reader source with conflict markers would produce a wrong
            # surface that its own --check cannot detect.
            check_surface_columns(root, path)
            deferred.append(path)
            continue
        if path in REGISTRIES:
            print(f"sync-branch: union the sorted registry: {path}")
            resolve_registry(root, path, REGISTRIES[path])
            continue
        remaining.append(path)
    if remaining:
        if deferred:
            print("sync-branch: the reviewed surface is still conflicted; it is regenerated only after the source conflicts are resolved")
            print("sync-branch: after you resolve and stage the source conflicts, run:")
            for command in (inventory_command + ["--review-sources"], inventory_command + ["--check"]):
                print(f"  {shlex.join(command)}")
            for path in deferred:
                print(f"  git add -- {path}")
            print("  git commit")
        return remaining
    for path in deferred:
        print(f"sync-branch: take main's side, then regenerate: {path}")
        resolve_inventory(root, path, inventory_command)
    return []


def run(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true", help="print what the merge would do")
    parser.add_argument("--no-fetch", action="store_true", help="use the current origin/main")
    parser.add_argument("--base", default=DEFAULT_BASE, help=f"merge base ref (default: {DEFAULT_BASE})")
    parser.add_argument(
        "--inventory-command",
        help="command for the inventory tool, before --review-sources/--check (default: the repo tool)",
    )
    args = parser.parse_args(argv)

    root = top_level()
    if not args.no_fetch:
        print("sync-branch: fetching origin/main")
        fetch_main(root)
    resolved_base = git(root, "rev-parse", "--verify", "--quiet", f"{args.base}^{{commit}}", check=False)
    if resolved_base.returncode != 0:
        raise SyncError(f"base ref does not exist: {args.base}")

    if args.dry_run:
        dry_run(root, args.base)
        return 0

    ensure_clean(root)
    if merge_in_progress(root):
        raise SyncError("a merge is already in progress; finish or abort it first")
    branch_name(root)

    if args.inventory_command:
        inventory_command = shlex.split(args.inventory_command)
    else:
        inventory_command = python_command() + ["tools/world_data_inventory/inventory.py"]
    if not inventory_command:
        raise SyncError("--inventory-command is empty")

    had_conflicts = merge(root, args.base)
    if had_conflicts:
        remaining = resolve(root, args.base, inventory_command)
        if remaining:
            print(f"sync-branch: {len(remaining)} real conflict(s) need a person:")
            for path in remaining:
                print(f"  {path}: {short_reason(root, path)}")
            print("sync-branch: the merge is still in progress; resolve these paths, then run `git commit`")
            return 1
        git(root, "commit", "--no-edit")
        print("sync-branch: merge commit created after mechanical resolution")

    run_gates(root, args.base, inventory_command)
    return 0


def main():
    try:
        sys.exit(run(sys.argv[1:]))
    except SyncError as error:
        print(f"sync-branch: error: {error}", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
