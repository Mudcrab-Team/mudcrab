"""Small serialization and resume-history regressions; no converter workloads."""

import json
from pathlib import Path
import struct
import tempfile
import unittest

import audit_lod_pack as audit
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


if __name__ == "__main__":
    unittest.main()
