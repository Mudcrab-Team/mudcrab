import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "workspace.py"
SPEC = importlib.util.spec_from_file_location("workspace", SCRIPT)
workspace = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(workspace)


class WorkspaceTests(unittest.TestCase):
    def setUp(self):
        # The tests must not depend on the free space of the host disk.
        reserve = patch.dict(os.environ, {"MUDCRAB_RESERVE_GIB": "0"})
        reserve.start()
        self.addCleanup(reserve.stop)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.repo = self.base / "repo with spaces"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Storage Tests")
        self.git("config", "user.email", "storage@example.invalid")
        (self.repo / "Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.store = self.base / "shared storage"
        self.ws = workspace.Workspace(self.repo, self.store)
        self.ws.setup()

    def git(self, *args):
        return workspace.git(self.repo, *args)

    def run_cargo(self, args, **kwargs):
        observed = []
        original_run = subprocess.run
        def run(command, **run_kwargs):
            if command[0] == "git":
                return original_run(command, **run_kwargs)
            if os.path.basename(str(command[0])) == "lsof":
                return subprocess.CompletedProcess(command, 1, "", "")
            observed.append(command)
            return subprocess.CompletedProcess(command, 0)
        with patch.object(self.ws, "build_key", return_value="compatible"), patch.object(workspace.subprocess, "run", side_effect=run):
            self.ws.cargo(args, None, 0, **kwargs)
        return observed[-1]

    def test_builds_and_tests_run_at_low_priority(self):
        prefix = workspace.Workspace.low_priority_prefix()
        self.assertTrue(prefix, "no priority tool found")
        self.assertIn("19", prefix)
        if sys.platform == "darwin":
            self.assertEqual(prefix[:3], ["/usr/sbin/taskpolicy", "-c", "utility"])
        for sub in ("build", "check", "clippy", "test", "doc"):
            command = self.run_cargo([sub, "--locked"])
            self.assertEqual(command[:len(prefix)], prefix, sub)
            self.assertEqual(command[len(prefix):len(prefix) + 2], ["cargo", sub])

    def test_run_and_opt_out_keep_normal_priority(self):
        self.assertEqual(self.run_cargo(["run", "--locked"])[:2], ["cargo", "run"])
        self.assertEqual(self.run_cargo(["test", "--locked"], normal_priority=True)[:2], ["cargo", "test"])

    def run_build(self, label=None, returncode=0):
        observed = []
        original_run = subprocess.run
        def run(command, **kwargs):
            if command[0] == "git":
                return original_run(command, **kwargs)
            if os.path.basename(str(command[0])) == "lsof":
                return subprocess.CompletedProcess(command, 1, "", "")
            observed.append((command, kwargs["env"]))
            return subprocess.CompletedProcess(command, returncode)
        with patch.object(self.ws, "build_key", return_value="compatible"), patch.object(workspace.subprocess, "run", side_effect=run):
            result = self.ws.cargo(["test", "--locked"], label, 0)
        return result, observed

    def managed(self, name="slot-0", owner="shared"):
        path = self.store / "builds" / name
        path.mkdir(exist_ok=True)
        workspace.write_json(path / "build.json", {
            "version": 1, "owner": owner, "key": "compatible", "last_used": 0,
        })
        (path / "artifact").write_text("disposable")
        return path

    def test_linked_worktrees_resolve_one_default_storage_root(self):
        linked = self.base / "linked checkout"
        self.git("worktree", "add", "-qb", "linked", str(linked))
        self.assertEqual(workspace.Workspace(self.repo).root, workspace.Workspace(linked).root)
        self.assertEqual(workspace.Workspace(linked).root, self.repo / ".mudcrab-local")

    def test_read_only_paths_do_not_initialize_storage(self):
        other = self.base / "not created"
        self.assertEqual(workspace.main(["--repo", str(self.repo), "--root", str(other), "paths"]), 0)
        self.assertFalse(other.exists())

    def test_reject_git_or_checkout_as_storage(self):
        for root in (self.repo, self.repo / ".git", self.repo / ".git" / "objects", self.repo.parent):
            with self.subTest(root=root), self.assertRaises(ValueError):
                workspace.Workspace(self.repo, root)

    def test_storage_marker_cannot_be_claimed_by_another_repo(self):
        workspace.write_json(self.store / "workspace.json", {"version": 1, "git_common_dir": "another repository"})
        with self.assertRaises(ValueError):
            self.ws.setup()

    def test_builds_reuse_slots_and_override_inherited_target(self):
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": "/unwanted/duplicate/target"}):
            _, first = self.run_build()
            _, second = self.run_build()
        self.assertEqual(first[0][1]["CARGO_TARGET_DIR"], second[0][1]["CARGO_TARGET_DIR"])
        self.assertEqual(Path(first[0][1]["CARGO_TARGET_DIR"]), self.store / "builds/slot-0/target")
        self.assertFalse((self.repo / "target").exists())

    def test_busy_slots_refuse_third_build_without_creating_a_directory(self):
        first = self.managed("slot-0")
        self.managed("slot-1")
        with workspace.lease(self.store / "locks/slot-0.lock"):
            _, observed = self.run_build()
            self.assertIn("slot-1", observed[0][1]["CARGO_TARGET_DIR"])
            with workspace.lease(self.store / "locks/slot-1.lock"):
                with self.assertRaisesRegex(ValueError, "slots are busy"):
                    self.run_build()
        self.assertTrue(first.exists())
        self.assertEqual(len(list((self.store / "builds").iterdir())), 2)

    def test_lease_covers_the_entire_child_command(self):
        original_run = subprocess.run
        def child(command, **kwargs):
            if command[0] == "git":
                return original_run(command, **kwargs)
            lease_path = Path(kwargs["env"]["MUDCRAB_BUILD_LEASE"])
            with self.assertRaises(OSError):
                with workspace.lease(self.store / "locks" / f"{lease_path.name}.lock"):
                    pass
            return subprocess.CompletedProcess(command, 0)
        with patch.object(self.ws, "build_key", return_value="compatible"), patch.object(workspace.subprocess, "run", side_effect=child):
            self.ws.run_task(["a-foreground-campaign"], "campaign", 0)

    def test_isolated_builds_separate_tasks_checkouts_and_commits(self):
        _, observed = self.run_build("review")
        first = observed[0][1]
        self.assertEqual(first["CARGO_INCREMENTAL"], "0")
        _, observed = self.run_build("another-task")
        self.assertNotEqual(first["CARGO_TARGET_DIR"], observed[0][1]["CARGO_TARGET_DIR"])
        (self.repo / "new-file").write_text("another commit")
        self.git("add", ".")
        self.git("commit", "-qm", "next head")
        _, observed = self.run_build("review")
        self.assertNotEqual(first["CARGO_TARGET_DIR"], observed[0][1]["CARGO_TARGET_DIR"])
        linked = self.base / "second checkout"
        self.git("worktree", "add", "-qb", "second", str(linked))
        other = workspace.Workspace(linked, self.store)
        self.assertNotEqual(other.candidates("review", "compatible"), self.ws.candidates("review", "compatible"))

    def test_exit_status_propagates_and_lease_is_released(self):
        result, _ = self.run_build(returncode=37)
        self.assertEqual(result, 37)
        with workspace.lease(self.store / "locks/slot-0.lock"):
            pass

    def test_output_overrides_and_clean_are_rejected(self):
        for args in (["clean"], ["test", "--target-dir=/elsewhere"], ["test", "--config", "build.target-dir='elsewhere'"], ["test", "--manifest-path=other/Cargo.toml"], ["build", "--artifact-dir=/elsewhere"]):
            with self.subTest(args=args), self.assertRaises(ValueError):
                self.ws.cargo(args, None, 0)

    def test_process_exit_releases_lease_without_stale_pid_cleanup(self):
        code = '''import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("workspace", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with module.lease(pathlib.Path(sys.argv[2])):
    print("locked", flush=True)
    sys.stdin.read(1)
'''
        path = self.store / "locks/process.lock"
        child = subprocess.Popen([sys.executable, "-c", code, str(SCRIPT), str(path)],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            self.assertEqual(child.stdout.readline().strip(), "locked")
            with self.assertRaises(OSError):
                with workspace.lease(path):
                    pass
            child.communicate("x", timeout=5)
            self.assertEqual(child.returncode, 0)
            with workspace.lease(path):
                pass
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate()

    def test_invalid_task_labels_cannot_escape_build_root(self):
        for name in ("../outside", "/absolute", "..", "a/b"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.ws.candidates(name, "compatible")

    def test_preflight_checks_free_space_and_build_budget(self):
        usage = shutil.disk_usage(self.store)
        with patch.dict(os.environ, {"MUDCRAB_RESERVE_GIB": "20"}), \
                patch.object(workspace.shutil, "disk_usage", return_value=usage._replace(free=20 * workspace.GIB)):
            self.ws.preflight(0)
            with self.assertRaisesRegex(ValueError, "free space"):
                self.ws.preflight(1)
        self.managed()
        with patch.dict(os.environ, {"MUDCRAB_BUILD_BUDGET_GIB": "0"}):
            with self.assertRaisesRegex(ValueError, "budget exceeded"):
                self.ws.preflight(0)
            self.ws.preflight(0, self.base / "new pack")
        for amount in (float("nan"), float("inf"), -1):
            with self.subTest(amount=amount), self.assertRaises(ValueError):
                self.ws.preflight(amount)

    def test_inventory_deduplicates_hardlinks_and_skips_symlinks(self):
        original = self.base / "original"
        original.write_bytes(b"12345")
        os.link(original, self.base / "hardlink")
        (self.base / "symlink").symlink_to(original)
        seen = set()
        first = workspace.inventory(original, seen)
        second = workspace.inventory(self.base / "hardlink", seen)
        self.assertEqual(first["logical_bytes"] + second["logical_bytes"], 5)
        self.assertEqual(workspace.inventory(self.base / "symlink")["symlinks"], 1)

    def test_audit_includes_legacy_targets_in_linked_checkouts(self):
        linked = self.base / "nested temp checkout"
        self.git("worktree", "add", "-qb", "audit", str(linked))
        (linked / "target").mkdir()
        (linked / "target/payload").write_text("legacy")
        paths = {row["path"] for row in self.ws.audit()["artifacts"]}
        self.assertIn(str(linked / "target"), paths)

    def test_prune_defaults_to_dry_run_and_ignores_assets_legacy_and_unowned(self):
        build = self.managed()
        (self.store / "builds/unowned").mkdir()
        pack = self.store / "packs/keep"
        pack.write_text("pack")
        (self.repo / "target").mkdir()
        rows = self.ws.prune(7, False)
        self.assertEqual(rows, [{"path": str(build), "status": "candidate"}])
        self.assertTrue(build.exists())
        self.assertTrue(pack.exists())
        self.assertTrue((self.repo / "target").exists())

    def test_prune_preserves_active_build_and_missing_consumer_check(self):
        build = self.managed()
        with workspace.lease(self.store / "locks/slot-0.lock"):
            self.assertEqual(self.ws.prune(7, True)[0]["status"], "held: active lease")
        with patch.object(workspace.shutil, "which", return_value=None):
            self.assertEqual(self.ws.prune(7, True)[0]["status"], "held: lsof unavailable")
        self.assertTrue(build.exists())

    def test_prune_preserves_open_files_and_failed_consumer_checks(self):
        build = self.managed()
        for result in (subprocess.CompletedProcess([], 0, "open file", ""), subprocess.CompletedProcess([], 1, "", "permission denied")):
            with patch.object(workspace.shutil, "which", return_value="lsof"), patch.object(workspace.subprocess, "run", return_value=result):
                self.assertIn("held:", self.ws.prune(7, True)[0]["status"])
                self.assertTrue(build.exists())

    def test_prune_rechecks_age_after_acquiring_lease(self):
        build = self.managed()
        original_read = workspace.read_json
        reads = 0
        def read(path):
            nonlocal reads
            data = original_read(path)
            if path == build / "build.json":
                reads += 1
                if reads > 1:
                    data["last_used"] = time.time()
            return data
        with patch.object(workspace, "read_json", side_effect=read):
            self.assertEqual(self.ws.prune(7, True)[0]["status"], "held: recently used")
        self.assertTrue(build.exists())

    def test_prune_saves_receipt_and_only_removes_idle_owned_build(self):
        build = self.managed()
        idle = subprocess.CompletedProcess([], 1, "", "")
        with patch.object(workspace.shutil, "which", return_value="lsof"), patch.object(workspace.subprocess, "run", return_value=idle):
            self.assertEqual(self.ws.prune(7, True)[0]["status"], "removed")
        self.assertFalse(build.exists())
        receipt = workspace.read_json(next((self.store / "evidence").glob("retired-*.json")))
        self.assertIn("free_before", receipt)
        self.assertIn("free_after", receipt)

    def test_prune_refuses_nested_symlinks(self):
        build = self.managed()
        external = self.base / "retained"
        external.mkdir()
        (build / "outside").symlink_to(external, target_is_directory=True)
        idle = subprocess.CompletedProcess([], 1, "", "")
        with patch.object(workspace.shutil, "which", return_value="lsof"), patch.object(workspace.subprocess, "run", return_value=idle):
            self.assertEqual(self.ws.prune(7, True)[0]["status"], "held: scan errors or symlinks")
        self.assertTrue(external.is_dir())

    def test_settle_only_lists_this_checkout_isolated_builds(self):
        self.managed()
        own = self.managed(f"isolated-{self.ws.owner}-review", self.ws.owner)
        self.managed("isolated-0000000000000000-review", "0000000000000000")
        self.assertEqual(self.ws.prune(0, False, True), [{"path": str(own), "status": "candidate"}])

    def test_pack_registration_references_without_copying_and_rejects_changes(self):
        pack = self.base / "existing pack"
        pack.mkdir()
        workspace.write_json(pack / "conversion-manifest.json", {"complete": True})
        workspace.write_json(pack / "integration-report.json", {"passed": True})
        self.ws.register_pack("baseline", pack)
        self.assertEqual(self.ws.pack_path("baseline"), pack)
        self.assertEqual(list((self.store / "packs").iterdir()), [])
        workspace.write_json(pack / "conversion-manifest.json", {"complete": True, "changed": True})
        with self.assertRaises(ValueError):
            self.ws.pack_path("baseline")
        with self.assertRaises(ValueError):
            self.ws.register_pack("baseline", pack)

    def test_incomplete_packs_are_not_registered(self):
        pack = self.base / "incomplete"
        pack.mkdir()
        workspace.write_json(pack / "conversion-manifest.json", {"complete": False})
        with self.assertRaises(ValueError):
            self.ws.register_pack("incomplete", pack)

    def test_bootstrap_never_overwrites_existing_agent_instructions(self):
        (self.store / "bin/AGENTS.md").write_text("storage policy")
        self.ws.bootstrap_guidance()
        self.assertEqual((self.repo / "AGENTS.md").read_text(), "storage policy")
        (self.repo / "AGENTS.md").write_text("existing instructions")
        self.ws.bootstrap_guidance()
        self.assertEqual((self.repo / "AGENTS.md").read_text(), "existing instructions")

    def test_installed_git_hook_bootstraps_a_branch_without_tracked_guidance(self):
        (self.repo / "AGENTS.md").write_text("shared storage policy")
        guide = self.repo / "docs/contributing/storage.md"
        guide.parent.mkdir(parents=True)
        guide.write_text("storage guide")
        self.ws.install(git_hook=True)
        linked = self.base / "older checkout"
        self.git("worktree", "add", "-qb", "bootstrap", str(linked))
        self.assertEqual((linked / "AGENTS.md").read_text(), "shared storage policy")
        hook = self.ws.common / "hooks/post-checkout"
        self.assertTrue(os.access(hook, os.X_OK))
        self.assertIn(str(self.store), hook.read_text())

    def test_git_hook_install_preserves_existing_hooks_and_hook_managers(self):
        hook = self.ws.common / "hooks/post-checkout"
        hook.write_text("#!/bin/sh\necho existing hook\n")
        with self.assertRaisesRegex(ValueError, "preserved"):
            self.ws.install_git_hook()
        self.assertIn("existing hook", hook.read_text())
        hook.unlink()
        self.git("config", "core.hooksPath", str(self.base / "hook manager"))
        with self.assertRaisesRegex(ValueError, "hook manager"):
            self.ws.install_git_hook()
        self.assertFalse(hook.exists())


    def hold_briefly(self, name, seconds=0.6):
        """Hold a lease from a helper thread, so the caller must wait for it."""
        held = threading.Event()
        def hold():
            with workspace.lease(self.store / "locks" / name):
                held.set()
                time.sleep(seconds)
        thread = threading.Thread(target=hold)
        thread.start()
        self.addCleanup(thread.join)
        self.assertTrue(held.wait(5))

    def test_a_second_launch_waits_for_the_allocation_lock(self):
        self.hold_briefly("allocation.lock")
        result, observed = self.run_build()
        self.assertEqual(result, 0)
        self.assertEqual(len(observed), 1)

    def test_pack_registration_waits_for_the_registry_lock(self):
        pack = self.base / "waiting pack"
        pack.mkdir()
        (pack / "conversion-manifest.json").write_text('{"complete": true}')
        (pack / "integration-report.json").write_text('{"passed": true}')
        self.hold_briefly("registry.lock")
        self.ws.register_pack("waiting", pack)
        self.assertEqual(self.ws.pack_path("waiting"), pack.resolve())

    def test_a_failed_first_marker_write_leaves_no_directory(self):
        original = workspace.write_json
        def fail(path, data):
            if path.name == "build.json":
                raise OSError("disk full")
            return original(path, data)
        with patch.object(workspace, "write_json", side_effect=fail):
            with self.assertRaisesRegex(OSError, "disk full"):
                self.run_build("fresh")
        self.assertEqual([p.name for p in (self.store / "builds").iterdir()], [])
        result, _ = self.run_build("fresh")
        self.assertEqual(result, 0)

    def test_write_json_removes_its_temporary_file_when_the_disk_fills(self):
        target = self.base / "data.json"
        def full(self_path, *args, **kwargs):
            self_path.touch()
            raise OSError(28, "No space left on device")
        with patch.object(Path, "write_text", full):
            with self.assertRaises(OSError):
                workspace.write_json(target, {"a": 1})
        self.assertEqual([p.name for p in self.base.glob("data.json*")], [])

    def test_a_disk_full_first_marker_write_leaves_no_directory(self):
        def full(self_path, *args, **kwargs):
            self_path.touch()
            raise OSError(28, "No space left on device")
        with patch.object(Path, "write_text", full):
            with self.assertRaises(OSError):
                self.run_build("fresh")
        self.assertEqual([p.name for p in (self.store / "builds").iterdir()], [])

    def test_a_failed_final_marker_update_keeps_the_exit_status(self):
        original = workspace.write_json
        calls = []
        def fail_second(path, data):
            calls.append(path.name)
            if path.name == "build.json" and calls.count("build.json") == 2:
                raise OSError("disk full")
            return original(path, data)
        with patch.object(workspace, "write_json", side_effect=fail_second):
            result, _ = self.run_build("campaign", returncode=37)
        self.assertEqual(result, 37)
        self.assertEqual(len(list((self.store / "builds").iterdir())), 1)

    def test_install_skips_a_missing_agent_file_and_copies_the_tool_last(self):
        guide = self.repo / "docs/contributing/storage.md"
        guide.parent.mkdir(parents=True)
        guide.write_text("guide")
        self.assertFalse((self.repo / "AGENTS.md").exists())
        self.ws.install()
        installed = self.store / "bin"
        self.assertEqual((installed / "storage.md").read_text(), "guide")
        self.assertTrue((installed / "workspace.py").is_file())
        self.assertFalse((installed / "AGENTS.md").exists())


if __name__ == "__main__":
    unittest.main()
