"""Small plan, serialization and resume regressions; no converter workloads."""

from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest import mock

import audit_lod_pack as audit
import plan_lod_validation as planner
import run_validation_plan as runner


class AuditSerializationTests(unittest.TestCase):
    def test_chunk_receipt_survives_json_roundtrip(self):
        atlas = bytearray(180)
        atlas[:12] = b"\xabKTX 20\xbb\r\n\x1a\n"
        struct.pack_into("<13I", atlas, 12, 0, 1, 16, 16, 0, 0, 1, 3, 0, 152, 28, 0, 0)
        atlas[164] = 166
        atlas[166] = 2
        for mip, size in enumerate((256, 64, 16)):
            struct.pack_into("<3Q", atlas, 80 + mip * 24, len(atlas), size, size)
            atlas.extend(bytes(size))
        coordinates = [(0, 1), (-1, 2)]
        nodes = [{"name": "chunk", "children": []}]
        for cell_id, (x, y) in enumerate(coordinates, 10):
            index = len(nodes)
            nodes[0]["children"].append(index)
            nodes.append({"name": f"cell_{x}_{y}", "extras": {"cell_id": cell_id}, "children": [index + 1]})
            nodes.append({"name": "terrain", "children": list(range(index + 2, index + 6))})
            nodes.extend({"name": "terrain_quadrant_" + quadrant} for quadrant in ("sw", "se", "nw", "ne"))
        document = {
            "asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0]}],
            "nodes": nodes, "buffers": [{"byteLength": len(atlas)}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": len(atlas)}],
            "images": [{"bufferView": 0, "mimeType": "image/ktx2"}],
        }
        encoded = json.dumps(document).encode()
        encoded += b" " * (-len(encoded) % 4)
        binary = bytes(atlas)
        binary += bytes(-len(binary) % 4)
        payload = (
            b"glTF" + struct.pack("<II", 2, 28 + len(encoded) + len(binary))
            + struct.pack("<II", len(encoded), 0x4E4F534A) + encoded
            + struct.pack("<II", len(binary), 0x004E4942) + binary
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            relative = "lod/00000001/4/cell_0_0.glb"
            path = root / relative
            path.parent.mkdir(parents=True)
            path.write_bytes(payload)
            row = (1, 4, 0, 0, relative, audit.sha(payload), 0, 0, 0, 1, 1, 1, "0,1;-1,2")
            cells = {(1, *xy): cell_id for cell_id, xy in enumerate(coordinates, 10)}
            _, receipt = audit.audit_chunk(root, row, cells, {relative: "a" * 64})
        self.assertEqual(receipt["source_cells"], [[-1, 2], [0, 1]])
        self.assertEqual(receipt, json.loads(json.dumps(receipt)))


class ResumeHistoryTests(unittest.TestCase):
    def write_status(self, path, label, binary, previous=None):
        value = {
            "provenance": {"converter_sha256": binary},
            "runs": [{"label": label, "output": str(path.parent / label)}],
        }
        if previous:
            value["resume"] = {
                "previous_status": str(previous), "previous_status_sha256": runner.file_hash(previous),
            }
        path.write_text(json.dumps(value))

    def test_recursive_history_retains_each_leg_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            first, second, third = [Path(directory).resolve() / name for name in ("first.json", "second.json", "third.json")]
            self.write_status(first, "cold", "old-binary")
            self.write_status(second, "warm", "new-binary", first)
            self.write_status(third, "gpu", "new-binary", second)
            records, history = runner.load_resume_history(third)
            self.assertEqual(list(records), ["cold", "warm", "gpu"])
            self.assertEqual(records["cold"]["converter_sha256"], "old-binary")
            self.assertEqual(records["warm"]["converter_sha256"], "new-binary")
            self.assertEqual([item["path"] for item in history], [str(first), str(second), str(third)])
            first.write_text(first.read_text() + "\n")
            with self.assertRaisesRegex(ValueError, "recorded previous status changed"):
                runner.load_resume_history(third)

    def test_cycle_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "self.json"
            path.write_text(json.dumps({"resume": {"previous_status": str(path), "previous_status_sha256": "0" * 64}}))
            with self.assertRaisesRegex(ValueError, "cyclic"):
                runner.load_resume_history(path)


class ValidationPlanTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        self.matrix = self.root / "matrix"
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "conversion-manifest.json").write_text('{"fixture": true}\n')
        self.data = self.root / "Data"
        self.data.mkdir()
        self.binary = self.root / "converter"
        self.binary.write_text("unused fixture binary\n")
        self.plan = self.make_plan("cpu")
        self.plan_path = self.root / "plan.json"
        self.plan_path.write_text(json.dumps(self.plan))
        self.previous_status = self.root / "previous-status.json"
        self.status_path = self.matrix / "resumed-status.json"
        self.stdout, self.stderr = io.StringIO(), io.StringIO()

    def make_plan(self, ordinary_encoder):
        arguments = [
            "plan_lod_validation.py", "--converter", str(self.binary),
            "--data", str(self.data), "--source-pack", str(self.source),
            "--root", str(self.matrix), "--guard-pid", "0", "--expected-chunks", "1",
            "--texture-encoder", ordinary_encoder, "--gpu-quality", "4", "--gpu-batch-mb", "32",
        ]
        output = io.StringIO()
        with mock.patch.object(sys, "argv", arguments), redirect_stdout(output):
            planner.main()
        return json.loads(output.getvalue())

    def write_completed_prefix(self, count=5):
        records = []
        source_hash = runner.file_hash(self.source / "conversion-manifest.json")
        for run in self.plan["runs"][:count]:
            output = Path(run["output"])
            output.mkdir(parents=True, exist_ok=True)
            ordinary_manifest = output / "conversion-manifest.json"
            ordinary_manifest.write_text(json.dumps({"label": run["label"]}))
            lod_manifest = {"chunk_inputs": {"fixture": "a" * 64}}
            (output / "lod-manifest.json").write_text(json.dumps(lod_manifest))
            audit_path = Path(runner.option(run["lod_validation"], "--output"))
            audit_path.parent.mkdir(parents=True, exist_ok=True)
            audit_path.write_text(json.dumps({
                "passed": True, "pack": str(output), "chunks": {"fixture": {}},
                "ordinary_manifest_hash": runner.file_hash(ordinary_manifest),
                "lod_manifest": lod_manifest,
                "coverage_reference": {"pack": str(self.source), "manifest_hash": source_hash},
            }))
            records.append({
                "label": run["label"], "output": str(output), "passed": True,
                "steps": [{"phase": phase, "exit_code": 0} for phase in ("run", "ordinary_validation", "lod_validation")],
            })
        self.previous_status.write_text(json.dumps({
            "provenance": {
                "converter_sha256": runner.file_hash(self.binary),
                "protected_pack": str(self.source), "protected_manifest_sha256": source_hash,
            },
            "runs": records,
        }))

    def invoke_runner(self, after):
        arguments = [
            "run_validation_plan.py", "--plan", str(self.plan_path),
            "--resume-after", after, "--previous-status", str(self.previous_status),
            "--status", str(self.status_path),
        ]
        with mock.patch.object(sys, "argv", arguments), redirect_stdout(self.stdout), redirect_stderr(self.stderr):
            runner.main()

    def test_gpu_tuning_matches_actual_encoders(self):
        for ordinary_encoder in ("cpu", "gpu"):
            for run in self.make_plan(ordinary_encoder)["runs"]:
                with self.subTest(ordinary_encoder=ordinary_encoder, leg=run["label"]):
                    command = run["run"][run["run"].index("--") + 1:]
                    uses_gpu = ordinary_encoder == "gpu" or runner.option(command, "--lod-encoder") == "gpu"
                    self.assertEqual("--gpu-quality" in command, uses_gpu)
                    self.assertEqual("--gpu-batch-mb" in command, uses_gpu)
                    if uses_gpu:
                        self.assertEqual(runner.option(command, "--gpu-quality"), "4")
                        self.assertEqual(runner.option(command, "--gpu-batch-mb"), "32")

    def test_verified_fresh_output_can_resume_final_warm(self):
        self.write_completed_prefix()
        with mock.patch.object(runner.subprocess, "run", return_value=mock.Mock(returncode=0)) as execute:
            self.invoke_runner("full-gpu-fresh")
        warm = self.plan["runs"][-1]
        self.assertEqual([call.args[0] for call in execute.call_args_list], [warm[phase] for phase in ("run", "ordinary_validation", "lod_validation")])
        status = json.loads(self.status_path.read_text())
        self.assertTrue(status["passed"])
        self.assertEqual([run["label"] for run in status["runs"]], ["full-gpu-warm"])
        fresh = status["resume"]["preserved_prefix"][-1]
        self.assertEqual(fresh["label"], "full-gpu-fresh")
        self.assertEqual(fresh["audit_sha256"], runner.file_hash(Path(fresh["audit"])))
        self.assertEqual(fresh["converter_sha256"], runner.file_hash(self.binary))

    def test_changed_fresh_manifest_is_rejected_before_warm(self):
        self.write_completed_prefix()
        manifest = Path(self.plan["runs"][-1]["output"]) / "conversion-manifest.json"
        manifest.write_text(manifest.read_text() + "\n")
        with mock.patch.object(runner.subprocess, "run") as execute, self.assertRaises(SystemExit):
            self.invoke_runner("full-gpu-fresh")
        execute.assert_not_called()
        self.assertIn("resume prefix manifests changed after audit: full-gpu-fresh", self.stderr.getvalue())
        self.assertFalse(self.status_path.exists())

    def test_failed_fresh_audit_is_rejected_before_warm(self):
        self.write_completed_prefix()
        audit_path = Path(runner.option(self.plan["runs"][-2]["lod_validation"], "--output"))
        receipt = json.loads(audit_path.read_text())
        receipt["passed"] = False
        audit_path.write_text(json.dumps(receipt))
        with mock.patch.object(runner.subprocess, "run") as execute, self.assertRaises(SystemExit):
            self.invoke_runner("full-gpu-fresh")
        execute.assert_not_called()
        self.assertIn("resume prefix lacks a successful complete LOD audit: full-gpu-fresh", self.stderr.getvalue())
        self.assertFalse(self.status_path.exists())

    def test_changed_source_provenance_is_rejected_before_warm(self):
        self.write_completed_prefix()
        previous = json.loads(self.previous_status.read_text())
        previous["provenance"]["protected_manifest_sha256"] = "0" * 64
        self.previous_status.write_text(json.dumps(previous))
        with mock.patch.object(runner.subprocess, "run") as execute, self.assertRaises(SystemExit):
            self.invoke_runner("full-gpu-fresh")
        execute.assert_not_called()
        self.assertIn("protected source differs from an inherited run", self.stderr.getvalue())
        self.assertFalse(self.status_path.exists())

    def test_changed_binary_requires_explicit_permission_before_warm(self):
        self.write_completed_prefix()
        self.binary.write_text("different unused fixture binary\n")
        with mock.patch.object(runner.subprocess, "run") as execute, self.assertRaises(SystemExit):
            self.invoke_runner("full-gpu-fresh")
        execute.assert_not_called()
        self.assertIn("resume binary changed", self.stderr.getvalue())
        self.assertFalse(self.status_path.exists())

    def test_existing_ordinary_output_remains_protected(self):
        self.write_completed_prefix(count=1)
        output = Path(self.plan["runs"][1]["output"])
        output.mkdir()
        with mock.patch.object(runner.subprocess, "run") as execute, self.assertRaises(SystemExit):
            self.invoke_runner("meta-cpu-cold")
        execute.assert_not_called()
        self.assertIn(f"isolated output already exists: {output}", self.stderr.getvalue())
        self.assertFalse(self.status_path.exists())

    def test_final_warm_cannot_reuse_an_unrelated_existing_output(self):
        self.write_completed_prefix()
        output = self.matrix / "unrelated-output"
        output.mkdir()
        warm = self.plan["runs"][-1]
        warm["output"] = str(output)
        warm["run"][warm["run"].index("--") + 3] = str(output)
        warm["ordinary_validation"][2] = str(output)
        warm["lod_validation"][2] = str(output)
        self.plan_path.write_text(json.dumps(self.plan))
        with mock.patch.object(runner.subprocess, "run") as execute, self.assertRaises(SystemExit):
            self.invoke_runner("full-gpu-fresh")
        execute.assert_not_called()
        self.assertIn(f"isolated output already exists: {output}", self.stderr.getvalue())
        self.assertFalse(self.status_path.exists())

    def test_final_warm_cannot_target_protected_source(self):
        self.plan["runs"][-1]["output"] = str(self.source)
        with self.assertRaisesRegex(ValueError, "disjoint from source assets"):
            runner.validate(self.plan)


if __name__ == "__main__":
    unittest.main()
