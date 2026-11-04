"""Receipt and frozen-reference guards for the tone-map tuning workflow."""

import base64
import copy
import importlib.util
import json
import math
import os
from pathlib import Path
import struct
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zlib


SCRIPT = Path(__file__).resolve().parents[1] / "capture_tonemap_tuning.py"
SPEC = importlib.util.spec_from_file_location("capture_tonemap_tuning", SCRIPT)
TUNING = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TUNING)


def png(width=32, height=18):
    def chunk(kind, payload):
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress((b"\0" + b"\x60\x80\xa0" * width) * height)) + chunk(b"IEND", b""))


def camera_and_receipt():
    camera = {"width": 32, "height": 18, "shots": [{
        "name": "riverwood", "position": [21750.0, -44530.0, 140.0],
        "yaw": 90.0, "pitch": -3.0, "hfov": 72.73435310118231,
    }]}
    rotation = [0.018509898334741592, -0.7068644762039185, 0.018509898334741592, 0.7068644762039185]
    grade = {"color_grade": "snapshot-srgb", "visual_preset": "cool-gray", "preset_saturation": 1.25,
             "resolved_grade": {"brightness": 1.38},
             "source": {"tonemapping": "TonyMcMapface", "resolution_error": None}}
    receipt = {
        "capture": 0, "kind": "settled", "timed_out": False, "water_time_seconds": 1.0,
        "dimensions": [32, 18], "requested_camera_creation": camera["shots"][0]["position"],
        "requested_rotation_runtime_xyzw": rotation, "requested_vertical_fov_radians": math.pi / 4,
        **{key: grade[key] for key in ["color_grade", "visual_preset", "preset_saturation", "resolved_grade"]},
        "grading_source": grade["source"],
        "captured_frame": {"camera_creation": camera["shots"][0]["position"],
                           "rotation_runtime_xyzw": rotation, "vertical_fov_radians": math.pi / 4,
                           "render_origin": [5, -11], "pending_specializations": 0,
                           "work": {**dict.fromkeys(TUNING.MINIMUM_WORK_COUNTERS, 0),
                                    "renderer_ready": True, "quiet_frames": 0}, "grading": grade},
    }
    return camera, receipt


class CaptureEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.image = self.root / "capture.png"
        self.image.write_bytes(png())
        self.camera, self.receipt = camera_and_receipt()
        self.commands = []
        self.environments = []
        native = patch.object(TUNING.subprocess, "run", side_effect=self.fake_engine)
        native.start()
        self.addCleanup(native.stop)

    def tuning_receipt(self, path):
        return {"path": str(path), "sha256": TUNING.sha256(path),
                "effective": {"brightness": 0.6}, "accepted": True, "enabled": True}

    def fake_engine(self, command, **kwargs):
        """Exercise orchestration without a native executable or GPU on every OS."""
        self.commands.append(command)
        self.environments.append(kwargs.get("env"))
        if "--grass-tuning-info" in command:
            path = Path(command[command.index("--grass-tuning-info") + 1])
            return subprocess.CompletedProcess(command, 0, json.dumps(self.tuning_receipt(path)), "")
        directory = Path(command[command.index("--shots-out") + 1])
        directory.mkdir(parents=True)
        fixture = json.loads(Path(command[command.index("--fixture") + 1]).read_text(encoding="utf-8"))
        receipt = fixture["receipt"]
        self.assertFalse(any(value.startswith("--grass-tuning=") for value in command))
        if "--grass-tuning" in command:
            path = Path(command[command.index("--grass-tuning") + 1])
            receipt["grass_tuning"] = self.tuning_receipt(path)
            receipt["captured_frame"]["grass_tuning"] = receipt["grass_tuning"]
        (directory / "riverwood.png").write_bytes(base64.b64decode(fixture["png"]))
        (directory / "shots.log").write_text(json.dumps(receipt) + "\n", encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    def validate(self, receipt):
        return TUNING.validate_capture(receipt, self.receipt, self.camera, self.image)

    def test_settled_receipt_passes_and_pose_or_fov_drift_fails(self):
        self.assertTrue(self.validate(self.receipt)["passed"])
        for key, replacement in (("camera_creation", [21751, -44530, 140]),
                                 ("rotation_runtime_xyzw", [0, 0, 0, 1]),
                                 ("vertical_fov_radians", 1.0), ("render_origin", [5, -12])):
            with self.subTest(key=key), self.assertRaises(ValueError):
                changed = copy.deepcopy(self.receipt)
                changed["captured_frame"][key] = replacement
                self.validate(changed)

    def test_readiness_missing_failed_pending_or_unknown_is_rejected(self):
        for key, value in (("failed_cells", 1), ("pending_render_transfers", 2),
                           ("renderer_ready", False), ("validation_failures", None),
                           ("future_pending_counter", 1)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                changed = copy.deepcopy(self.receipt)
                changed["captured_frame"]["work"][key] = value
                self.validate(changed)
        changed = copy.deepcopy(self.receipt)
        del changed["captured_frame"]["work"]["failed_cells"]
        with self.assertRaisesRegex(ValueError, "missing readiness"):
            self.validate(changed)

    def test_late_callback_cannot_relabel_captured_grade(self):
        changed = copy.deepcopy(self.receipt)
        changed["grading_source"] = {"tonemapping": "SomeLaterMode"}
        with self.assertRaisesRegex(ValueError, "source disagree"):
            self.validate(changed)
        changed = copy.deepcopy(self.receipt)
        changed["color_grade"] = "record-srgb"
        with self.assertRaisesRegex(ValueError, "color_grade disagree"):
            self.validate(changed)

    def test_water_clock_timeout_and_png_size_are_guarded(self):
        for key, value in (("timed_out", True), ("water_time_seconds", None), ("dimensions", [64, 36])):
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.validate({**self.receipt, key: value})
        self.image.write_bytes(png(64, 36))
        with self.assertRaisesRegex(ValueError, "dimensions"):
            self.validate(self.receipt)

    def test_variant_cannot_replace_frozen_scene_inputs(self):
        config = {"common_args": ["--sun-position", "180,45"], "variants": [{"name": "test", "args": []}]}
        TUNING.validate_variants(config)
        for arguments in (["--shots", "other.json"], ["--ini=other.ini"],
                          ["--sun-position", "0,0"], ["--shots-out", "elsewhere"]):
            with self.subTest(args=arguments), self.assertRaises(ValueError):
                TUNING.validate_variants({**config, "variants": [{"name": "test", "args": arguments}]})

    def test_expected_raw_record_subset_is_checked_without_inventing_fields(self):
        TUNING.assert_expected(self.receipt, {"grading_source": {"tonemapping": "TonyMcMapface"}})
        with self.assertRaisesRegex(ValueError, "missing loaded_record"):
            TUNING.assert_expected(self.receipt, {"grading_source": {"loaded_record": {"form_id": 77703}}})

    def test_wrong_frozen_reference_hash_is_rejected_before_capture(self):
        with self.assertRaisesRegex(ValueError, "hash changed"):
            TUNING.freeze_input(self.root / "config.json", {"path": str(self.image), "sha256": "0" * 64},
                                self.root / "copied.png")

    def make_campaign(self):
        camera = self.root / "camera.json"
        TUNING.write_json(camera, self.camera)
        ini = self.root / "capture.ini"
        ini.write_text("[Display]\nfShadowDistance=8000\n")
        log = self.root / "approved.log"
        log.write_text("engine log before receipt\n" + json.dumps(self.receipt) + "\n")
        fixture = self.root / "fixture.json"
        TUNING.write_json(fixture, {"png": base64.b64encode(png()).decode(), "receipt": self.receipt})
        engine = self.root / "fake-engine"
        fake = engine.with_suffix(".py") if os.name == "nt" else engine
        fake.write_text("""#!/usr/bin/env python3
import base64,json,sys
from pathlib import Path
arguments=sys.argv[1:]
directory=Path(arguments[arguments.index('--shots-out')+1])
directory.mkdir(parents=True)
fixture=json.loads(Path(arguments[arguments.index('--fixture')+1]).read_text())
(directory/'riverwood.png').write_bytes(base64.b64decode(fixture['png']))
(directory/'shots.log').write_text(json.dumps(fixture['receipt'])+'\\n')
""")
        if os.name == "nt":
            # Windows cannot execute a shebang script: a batch launcher runs it with this Python.
            engine = engine.with_suffix(".cmd")
            engine.write_text(f'@"{sys.executable}" "%~dp0{fake.name}" %*\n')
        else:
            engine.chmod(0o755)
        assets = self.root / "assets"
        assets.mkdir()
        config = {"schema_version": 1, "common_args": ["--fixture", "{input:fixture}"],
                  "variants": [{"name": "approved", "label": "Approved </script><script>bad()</script>",
                                "args": [], "expect_receipt": {"color_grade": "snapshot-srgb"}},
                               {"name": "repeat", "args": []}],
                  "files": {"fixture": {"path": str(fixture), "sha256": TUNING.sha256(fixture)}}}
        for name, path in (("approved", self.image), ("skyrim", self.image), ("camera", camera),
                           ("ini", ini), ("approved_receipt", log)):
            config[name] = {"path": str(path), "sha256": TUNING.sha256(path)}
        config_path = self.root / "config.json"
        TUNING.write_json(config_path, config)
        source = {"git_head": "fixture", "files": {"file": "frozen"}}
        source["sha256"] = TUNING.canonical_hash(source)
        return config_path, engine, assets, source

    def test_campaign_preserves_references_and_revalidates_without_engine(self):
        config, engine, assets, source = self.make_campaign()
        output = self.root / "campaign"
        with patch.object(TUNING, "source_snapshot", return_value=source):
            manifest = TUNING.capture(config, engine, assets, output)
        self.assertTrue(manifest["passed"])
        self.assertEqual(len(manifest["runs"]), 2)
        self.assertEqual((output / "references/approved.png").read_bytes(), self.image.read_bytes())
        page = (output / "comparison.html").read_text(encoding="utf-8")
        self.assertNotIn("Approved </script><script>bad()", page)
        self.assertIn("data:image/png;base64,", page)
        self.assertNotIn('src="https://', page)
        self.assertIn("Previous approved setup", page)
        self.assertIn("Skyrim reference", page)
        preview = (output / "comparison-preview.html").read_text(encoding="utf-8")
        preview_data = json.loads(preview.split('<script id="comparison-data" type="application/json">', 1)[1].split('</script>', 1)[0])
        self.assertEqual(preview_data["approved"], str(output / "references/approved.png"))
        self.assertEqual(preview_data["skyrim"], str(output / "references/skyrim.png"))
        for name in ("approved", "skyrim"):
            for field in ("original_path", "path"):
                self.assertFalse(Path(preview_data["references"][name][field]).is_absolute())
                self.assertTrue(Path(manifest["inputs"][name][field]).is_absolute())
        # The preview embeds paths as JSON, which doubles a Windows backslash.
        self.assertEqual(preview.count(json.dumps(str(output / "references/approved.png"))[1:-1]), 1)
        self.assertEqual(preview.count(json.dumps(str(output / "references/skyrim.png"))[1:-1]), 1)
        self.assertEqual(len(preview_data["runs"]), 2)
        for run in manifest["runs"]:
            stage_page = (output / f"comparison-{run['name']}-preview.html").read_text(encoding="utf-8")
            stage_data = json.loads(stage_page.split('<script id="comparison-data" type="application/json">', 1)[1].split('</script>', 1)[0])
            self.assertEqual([entry["name"] for entry in stage_data["runs"]], [run["name"]])
            self.assertEqual(stage_data["approved"], preview_data["approved"])
            self.assertEqual(stage_data["skyrim"], preview_data["skyrim"])
            self.assertEqual(stage_data["runs"][0]["image"], str(output / run["image_file"]))
            self.assertEqual(stage_data["runs"][0]["sha256"], run["image_sha256"])
        with patch.object(TUNING.subprocess, "run", side_effect=AssertionError("Offline validation launched a process")):
            self.assertTrue(TUNING.validate_existing(output)["passed"])
        (output / "references/skyrim.png").write_bytes(png(64, 36))
        with self.assertRaisesRegex(ValueError, "Frozen skyrim changed"):
            TUNING.validate_existing(output)

    def test_existing_output_is_refused(self):
        config, engine, assets, source = self.make_campaign()
        output = self.root / "campaign"
        output.mkdir()
        with patch.object(TUNING, "source_snapshot", return_value=source), self.assertRaises(FileExistsError):
            TUNING.capture(config, engine, assets, output)

    def test_grass_tuning_file_matrix_is_frozen_receipted_and_revalidates_offline(self):
        config, engine, assets, source = self.make_campaign()
        files = [self.root / "d.toml", self.root / "darker-start.toml"]
        for path in files:
            path.write_text("brightness = 0.6\n", encoding="utf-8")
        output = self.root / "matrix"
        with patch.object(TUNING, "source_snapshot", return_value=source), patch.dict(
                TUNING.os.environ, {"MUDCRAB_GRASS_SHADING": "f", "MUDCRAB_GRASS_TUNING": "other.toml"}):
            manifest = TUNING.capture(config, engine, assets, output, files)
        self.assertEqual([run["name"] for run in manifest["runs"]], ["d", "darker-start"])
        for run in manifest["runs"]:
            receipt = run["grass_tuning"]
            self.assertEqual(receipt["effective"], {"brightness": 0.6})
            self.assertIn(str(output / run["name"]), receipt["path"])
            self.assertEqual(Path(receipt["path"]).read_bytes(), files[0].read_bytes())
            self.assertIn(receipt["path"], run["command"])
        self.assertEqual(sum("--grass-tuning-info" in cmd for cmd in self.commands), 2)
        for environment in self.environments:
            self.assertNotIn("MUDCRAB_GRASS_SHADING", environment)
            self.assertNotIn("MUDCRAB_GRASS_TUNING", environment)
        self.commands.clear()
        self.assertTrue(TUNING.validate_existing(output)["passed"])
        self.assertFalse(self.commands)
        Path(manifest["runs"][0]["grass_tuning"]["path"]).write_text("changed", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Frozen grass tuning changed"):
            TUNING.validate_existing(output)

    def test_grass_tuning_callback_and_frame_must_match_authoritative_receipt(self):
        expected = {"path": "settings.toml", "sha256": "hash", "effective": {"brightness": 0.6}, "selection": "cli"}
        receipt = copy.deepcopy(self.receipt)
        receipt["grass_tuning"] = expected
        receipt["captured_frame"]["grass_tuning"] = expected
        TUNING.validate_capture(receipt, self.receipt, self.camera, self.image, expected_grass_tuning=expected)
        receipt["grass_tuning"] = {**expected, "effective": {"brightness": 1.0}}
        with self.assertRaisesRegex(ValueError, "grass tuning differs"):
            TUNING.validate_capture(receipt, self.receipt, self.camera, self.image, expected_grass_tuning=expected)

    def test_grass_tuning_paths_info_and_duplicates_are_guarded(self):
        for arguments in (["--grass-tuning"], ["--grass-tuning="],
                          ["--grass-tuning", "--grass"],
                          ["--grass-tuning=a", "--grass-tuning", "b"]):
            with self.subTest(arguments=arguments), self.assertRaises(ValueError):
                TUNING.grass_tuning_argument(arguments)
        path = self.root / "tuning.toml"
        path.write_text("opaque to harness", encoding="utf-8")
        target = self.root / "stage"
        target.mkdir()
        arguments, receipt = TUNING.freeze_grass_tuning([f"--grass-tuning={path}"], self.root / "engine", target)
        self.assertEqual(arguments, ["--grass-tuning", str(target / "grass-tuning.toml")])
        self.assertEqual(receipt["sha256"], TUNING.sha256(path))
        bad = {**receipt, "sha256": "0" * 64}
        with patch.object(TUNING.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(bad))):
            with self.assertRaisesRegex(ValueError, "receipt hash differs"):
                TUNING.engine_grass_tuning_receipt(self.root / "engine", target / "grass-tuning.toml")
        with patch.object(TUNING.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 0, json.dumps({**receipt, "accepted": False}))):
            with self.assertRaisesRegex(ValueError, "rejected engine grass tuning"):
                TUNING.engine_grass_tuning_receipt(self.root / "engine", target / "grass-tuning.toml")

    def test_environment_grass_tuning_is_frozen_and_receipted(self):
        config, engine, assets, source = self.make_campaign()
        selected = self.root / "environment.toml"
        selected.write_text("brightness = 0.6\n", encoding="utf-8")
        output = self.root / "environment-campaign"
        with patch.object(TUNING, "source_snapshot", return_value=source), patch.dict(
                TUNING.os.environ, {"MUDCRAB_GRASS_TUNING": str(selected)}):
            manifest = TUNING.capture(config, engine, assets, output)
        for run in manifest["runs"]:
            receipt = run["grass_tuning"]
            self.assertIsNotNone(receipt)
            self.assertEqual(Path(receipt["path"]).read_bytes(), selected.read_bytes())
            self.assertEqual(run["command"].count("--grass-tuning"), 1)
            index = run["command"].index("--grass-tuning")
            self.assertEqual(run["command"][index + 1], receipt["path"])
        for environment in self.environments:
            self.assertNotIn("MUDCRAB_GRASS_TUNING", environment)
        self.assertTrue(TUNING.validate_existing(output)["passed"])

    def test_failed_variant_keeps_earlier_gallery_and_failure_manifest(self):
        config_path, engine, assets, source = self.make_campaign()
        config = json.loads(config_path.read_text(encoding="utf-8"))
        config["variants"][1]["expect_receipt"] = {"color_grade": "record-srgb"}
        TUNING.write_json(config_path, config)
        output = self.root / "campaign"
        with patch.object(TUNING, "source_snapshot", return_value=source), self.assertRaises(ValueError):
            TUNING.capture(config_path, engine, assets, output)
        manifest = json.loads((output / "manifest.json").read_text(encoding="utf-8"))
        self.assertFalse(manifest["passed"])
        self.assertIn("error", manifest["runs"][1])
        self.assertTrue((output / "comparison.html").is_file())


if __name__ == "__main__":
    unittest.main()
