"""Trust-boundary tests for the incomplete xEdit console capability probe."""
from contextlib import contextmanager
import os
import hashlib
import json
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


@contextmanager
def _popen_waits_for_markers(*markers: Path, readiness_timeout: float = 5):
    """Start the real child, then start its supervised deadline after readiness."""
    real_popen = subprocess.Popen
    ready_at = {}

    def popen_and_wait(*args, **kwargs):
        process = real_popen(*args, **kwargs)
        deadline = time.monotonic() + readiness_timeout
        try:
            while not all(marker.is_file() for marker in markers):
                returncode = process.poll()
                if returncode is not None:
                    raise AssertionError(
                        f"child exited with status {returncode} before readiness markers {markers}"
                    )
                if time.monotonic() >= deadline:
                    raise AssertionError(
                        f"child did not create readiness markers {markers} "
                        f"within {readiness_timeout:g} seconds"
                    )
                time.sleep(0.01)
            ready_at["monotonic"] = time.monotonic()
            return process
        except BaseException:
            if process.poll() is None:
                try:
                    if os.name == "posix":
                        os.killpg(process.pid, 9)
                    else:
                        process.kill()
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)
            for stream in (process.stdout, process.stderr):
                if stream is not None:
                    stream.close()
            raise

    with mock.patch.object(p0_tools.subprocess, "Popen", side_effect=popen_and_wait):
        yield ready_at


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
            root = Path(directory)
            ready = root / "ready"
            command = [
                sys.executable, "-u", "-c",
                "import time; from pathlib import Path; "
                "print('started', flush=True); Path('ready').write_text('ready'); time.sleep(60)",
            ]
            with _popen_waits_for_markers(ready):
                with self.assertRaises(p0_tools.ProcessTimeout) as caught:
                    p0_tools._run_supervised(
                        command, cwd=root, env=dict(os.environ), label="synthetic-hang", timeout=0.2,
                    )
        self.assertIn("started", caught.exception.stdout)

    def test_v156_supervisor_rejects_undecodable_output_without_lossy_text(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            p0_tools, "_output_encoding", return_value="utf-8"
        ):
            with self.assertRaises(p0_tools.ProcessOutputDecodeError) as caught:
                p0_tools._run_supervised(
                    [sys.executable, "-c", "import sys; sys.stdout.buffer.write(b'\\xff')"],
                    cwd=Path(directory), env=dict(os.environ), label="invalid-output", timeout=5,
                )
        self.assertIn("undecodable stdout bytes", str(caught.exception))
        self.assertEqual(caught.exception.stdout_bytes, b"\xff")
        self.assertEqual(caught.exception.stderr_bytes, b"")
        self.assertEqual(caught.exception.failure_record()["raw_stdout_size_bytes"], 1)

    def test_v156_undecodable_timeout_output_stays_incomplete_with_raw_evidence(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            p0_tools, "_output_encoding", return_value="utf-8"
        ):
            root = Path(directory)
            ready = root / "ready"
            command = [
                sys.executable, "-u", "-c",
                "import sys,time; from pathlib import Path; "
                "sys.stdout.buffer.write(b'\\xff'); sys.stdout.flush(); "
                "Path('ready').write_text('ready'); time.sleep(60)",
            ]
            with _popen_waits_for_markers(ready):
                with self.assertRaises(p0_tools.ProcessOutputDecodeError) as caught:
                    p0_tools._run_supervised(
                        command, cwd=root, env=dict(os.environ), label="invalid-timeout", timeout=0.2,
                    )
        self.assertIsNotNone(caught.exception.timeout)
        self.assertTrue(caught.exception.failure_record()["timed_out"])
        self.assertEqual(caught.exception.stdout_bytes, b"\xff")

    @unittest.skipUnless(os.name == "posix", "requires a detached POSIX process group")
    def test_v104_detached_descendant_cannot_hold_timeout_pipes_open(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_pid = root / "child.pid"
            spawned_pid = root / "spawned.pid"
            ready = root / "ready"
            child = (
                "import os,time\n"
                "from pathlib import Path\n"
                "Path('child.pid').write_text(str(os.getpid()))\n"
                "time.sleep(60)\n"
            )
            parent = (
                "import subprocess,sys,time\n"
                "from pathlib import Path\n"
                f"spawned = subprocess.Popen([sys.executable, '-c', {child!r}], start_new_session=True)\n"
                "Path('spawned.pid').write_text(str(spawned.pid))\n"
                "while not Path('child.pid').is_file():\n"
                "    time.sleep(0.01)\n"
                "print('parent started', flush=True)\n"
                "Path('ready').write_text('ready')\n"
                "time.sleep(60)\n"
            )
            try:
                with _popen_waits_for_markers(ready) as readiness:
                    with self.assertRaises(p0_tools.ProcessTimeout) as caught:
                        p0_tools._run_supervised(
                            [sys.executable, "-c", parent], cwd=root, env=dict(os.environ),
                            label="detached-pipe-holder", timeout=0.2,
                        )
                self.assertLess(time.monotonic() - readiness["monotonic"], 6)
                self.assertIn("parent started", caught.exception.stdout)
                self.assertTrue(child_pid.is_file())
            finally:
                for pid_file in (child_pid, spawned_pid):
                    if pid_file.is_file():
                        try:
                            os.kill(int(pid_file.read_text()), 9)
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

    def test_v156_undecodable_xedit_case_keeps_report_and_raw_output_without_verdict(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tool = root / "tools" / "xDump64.exe"
            tool.parent.mkdir()
            with tool.open("wb") as stream:
                stream.truncate(probe.XDUMP_SIZE)
            args = SimpleNamespace(
                xdump=str(tool), wine=sys.executable, artifact_dir=str(root / "artifacts")
            )
            cases = {"p0-hand-light.esl": (b"synthetic plugin", "hand-encoded provenance")}
            invalid_output = p0_tools.ProcessOutputDecodeError(
                "p0-hand-light.esl", "utf-8", b"\xffraw stdout", b"raw stderr",
                stream="stdout", error=UnicodeDecodeError(
                    "utf-8", b"\xffraw stdout", 0, 1, "invalid start byte"
                ),
            )
            outcomes = [
                subprocess.CompletedProcess([], 0, "wine-test\n", ""),
                subprocess.CompletedProcess([], 0, "", "SSEDump 4.1.5f x64"),
                invalid_output,
            ]
            with mock.patch.object(
                probe, "_read_verified_bytes",
                return_value=(probe.XDUMP_SHA256, probe.XDUMP_SIZE, b"synthetic tool"),
            ), mock.patch.object(probe.p0_fixtures, "hand_encoded_cases", return_value=cases), \
                 mock.patch.dict(os.environ, {
                     name: str(p0_tools.REPO_ROOT) for name in ("TMPDIR", "TMP", "TEMP")
                 }), mock.patch.object(probe, "_run_supervised", side_effect=outcomes):
                report, artifact = probe.run_probe(args)
            observation = report["cases"][0]
            self.assertEqual(observation["status"], "incomplete")
            self.assertFalse(observation["completed_verdict_saved"])
            self.assertEqual(observation["diagnostic_output_decode_failure"]["stream"], "stdout")
            self.assertEqual(observation["diagnostic_output_decode_failure"]["raw_stdout_size_bytes"], 11)
            self.assertEqual(
                (artifact / "logs/p0-hand-light.esl.stdout.raw.bin").read_bytes(), b"\xffraw stdout"
            )
            self.assertEqual(
                (artifact / "logs/p0-hand-light.esl.stderr.raw.bin").read_bytes(), b"raw stderr"
            )
            saved = json.loads((artifact / "probe-results.json").read_text(encoding="utf-8"))
            self.assertEqual(saved["cases"][0]["status"], "incomplete")

    def test_v156_undecodable_xedit_startup_keeps_report_and_raw_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tool = root / "tools" / "xDump64.exe"
            tool.parent.mkdir()
            with tool.open("wb") as stream:
                stream.truncate(probe.XDUMP_SIZE)
            args = SimpleNamespace(
                xdump=str(tool), wine=sys.executable, artifact_dir=str(root / "artifacts")
            )
            invalid_output = p0_tools.ProcessOutputDecodeError(
                "wine-version", "utf-8", b"\xffwine version", b"",
                stream="stdout", error=UnicodeDecodeError(
                    "utf-8", b"\xffwine version", 0, 1, "invalid start byte"
                ),
            )
            with mock.patch.object(
                probe, "_read_verified_bytes",
                return_value=(probe.XDUMP_SHA256, probe.XDUMP_SIZE, b"synthetic tool"),
            ), mock.patch.dict(os.environ, {
                name: str(p0_tools.REPO_ROOT) for name in ("TMPDIR", "TMP", "TEMP")
            }), mock.patch.object(probe, "_run_supervised", side_effect=[invalid_output]):
                report, artifact = probe.run_probe(args)
            self.assertIn("startup_failure", report)
            self.assertFalse(report["completed_verdict_saved"])
            self.assertEqual(report["startup_output_decode_failure"]["stream"], "stdout")
            self.assertEqual(
                (artifact / "logs/wine-version.stdout.raw.bin").read_bytes(), b"\xffwine version"
            )
            saved = json.loads((artifact / "probe-results.json").read_text(encoding="utf-8"))
            self.assertIn("startup_output_decode_failure", saved)

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
