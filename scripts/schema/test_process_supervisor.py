"""Bounded timeout cleanup, including inherited child pipe handles."""
import importlib
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import run_mutagen_p0


supervisor = importlib.import_module(run_mutagen_p0._run_supervised.__module__)


class ProcessSupervisorTests(unittest.TestCase):
    def test_v148_partial_multibyte_timeout_preserves_raw_evidence(self):
        root = Path.cwd()
        process = mock.Mock(pid=1234)
        process.stdout.encoding = process.stderr.encoding = "utf-8"
        process.communicate.side_effect = [
            subprocess.TimeoutExpired("tool", 0.1),
            subprocess.TimeoutExpired("tool", 2),
            subprocess.TimeoutExpired("tool", 2, output=b"\xf0\x9f", stderr=b"partial stderr"),
        ]
        with mock.patch.object(supervisor.os, "name", "posix"), \
             mock.patch.object(supervisor.signal, "SIGKILL", 9, create=True), \
             mock.patch.object(supervisor.os, "killpg", create=True), \
             mock.patch.object(supervisor, "_output_encoding", return_value="utf-8", create=True), \
             mock.patch.object(supervisor.subprocess, "Popen", return_value=process):
            with self.assertRaises(run_mutagen_p0.QualificationError) as caught:
                run_mutagen_p0._run_supervised(
                    ["synthetic-tool"], cwd=root, env={}, label="partial-unicode", timeout=0.1,
                )
        raw_output = getattr(caught.exception, "raw_output", None)
        if raw_output is not None:
            self.assertEqual(raw_output, (b"\xf0\x9f", b"partial stderr"))
            with tempfile.TemporaryDirectory() as directory:
                logs = Path(directory)
                with mock.patch.object(run_mutagen_p0, "_run_supervised", side_effect=caught.exception):
                    with self.assertRaises(run_mutagen_p0.QualificationError):
                        run_mutagen_p0._run_checked(
                            ["synthetic-tool"], cwd=logs, env={}, label="partial-unicode",
                            log_dir=logs, timeout=0.1,
                        )
                metadata = json.loads((logs / "partial-unicode.timeout.json").read_text(encoding="utf-8"))
                self.assertEqual(metadata["decoded_output"], "unavailable")
                self.assertFalse(metadata["completed_verdict_saved"])
                self.assertEqual((logs / metadata["raw_stdout_file"]).read_bytes(), b"\xf0\x9f")
                self.assertEqual(metadata["raw_stdout_sha256"], hashlib.sha256(b"\xf0\x9f").hexdigest())
        else:
            # The next layer's binary supervisor owns strict decoding and
            # exposes retained streams through ProcessOutputDecodeError.
            self.assertEqual(caught.exception.stdout_bytes, b"\xf0\x9f")
            self.assertEqual(caught.exception.stderr_bytes, b"partial stderr")
            with tempfile.TemporaryDirectory() as directory:
                logs = Path(directory)
                with mock.patch.object(run_mutagen_p0, "_run_supervised", side_effect=caught.exception):
                    with self.assertRaises(run_mutagen_p0.QualificationError):
                        run_mutagen_p0._run_checked(
                            ["synthetic-tool"], cwd=logs, env={}, label="partial-unicode",
                            log_dir=logs, timeout=0.1,
                        )
                metadata = json.loads((logs / "partial-unicode.decode-error.json").read_text(encoding="utf-8"))
                self.assertEqual(metadata["decoded_output"], "unavailable")
                self.assertFalse(metadata["completed_verdict_saved"])
                self.assertEqual((logs / metadata["raw_stdout_file"]).read_bytes(), b"\xf0\x9f")
                self.assertEqual(metadata["raw_stdout_sha256"], hashlib.sha256(b"\xf0\x9f").hexdigest())

    def test_v148_real_descendant_cannot_keep_timeout_open(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_pid = root / "child.pid"
            ready = root / "ready"
            parent = (
                "import subprocess,sys,time\n"
                "from pathlib import Path\n"
                "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'], "
                f"start_new_session={os.name == 'posix'!r})\n"
                "Path('child.pid').write_text(str(child.pid))\n"
                "print('parent ready', flush=True)\n"
                "Path('ready').write_text('ready')\n"
                "time.sleep(60)\n"
            )
            command = [sys.executable, "-u", "-c", parent]
            real_popen = subprocess.Popen
            supervised = []
            ready_at = []

            def popen_after_readiness(argv, **kwargs):
                process = real_popen(argv, **kwargs)
                if argv != command:
                    return process
                supervised.append(process)
                deadline = time.monotonic() + 5
                while not ready.is_file():
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise AssertionError("synthetic parent failed to become ready")
                    time.sleep(0.01)
                ready_at.append(time.monotonic())
                return process

            try:
                with mock.patch.object(supervisor.subprocess, "Popen", side_effect=popen_after_readiness):
                    with self.assertRaises(run_mutagen_p0.ProcessTimeout) as caught:
                        run_mutagen_p0._run_supervised(
                            command, cwd=root, env=dict(os.environ), label="real-pipe-holder", timeout=0.1,
                        )
                self.assertLess(time.monotonic() - ready_at[0], 8)
                self.assertIn("parent ready", caught.exception.stdout)
                self.assertTrue(child_pid.is_file())
            finally:
                # Only the synthetic processes created by this test are owned.
                if child_pid.is_file():
                    pid = int(child_pid.read_text())
                    if os.name == "posix":
                        try:
                            os.kill(pid, 9)
                        except ProcessLookupError:
                            pass
                    else:
                        subprocess.run(
                            ["taskkill", "/T", "/F", "/PID", str(pid)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2, check=False,
                        )
                for process in supervised:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=2)

    def test_v148_windows_tree_kill_and_pipe_cleanup_remain_bounded(self):
        root = Path.cwd()
        for cleanup_failure in (None, OSError("taskkill unavailable"), subprocess.TimeoutExpired("taskkill", 2)):
            with self.subTest(cleanup_failure=cleanup_failure):
                process = mock.Mock(pid=1234)
                process.stdout.encoding = process.stderr.encoding = "utf-8"
                process.stdout.close.side_effect = AssertionError("Windows reader owns pipe lock")
                process.stderr.close.side_effect = AssertionError("Windows reader owns pipe lock")
                process.communicate.side_effect = [
                    subprocess.TimeoutExpired("tool", 0.1),
                    subprocess.TimeoutExpired("tool", 2),
                    subprocess.TimeoutExpired("tool", 2, output=b"partial stdout", stderr=b"partial stderr"),
                ]
                process.poll.return_value = None
                process.wait.side_effect = subprocess.TimeoutExpired("tool", 2)
                with mock.patch.object(supervisor.os, "name", "nt"), \
                     mock.patch.object(supervisor.subprocess, "Popen", return_value=process), \
                     mock.patch.object(supervisor.subprocess, "run", side_effect=cleanup_failure) as tree_kill:
                    with self.assertRaises(run_mutagen_p0.ProcessTimeout) as caught:
                        run_mutagen_p0._run_supervised(
                            ["synthetic-tool"], cwd=root, env={}, label="pipe-holder", timeout=0.1,
                        )
                self.assertEqual(caught.exception.stdout, "partial stdout")
                self.assertEqual(caught.exception.stderr, "partial stderr")
                self.assertEqual(tree_kill.call_args.args[0], ["taskkill", "/T", "/F", "/PID", "1234"])
                self.assertEqual(tree_kill.call_args.kwargs["timeout"], 2)
                self.assertTrue(all(call.kwargs.get("timeout") is not None for call in process.communicate.call_args_list))
                process.kill.assert_called()
                process.wait.assert_called_once_with(timeout=2)
                process.stdout.close.assert_not_called()
                process.stderr.close.assert_not_called()

    def test_v148_posix_escaped_pipe_holder_has_bounded_final_drain(self):
        root = Path.cwd()
        process = mock.Mock(pid=1234)
        process.stdout.encoding = process.stderr.encoding = "utf-8"
        process.communicate.side_effect = [
            subprocess.TimeoutExpired("tool", 0.1),
            subprocess.TimeoutExpired("tool", 2),
            subprocess.TimeoutExpired("tool", 2, output=b"partial stdout", stderr=b"partial stderr"),
        ]
        process.wait.side_effect = subprocess.TimeoutExpired("tool", 2)
        with mock.patch.object(supervisor.os, "name", "posix"), \
             mock.patch.object(supervisor.signal, "SIGKILL", 9, create=True), \
             mock.patch.object(supervisor.os, "killpg", create=True) as killpg, \
             mock.patch.object(supervisor.subprocess, "Popen", return_value=process):
            with self.assertRaises(run_mutagen_p0.ProcessTimeout) as caught:
                run_mutagen_p0._run_supervised(
                    ["synthetic-tool"], cwd=root, env={}, label="pipe-holder", timeout=0.1,
                )
        self.assertEqual(caught.exception.stdout, "partial stdout")
        self.assertEqual(caught.exception.stderr, "partial stderr")
        self.assertEqual(killpg.call_count, 2)
        self.assertTrue(all(call.kwargs.get("timeout") is not None for call in process.communicate.call_args_list))
        process.stdout.close.assert_called_once()
        process.stderr.close.assert_called_once()
        process.wait.assert_called_once_with(timeout=2)


if __name__ == "__main__":
    unittest.main()
