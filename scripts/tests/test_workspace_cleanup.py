"""Tests for automatic build cleanup in workspace.py (deletion logic: every rule fails closed)."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "workspace.py"
SPEC = importlib.util.spec_from_file_location("workspace", SCRIPT)
workspace = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(workspace)

REAL_LSOF = shutil.which("lsof")
IDLE = subprocess.CompletedProcess([], 1, "", "")


class CleanupCase(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.repo = self.base / "repo"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Cleanup Tests")
        self.git("config", "user.email", "cleanup@example.invalid")
        (self.repo / "Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.store = self.base / "storage"
        self.ws = workspace.Workspace(self.repo, self.store)
        self.ws.setup()
        self.handles = []
        self.addCleanup(lambda: [handle.close() for handle in self.handles])
        self.real_lsof = False
        self.env = patch.dict(os.environ, {"MUDCRAB_RESERVE_GIB": "0"})
        self.env.start()
        self.addCleanup(self.env.stop)

    def git(self, *args):
        return workspace.git(self.repo, *args)

    @property
    def builds(self):
        return self.store / "builds"

    def head(self):
        return self.git("rev-parse", "HEAD")[:12]

    def isolated(self, label, head=None):
        return self.builds / f"isolated-{self.ws.owner}-{label}-{head or self.head()}"

    def make_build(self, name, owner="shared", last_used=0.0, version=1, size=16, deliverable=False):
        """`deliverable=None` writes the old-script form of build.json: no `deliverable` key."""
        path = self.builds / name
        path.mkdir(exist_ok=True)
        marker = {"version": version, "owner": owner, "key": "compatible", "last_used": last_used}
        if deliverable is not None:
            marker["deliverable"] = deliverable
        workspace.write_json(path / "build.json", marker)
        (path / "artifact").write_bytes(b"x" * size)
        return path

    def make_isolated(self, label, head="aaaaaaaaaaaa", owner=None, last_used=0.0, deliverable=False):
        owner = owner or self.ws.owner
        return self.make_build(f"isolated-{owner}-{label}-{head}", owner, last_used, deliverable=deliverable)

    def receipts(self):
        path = self.store / "evidence" / "cleanup-receipts.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text().splitlines()]

    def fake_run(self, returncode=0, writer=None, cargo_calls=None):
        original = subprocess.run
        def run(command, **kwargs):
            if command[0] == "git":
                return original(command, **kwargs)
            if os.path.basename(str(command[0])) == "lsof":
                return original(command, **kwargs) if self.real_lsof else IDLE
            if cargo_calls is not None:
                cargo_calls.append(command)
            if writer:
                writer(Path(kwargs["env"]["CARGO_TARGET_DIR"]))
            return subprocess.CompletedProcess(command, returncode)
        return run

    def run_cargo(self, args=("check", "--locked"), label=None, returncode=0, writer=None, **options):
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(self.ws, "build_key", return_value="compatible"))
            if not self.real_lsof:
                stack.enter_context(patch.object(workspace.shutil, "which", return_value="lsof"))
            stack.enter_context(patch.object(workspace.subprocess, "run", side_effect=self.fake_run(returncode, writer)))
            captured = stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            result = self.ws.cargo(list(args), label, 0, **options)
        self.stderr = captured.getvalue()
        return result

    def run_exec(self, label, returncode=0):
        with patch.object(self.ws, "build_key", return_value="compatible"), \
                patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", side_effect=self.fake_run(returncode)):
            with contextlib.redirect_stderr(io.StringIO()):
                return self.ws.run_task(["a-campaign"], label, 0)

    def retire(self, path, reason="manual", **options):
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            return self.ws.retire(path, reason, **options)


class IsolatedRetireOnSuccess(CleanupCase):
    def test_successful_check_removes_its_own_isolated_build_and_writes_one_receipt_line(self):
        self.run_cargo(("check", "--locked"), "review")
        self.assertFalse(self.isolated("review").exists())
        lines = (self.store / "evidence" / "cleanup-receipts.jsonl").read_text().splitlines()
        self.assertEqual(len(lines), 1)
        receipt = json.loads(lines[0])
        self.assertEqual(receipt["reason"], "on-success")
        self.assertEqual(receipt["status"], "removed")
        self.assertEqual(receipt["path"], str(self.isolated("review")))
        self.assertIsInstance(receipt["bytes"], int)
        for check in ("lease", "no-open-files", "no-pending-receipt", "unpinned", "tree-scan"):
            self.assertIn(check, receipt["checks"])

    def test_each_no_deliverable_subcommand_retires_the_build(self):
        for subcommand in ("check", "clippy", "fmt", "test", "nextest"):
            args = ("nextest", "run") if subcommand == "nextest" else (subcommand,)
            with self.subTest(subcommand=subcommand):
                self.run_cargo(args, "review")
                self.assertFalse(self.isolated("review").exists())

    def test_failed_command_keeps_the_build_as_evidence(self):
        self.assertEqual(self.run_cargo(("test",), "review", returncode=101), 101)
        self.assertTrue(self.isolated("review").exists())
        self.assertEqual(self.receipts(), [])

    def test_deliverable_commands_keep_the_build(self):
        for args in (("build", "--release"), ("run",), ("test", "--no-run"), ("doc",), ("doc", "--open"),
                     ("nextest", "archive"), ("nextest", "list")):
            with self.subTest(args=args):
                self.run_cargo(args, "review")
                self.assertTrue(self.isolated("review").exists())
                shutil.rmtree(self.isolated("review"))

    def test_exec_campaigns_never_retire_the_build(self):
        self.run_exec("campaign")
        self.assertTrue(self.isolated("campaign").exists())

    def test_keep_flag_pins_the_build_and_the_pin_survives_a_later_prune(self):
        self.run_cargo(("check",), "review", keep=True)
        build = self.isolated("review")
        self.assertTrue(build.exists())
        self.assertTrue((self.store / "pins" / f"{build.name}.json").is_file())
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            rows = self.ws.prune(0, True)
        self.assertEqual([row["status"] for row in rows if row["path"] == str(build)], ["held: pinned"])
        self.assertTrue(build.exists())

    def test_an_expired_pin_no_longer_protects(self):
        build = self.make_isolated("review")
        self.ws.pin(build.name, "test", days=1, now=time.time() - 3 * 86400)
        self.assertEqual(self.retire(build, leased=False)["status"], "removed")

    def test_keep_list_label_is_never_retired(self):
        with patch.dict(os.environ, {"MUDCRAB_KEEP_LABELS": "other, review"}):
            self.run_cargo(("check",), "review")
            self.assertTrue(self.isolated("review").exists())
            self.assertEqual(self.retire(self.isolated("review"))["status"], "held: label is in the keep list")
        self.run_cargo(("check",), "review")
        self.assertFalse(self.isolated("review").exists())

    def test_a_real_open_file_blocks_retirement(self):
        if not REAL_LSOF:
            self.skipTest("lsof is not installed")
        self.real_lsof = True
        def writer(target):
            target.mkdir(parents=True, exist_ok=True)
            self.handles.append(open(target / "held.bin", "wb"))
        self.assertEqual(self.run_cargo(("check",), "review", writer=writer), 0)
        self.assertTrue(self.isolated("review").exists())
        self.assertEqual(self.receipts(), [])

    def test_open_file_and_failed_consumer_checks_hold_the_build(self):
        build = self.make_isolated("review")
        for result in (subprocess.CompletedProcess([], 0, "node 1 open", ""),
                       subprocess.CompletedProcess([], 1, "", "permission denied"),
                       subprocess.CompletedProcess([], 2, "", "")):
            with patch.object(workspace.shutil, "which", return_value="lsof"), \
                    patch.object(workspace.subprocess, "run", return_value=result):
                self.assertEqual(self.ws.retire(build, "manual")["status"], "held: open files or consumer check failed")
        with patch.object(workspace.shutil, "which", return_value=None):
            self.assertEqual(self.ws.retire(build, "manual")["status"], "held: lsof unavailable")
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", side_effect=subprocess.TimeoutExpired("lsof", 1)):
            self.assertEqual(self.ws.retire(build, "manual")["status"], "held: consumer check timed out")
        self.assertTrue(build.exists())

    def test_a_cleanup_failure_never_changes_the_command_result(self):
        with patch.object(workspace.Workspace, "retire", side_effect=RuntimeError("boom")):
            self.assertEqual(self.run_cargo(("check",), "review"), 0)
        self.assertIn("cleanup skipped", self.stderr)
        self.assertTrue(self.isolated("review").exists())

    def test_concurrent_lease_holder_blocks_retirement(self):
        build = self.make_isolated("review")
        with workspace.lease(self.store / "locks" / f"{build.name}.lock"):
            row = self.retire(build)
        self.assertEqual(row["status"], "held: active lease")
        self.assertTrue(build.exists())

    def test_the_lease_is_still_held_while_the_own_build_is_retired(self):
        seen = []
        original = workspace.Workspace.retire
        def spy(ws, path, reason, **kwargs):
            try:
                with workspace.lease(ws.root / "locks" / f"{path.name}.lock"):
                    seen.append("lease was free")
            except OSError:
                seen.append("lease held")
            return original(ws, path, reason, **kwargs)
        with patch.object(workspace.Workspace, "retire", spy):
            self.run_cargo(("check",), "review")
        self.assertEqual(seen, ["lease held"])


class RetireGuards(CleanupCase):
    def test_symlink_inside_the_build_blocks_retirement(self):
        build = self.make_isolated("review")
        external = self.base / "precious"
        external.mkdir()
        (external / "data").write_text("keep")
        (build / "link").symlink_to(external, target_is_directory=True)
        self.assertEqual(self.retire(build)["status"], "held: scan errors or symlinks")
        self.assertTrue(build.exists())
        self.assertEqual((external / "data").read_text(), "keep")

    def test_symlinked_build_directory_is_never_followed(self):
        external = self.base / "precious"
        external.mkdir()
        (external / "data").write_text("keep")
        (external / "build.json").write_text("{}")
        link = self.builds / f"isolated-{self.ws.owner}-review-aaaaaaaaaaaa"
        link.symlink_to(external, target_is_directory=True)
        self.assertEqual(self.retire(link)["status"], "held: symlink")
        self.assertEqual((external / "data").read_text(), "keep")

    def test_paths_outside_the_builds_root_are_refused(self):
        outside = self.store / "packs" / f"isolated-{self.ws.owner}-review-aaaaaaaaaaaa"
        outside.mkdir()
        workspace.write_json(outside / "build.json", {"version": 1, "owner": self.ws.owner, "last_used": 0})
        elsewhere = self.base / f"isolated-{self.ws.owner}-review-bbbbbbbbbbbb"
        elsewhere.mkdir()
        workspace.write_json(elsewhere / "build.json", {"version": 1, "owner": self.ws.owner, "last_used": 0})
        nested = self.make_isolated("review") / "target"
        nested.mkdir()
        for path in (outside, elsewhere, nested, self.builds, self.store, self.store / "evidence", self.store / "scratch"):
            with self.subTest(path=path):
                row = self.retire(path)
                self.assertTrue(row["status"].startswith("held:"), row)
                self.assertTrue(path.exists())
        self.assertEqual(self.receipts(), [])

    def test_directories_that_only_look_like_builds_are_not_tool_owned(self):
        no_marker = self.builds / f"isolated-{self.ws.owner}-review-aaaaaaaaaaaa"
        no_marker.mkdir()
        (no_marker / "user-file").write_text("not ours")
        wrong_version = self.make_build(f"isolated-{self.ws.owner}-other-aaaaaaaaaaaa", self.ws.owner, version=2)
        bad_name = self.make_build("isolated-xyz-review-aaaaaaaaaaaa")
        slot_like = self.make_build("slot-2")
        corrupt = self.builds / f"isolated-{self.ws.owner}-corrupt-aaaaaaaaaaaa"
        corrupt.mkdir()
        (corrupt / "build.json").write_text("{not json")
        marker_link = self.builds / f"isolated-{self.ws.owner}-linked-aaaaaaaaaaaa"
        marker_link.mkdir()
        (self.base / "marker.json").write_text(json.dumps({"version": 1, "owner": self.ws.owner, "last_used": 0}))
        (marker_link / "build.json").symlink_to(self.base / "marker.json")
        for path in (no_marker, wrong_version, bad_name, slot_like, corrupt, marker_link):
            with self.subTest(path=path.name):
                self.assertTrue(self.retire(path)["status"].startswith("held:"))
                self.assertTrue(path.exists())
        self.assertTrue((no_marker / "user-file").exists())

    def test_another_owner_is_skipped_when_owner_only_is_set(self):
        other = self.make_isolated("review", owner="0000000000000000")
        self.assertEqual(self.retire(other, owner_only=True)["status"], "held: another owner")
        self.assertTrue(other.exists())

    def test_pending_receipt_blocks_and_final_receipt_allows(self):
        build = self.make_isolated("review")
        folder = self.store / "evidence" / "task"
        folder.mkdir()
        receipt = folder / "build-receipt.json"
        for body in ({"state": "pending", "builds": [build.name]},
                     {"builds": [str(build)]},
                     {"state": "retained", "builds": [build.name]}):
            workspace.write_json(receipt, body)
            self.assertEqual(self.retire(build)["status"], "held: named in a pending receipt", body)
        receipt.write_text("{broken")
        self.assertEqual(self.retire(build)["status"], "held: receipt check failed")
        workspace.write_json(receipt, {"state": "pending", "builds": ["some-other-build"]})
        workspace.write_json(folder / "other.json", {"state": "pending", "builds": [build.name]})
        workspace.write_json(receipt, {"state": "final", "builds": [build.name]})
        self.assertEqual(self.retire(build)["status"], "removed")

    def test_recently_used_builds_are_held_when_an_idle_time_is_required(self):
        build = self.make_isolated("review", last_used=time.time())
        self.assertEqual(self.retire(build, min_idle_seconds=1800)["status"], "held: recently used")
        self.assertTrue(build.exists())

    def test_removal_is_a_rename_then_delete_and_never_touches_siblings(self):
        build = self.make_isolated("review")
        other = self.make_isolated("other")
        pack = self.store / "packs" / "keep"
        pack.mkdir()
        (pack / "data").write_text("pack")
        scratch = self.store / "scratch" / "task"
        scratch.mkdir()
        self.assertEqual(self.retire(build)["status"], "removed")
        self.assertFalse(build.exists())
        self.assertEqual(list(self.builds.glob(".retiring-*")), [])
        self.assertTrue(other.exists() and pack.exists() and scratch.exists())
        self.assertEqual([row["path"] for row in self.receipts()], [str(build)])
        full = workspace.read_json(next((self.store / "evidence").glob("retired-*.json")))
        self.assertIn("free_before", full)
        self.assertIn("free_after", full)


class MoreGuards(CleanupCase):
    def test_automatic_paths_never_retire_a_whole_slot(self):
        slot = self.make_build("slot-0")
        self.assertEqual(self.retire(slot)["status"], "held: whole slots are never retired automatically")
        self.assertEqual(self.retire(slot, allow_slot=True)["status"], "removed")

    def test_a_failed_rename_keeps_the_build_and_leaves_no_false_receipt(self):
        build = self.make_isolated("review")
        with patch.object(Path, "rename", side_effect=OSError("busy")):
            row = self.retire(build)
        self.assertTrue(row["status"].startswith("held: could not prepare removal"), row)
        self.assertTrue(build.exists())
        self.assertEqual(list((self.store / "evidence").glob("retired-*.json")), [])
        self.assertEqual(self.receipts(), [])

    def test_a_receipt_that_cannot_be_written_keeps_the_build(self):
        build = self.make_isolated("review")
        with patch.object(workspace, "write_json", side_effect=OSError("disk full")):
            row = self.retire(build)
        self.assertTrue(row["status"].startswith("held:"), row)
        self.assertTrue(build.exists())

    def test_removing_a_build_drops_its_expired_pin(self):
        build = self.make_isolated("review")
        pin = self.ws.pin(build.name, "old", days=1, now=time.time() - 5 * 86400)
        self.assertEqual(self.retire(build)["status"], "removed")
        self.assertFalse(pin.exists())

    def test_a_pin_file_that_is_unreadable_or_a_symlink_holds_the_build(self):
        build = self.make_isolated("review")
        (self.store / "pins").mkdir()
        pin = self.store / "pins" / f"{build.name}.json"
        pin.write_text("{broken")
        self.assertEqual(self.retire(build)["status"], "held: pin check failed")
        pin.unlink()
        pin.symlink_to(self.base / "elsewhere.json")
        self.assertEqual(self.retire(build)["status"], "held: pin check failed")
        pin.unlink()
        workspace.write_json(pin, {"reason": "no expiry"})
        self.assertEqual(self.retire(build)["status"], "held: pinned")
        self.assertTrue(build.exists())


class CrashDuringDelete(CleanupCase):
    def partial_rmtree(self, error):
        original = shutil.rmtree
        def crash(path, *args, **kwargs):
            victim = next(Path(path).rglob("artifact"), None)
            if victim:
                victim.unlink()
            raise error
        return patch.object(workspace.shutil, "rmtree", side_effect=crash), original

    def test_an_error_during_delete_leaves_a_recoverable_retiring_directory(self):
        build = self.make_isolated("review")
        patcher, _ = self.partial_rmtree(OSError("disk error"))
        with patcher:
            row = self.retire(build)
        self.assertEqual(row["status"], "held: removal incomplete")
        self.assertFalse(build.exists())
        leftovers = list(self.builds.glob(".retiring-*"))
        self.assertEqual(len(leftovers), 1)
        self.assertEqual(self.receipts()[-1]["status"], "removal incomplete")
        # A new build for the same label is not blocked by the leftover.
        self.make_isolated("review")
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            rows = self.ws.recover_interrupted()
        self.assertEqual([row["status"] for row in rows], ["removed"])
        self.assertEqual(list(self.builds.glob(".retiring-*")), [])
        self.assertTrue(self.isolated("review", "aaaaaaaaaaaa").exists())

    def test_a_killed_process_leaves_the_same_recoverable_state(self):
        build = self.make_isolated("review")
        patcher, _ = self.partial_rmtree(KeyboardInterrupt())
        with patcher, self.assertRaises(KeyboardInterrupt):
            self.retire(build)
        self.assertFalse(build.exists())
        self.assertEqual(len(list(self.builds.glob(".retiring-*"))), 1)
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            self.assertEqual([row["status"] for row in self.ws.recover_interrupted()], ["removed"])
        self.assertEqual(list(self.builds.glob(".retiring-*")), [])

    def test_recovery_skips_leased_open_unrecognized_and_symlinked_leftovers(self):
        held = self.builds / f".retiring-isolated-{self.ws.owner}-a-aaaaaaaaaaaa-1"
        held.mkdir()
        opened = self.builds / f".retiring-isolated-{self.ws.owner}-b-aaaaaaaaaaaa-2"
        opened.mkdir()
        foreign = self.builds / ".retiring-not-ours"
        foreign.mkdir()
        (foreign / "data").write_text("keep")
        external = self.base / "precious"
        external.mkdir()
        linked = self.builds / f".retiring-isolated-{self.ws.owner}-c-aaaaaaaaaaaa-3"
        linked.symlink_to(external, target_is_directory=True)
        nested = self.builds / f".retiring-isolated-{self.ws.owner}-d-aaaaaaaaaaaa-4"
        nested.mkdir()
        (nested / "link").symlink_to(external, target_is_directory=True)
        for proven in (held, opened, nested):
            workspace.write_json(proven / "build.json", {"version": 1})
        with workspace.lease(self.store / "locks" / f"{held.name}.lock"):
            def run(command, **kwargs):
                return subprocess.CompletedProcess(command, 0 if str(opened) in command else 1, "open" if str(opened) in command else "", "")
            with patch.object(workspace.shutil, "which", return_value="lsof"), \
                    patch.object(workspace.subprocess, "run", side_effect=run):
                rows = self.ws.recover_interrupted()
        statuses = {Path(row["path"]).name: row["status"] for row in rows}
        self.assertTrue(all(status.startswith("held:") for status in statuses.values()), statuses)
        for path in (held, opened, foreign, nested):
            self.assertTrue(path.exists())
        self.assertTrue(linked.is_symlink() and external.is_dir())
        self.assertNotIn(".retiring-not-ours", statuses)
        self.assertEqual(statuses[held.name], "held: active lease")
        self.assertIn("open", statuses[opened.name])
        self.assertEqual(statuses[nested.name], "held: scan errors or symlinks")

    def test_old_unreadable_or_symlinked_receipt_cannot_authorize_recovery(self):
        leftover = self.builds / ".retiring-slot-0-1"
        leftover.mkdir()
        personal = leftover / "data"
        personal.write_bytes(b"keep")
        receipt = self.store / "evidence" / "retired-slot-0-1.json"
        external = self.base / "external.json"
        workspace.write_json(external, {"leftovers": [leftover.name]})
        with patch.object(self.ws, "consumer_hold", return_value=None):
            for contents in ('{"inventory": {}}', 'invalid JSON', None):
                with self.subTest(contents=contents):
                    if contents is None:
                        receipt.symlink_to(external)
                    else:
                        receipt.write_text(contents)
                    self.assertTrue(self.ws.recover_interrupted()[0]["status"].startswith("held:"))
                    self.assertEqual(personal.read_bytes(), b"keep")
                    receipt.unlink()

    def test_recovery_runs_before_a_prune_and_before_a_build(self):
        leftover = self.builds / f".retiring-isolated-{self.ws.owner}-a-aaaaaaaaaaaa-1"
        leftover.mkdir()
        workspace.write_json(leftover / "build.json", {"version": 1})
        (leftover / "artifact").write_text("x")
        self.run_cargo(("build",), "review")
        self.assertFalse(leftover.exists())


class SlotIncrementalTrim(CleanupCase):
    def layout(self, target):
        for profile in ("debug", "release"):
            incremental = target / profile / "incremental" / "crate-abc"
            incremental.mkdir(parents=True, exist_ok=True)
            (incremental / "state.bin").write_bytes(b"x" * 65536)
        (target / "debug" / "deps").mkdir(parents=True, exist_ok=True)
        (target / "debug" / "deps" / "libkeep.rlib").write_bytes(b"deps")
        (target / "debug" / "build" / "incremental").mkdir(parents=True, exist_ok=True)
        (target / "debug" / "build" / "incremental" / "decoy").write_text("decoy")
        (target / "incremental").mkdir(exist_ok=True)
        (target / "incremental" / "decoy").write_text("decoy")

    def trimmed(self, **env):
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001", **env}):
            self.run_cargo(("test",), None, writer=self.layout)
        return self.builds / "slot-0" / "target"

    def test_default_cap_is_eight_gib(self):
        self.assertEqual(workspace.SLOT_INCREMENTAL_GIB, 8)

    def test_releasing_a_slot_over_the_cap_trims_only_the_incremental_directories(self):
        target = self.trimmed()
        self.assertFalse((target / "debug" / "incremental").exists())
        self.assertFalse((target / "release" / "incremental").exists())
        for kept in (target / "debug" / "deps" / "libkeep.rlib", target / "debug" / "build" / "incremental" / "decoy",
                     target / "incremental" / "decoy", self.builds / "slot-0" / "build.json"):
            self.assertTrue(kept.exists(), kept)
        self.assertEqual(list(target.rglob(".trim-*")), [])
        receipt = self.receipts()[-1]
        self.assertEqual(receipt["reason"], "slot-incremental")
        self.assertGreater(receipt["bytes"], 100000)

    def test_a_slot_under_the_cap_or_with_the_cap_disabled_is_left_alone(self):
        for value in ("1", "0"):
            with self.subTest(cap=value):
                shutil.rmtree(self.builds, ignore_errors=True)
                self.builds.mkdir()
                target = self.trimmed(MUDCRAB_SLOT_INCREMENTAL_GIB=value)
                self.assertTrue((target / "debug" / "incremental").exists())
        self.assertEqual(self.receipts(), [])

    def test_bad_cap_values_disable_trimming(self):
        for value in ("nan", "-1", "inf", "many"):
            with self.subTest(cap=value):
                shutil.rmtree(self.builds, ignore_errors=True)
                self.builds.mkdir()
                target = self.trimmed(MUDCRAB_SLOT_INCREMENTAL_GIB=value)
                self.assertTrue((target / "debug" / "incremental").exists())

    def test_a_failed_command_still_trims_regenerable_state(self):
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}):
            self.assertEqual(self.run_cargo(("test",), None, returncode=101, writer=self.layout), 101)
        self.assertFalse((self.builds / "slot-0" / "target" / "debug" / "incremental").exists())

    def test_isolated_builds_are_not_trimmed(self):
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}):
            self.run_cargo(("build",), "review", writer=self.layout)
        self.assertTrue((self.isolated("review") / "target" / "debug" / "incremental").exists())

    def test_a_concurrent_lease_holder_blocks_a_direct_trim(self):
        slot = self.make_build("slot-0")
        self.layout(slot / "target")
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}):
            with workspace.lease(self.store / "locks" / "slot-0.lock"):
                with patch.object(workspace.shutil, "which", return_value="lsof"), \
                        patch.object(workspace.subprocess, "run", return_value=IDLE):
                    row = self.ws.trim_slot_incremental(slot, leased=False)
            self.assertEqual(row["status"], "held: active lease")
            self.assertTrue((slot / "target" / "debug" / "incremental").exists())
            with patch.object(workspace.shutil, "which", return_value="lsof"), \
                    patch.object(workspace.subprocess, "run", return_value=IDLE):
                self.assertEqual(self.ws.trim_slot_incremental(slot, leased=False)["status"], "trimmed")

    def test_symlinks_are_never_followed_or_removed_with_content(self):
        slot = self.make_build("slot-0")
        target = slot / "target"
        external = self.base / "precious"
        external.mkdir()
        (external / "data").write_text("keep")
        target.mkdir()
        (target / "debug").mkdir()
        (target / "debug" / "incremental").symlink_to(external, target_is_directory=True)
        (target / "linked-profile").symlink_to(external, target_is_directory=True)
        nested = target / "release" / "incremental"
        nested.mkdir(parents=True)
        (nested / "big").write_bytes(b"x" * 65536)
        (nested / "link").symlink_to(external, target_is_directory=True)
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}):
            row = self.trim(slot)
        self.assertTrue(row["status"].startswith("held:"), row)
        self.assertEqual((external / "data").read_text(), "keep")
        self.assertTrue((target / "debug" / "incremental").is_symlink())
        self.assertTrue(nested.exists())

    def trim(self, slot):
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            return self.ws.trim_slot_incremental(slot, leased=False)

    def test_an_open_file_in_the_incremental_state_blocks_the_trim(self):
        if not REAL_LSOF:
            self.skipTest("lsof is not installed")
        self.real_lsof = True
        def writer(target):
            self.layout(target)
            self.handles.append(open(target / "debug" / "incremental" / "crate-abc" / "state.bin", "rb"))
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}):
            self.run_cargo(("test",), None, writer=writer)
        self.assertTrue((self.builds / "slot-0" / "target" / "debug" / "incremental").exists())

    def test_a_crash_during_the_trim_is_recovered_by_the_next_pass(self):
        slot = self.make_build("slot-0")
        self.layout(slot / "target")
        def crash(path, *args, **kwargs):
            raise KeyboardInterrupt()
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}), \
                patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE), \
                patch.object(workspace.shutil, "rmtree", side_effect=crash), self.assertRaises(KeyboardInterrupt):
            self.ws.trim_slot_incremental(slot, leased=False)
        leftovers = list((slot / "target").rglob(".trim-incremental-*"))
        self.assertGreaterEqual(len(leftovers), 1)
        self.assertFalse((slot / "target" / "debug" / "incremental").exists() and not leftovers)
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            rows = self.ws.recover_interrupted()
        self.assertTrue(all(row["status"] == "removed" for row in rows), rows)
        self.assertEqual(list((slot / "target").rglob(".trim-incremental-*")), [])
        self.assertTrue((slot / "target" / "debug" / "deps" / "libkeep.rlib").exists())


class SupersedeOldHeads(CleanupCase):
    def test_a_successful_test_retires_older_heads_of_the_same_owner_and_label_only(self):
        old = self.make_isolated("review", "aaaaaaaaaaaa")
        other_label = self.make_isolated("review-two", "aaaaaaaaaaaa")
        other_owner = self.make_isolated("review", "aaaaaaaaaaaa", owner="0000000000000000")
        pinned = self.make_isolated("review", "bbbbbbbbbbbb")
        self.ws.pin(pinned.name, "evidence", days=7)
        cited = self.make_isolated("review", "cccccccccccc")
        (self.store / "evidence" / "task").mkdir()
        workspace.write_json(self.store / "evidence" / "task" / "build-receipt.json",
                             {"state": "pending", "builds": [cited.name]})
        leased = self.make_isolated("review", "dddddddddddd")
        with workspace.lease(self.store / "locks" / f"{leased.name}.lock"):
            self.run_cargo(("test",), "review")
        self.assertFalse(old.exists())
        for kept in (other_label, other_owner, pinned, cited, leased):
            self.assertTrue(kept.exists(), kept)
        self.assertEqual([row["reason"] for row in self.receipts()], ["superseded", "on-success"])
        self.assertIn("superseded", self.stderr)

    def test_a_failed_command_does_not_supersede_anything(self):
        old = self.make_isolated("review", "aaaaaaaaaaaa")
        self.run_cargo(("test",), "review", returncode=1)
        self.assertTrue(old.exists())

    def test_a_label_that_ends_in_hex_is_matched_by_its_full_label(self):
        near = self.make_isolated("review-0123456789ab", "aaaaaaaaaaaa")
        self.run_cargo(("test",), "review")
        self.assertTrue(near.exists())

    def test_a_label_in_the_keep_list_keeps_its_old_heads(self):
        old = self.make_isolated("review", "aaaaaaaaaaaa")
        with patch.dict(os.environ, {"MUDCRAB_KEEP_LABELS": "review"}):
            self.run_cargo(("test",), "review")
        self.assertTrue(old.exists())


class TtlAndSettle(CleanupCase):
    def prune(self, *args, **kwargs):
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            return self.ws.prune(*args, **kwargs)

    def test_default_prune_keeps_seven_days_for_every_build_including_other_owners(self):
        now = time.time()
        mine = self.make_isolated("mine", last_used=now - 72 * 3600)
        theirs = self.make_isolated("theirs", owner="0000000000000000", last_used=now - 72 * 3600)
        slot = self.make_build("slot-0", last_used=now - 49 * 3600)
        self.assertEqual(self.prune(None, False), [])
        self.assertEqual(self.prune(None, True), [])
        self.assertTrue(mine.exists() and theirs.exists() and slot.exists())
        old = self.make_isolated("old", head="bbbbbbbbbbbb", last_used=now - 8 * 86400)
        self.assertEqual([r["status"] for r in self.prune(None, True)], ["removed"])
        self.assertFalse(old.exists())
        self.assertEqual([row["reason"] for row in self.receipts()], ["prune"])

    def test_the_isolated_ttl_applies_only_when_a_flag_asks_for_it(self):
        now = time.time()
        old_isolated = self.make_isolated("old", last_used=now - 49 * 3600)
        young_isolated = self.make_isolated("young", head="bbbbbbbbbbbb", last_used=now - 47 * 3600)
        slot = self.make_build("slot-0", last_used=now - 49 * 3600)
        rows = {Path(row["path"]).name: row["status"] for row in self.prune(None, False, isolated_ttl_hours=48)}
        self.assertEqual(rows, {old_isolated.name: "candidate"})
        self.assertTrue(old_isolated.exists() and young_isolated.exists() and slot.exists())
        rows = {Path(row["path"]).name: row["status"] for row in self.prune(None, True, isolated_ttl_hours=48)}
        self.assertEqual(rows, {old_isolated.name: "removed"})
        self.assertTrue(young_isolated.exists() and slot.exists())
        self.assertEqual([row["reason"] for row in self.receipts()], ["ttl"])

    def test_the_ttl_flag_refuses_values_that_would_widen_deletion_or_make_no_sense(self):
        for value in (0, 0.5, -1, float("nan"), float("inf")):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.prune(None, False, isolated_ttl_hours=value)

    def test_the_old_ttl_environment_variable_changes_nothing(self):
        build = self.make_isolated("old", last_used=time.time() - 72 * 3600)
        with patch.dict(os.environ, {"MUDCRAB_ISOLATED_TTL_HOURS": "1"}):
            self.assertEqual(self.prune(None, True), [])
        self.assertTrue(build.exists())

    def test_an_explicit_age_applies_to_every_build_as_before(self):
        now = time.time()
        isolated = self.make_isolated("old", last_used=now - 49 * 3600)
        self.assertEqual(self.prune(7, False), [])
        self.assertEqual(len(self.prune(1, False)), 1)
        self.assertTrue(isolated.exists())

    def test_prune_is_a_dry_run_unless_apply_is_set(self):
        build = self.make_isolated("old")
        rows = self.prune(None, False)
        self.assertEqual(rows, [{"path": str(build), "status": "candidate"}])
        self.assertTrue(build.exists())
        self.assertEqual(self.receipts(), [])

    def test_prune_holds_pinned_cited_kept_and_leased_builds(self):
        pinned = self.make_isolated("pinned")
        self.ws.pin(pinned.name, "evidence", days=7)
        cited = self.make_isolated("cited")
        (self.store / "evidence" / "task").mkdir()
        workspace.write_json(self.store / "evidence" / "task" / "build-receipt.json",
                             {"state": "pending", "builds": [cited.name]})
        kept = self.make_isolated("kept")
        leased = self.make_isolated("leased")
        with patch.dict(os.environ, {"MUDCRAB_KEEP_LABELS": "kept"}), \
                workspace.lease(self.store / "locks" / f"{leased.name}.lock"):
            rows = {Path(row["path"]).name: row["status"] for row in self.prune(None, True)}
        self.assertEqual(rows, {pinned.name: "held: pinned", cited.name: "held: named in a pending receipt",
                                kept.name: "held: label is in the keep list", leased.name: "held: active lease"})
        for path in (pinned, cited, kept, leased):
            self.assertTrue(path.exists())

    def test_one_slow_consumer_check_does_not_abort_the_rest_of_the_prune(self):
        first = self.make_isolated("first")
        second = self.make_isolated("second", head="bbbbbbbbbbbb")
        calls = []
        def run(command, **kwargs):
            calls.append(command)
            if len(calls) == 1:
                raise subprocess.TimeoutExpired(command, 1)
            return IDLE
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", side_effect=run):
            rows = self.ws.prune(0, True)
        statuses = sorted(row["status"] for row in rows)
        self.assertEqual(statuses, ["held: consumer check timed out", "removed"])
        self.assertEqual(sum(path.exists() for path in (first, second)), 1)

    def test_settle_is_a_dry_run_and_apply_retires_only_this_checkouts_idle_isolated_builds(self):
        own = self.make_isolated("review")
        recent = self.make_isolated("recent", head="bbbbbbbbbbbb", last_used=time.time())
        other = self.make_isolated("review", owner="0000000000000000")
        slot = self.make_build("slot-0")
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            rows = self.ws.settle(False)
            self.assertEqual({Path(r["path"]).name: r["status"] for r in rows},
                             {own.name: "candidate", recent.name: "held: recently used"})
            self.assertTrue(own.exists() and recent.exists())
            rows = self.ws.settle(True)
        self.assertEqual({Path(r["path"]).name: r["status"] for r in rows},
                         {own.name: "removed", recent.name: "held: recently used"})
        self.assertTrue(recent.exists() and other.exists() and slot.exists())
        self.assertFalse(own.exists())
        self.assertEqual([row["reason"] for row in self.receipts()], ["settle"])


class ResultArtifactsSurviveLaterChecks(CleanupCase):
    """F1: a successful test or check must not delete the output of an earlier build, run or exec."""

    def release_binary(self, target):
        (target / "release").mkdir(parents=True, exist_ok=True)
        (target / "release" / "engine").write_bytes(b"binary")

    def test_test_after_build_with_the_same_label_keeps_the_release_binary(self):
        self.run_cargo(("build", "--release"), "bench", writer=self.release_binary)
        for follow_up in (("test",), ("check",), ("clippy",), ("fmt",), ("nextest", "run")):
            with self.subTest(follow_up=follow_up):
                self.assertEqual(self.run_cargo(follow_up, "bench"), 0)
                self.assertTrue((self.isolated("bench") / "target" / "release" / "engine").exists())
                self.assertIn("kept", self.stderr)
        marker = workspace.read_json(self.isolated("bench") / "build.json")
        self.assertIs(marker["deliverable"], True)

    def test_exec_and_run_and_no_run_mark_the_directory_too(self):
        for first in (lambda: self.run_exec("bench"), lambda: self.run_cargo(("run",), "bench"),
                      lambda: self.run_cargo(("test", "--no-run"), "bench")):
            with self.subTest(first=first):
                first()
                self.run_cargo(("test",), "bench")
                self.assertTrue(self.isolated("bench").exists())
                shutil.rmtree(self.isolated("bench"))

    def test_a_test_only_label_is_still_retired_and_the_marker_is_never_set_by_it(self):
        self.run_cargo(("test",), "plain")
        self.assertFalse(self.isolated("plain").exists())
        self.run_cargo(("test",), "plain", keep=True)
        self.assertIs(workspace.read_json(self.isolated("plain") / "build.json")["deliverable"], False)

    def test_the_marker_survives_every_later_start_of_the_directory(self):
        self.run_cargo(("build",), "bench")
        for _ in range(3):
            self.run_cargo(("test",), "bench")
        self.assertIs(workspace.read_json(self.isolated("bench") / "build.json")["deliverable"], True)

    def test_retire_refuses_a_deliverable_for_the_automatic_reasons_only(self):
        build = self.make_isolated("bench")
        data = workspace.read_json(build / "build.json")
        workspace.write_json(build / "build.json", {**data, "deliverable": True})
        for reason in ("on-success", "superseded"):
            self.assertEqual(self.retire(build, reason)["status"], "held: holds result artifacts", reason)
        self.assertEqual(self.retire(build, "settle")["status"], "removed")


class OlderHeadsAreNotSupersededByBuilds(CleanupCase):
    """F3: A/B comparisons keep both builds."""

    def test_build_and_exec_never_supersede_an_older_head(self):
        old = self.make_isolated("ab", "aaaaaaaaaaaa")
        self.run_cargo(("build", "--release"), "ab")
        self.run_exec("ab")
        self.assertTrue(old.exists())

    def test_a_deliverable_older_head_survives_a_later_successful_test(self):
        self.run_cargo(("build", "--release"), "ab")
        first = self.isolated("ab")
        data = workspace.read_json(first / "build.json")
        workspace.write_json(first / "build.json", {**data, "last_used": 0})
        (self.repo / "next").write_text("second head")
        self.git("add", ".")
        self.git("commit", "-qm", "second head")
        self.run_cargo(("test",), "ab")
        self.assertTrue(first.exists())
        self.assertEqual(self.receipts()[-1]["reason"], "on-success")


class KeepFile(CleanupCase):
    """F5: the keep list is shared through <root>/keep-labels and ignores case."""

    def write_keep(self, text):
        (self.store / "keep-labels").write_text(text, encoding="utf-8")

    def test_a_label_in_the_keep_file_protects_without_any_environment_variable(self):
        build = self.make_isolated("Review")
        self.write_keep("# kept for the acceptance run\n\nother\nreview, spare\n")
        self.assertEqual(self.retire(build)["status"], "held: label is in the keep list")
        self.assertEqual(self.ws.prune(0, False)[0]["status"], "held: label is in the keep list")
        self.assertTrue(build.exists())

    def test_matching_ignores_case_in_the_file_and_the_environment(self):
        build = self.make_isolated("review")
        self.write_keep("REVIEW\n")
        self.assertEqual(self.retire(build)["status"], "held: label is in the keep list")
        (self.store / "keep-labels").unlink()
        with patch.dict(os.environ, {"MUDCRAB_KEEP_LABELS": "Review"}):
            self.assertEqual(self.retire(build)["status"], "held: label is in the keep list")

    def test_a_missing_file_keeps_nothing_and_an_unreadable_one_keeps_everything(self):
        build = self.make_isolated("review")
        other = self.make_isolated("other", "bbbbbbbbbbbb")
        (self.store / "keep-labels").mkdir()
        self.assertEqual(self.retire(build)["status"], "held: keep list check failed")
        (self.store / "keep-labels").rmdir()
        (self.store / "keep-labels").write_bytes(b"\xff\xfe not utf-8")
        self.assertEqual(self.retire(other)["status"], "held: keep list check failed")
        (self.store / "keep-labels").unlink()
        (self.store / "keep-labels").symlink_to(self.base / "elsewhere")
        self.assertEqual(self.retire(other)["status"], "held: keep list check failed")
        (self.store / "keep-labels").unlink()
        self.assertEqual(self.retire(build)["status"], "removed")

    def test_the_automatic_rules_honour_the_file(self):
        self.write_keep("review\n")
        self.run_cargo(("test",), "Review")
        self.assertTrue(self.isolated("Review").exists())


class DeviceBoundaries(CleanupCase):
    """F2: never delete across a mount point or another volume."""

    def other_device(self, marker="mounted"):
        real = os.lstat
        def lstat(path, *args, **kwargs):
            info = real(path, *args, **kwargs)
            if marker in str(path):
                values = list(info)
                values[2] += 1
                return os.stat_result(values)
            return info
        return patch.object(workspace.os, "lstat", side_effect=lstat)

    def mounted_build(self, label="review"):
        build = self.make_isolated(label)
        (build / "target" / "mounted").mkdir(parents=True)
        (build / "target" / "mounted" / "precious.txt").write_text("other volume")
        return build

    def test_a_directory_on_another_device_inside_a_build_holds_it(self):
        build = self.mounted_build()
        with self.other_device():
            row = self.retire(build)
        self.assertEqual(row["status"], "held: contains another filesystem")
        self.assertTrue((build / "target" / "mounted" / "precious.txt").exists())
        self.assertEqual(self.receipts(), [])

    def test_the_old_prune_apply_path_has_the_same_protection(self):
        build = self.mounted_build()
        with self.other_device(), patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            rows = self.ws.prune(0, True)
        self.assertEqual([row["status"] for row in rows], ["held: contains another filesystem"])
        self.assertTrue((build / "target" / "mounted" / "precious.txt").exists())

    def test_a_build_directory_that_is_itself_a_mount_point_is_held(self):
        build = self.make_isolated("review")
        with patch.object(workspace.os.path, "ismount", side_effect=lambda p: Path(p) == build):
            self.assertEqual(self.retire(build)["status"], "held: mount point")
        with self.other_device(marker=build.name), patch.object(workspace.os.path, "ismount", return_value=False):
            self.assertEqual(self.retire(build)["status"], "held: contains another filesystem")
        self.assertTrue(build.exists())

    def test_the_slot_trim_and_recovery_apply_the_same_rule(self):
        slot = self.make_build("slot-0")
        incremental = slot / "target" / "debug" / "incremental"
        (incremental / "mounted").mkdir(parents=True)
        (incremental / "mounted" / "precious.txt").write_text("other volume")
        (incremental / "big").write_bytes(b"x" * 65536)
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}), self.other_device():
            self.assertEqual(self.trim(slot)["status"], "held: contains another filesystem")
        leftover = self.builds / f".retiring-isolated-{self.ws.owner}-a-aaaaaaaaaaaa-1"
        (leftover / "mounted").mkdir(parents=True)
        workspace.write_json(leftover / "build.json", {"version": 1})
        (leftover / "mounted" / "precious.txt").write_text("other volume")
        with self.other_device(), patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            rows = self.ws.recover_interrupted()
        self.assertEqual([row["status"] for row in rows], ["held: contains another filesystem"])
        self.assertTrue((incremental / "mounted" / "precious.txt").exists())
        self.assertTrue((leftover / "mounted" / "precious.txt").exists())

    def trim(self, slot):
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            return self.ws.trim_slot_incremental(slot, leased=False)


class BuildTimes(CleanupCase):
    """m-1: a corrupt last_used never makes a build look old."""

    def test_non_finite_or_missing_times_hold_the_build_everywhere(self):
        for value in (float("nan"), float("-inf"), float("inf"), "yesterday", None, True):
            build = self.make_isolated("review")
            data = workspace.read_json(build / "build.json")
            workspace.write_json(build / "build.json", {**data, "last_used": value})
            with self.subTest(value=value):
                self.assertTrue(self.retire(build)["status"].startswith("held:"))
                with patch.object(workspace.shutil, "which", return_value="lsof"), \
                        patch.object(workspace.subprocess, "run", return_value=IDLE):
                    self.assertEqual(self.ws.prune(0, True), [])
                    self.assertEqual(self.ws.settle(True), [])
                self.assertTrue(build.exists())
        missing = self.make_isolated("review", "bbbbbbbbbbbb")
        data = workspace.read_json(missing / "build.json")
        del data["last_used"]
        workspace.write_json(missing / "build.json", data)
        self.assertTrue(self.retire(missing)["status"].startswith("held:"))


class OwnershipIsCheckedAgainAfterTheLock(CleanupCase):
    """The state can change between the first check and the lease."""

    def swap_after_lock(self, mutate):
        real = workspace.try_lease
        @contextlib.contextmanager
        def swapping(path):
            with real(path) as taken:
                if taken and path.name.startswith("isolated-"):
                    mutate()
                yield taken
        return patch.object(workspace, "try_lease", swapping)

    def test_a_build_json_removed_after_the_lock_keeps_the_directory(self):
        build = self.make_isolated("review")
        with self.swap_after_lock(lambda: (build / "build.json").unlink()):
            self.assertEqual(self.retire(build)["status"], "held: not tool-owned (no build.json)")
        self.assertTrue((build / "artifact").exists())

    def test_an_owner_change_after_the_lock_keeps_the_directory(self):
        build = self.make_isolated("review")
        def other_owner():
            data = workspace.read_json(build / "build.json")
            workspace.write_json(build / "build.json", {**data, "owner": "0000000000000000"})
        with self.swap_after_lock(other_owner):
            self.assertEqual(self.retire(build, owner_only=True)["status"], "held: another owner")
        self.assertTrue(build.exists())

    def test_a_symlink_swapped_in_after_the_lock_keeps_the_target(self):
        build = self.make_isolated("review")
        external = self.base / "precious"
        external.mkdir()
        (external / "build.json").write_text("{}")
        def swap():
            shutil.rmtree(build)
            build.symlink_to(external, target_is_directory=True)
        with self.swap_after_lock(swap):
            self.assertEqual(self.retire(build)["status"], "held: symlink")
        self.assertTrue((external / "build.json").exists())


class RecoveryAndTrimSymlinkGuards(CleanupCase):
    def test_recovery_names_the_exact_reason_for_a_symlink_or_a_plain_file(self):
        external = self.base / "precious"
        external.mkdir()
        link = self.builds / f".retiring-isolated-{self.ws.owner}-c-aaaaaaaaaaaa-3"
        link.symlink_to(external, target_is_directory=True)
        plain = self.builds / f".retiring-isolated-{self.ws.owner}-d-aaaaaaaaaaaa-4"
        plain.write_text("not a directory")
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            rows = {Path(row["path"]).name: row["status"] for row in self.ws.recover_interrupted()}
        self.assertEqual(set(rows.values()), {"held: symlink or not a directory"})
        self.assertTrue(link.is_symlink() and plain.exists() and external.is_dir())

    def test_a_symlinked_incremental_directory_is_named_as_the_reason(self):
        slot = self.make_build("slot-0")
        external = self.base / "precious"
        external.mkdir()
        (external / "data").write_text("keep")
        (slot / "target" / "debug").mkdir(parents=True)
        (slot / "target" / "debug" / "incremental").symlink_to(external, target_is_directory=True)
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}), \
                patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            row = self.ws.trim_slot_incremental(slot, leased=False)
        self.assertEqual(row["status"], "held: symlink")
        self.assertEqual((external / "data").read_text(), "keep")


class ReviewMinorFindings(CleanupCase):
    def test_a_failed_trim_rename_leaves_no_receipt_and_keeps_the_state(self):
        slot = self.make_build("slot-0")
        incremental = slot / "target" / "debug" / "incremental"
        incremental.mkdir(parents=True)
        (incremental / "big").write_bytes(b"x" * 65536)
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}), \
                patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE), \
                patch.object(Path, "rename", side_effect=OSError("read-only")):
            row = self.ws.trim_slot_incremental(slot, leased=False)
        self.assertTrue(row["status"].startswith("held:"), row)
        self.assertTrue((incremental / "big").exists())
        self.assertEqual(list((self.store / "evidence").glob("trimmed-*.json")), [])
        self.assertEqual(self.receipts(), [])

    def test_a_dry_run_reports_the_holds_that_apply_would_hit(self):
        leased = self.make_isolated("leased")
        recent = self.make_isolated("recent", "bbbbbbbbbbbb", last_used=time.time() - 60)
        free = self.make_isolated("free", "cccccccccccc")
        with workspace.lease(self.store / "locks" / f"{leased.name}.lock"):
            rows = {Path(r["path"]).name: r["status"] for r in self.ws.settle(False)}
        self.assertEqual(rows, {leased.name: "held: active lease", recent.name: "held: recently used",
                                free.name: "candidate"})

    def test_every_removal_prints_one_line(self):
        self.run_cargo(("test",), "review")
        lines = [line for line in self.stderr.splitlines() if line.startswith("workspace: removed")]
        self.assertEqual(len(lines), 1)
        self.assertIn("isolated-", lines[0])
        self.assertIn("on-success", lines[0])

    def test_a_trim_that_is_held_says_why(self):
        def layout(target):
            (target / "debug" / "incremental").mkdir(parents=True)
            (target / "debug" / "incremental" / "big").write_bytes(b"x" * 65536)
            (target / "debug" / "incremental" / "link").symlink_to(self.base)
        with patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.00001"}):
            self.run_cargo(("test",), None, writer=layout)
        self.assertIn("trim held", self.stderr)

    def test_a_malformed_receipt_holds_builds_and_warns_once(self):
        build = self.make_isolated("review")
        other = self.make_isolated("other", "bbbbbbbbbbbb")
        folder = self.store / "evidence" / "other-task"
        folder.mkdir()
        (folder / "build-receipt.json").write_text("{bad")
        with contextlib.redirect_stderr(io.StringIO()) as captured:
            self.assertEqual(self.retire(build)["status"], "held: receipt check failed")
            self.assertEqual(self.retire(other)["status"], "held: receipt check failed")
        self.assertEqual(captured.getvalue().count("build-receipt.json"), 1)

    def test_setup_waits_for_a_concurrent_start_instead_of_failing(self):
        import threading
        ready = threading.Event()
        def hold():
            with workspace.lease(self.store / "locks" / "registry.lock"):
                ready.set()
                time.sleep(0.4)
        thread = threading.Thread(target=hold)
        thread.start()
        ready.wait(5)
        try:
            self.ws.setup()
        finally:
            thread.join()

    def test_a_leftover_that_another_process_already_deleted_counts_as_removed(self):
        build = self.make_isolated("review")
        real = shutil.rmtree
        def raced(path, *args, **kwargs):
            real(path)
            raise FileNotFoundError(path)
        with patch.object(workspace.shutil, "rmtree", side_effect=raced):
            self.assertEqual(self.retire(build)["status"], "removed")


class MissingMarkMeansProtected(CleanupCase):
    """N-1: only an explicit "deliverable": false allows an automatic retirement."""

    def test_a_build_json_without_the_key_is_kept_by_the_automatic_reasons(self):
        legacy = self.make_isolated("legacy", deliverable=None)
        self.assertNotIn("deliverable", workspace.read_json(legacy / "build.json"))
        for reason in ("on-success", "superseded"):
            self.assertEqual(self.retire(legacy, reason)["status"], "held: holds result artifacts", reason)
        self.assertEqual(self.retire(legacy, "settle")["status"], "removed")

    def test_an_old_script_build_survives_a_passing_test_through_the_whole_flow(self):
        legacy = self.isolated("legacy")
        legacy.mkdir()
        (legacy / "target" / "release").mkdir(parents=True)
        (legacy / "target" / "release" / "engine").write_bytes(b"binary")
        workspace.write_json(legacy / "build.json", {"version": 1, "key": "compatible", "owner": self.ws.owner,
                             "checkout": str(self.repo), "commit": self.git("rev-parse", "HEAD"), "last_used": 0})
        self.run_cargo(("test",), "legacy")
        self.assertTrue((legacy / "target" / "release" / "engine").exists())
        self.assertIs(workspace.read_json(legacy / "build.json")["deliverable"], True)
        self.assertIn("kept", self.stderr)

    def test_an_old_copy_that_rewrites_build_json_cannot_strip_the_protection(self):
        self.run_cargo(("build", "--release"), "mix")
        build = self.isolated("mix")
        data = workspace.read_json(build / "build.json")
        del data["deliverable"]  # what the old script does when it starts a command here
        workspace.write_json(build / "build.json", data)
        self.run_cargo(("test",), "mix")
        self.assertTrue(build.exists())

    def test_an_unmarked_older_head_is_not_superseded(self):
        legacy = self.make_isolated("review", "aaaaaaaaaaaa", deliverable=None)
        marked_false = self.make_isolated("review", "bbbbbbbbbbbb", deliverable=False)
        self.run_cargo(("test",), "review")
        self.assertTrue(legacy.exists())
        self.assertFalse(marked_false.exists())

    def test_a_new_directory_used_only_by_checks_is_marked_false(self):
        self.run_cargo(("test",), "plain", keep=True)
        self.assertIs(workspace.read_json(self.isolated("plain") / "build.json")["deliverable"], False)
        self.run_cargo(("test",), "plain", keep=True)
        self.assertIs(workspace.read_json(self.isolated("plain") / "build.json")["deliverable"], False)


class SmallReviewFollowUps(CleanupCase):
    def test_a_longer_older_than_days_wins_over_a_shorter_ttl_for_isolated_builds(self):
        build = self.make_isolated("review", last_used=time.time() - 2 * 3600)
        with patch.object(workspace.shutil, "which", return_value="lsof"), \
                patch.object(workspace.subprocess, "run", return_value=IDLE):
            self.assertEqual(self.ws.prune(30, False, isolated_ttl_hours=1), [])
            self.assertEqual(len(self.ws.prune(None, False, isolated_ttl_hours=1)), 1)
        self.assertTrue(build.exists())

    def test_a_dry_run_probe_does_not_make_a_starting_build_fail(self):
        import threading
        build = self.make_isolated("review", self.head())
        lock = self.store / "locks" / f"{build.name}.lock"
        ready = threading.Event()
        def probe():
            with workspace.lease(lock):  # a dry run holds the lock for a moment
                ready.set()
                time.sleep(0.15)
        thread = threading.Thread(target=probe)
        thread.start()
        ready.wait(5)
        try:
            self.assertEqual(self.run_cargo(("build",), "review"), 0)
        finally:
            thread.join()

    def test_a_start_still_fails_fast_when_another_command_really_holds_the_build(self):
        build = self.make_isolated("review", self.head())
        self.make_isolated("review2")
        with workspace.lease(self.store / "locks" / f"{build.name}.lock"):
            started = time.monotonic()
            with self.assertRaisesRegex(ValueError, "busy"):
                self.run_cargo(("build",), "review")
        self.assertLess(time.monotonic() - started, 3)

    def test_an_unreadable_keep_file_is_named_once_in_a_warning(self):
        build = self.make_isolated("review")
        other = self.make_isolated("other", "bbbbbbbbbbbb")
        (self.store / "keep-labels").mkdir()
        with contextlib.redirect_stderr(io.StringIO()) as captured:
            self.assertEqual(self.retire(build)["status"], "held: keep list check failed")
            self.assertEqual(self.retire(other)["status"], "held: keep list check failed")
        self.assertEqual(captured.getvalue().count("keep-labels"), 1)


class LabelCapWarning(CleanupCase):
    def test_a_label_over_its_cap_warns_and_deletes_nothing(self):
        first = self.make_isolated("review", "aaaaaaaaaaaa", last_used=time.time())
        (first / "artifact").write_bytes(b"x" * 70000)
        with patch.dict(os.environ, {"MUDCRAB_LABEL_CAP_GIB": "0.00005"}):
            self.run_cargo(("build",), "review", returncode=1, writer=lambda t: t.mkdir(parents=True, exist_ok=True))
        self.assertIn("review", self.stderr)
        self.assertIn("exceeds", self.stderr)
        self.assertTrue(first.exists() and self.isolated("review").exists())

    def test_a_label_under_its_cap_or_with_the_cap_disabled_is_silent(self):
        self.make_isolated("review", "aaaaaaaaaaaa")
        for value in ("1", "0"):
            with patch.dict(os.environ, {"MUDCRAB_LABEL_CAP_GIB": value}):
                self.run_cargo(("build",), "review", returncode=1)
            self.assertNotIn("exceeds", self.stderr)


class CommandLine(CleanupCase):
    def main(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            code = workspace.main(["--repo", str(self.repo), "--root", str(self.store), *argv])
        return code, out.getvalue()

    def test_keep_and_keep_days_are_passed_to_cargo(self):
        with patch.object(workspace.Workspace, "cargo", return_value=0) as cargo:
            self.assertEqual(self.main("cargo", "--isolated", "review", "--keep", "--keep-days", "3", "check")[0], 0)
        args, kwargs = cargo.call_args
        self.assertEqual(args[:2], (["check"], "review"))
        self.assertTrue(kwargs["keep"])
        self.assertEqual(kwargs["keep_days"], 3.0)

    def test_keep_needs_an_isolated_label_and_a_sane_duration(self):
        self.assertEqual(self.main("cargo", "--keep", "check")[0], 2)
        for days in ("0", "-1", "nan", "inf"):
            with self.subTest(days=days):
                self.assertEqual(self.main("cargo", "--isolated", "x", "--keep", "--keep-days", days, "check")[0], 2)
        self.assertFalse((self.store / "pins").exists() and any((self.store / "pins").iterdir()))

    def test_settle_and_prune_default_to_dry_run(self):
        build = self.make_isolated("review")
        for argv in (("settle",), ("prune",), ("prune", "--older-than-days", "0")):
            code, output = self.main(*argv)
            self.assertEqual(code, 0)
        self.assertTrue(build.exists())
        self.assertIn("candidate", self.main("settle")[1])

    def test_the_isolated_ttl_flag_is_passed_to_prune(self):
        with patch.object(workspace.Workspace, "prune", return_value=[]) as prune:
            self.assertEqual(self.main("prune", "--isolated-ttl-hours", "48")[0], 0)
            self.assertEqual(prune.call_args.kwargs["isolated_ttl_hours"], 48.0)
            self.assertEqual(self.main("prune")[0], 0)
            self.assertIsNone(prune.call_args.kwargs["isolated_ttl_hours"])
        self.assertEqual(self.main("prune", "--isolated-ttl-hours", "0.1")[0], 2)

    def test_settle_apply_is_accepted(self):
        with patch.object(workspace.Workspace, "settle", return_value=[]) as settle:
            self.assertEqual(self.main("settle", "--apply")[0], 0)
        settle.assert_called_once_with(True)


if __name__ == "__main__":
    unittest.main()
