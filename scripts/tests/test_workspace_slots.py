"""A shared build slot must not reuse another checkout's build (incident of 2026-11-25).

Cargo gives a path package the same fingerprint in every checkout and compares only mtimes.
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "workspace.py"
SPEC = importlib.util.spec_from_file_location("workspace", SCRIPT)
workspace = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(workspace)

UNIT = "0123456789abcdef"


class SlotCase(unittest.TestCase):
    """Two linked worktrees of one repository that share one storage root."""

    def setUp(self):
        reserve = patch.dict(os.environ, {"MUDCRAB_RESERVE_GIB": "0"})
        reserve.start()
        self.addCleanup(reserve.stop)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.a = self.base / "checkout a"
        self.b = self.base / "checkout b"
        self.store = self.base / "shared storage"
        self.a.mkdir()
        self.git(self.a, "init", "-q")
        self.git(self.a, "config", "user.name", "Storage Tests")
        self.git(self.a, "config", "user.email", "storage@example.invalid")

    def git(self, repo, *args):
        return workspace.git(repo, *args)

    def write(self, root, files):
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

    def checkouts(self, files):
        self.write(self.a, files)
        self.git(self.a, "add", ".")
        self.git(self.a, "commit", "-qm", "fixture")
        self.git(self.a, "worktree", "add", "-qb", "second", str(self.b))


@unittest.skipUnless(shutil.which("cargo") and shutil.which("rustc"), "needs a Rust toolchain")
class RealCargoTests(SlotCase):
    """The reproduction, through the real entrypoint and a real Cargo."""

    def sources(self, value):
        return {
            "app/src/main.rs": f'fn main() {{ println!("app={value} dep={{}}", dep::dep()); }}\n',
            "vendor/dep/src/lib.rs": f'pub fn dep() -> &\'static str {{ "{value}" }}\n',
        }

    def run_app(self, checkout):
        # Only slot-0 is free, so both checkouts must share it.
        env = {k: v for k, v in os.environ.items()
               if not k.startswith("CARGO_") and k not in {"RUSTC_WRAPPER", "RUSTFLAGS"} or k == "CARGO_HOME"}
        with workspace.lease(self.store / "locks" / "slot-1.lock", wait=1.0):
            done = subprocess.run(
                [sys.executable, "-I", str(SCRIPT), "--repo", str(checkout), "--root", str(self.store),
                 "cargo", "--needed-gib", "0", "run", "--offline", "--quiet"],
                env=env, capture_output=True, text=True, timeout=240)
        self.assertEqual(done.returncode, 0, done.stderr)
        return done.stdout.strip()

    def test_a_slot_never_serves_another_checkouts_build(self):
        self.checkouts({
            "Cargo.toml": '[workspace]\nresolver = "2"\nmembers = ["app"]\nexclude = ["vendor"]\n',
            "app/Cargo.toml": '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n'
                              '[dependencies]\ndep = { path = "../vendor/dep" }\n',
            "vendor/dep/Cargo.toml": '[package]\nname = "dep"\nversion = "0.1.0"\nedition = "2021"\n',
            ".gitignore": "Cargo.lock\n",
            **self.sources("A"),
        })
        self.write(self.b, self.sources("B"))
        # B's files are older than anything that A will build. This is the state that fooled Cargo.
        past = time.time() - 3600
        for path in self.b.rglob("*"):
            if path.is_file():
                os.utime(path, (past, past))
        workspace.Workspace(self.a, self.store).setup()
        self.assertEqual(self.run_app(self.a), "app=A dep=A")
        self.assertEqual(self.run_app(self.b), "app=B dep=B")
        # A's files are now older than B's build.
        self.assertEqual(self.run_app(self.a), "app=A dep=A")


class ForgetTests(SlotCase):
    """Which fingerprints go, and when. Cargo does not run here."""

    LOCK = ('[[package]]\nname = "app"\nversion = "0.1.0"\n\n'
            '[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "registry+index"\n\n'
            '[[package]]\nname = "two-sources"\nversion = "1.0.0"\n\n'
            '[[package]]\nname = "two-sources"\nversion = "2.0.0"\nsource = "registry+index"\n\n'
            '[[package]]\nname = "newly-patched"\nversion = "1.0.0"\nsource = "registry+index"\n\n'
            '[[package]]\nname = "git-dep"\nversion = "1.0.0"\nsource = "git+https://example.invalid#abc"\n')

    def setUp(self):
        super().setUp()
        self.checkouts({
            "Cargo.toml": '[workspace]\nmembers = ["app"]\n',
            "Cargo.lock": self.LOCK,
            "app/Cargo.toml": '[package]\nname = "app"\nversion = "0.1.0"\n',
            "vendor/patched/Cargo.toml": '[package] # vendored\nauthors = [\n    "someone",\n]\n'
                                         "name = 'newly-patched'\nversion = \"1.0.0\"\n",
        })
        self.ws_a = workspace.Workspace(self.a, self.store)
        self.ws_b = workspace.Workspace(self.b, self.store)
        self.ws_a.setup()
        self.target = self.store / "builds/slot-0/target"

    def run_slot(self, ws, command=("cargo", "build")):
        original = subprocess.run
        def run(args, **kwargs):
            if args[0] == "git":
                return original(args, **kwargs)
            return subprocess.CompletedProcess(args, 0, "", "")
        with patch.object(ws, "build_key", return_value="same"), \
                patch.object(workspace.subprocess, "run", side_effect=run), \
                workspace.lease(self.store / "locks/slot-1.lock", wait=1.0):
            return ws.run_task(list(command), None, 0)

    def fill(self, *profiles):
        names = ("app", "serde", "two-sources", "newly-patched", "git-dep", "only-in-other-checkout")
        for profile in profiles or ("debug",):
            for name in names:
                unit = self.target / profile / ".fingerprint" / f"{name}-{UNIT}"
                unit.mkdir(parents=True, exist_ok=True)
                (unit / "stamp").write_text("x")

    def left(self, profile="debug"):
        return sorted(p.name[:-len(UNIT) - 1] for p in (self.target / profile / ".fingerprint").iterdir())

    def test_another_checkout_removes_every_unit_that_is_not_immutable(self):
        self.run_slot(self.ws_a)
        profiles = ("debug", "release", "aarch64-apple-darwin/quick")
        self.fill(*profiles)
        self.run_slot(self.ws_b)
        for profile in profiles:
            self.assertEqual(self.left(profile), ["git-dep", "serde"])

    def test_the_same_checkout_keeps_every_fingerprint(self):
        self.run_slot(self.ws_a)
        self.fill()
        self.run_slot(self.ws_a)
        self.assertEqual(len(self.left()), 6)

    def test_an_exec_command_is_guarded_too(self):
        self.run_slot(self.ws_a)
        self.fill()
        self.run_slot(self.ws_b, ("a-campaign",))
        self.assertEqual(self.left(), ["git-dep", "serde"])

    def test_a_marker_from_an_older_script_removes_them(self):
        self.run_slot(self.ws_a)
        self.fill()
        marker = self.store / "builds/slot-0/build.json"
        old = workspace.read_json(marker)
        del old["fresh_for"]  # An older script rewrites the whole marker, with "checkout" only.
        workspace.write_json(marker, old)
        self.run_slot(self.ws_a)
        self.assertEqual(self.left(), ["git-dep", "serde"])

    def test_another_directory_at_the_same_path_is_another_checkout(self):
        self.run_slot(self.ws_a)
        self.fill()
        self.a.rename(self.base / "moved away")
        shutil.copytree(self.base / "moved away", self.a, symlinks=True)  # copytree keeps mtimes
        self.run_slot(workspace.Workspace(self.a, self.store))
        self.assertEqual(self.left(), ["git-dep", "serde"])

    def test_format_neither_removes_nor_takes_the_slot(self):
        self.run_slot(self.ws_a)
        self.fill()
        self.run_slot(self.ws_b, ("cargo", "fmt"))
        self.assertEqual(len(self.left()), 6)
        self.run_slot(self.ws_a)
        self.assertEqual(len(self.left()), 6)

    def test_an_unknown_layout_stops_the_command_and_keeps_the_old_marker(self):
        for layout in ("debug/deps", "debug/build/app/0123456789abcdef/fingerprint"):
            with self.subTest(layout=layout):
                shutil.rmtree(self.store / "builds", ignore_errors=True)
                (self.store / "builds").mkdir()
                self.run_slot(self.ws_a)
                (self.target / layout).mkdir(parents=True)
                before = workspace.read_json(self.store / "builds/slot-0/build.json")
                with self.assertRaisesRegex(ValueError, "Unknown Cargo layout"):
                    self.run_slot(self.ws_b)
                self.assertEqual(workspace.read_json(self.store / "builds/slot-0/build.json"), before)
                with workspace.lease(self.store / "locks/slot-0.lock"):
                    pass

    def test_a_failed_removal_stops_the_command_and_keeps_the_old_marker(self):
        self.run_slot(self.ws_a)
        self.fill()
        with patch.object(workspace.shutil, "rmtree", side_effect=OSError("denied")), \
                self.assertRaisesRegex(OSError, "denied"):
            self.run_slot(self.ws_b)
        marker = workspace.read_json(self.store / "builds/slot-0/build.json")
        self.assertEqual(marker["fresh_for"], self.ws_a.checkout_identity())

    def test_a_checkout_prefers_the_slot_that_last_served_it(self):
        for slot, ws in (("slot-0", self.ws_a), ("slot-1", self.ws_b)):
            path = self.store / "builds" / slot
            path.mkdir()
            workspace.write_json(path / "build.json",
                                 {"version": 1, "key": "same", "fresh_for": ws.checkout_identity()})
        self.assertEqual(self.ws_b.candidates(None, "same")[0].name, "slot-1")
        self.assertEqual(self.ws_a.candidates(None, "same")[0].name, "slot-0")


if __name__ == "__main__":
    unittest.main()
