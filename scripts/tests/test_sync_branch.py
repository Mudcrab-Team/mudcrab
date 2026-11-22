#!/usr/bin/env python3
"""Tests for scripts/sync_branch.py, using a throwaway git repository.

Every test makes its own repository and bare origin. The inventory tool is a stub command,
so the tests need no converter output, network or game data. The ratchets and the retail
claims check are stubs committed in the repository, so the fast gates run in the test.
"""

import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "sync_branch.py"

STUB_EXIT_ZERO = """import sys
print("stub: ok")
sys.exit(0)
"""

INVENTORY_STUB = """import json
import sys
from pathlib import Path

args = sys.argv[1:]
with open("sync-inventory-log.txt", "a", encoding="utf-8") as handle:
    handle.write(" ".join(args) + "\\n")
if "--review-sources" in args:
    path = Path("tools/world_data_inventory/reviewed-surface.json")
    path.write_text(
        json.dumps({"format_version": 1, "readers": {"merged": "stub"}, "columns": []})
        + "\\n",
        encoding="utf-8",
    )
print("stub inventory: " + " ".join(args))
sys.exit(0)
"""

RETAIL_STUB = """import os
import sys
if os.environ.get("SYNC_TEST_RETAIL_FAIL"):
    print("stub retail: refused")
    sys.exit(1)
print("stub retail: ok")
sys.exit(0)
"""

GIT_LOCATION_VARS = frozenset({
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
})


def clean_git_env(extra=None):
    """The process environment without Git repository-location overrides."""
    env = {key: value for key, value in os.environ.items() if key not in GIT_LOCATION_VARS}
    if extra:
        env.update({key: value for key, value in extra.items() if key not in GIT_LOCATION_VARS})
    return env


class Repo:
    """A git repository with one bare origin."""

    def __init__(self, directory):
        self.root = Path(directory)
        self.origin = self.root / "origin.git"
        self.work = self.root / "work"
        self.git("init", "--bare", "--initial-branch=main", str(self.origin), cwd=self.root)
        self.git("init", "--initial-branch=main", str(self.work), cwd=self.root)
        self.run("config", "user.name", "Sync Test")
        self.run("config", "user.email", "sync@example.invalid")
        self.run("config", "commit.gpgsign", "false")
        self.run("remote", "add", "origin", str(self.origin))

    def run(self, *args, check=True):
        return subprocess.run(
            ["git", *args],
            cwd=str(self.work),
            text=True,
            encoding="utf-8",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=check,
            env=clean_git_env(),
        )

    def git(self, *args, cwd=None, check=True):
        return subprocess.run(
            ["git", *args],
            cwd=str(cwd) if cwd else str(self.work),
            text=True,
            encoding="utf-8",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=check,
            env=clean_git_env(),
        )

    def write(self, path, text):
        target = self.work / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def read(self, path):
        return (self.work / path).read_text(encoding="utf-8")

    def commit(self, message):
        self.run("add", "-A")
        self.run("commit", "-m", message)

    def stage_initial(self):
        self.write("crates/engine/src/lib.rs", "pub fn value() -> u32 { 1 }\n")
        self.write("crates/alpha/Cargo.toml", "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n")
        self.write("crates/beta/Cargo.toml", "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n")
        self.write(
            "Cargo.toml",
            "[workspace]\nresolver = \"2\"\nmembers = [\n"
            "    \"crates/alpha\",\n    \"crates/beta\",\n]\n",
        )
        self.write(
            "tools/world_data_inventory/reviewed-surface.json",
            json.dumps(
                {"format_version": 1, "readers": {"reader": "aaaa"}, "columns": []},
                indent=2,
            )
            + "\n",
        )
        for path in (
            "docs/data-inventory/inventory.json",
            "docs/data-inventory/report.md",
            "tools/world_data_inventory/reviewed-sources.json",
        ):
            self.write(path, "old generated content\n")
        self.write("tools/silent_failure_ratchet/silent_failure_ratchet.py", STUB_EXIT_ZERO)
        self.write("tools/unwrap_ratchet/unwrap_ratchet.py", STUB_EXIT_ZERO)
        self.write("scripts/check_retail_claims.py", RETAIL_STUB)
        self.write("tools/world_data_inventory/inventory.py", INVENTORY_STUB)
        self.write("README.md", "fixture\n")

    def start(self):
        self.stage_initial()
        self.commit("fixture: initial main")
        self.run("push", "-u", "origin", "main")

    def branch(self, name="topic"):
        self.run("checkout", "-b", name)

    def advance_main(self, edits):
        """Commit `edits` on main, push, then return to the topic branch."""
        self.run("checkout", "main")
        for path, text in edits.items():
            if text is None:
                self.run("rm", "-q", "--", path)
            else:
                self.write(path, text)
        self.commit("fixture: main moved")
        self.run("push", "origin", "main")
        self.run("checkout", "topic")

    def sync(self, *extra, env=None):
        command = [
            sys.executable,
            str(SCRIPT),
            "--no-fetch",
            "--inventory-command",
            shlex.join([sys.executable, "tools/world_data_inventory/inventory.py"]),
            *extra,
        ]
        merged_env = clean_git_env(env)
        return subprocess.run(
            command,
            cwd=str(self.work),
            text=True,
            encoding="utf-8",
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env=merged_env,
        )


class SyncBranchTests(unittest.TestCase):
    def repo(self, directory):
        repo = Repo(directory)
        repo.start()
        repo.branch()
        return repo

    def test_deletes_generated_file_that_main_deleted(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            repo.write("docs/data-inventory/inventory.json", "branch content\n")
            repo.commit("fixture: branch edits a generated file")
            repo.advance_main({"docs/data-inventory/inventory.json": None})

            done = repo.sync()

            self.assertEqual(done.returncode, 0, done.stdout)
            self.assertIn("delete generated file that main removed", done.stdout)
            self.assertFalse((repo.work / "docs/data-inventory/inventory.json").exists())
            self.assertNotIn("docs/data-inventory/inventory.json", repo.run("ls-files").stdout)
            self.assertEqual(len(repo.run("rev-list", "--parents", "-1", "HEAD").stdout.split()), 3)

    def test_takes_main_and_regenerates_reviewed_surface(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            repo.write(
                "tools/world_data_inventory/reviewed-surface.json",
                json.dumps(
                    {"format_version": 1, "readers": {"reader": "branch"}, "columns": []},
                    indent=2,
                )
                + "\n",
            )
            repo.commit("fixture: branch changes a reader")
            repo.advance_main(
                {
                    "tools/world_data_inventory/reviewed-surface.json": json.dumps(
                        {"format_version": 1, "readers": {"reader": "main"}, "columns": []},
                        indent=2,
                    )
                    + "\n"
                }
            )

            done = repo.sync()

            self.assertEqual(done.returncode, 0, done.stdout)
            self.assertIn("take main's side, then regenerate", done.stdout)
            committed = json.loads(repo.read("tools/world_data_inventory/reviewed-surface.json"))
            self.assertEqual(committed["readers"], {"merged": "stub"})
            log = (repo.work / "sync-inventory-log.txt").read_text(encoding="utf-8")
            self.assertIn("--review-sources", log)
            self.assertIn("--check", log)
            merged = repo.git("show", "--name-only", "--format=", "HEAD").stdout
            self.assertIn("tools/world_data_inventory/reviewed-surface.json", merged)

    def test_unions_sorted_registry(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            repo.write("crates/gamma/Cargo.toml", "[package]\nname = \"gamma\"\nversion = \"0.1.0\"\n")
            repo.write(
                "Cargo.toml",
                "[workspace]\nresolver = \"2\"\nmembers = [\n"
                "    \"crates/alpha\",\n    \"crates/gamma\",\n    \"crates/beta\",\n]\n",
            )
            repo.commit("fixture: branch adds a crate")
            repo.write("crates/delta/Cargo.toml", "[package]\nname = \"delta\"\nversion = \"0.1.0\"\n")
            repo.advance_main(
                {
                    "Cargo.toml": "[workspace]\nresolver = \"2\"\nmembers = [\n"
                    "    \"crates/alpha\",\n    \"crates/delta\",\n    \"crates/beta\",\n]\n",
                    "crates/delta/Cargo.toml": "[package]\nname = \"delta\"\nversion = \"0.1.0\"\n",
                }
            )

            done = repo.sync()

            self.assertEqual(done.returncode, 0, done.stdout)
            self.assertIn("union the sorted registry", done.stdout)
            text = repo.read("Cargo.toml")
            self.assertIn(
                '    "crates/alpha",\n    "crates/beta",\n    "crates/delta",\n    "crates/gamma",\n',
                text,
            )
            self.assertNotIn("<<<<<<<", text)

    def test_defers_surface_regeneration_when_source_conflicts_remain(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            surface = "tools/world_data_inventory/reviewed-surface.json"
            repo.write(
                surface,
                json.dumps({"format_version": 1, "readers": {"reader": "branch"}, "columns": []},
                           indent=2) + "\n",
            )
            repo.write("crates/engine/src/lib.rs", "pub fn value() -> u32 { 2 }\n")
            repo.commit("fixture: branch changes a reader and source")
            repo.advance_main(
                {
                    surface: json.dumps(
                        {"format_version": 1, "readers": {"reader": "main"}, "columns": []},
                        indent=2,
                    )
                    + "\n",
                    "crates/engine/src/lib.rs": "pub fn value() -> u32 { 3 }\n",
                }
            )

            done = repo.sync()

            self.assertEqual(done.returncode, 1, done.stdout)
            self.assertIn("reviewed surface is still conflicted", done.stdout)
            self.assertIn("inventory.py --review-sources", done.stdout)
            self.assertIn("<<<<<<<", repo.read(surface))
            self.assertFalse((repo.work / "sync-inventory-log.txt").exists())

    def test_refuses_surface_columns_that_differ(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            surface = "tools/world_data_inventory/reviewed-surface.json"
            repo.write(
                surface,
                json.dumps({"format_version": 1, "readers": {"reader": "branch"},
                            "columns": ["branch"]}, indent=2) + "\n",
            )
            repo.commit("fixture: branch changes a reviewed column")
            repo.advance_main(
                {
                    surface: json.dumps(
                        {"format_version": 1, "readers": {"reader": "main"},
                         "columns": ["main"]},
                        indent=2,
                    )
                    + "\n"
                }
            )

            done = repo.sync()

            self.assertEqual(done.returncode, 2, done.stdout)
            self.assertIn("reviewed columns differ on each side", done.stdout)
            self.assertIn("<<<<<<<", repo.read(surface))
            self.assertFalse((repo.work / "sync-inventory-log.txt").exists())

    def test_refuses_different_columns_before_deferring(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            surface = "tools/world_data_inventory/reviewed-surface.json"
            repo.write(
                surface,
                json.dumps({"format_version": 1, "readers": {"reader": "branch"},
                            "columns": ["branch"]}, indent=2) + "\n",
            )
            repo.write("crates/engine/src/lib.rs", "pub fn value() -> u32 { 2 }\n")
            repo.commit("fixture: branch changes a reader, column and source")
            repo.advance_main(
                {
                    surface: json.dumps(
                        {"format_version": 1, "readers": {"reader": "main"},
                         "columns": ["main"]},
                        indent=2,
                    )
                    + "\n",
                    "crates/engine/src/lib.rs": "pub fn value() -> u32 { 3 }\n",
                }
            )

            done = repo.sync()

            self.assertEqual(done.returncode, 2, done.stdout)
            self.assertIn("reviewed columns differ on each side", done.stdout)
            self.assertIn("<<<<<<<", repo.read(surface))
            self.assertFalse((repo.work / "sync-inventory-log.txt").exists())

    def test_does_not_readd_a_registry_member_removed_on_one_side(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            repo.write(
                "Cargo.toml",
                "[workspace]\nresolver = \"2\"\nmembers = [\n    \"crates/alpha\",\n]\n",
            )
            repo.commit("fixture: branch removes a workspace member")
            repo.write("crates/delta/Cargo.toml", "[package]\nname = \"delta\"\nversion = \"0.1.0\"\n")
            repo.advance_main(
                {
                    "Cargo.toml": "[workspace]\nresolver = \"2\"\nmembers = [\n"
                    "    \"crates/alpha\",\n    \"crates/delta\",\n    \"crates/beta\",\n]\n",
                    "crates/delta/Cargo.toml": "[package]\nname = \"delta\"\nversion = \"0.1.0\"\n",
                }
            )

            done = repo.sync()

            self.assertEqual(done.returncode, 2, done.stdout)
            self.assertIn("registry member removed on one side (crates/beta)", done.stdout)
            self.assertIn("<<<<<<<", repo.read("Cargo.toml"))

    def test_stops_on_real_code_conflict(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            repo.write("crates/engine/src/lib.rs", "pub fn value() -> u32 { 2 }\n")
            repo.commit("fixture: branch changes code")
            repo.advance_main({"crates/engine/src/lib.rs": "pub fn value() -> u32 { 3 }\n"})

            done = repo.sync()

            self.assertEqual(done.returncode, 1, done.stdout)
            self.assertIn("crates/engine/src/lib.rs: code changed on both sides", done.stdout)
            self.assertIn("merge is still in progress", done.stdout)
            self.assertEqual(repo.git("rev-parse", "--verify", "MERGE_HEAD").returncode, 0)
            self.assertEqual(repo.run("log", "-1", "--format=%s").stdout.strip(), "fixture: branch changes code")

    def test_dry_run_reports_the_rule_and_changes_nothing(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            repo.write("crates/engine/src/lib.rs", "pub fn value() -> u32 { 2 }\n")
            repo.commit("fixture: branch changes code")
            repo.advance_main({"crates/engine/src/lib.rs": "pub fn value() -> u32 { 3 }\n"})
            before = repo.run("rev-parse", "HEAD").stdout.strip()

            done = repo.sync("--dry-run")

            self.assertEqual(done.returncode, 0, done.stdout)
            self.assertIn("would conflict in 1 path(s)", done.stdout)
            self.assertIn("crates/engine/src/lib.rs: STOP: code changed on both sides", done.stdout)
            self.assertNotIn("Auto-merging", done.stdout)
            self.assertNotIn("CONFLICT", done.stdout)
            self.assertEqual(repo.run("rev-parse", "HEAD").stdout.strip(), before)
            self.assertNotEqual(repo.git("rev-parse", "--verify", "MERGE_HEAD", check=False).returncode, 0)

    def test_fails_closed_when_a_fast_gate_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = self.repo(directory)
            repo.write("branch.txt", "branch\n")
            repo.commit("fixture: branch adds a file")
            repo.advance_main({"main.txt": "main\n"})

            done = repo.sync(env={"SYNC_TEST_RETAIL_FAIL": "1"})

            self.assertEqual(done.returncode, 2, done.stdout)
            self.assertIn("fast gate(s) failed: retail claims", done.stdout)
            self.assertNotIn("<<<<<<<", repo.read("README.md"))


if __name__ == "__main__":
    unittest.main()
