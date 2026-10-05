"""Trust-boundary tests for the incomplete xEdit console capability probe."""
import os
import hashlib
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import p0_tools
import run_xedit_p0 as probe


class XEditProbeTests(unittest.TestCase):
    def test_v103_custom_wine_directory_cannot_receive_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tool = root / "source" / "xDump64.exe"
            tool.parent.mkdir()
            with tool.open("wb") as stream:
                stream.truncate(probe.XDUMP_SIZE)
            runtime = root / "wine-runtime"
            runtime.mkdir()
            wine = runtime / "wine"
            wine.write_text("#!/bin/sh\nexit 1\n")
            wine.chmod(0o700)
            artifact = runtime / "artifacts"
            args = SimpleNamespace(xdump=str(tool), wine=str(wine), artifact_dir=str(artifact))
            with mock.patch.object(probe, "_read_verified_bytes", return_value=(probe.XDUMP_SHA256, probe.XDUMP_SIZE, b"synthetic tool")), \
                 mock.patch.object(probe, "_run_supervised") as run:
                with self.assertRaisesRegex(probe.QualificationError, "protected path"):
                    probe.run_probe(args)
                run.assert_not_called()
            self.assertFalse(artifact.exists())

    def test_v100_zero_exit_after_wine_crash_is_incomplete(self):
        for stdout, stderr in [
            ("WineDbg attached", "<00:00:00.050> All Done.\n"),
            ("", "wine: Unhandled page fault\n<00:00:00.050> All Done.\n"),
            ("", "Can't determine GameMode.\n"),
        ]:
            result = probe.classify_dump("p0-hand-light.esl", 0, stdout, stderr)
            self.assertEqual(result["status"], "incomplete")
            self.assertFalse(result["completed_verdict_saved"])

    def test_v100_malformed_dump_diagnostics_do_not_prove_structural_rejection(self):
        for name in ("p0-truncated-tail.esp", "p0-truncated-header.esp", "p0-bad-tes4-length.esp"):
            result = probe.classify_dump(name, 0, "[ERROR: invalid data]", "<00:00:00.050> All Done.\n")
            self.assertEqual(result["status"], "unqualified_negative")
            self.assertFalse(result["completed_verdict_saved"])

    def test_v100_completion_without_expected_records_fails(self):
        result = probe.classify_dump("p0-hand-full.esp", 0, "", "<00:00:00.050> All Done.\n")
        self.assertEqual(result["status"], "failed_observation")

    def test_v100_clean_selected_values_do_not_claim_full_fixture_validation(self):
        stdout = """  Signature: TES4
  FormID: NULL [00000000]
  Signature: GLOB
  FormID: GLOB - Global [00000800]
EDID - Editor ID: P0LightGlobal
FLTV - Value: 2.500000
"""
        result = probe.classify_dump("p0-hand-light.esl", 0, stdout, "<00:00:00.050> All Done.\n")
        self.assertEqual(result["status"], "observed")
        self.assertTrue(result["known_values_checked"])
        self.assertFalse(result["schema_diagnostics_observed"])
        self.assertFalse(result["fixture_schema_validated"])

    def test_v100_localized_id_and_schema_errors_stay_distinct(self):
        stdout = """  Signature: TES4
  FormID: NULL [00000000]
  Signature: ARMO
  FormID: ARMO - Armor [00000800]
EDID - Editor ID: P0LocalizedArmor
FULL - Name: <Error: No strings file for lstring ID 12345678>
[ERROR: Missing required members: ARMO - Armor]
"""
        result = probe.classify_dump("p0-localized.esp", 0, stdout, "<00:00:00.050> All Done.\n")
        self.assertEqual(result["status"], "observed_with_diagnostics")
        self.assertFalse(result["fixture_schema_validated"])
        self.assertEqual(result["translated_text"], "unavailable")
        self.assertFalse(result["completed_verdict_saved"])
        self.assertIn("memory addresses", result["physical_offsets"])

    def test_v103_protected_destination_rejected_before_read_or_write(self):
        artifact = p0_tools.REPO_ROOT / "xedit-test-no-write"
        args = SimpleNamespace(xdump="/nonexistent/xDump64.exe", artifact_dir=str(artifact), wine="wine")
        with mock.patch.object(probe, "_read_verified_bytes") as read:
            with self.assertRaisesRegex(probe.QualificationError, "protected path"):
                probe.run_probe(args)
            read.assert_not_called()
        self.assertFalse(artifact.exists())

    def test_v103_input_tool_directory_cannot_be_artifact_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            args = SimpleNamespace(xdump=str(Path(directory) / "xDump64.exe"), artifact_dir=directory, wine="wine")
            with self.assertRaisesRegex(probe.QualificationError, "protected path"):
                probe.run_probe(args)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_v103_every_registered_checkout_is_protected_before_tool_read(self):
        roots = p0_tools._repository_worktree_roots()
        self.assertIn(p0_tools.REPO_ROOT, roots)
        for root in roots:
            candidate = root / "p0-no-artifact-write"
            args = SimpleNamespace(xdump="/nonexistent/xDump64.exe", artifact_dir=str(candidate), wine="wine")
            with self.subTest(root=root), mock.patch.object(probe, "_read_verified_bytes") as read:
                with self.assertRaisesRegex(probe.QualificationError, "protected path"):
                    probe.run_probe(args)
                read.assert_not_called()
                self.assertFalse(candidate.exists())

    def test_v103_worktree_discovery_failure_cannot_permit_artifact_writes(self):
        with mock.patch.object(p0_tools.subprocess, "run", side_effect=subprocess.TimeoutExpired("git", 5)):
            with self.assertRaisesRegex(probe.QualificationError, "cannot verify protected repository"):
                p0_tools.validate_artifact_destination(Path("/tmp/p0-no-artifact-write"), Path("/tmp/source"))

    def test_v104_supervisor_terminates_a_real_timed_out_process(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(p0_tools.ProcessTimeout) as caught:
                p0_tools._run_supervised(
                    [sys.executable, "-u", "-c", "import time; print('started'); time.sleep(60)"],
                    cwd=Path(directory), env=dict(os.environ), label="synthetic-hang", timeout=0.2,
                )
        self.assertIn("started", caught.exception.stdout)

    @unittest.skipUnless(os.name == "posix", "requires a detached POSIX process group")
    def test_v104_detached_descendant_cannot_hold_timeout_pipes_open(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_pid = root / "child.pid"
            child = "import os,time; from pathlib import Path; Path('child.pid').write_text(str(os.getpid())); time.sleep(60)"
            parent = (
                "import subprocess,sys,time; "
                f"subprocess.Popen([sys.executable,'-c',{child!r}],start_new_session=True); "
                "print('parent started',flush=True); time.sleep(60)"
            )
            start = time.monotonic()
            try:
                with self.assertRaises(p0_tools.ProcessTimeout) as caught:
                    p0_tools._run_supervised(
                        [sys.executable, "-c", parent], cwd=root, env=dict(os.environ),
                        label="detached-pipe-holder", timeout=0.2,
                    )
                self.assertLess(time.monotonic() - start, 6)
                self.assertIn("parent started", caught.exception.stdout)
                self.assertTrue(child_pid.is_file())
            finally:
                if child_pid.is_file():
                    try:
                        os.kill(int(child_pid.read_text()), 9)
                    except ProcessLookupError:
                        pass

    def test_v100_startup_failures_save_a_discoverable_incomplete_report(self):
        for failure in (subprocess.CompletedProcess([], 1, "", "help failed"), OSError("launch failed")):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                tool = root / "tools" / "xDump64.exe"
                tool.parent.mkdir()
                with tool.open("wb") as stream:
                    stream.truncate(probe.XDUMP_SIZE)
                args = SimpleNamespace(xdump=str(tool), artifact_dir=str(root / "artifacts"), wine=sys.executable)
                version = subprocess.CompletedProcess([], 0, "wine-test\n", "")
                outcomes = [version, failure]
                with mock.patch.object(probe, "_read_verified_bytes", return_value=(probe.XDUMP_SHA256, probe.XDUMP_SIZE, b"synthetic tool")), \
                     mock.patch.dict(os.environ, {name: str(p0_tools.REPO_ROOT) for name in ("TMPDIR", "TMP", "TEMP")}), \
                     mock.patch.object(probe, "_run_supervised", side_effect=outcomes) as run:
                    report, artifact = probe.run_probe(args)
                self.assertIn("startup_failure", report)
                self.assertFalse(report["qualified"])
                self.assertFalse(report["completed_verdict_saved"])
                self.assertTrue((artifact / "probe-results.json").is_file())
                for call in run.call_args_list:
                    for name in ("TMPDIR", "TMP", "TEMP"):
                        self.assertEqual(Path(call.kwargs["env"][name]), artifact / "tmp")
                self.assertTrue((artifact / "tmp").is_dir())
                decision_files = {item["path"]: item for item in report["runner"]["files"]}
                for name in ("run_xedit_p0.py", "p0_tools.py", "corpus_manifest.py", "p0_fixtures.py"):
                    path = "scripts/schema/" + name
                    self.assertEqual(decision_files[path]["sha256"], hashlib.sha256((p0_tools.REPO_ROOT / path).read_bytes()).hexdigest())

    def test_v103_capture_limit_rejects_oversized_pinned_input_before_hashing(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "input"
            source.write_bytes(b"12345")
            with mock.patch.object(p0_tools.corpus_manifest, "_read_verified_file") as read:
                with self.assertRaisesRegex(p0_tools.QualificationError, "capture limit"):
                    p0_tools._read_verified_bytes(source, max_bytes=4)
                read.assert_not_called()


if __name__ == "__main__":
    unittest.main()
