import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import capture_color_grade as capture


class CaptureConfigurationTests(unittest.TestCase):
    def campaign(self, preset):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            engine, assets, shots = root / "engine", root / "assets", root / "shots.json"
            engine.write_bytes(b"synthetic engine")
            assets.mkdir()
            pose = {"name": "test", "position": [1, 2, 3]}
            shots.write_text(json.dumps({"width": 2, "height": 3, "shots": [pose]}))
            commands = []

            def fake_run(command, **kwargs):
                commands.append(command)
                self.assertEqual(command[command.index("--visual-preset") + 1], "cool-gray")
                directory = Path(command[command.index("--shots-out") + 1])
                mode = command[command.index("--color-grade") + 1]
                receipt = {"capture": 0, "kind": "settled", "timed_out": False,
                           "color_grade": mode, "water_time_seconds": 1.0,
                           "dimensions": [2, 3], "requested_camera_creation": [1, 2, 3],
                           "requested_rotation_runtime_xyzw": [0, 0, 0, 1],
                           "requested_vertical_fov_radians": 1.0}
                if preset is not None:
                    receipt["visual_preset"] = preset
                (directory / "shots.log").write_text(json.dumps(receipt) + "\n")
                (directory / "test.png").write_bytes(
                    b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", 2, 3))
                return subprocess.CompletedProcess(command, 0)

            output = root / "output"
            argv = ["capture", "--engine", str(engine), "--assets", str(assets),
                    "--shots", str(shots), "--output", str(output)]
            with patch.object(sys, "argv", argv), patch.object(capture.subprocess, "run", side_effect=fake_run):
                if preset == "cool-gray":
                    capture.main()
                    manifest = json.loads((output / "manifest.json").read_text())
                    self.assertTrue(manifest["passed"])
                    self.assertEqual(manifest["visual_preset"], "cool-gray")
                    self.assertEqual(len(commands), 3)
                else:
                    with self.assertRaisesRegex(SystemExit, "invalid capture receipt"):
                        capture.main()
                    self.assertFalse(json.loads((output / "manifest.json").read_text())["passed"])

    def test_preset_is_pinned_for_all_three_modes(self):
        self.campaign("cool-gray")

    def test_missing_or_wrong_preset_is_refused(self):
        for preset in (None, "snowy"):
            with self.subTest(preset=preset):
                self.campaign(preset)


if __name__ == "__main__":
    unittest.main()
