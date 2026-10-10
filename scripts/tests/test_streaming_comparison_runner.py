"""Check campaign balance and effective configuration before native launches."""

import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "comparison_runner", Path(__file__).parents[1] / "compare-streaming-modes.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class CampaignTests(unittest.TestCase):
    def test_resume_preserves_completed_records_and_rejects_changed_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            completed = {"status": "completed", "artifact_dir": str(directory / "completed")}
            pending = {"status": "pending", "artifact_dir": str(directory / "pending")}
            experiment = {"status": "interrupted", "commit": "original",
                          "input_hashes": {"engine": "same"},
                          "protocol": {"modes": RUNNER.MODES},
                          "runner_sha256": "original-runner", "runs": [completed, pending]}
            RUNNER.write_json(directory / "experiment.json", experiment)
            args = SimpleNamespace(output=directory, commit="original", thermal_probe=None)
            with patch.object(RUNNER, "host_snapshot", return_value={}):
                resumed = RUNNER.resume_experiment(args, lambda: {"engine": "same"})
            self.assertEqual(resumed["runs"], [completed, pending])
            self.assertEqual(resumed["runner_sha256"], "original-runner")
            self.assertEqual(len(resumed["resume_history"]), 1)
            with self.assertRaisesRegex(ValueError, "original commit, binary"):
                RUNNER.resume_experiment(args, lambda: {"engine": "changed"})
            experiment["runs"].insert(1, {"status": "timed_out", "artifact_dir": str(directory / "failed")})
            RUNNER.write_json(directory / "experiment.json", experiment)
            with patch.object(RUNNER, "host_snapshot", return_value={}):
                resumed = RUNNER.resume_experiment(args, lambda: {"engine": "same"})
            self.assertEqual(resumed["runs"][1]["status"], "timed_out")
            experiment["runs"][1]["status"] = "running"
            RUNNER.write_json(directory / "experiment.json", experiment)
            with self.assertRaisesRegex(ValueError, "running or partial"):
                RUNNER.resume_experiment(args, lambda: {"engine": "same"})

    def isolated_runner(self, directory, timeout):
        source = str(Path(__file__).parents[1] / "compare-streaming-modes.py")
        program = f"""
import importlib.util, signal, sys
from pathlib import Path
from types import SimpleNamespace
spec = importlib.util.spec_from_file_location('runner', {source!r})
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
signal.signal(signal.SIGTERM, runner.interrupt_campaign)
runner.game_processes = lambda: []
runner.host_snapshot = lambda _probe: {{}}
runner.command = lambda *_args: [sys.executable, '-c', 'import time; time.sleep(60)']
args = SimpleNamespace(thermal_probe=None, timeout={timeout!r})
run = {{'artifact_dir': {str(directory)!r}, 'run_id': 'owned-test', 'speed': 0,
        'route_secs': 1, 'tail_secs': 1}}
runner.run_one(args, {{'cache_policy': 'test'}}, run, lambda: {{}})
"""
        return subprocess.Popen([sys.executable, "-B", "-c", program],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                start_new_session=True)

    def test_sigterm_cleans_owned_game_and_preserves_other_process(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "run"
            other = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                     start_new_session=True)
            controller = self.isolated_runner(directory, 60)
            try:
                deadline = time.monotonic() + 10
                owned = None
                while time.monotonic() < deadline:
                    path = directory / "run.json"
                    if path.exists():
                        owned = json.loads(path.read_text()).get("owned_process_group")
                        if owned:
                            break
                    time.sleep(0.01)
                self.assertIsNotNone(owned, "test controller did not launch its owned child")
                controller.send_signal(signal.SIGTERM)
                self.assertNotEqual(controller.wait(timeout=10), 0)
                with self.assertRaises(ProcessLookupError):
                    os.kill(owned, 0)
                self.assertIsNone(other.poll(), "unrelated process was stopped")
                record = json.loads((directory / "run.json").read_text())
                self.assertEqual(record["status"], "failed")
                self.assertTrue((directory / "rss.json").exists())
            finally:
                RUNNER.stop_owned_process(controller)
                RUNNER.stop_owned_process(other)

    def test_timeout_is_retained_and_owned_child_is_reaped(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "run"
            controller = self.isolated_runner(directory, 0.1)
            try:
                self.assertEqual(controller.wait(timeout=10), 0)
                record = json.loads((directory / "run.json").read_text())
                self.assertTrue(record["timed_out"])
                self.assertEqual(record["status"], "timed_out")
                with self.assertRaises(ProcessLookupError):
                    os.kill(record["owned_process_group"], 0)
            finally:
                RUNNER.stop_owned_process(controller)

    def test_pairs_are_matched_and_counterbalanced(self):
        runs = RUNNER.schedule(5, 3, 20261010, False)
        self.assertEqual(len(runs), 29)
        for scenario in ("normal", "stress"):
            selected = [run for run in runs if run["scenario"] == scenario]
            self.assertEqual(len(selected), 10)
            first_modes = []
            for pair in range(5):
                first, second = selected[pair * 2:pair * 2 + 2]
                self.assertEqual(first["pair_id"], second["pair_id"])
                self.assertEqual(first["speed"], second["speed"])
                self.assertEqual(first["route_secs"], second["route_secs"])
                self.assertEqual({first["mode"], second["mode"]}, {"fixed64", "adaptive128"})
                first_modes.append(first["mode"])
            self.assertLessEqual(abs(first_modes.count("fixed64") - first_modes.count("adaptive128")), 1)
        ablations = [run for run in runs if run["scenario"] == "ablation"]
        for offset in range(3):
            self.assertEqual({run["mode"] for run in ablations[offset * 3:offset * 3 + 3]},
                             {"fixed64", "controlled64", "adaptive64"})
        self.assertEqual(len({run["mode"] for run in ablations[::3]}), 3)

    def test_expanded_flags_preserve_actual_static_and_matched_controls(self):
        args = SimpleNamespace(engine=Path("/tmp/engine"), assets=Path("/tmp/assets"),
                               costs=Path("/tmp/costs.json"), commit="test")
        commands = {}
        for mode in RUNNER.MODES:
            run = {"mode": mode, "scenario": "normal", "run_id": mode,
                   "speed": 370, "route_secs": 45, "tail_secs": 10}
            commands[mode] = RUNNER.command(args, run, Path("/tmp/run"))
        fixed = commands["fixed64"]
        self.assertNotIn("--adaptive-streaming", fixed)
        self.assertNotIn("--streaming-costs", fixed)
        self.assertEqual(fixed[fixed.index("--max-scene-loads") + 1], "64")
        self.assertEqual(fixed[fixed.index("--max-streaming-backlog") + 1], "0")
        self.assertIn("--benchmark-vsync", fixed)
        for mode in ("controlled64", "adaptive64"):
            self.assertEqual(commands[mode][commands[mode].index("--max-scene-loads") + 1], "64")
            self.assertEqual(commands[mode][commands[mode].index("--streaming-memory-mib") + 1], "16384")
            self.assertIn("--streaming-costs", commands[mode])
        self.assertNotIn("--adaptive-streaming", commands["controlled64"])
        self.assertIn("--adaptive-streaming", commands["adaptive64"])


if __name__ == "__main__":
    unittest.main()
