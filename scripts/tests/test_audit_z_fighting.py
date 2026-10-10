"""Synthetic regressions for the static depth-risk audit; no retail assets."""

import importlib.util
import json
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("audit_z_fighting", Path(__file__).parents[1] / "audit-z-fighting.py")
AUDIT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = AUDIT
SPEC.loader.exec_module(AUDIT)


def material(first=AUDIT.DEPTH_TEST, second=AUDIT.DEPTH_WRITE, **extra):
    return {"extras": {"openSkyrim": {"shaderFlags1": first, "shaderFlags2": second,
                                       "shapeBlock": 9, "shaderBlock": 10}}, **extra}


def fixture(points, nodes=None, materials=None):
    binary = b"".join(struct.pack("<3f", *p) for p in points)
    document = {"asset": {"version": "2.0"}, "scene": 0,
        "scenes": [{"nodes": [0]}], "nodes": nodes or [{"mesh": 0}],
        "buffers": [{"byteLength": len(binary)}],
        "bufferViews": [{"buffer": 0, "byteLength": len(binary)}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": len(points), "type": "VEC3"}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
        "materials": materials or [material()]}
    return document, binary


def write_glb(path, document, binary):
    encoded = json.dumps(document).encode()
    encoded += b" " * (-len(encoded) % 4)
    binary += b"\0" * (-len(binary) % 4)
    chunks = struct.pack("<I4s", len(encoded), b"JSON") + encoded
    chunks += struct.pack("<I4s", len(binary), b"BIN\0") + binary
    path.write_bytes(struct.pack("<4sII", b"glTF", 2, 12+len(chunks)) + chunks)


def create_database(path):
    with sqlite3.connect(path) as db:
        db.executescript('''CREATE TABLE "references" (id INTEGER, cell_id INTEGER,
            worldspace_id INTEGER, is_exterior INTEGER, pos_x REAL, pos_y REAL, pos_z REAL,
            rot_x REAL, rot_y REAL, rot_z REAL, scale REAL, header_flags INTEGER,
            enable_parent_id INTEGER, base_form_id INTEGER);
            CREATE TABLE statics (id INTEGER, model_path TEXT);
            INSERT INTO statics VALUES (1, 'meshes/test.nif');''')


def insert_reference(connection, ident, flags=0, parent=None,
                     transform=(0, 0, 0, 0, 0, 0, 1)):
    connection.execute('INSERT INTO "references" VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
        (ident, 10, 60, 1, *transform, flags, parent, 1))


class OverlapTests(unittest.TestCase):
    def scan(self, points, **kwargs):
        document, binary = fixture(points, **kwargs)
        triangles, _ = AUDIT.triangles(document, binary, 100)
        return AUDIT.find_overlaps(triangles, 1e-4, 1e-8, 1000, 10)

    def test_duplicate_triangles_inside_one_primitive(self):
        result = self.scan([(0,0,0), (1,0,0), (0,1,0)] * 2)
        self.assertEqual(result["pairs"], 1)
        self.assertAlmostEqual(result["examples"][0]["overlap_area"], .5)
        self.assertEqual(result["examples"][0]["a"]["shape_block"], 9)

    def test_partial_overlap_with_different_triangulation(self):
        result = self.scan([(0,0,0), (2,0,0), (0,2,0), (1,0,0), (3,0,0), (1,2,0)])
        self.assertEqual(result["pairs"], 1)
        self.assertAlmostEqual(result["examples"][0]["overlap_area"], .5)

    def test_adjacent_triangles_are_not_overlaps(self):
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0), (1,0,0), (1,1,0), (0,1,0)])["pairs"], 0)

    def test_point_contact_is_not_an_overlap(self):
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0), (1,0,0), (2,0,0), (1,1,0)])["pairs"], 0)

    def test_bounding_boxes_alone_do_not_establish_overlap(self):
        self.assertEqual(self.scan([(0,0,0), (2,0,0), (0,2,0), (2,2,0), (1.5,2,0), (2,1.5,0)])["pairs"], 0)

    def test_separated_planes_are_not_candidates(self):
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0), (0,0,.01), (1,0,.01), (0,1,.01)])["pairs"], 0)

    def test_near_planes_are_candidates(self):
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0), (0,0,.00001), (1,0,.00001), (0,1,.00001)])["pairs"], 1)

    def test_crossing_surfaces_are_not_coplanar_candidates(self):
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0), (0,0,-1), (1,0,1), (0,1,-1)])["pairs"], 0)

    def test_opposing_single_sided_faces_are_excluded(self):
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0), (0,1,0), (1,0,0), (0,0,0)])["pairs"], 0)

    def test_opposing_double_sided_faces_are_candidates(self):
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0), (0,1,0), (1,0,0), (0,0,0)],
                                   materials=[material(doubleSided=True)])["pairs"], 1)

    def test_nested_node_transforms_prevent_false_overlap(self):
        nodes = [{"children": [1,2]}, {"mesh": 0}, {"translation": [0,0,1], "children": [3]}, {"mesh": 0}]
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0)], nodes=nodes)["pairs"], 0)

    def test_instances_at_same_transform_are_candidates(self):
        nodes = [{"children": [1,2]}, {"mesh": 0}, {"mesh": 0}]
        self.assertEqual(self.scan([(0,0,0), (1,0,0), (0,1,0)], nodes=nodes)["pairs"], 1)

    def test_rotated_scaled_node_transform(self):
        matrix = AUDIT.node_matrix({"translation": [10,20,30], "rotation": [0,0,1,0], "scale": [2,3,4]})
        self.assertEqual(AUDIT.transform(matrix, (1,2,3)), (8,14,42))

    def test_mirrored_instance_retains_effective_culling_orientation(self):
        nodes = [{"children": [1,2]}, {"mesh": 0}, {"mesh": 0, "scale": [-1,1,1]}]
        points = [(-1,0,0), (1,0,0), (0,1,0)]
        self.assertEqual(self.scan(points, nodes=nodes)["pairs"], 1)

    def test_comparison_budget_raises_instead_of_clean_result(self):
        document, binary = fixture([(0,0,0), (1,0,0), (0,1,0)] * 3)
        triangles, _ = AUDIT.triangles(document, binary, 100)
        with self.assertRaisesRegex(AUDIT.AuditError, "comparison budget"):
            AUDIT.find_overlaps(triangles, 1e-4, 1e-8, 1, 10)

    def test_example_budget_does_not_truncate_pair_count(self):
        document, binary = fixture([(0,0,0), (1,0,0), (0,1,0)] * 3)
        triangles, _ = AUDIT.triangles(document, binary, 100)
        result = AUDIT.find_overlaps(triangles, 1e-4, 1e-8, 100, 1)
        self.assertEqual(result["pairs"], 3)
        self.assertEqual(len(result["examples"]), 1)


class InputTests(unittest.TestCase):
    def test_optional_metadata_accepts_non_object_json(self):
        for value in (None, [], "x", 7, 1.5, True):
            for metadata in ({"extras": value}, {"extras": {"openSkyrim": value}}):
                with self.subTest(metadata=metadata):
                    self.assertIsNone(AUDIT.material_risks(metadata))
                    document, binary = fixture([(0,0,0), (1,0,0), (0,1,0)], materials=[metadata])
                    geometry, _ = AUDIT.triangles(document, binary, 100)
                    self.assertEqual(len(geometry), 1)
                    self.assertIsNone(geometry[0].identity["shape_block"])
                    self.assertIsNone(geometry[0].identity["shader_block"])

    def test_non_object_asset_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "test.glb"
            for value in (None, [], "x", 7, 1.5, True):
                with self.subTest(asset=value):
                    document, binary = fixture([(0,0,0), (1,0,0), (0,1,0)])
                    document["asset"] = value
                    write_glb(path, document, binary)
                    with self.assertRaisesRegex(AUDIT.AuditError, "missing glTF 2.0 document"):
                        AUDIT.load_glb(path)

    def test_cli_malformed_files_do_not_stop_serial_or_parallel_scan(self):
        for jobs in (1, 2):
            for risk in (False, True):
                with self.subTest(jobs=jobs, risk=risk), tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    meshes = root / "meshes"
                    meshes.mkdir()
                    create_database(root / "skyrim_world.db")
                    points = [(0,0,0), (1,0,0), (0,1,0)]
                    document, binary = fixture(points)
                    document["asset"] = []
                    write_glb(meshes / "a-invalid-asset.glb", document, binary)
                    document, binary = fixture(points)
                    document["materials"] = None
                    write_glb(meshes / "b-invalid-materials.glb", document, binary)
                    document, binary = fixture(points)
                    document["nodes"] = ["x"]
                    write_glb(meshes / "c-invalid-node.glb", document, binary)
                    for name, metadata in (("d-extras", {"extras": []}),
                                           ("e-tag", {"extras": {"openSkyrim": "x"}})):
                        document, binary = fixture(points, materials=[metadata])
                        write_glb(meshes / (name + ".glb"), document, binary)
                    document, binary = fixture(points, materials=[material(
                        first=AUDIT.DEPTH_TEST | (AUDIT.DECAL if risk else 0))])
                    write_glb(meshes / "z-valid.glb", document, binary)
                    report = root / "report.json"
                    process = subprocess.run([sys.executable, str(SPEC.origin), str(root),
                        "--jobs", str(jobs), "--out", str(report)], capture_output=True, text=True)
                    self.assertEqual(process.returncode, 2, process.stderr)
                    self.assertNotIn("Traceback", process.stderr)
                    result = json.loads(report.read_text())
                    self.assertEqual(result["coverage"]["selected_files"], 6)
                    self.assertEqual(result["coverage"]["geometry_complete_files"], 3)
                    self.assertEqual(result["material_requirements"]["unannotated"], 2)
                    self.assertEqual({entry["path"] for entry in result["errors"]},
                        {"a-invalid-asset.glb", "b-invalid-materials.glb", "c-invalid-node.glb"})
                    self.assertEqual(result["risks_found"], risk)
                    self.assertFalse(result["static_scope_passed"])
                    self.assertTrue(result["coverage_gaps"])
                    if risk:
                        self.assertIn("z-valid.glb", [entry["path"] for entry in result["files"]])

    def test_depth_requirements_and_blend_exception(self):
        self.assertEqual(AUDIT.material_risks(material()), [])
        self.assertEqual(AUDIT.material_risks(material(AUDIT.DEPTH_TEST | AUDIT.DECAL | AUDIT.DYNAMIC_DECAL, 0)),
            ["authored_decal", "authored_dynamic_decal", "opaque_or_mask_depth_write_disabled"])
        self.assertEqual(AUDIT.material_risks(material(second=0, alphaMode="BLEND")), [])
        self.assertEqual(AUDIT.material_risks(material(first=0)), ["depth_test_disabled"])
        self.assertIsNone(AUDIT.material_risks({}))

    def test_strided_positions_and_uint16_indices(self):
        binary = b"".join(struct.pack("<4f", *p, 99) for p in [(0,0,0), (1,0,0), (0,1,0)]) + struct.pack("<3H", 2,1,0)
        document, _ = fixture([(0,0,0)]*3)
        document["buffers"][0]["byteLength"] = len(binary)
        document["bufferViews"] = [{"buffer": 0, "byteLength": 48, "byteStride": 16}, {"buffer": 0, "byteOffset": 48, "byteLength": 6}]
        document["accessors"].append({"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"})
        document["meshes"][0]["primitives"][0]["indices"] = 1
        geometry, _ = AUDIT.triangles(document, binary, 100)
        self.assertEqual(geometry[0].points, ((0,1,0), (1,0,0), (0,0,0)))

    def test_accessor_boundaries(self):
        document, binary = fixture([(0,0,0)]*3)
        document["bufferViews"][0]["byteLength"] -= 1
        with self.assertRaises(AUDIT.AuditError):
            AUDIT.triangles(document, binary, 100)

    def test_negative_indices_and_singular_transforms_are_rejected(self):
        for mutation in (lambda d: d.update(scene=-1),
                         lambda d: d["nodes"][0].update(mesh=-1),
                         lambda d: d["nodes"][0].update(scale=[0,1,1]),
                         lambda d: d["meshes"][0]["primitives"][0].update(material=-1)):
            document, binary = fixture([(0,0,0)]*3)
            mutation(document)
            with self.assertRaises(AUDIT.AuditError):
                AUDIT.triangles(document, binary, 100)

    def test_animated_skin_and_sparse_are_coverage_gaps(self):
        for mutation in (lambda d: d.update(animations=[{}]),
                         lambda d: d["nodes"][0].update(skin=0),
                         lambda d: d["accessors"][0].update(sparse={})):
            document, binary = fixture([(0,0,0)]*3)
            mutation(document)
            with self.assertRaises(AUDIT.AuditError):
                AUDIT.triangles(document, binary, 100)

    def test_cycle_and_budget(self):
        document, binary = fixture([(0,0,0), (1,0,0), (0,1,0)])
        document["nodes"][0]["children"] = [0]
        with self.assertRaises(AUDIT.AuditError):
            AUDIT.triangles(document, binary, 100)
        document["nodes"][0].pop("children")
        with self.assertRaises(AUDIT.AuditError):
            AUDIT.triangles(document, binary, 0)

    def test_cli_pass_and_incomplete_scan_exit_codes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "meshes").mkdir()
            document, binary = fixture([(0,0,0), (1,0,0), (0,1,0)])
            write_glb(root / "meshes/test.glb", document, binary)
            create_database(root / "skyrim_world.db")
            report = root / "report.json"
            self.assertEqual(AUDIT.main([str(root), "--out", str(report)]), 0)
            self.assertTrue(json.loads(report.read_text())["static_scope_passed"])
            self.assertEqual(AUDIT.main([str(root), "--materials-only", "--out", str(report)]), 2)
            self.assertFalse(json.loads(report.read_text())["static_scope_passed"])
            document["materials"][0] = material(first=AUDIT.DEPTH_TEST | AUDIT.DECAL)
            write_glb(root / "meshes/test.glb", document, binary)
            self.assertEqual(AUDIT.main([str(root), "--out", str(report)]), 1)
            document["nodes"][0]["skin"] = 0
            write_glb(root / "meshes/test.glb", document, binary)
            self.assertEqual(AUDIT.main([str(root), "--out", str(report)]), 2)

    def test_glb_total_length_is_validated(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "test.glb"
            document, binary = fixture([(0,0,0)]*3)
            write_glb(path, document, binary)
            path.write_bytes(path.read_bytes()+b"bad")
            with self.assertRaises(AUDIT.AuditError):
                AUDIT.load_glb(path)

    def test_placement_scope_disable_flags_and_parent_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "world.db"
            create_database(path)
            with sqlite3.connect(path) as db:
                for ident, cell, exterior, flags, parent in [(1,10,1,0,None), (2,11,1,0,None),
                        (3,12,1,0x800,None), (4,12,1,0,1), (5,10,0,0,None), (6,11,0,0,None)]:
                    db.execute('INSERT INTO "references" VALUES (?,?,60,?,0,0,0,0,0,0,1,?,?,1)',
                               (ident,cell,exterior,flags,parent))
            result = AUDIT.duplicate_placements(path, 10)
            self.assertEqual(result["duplicate_groups"], 1)
            self.assertEqual(result["examples"][0]["reference_ids"], [1,2])
            self.assertEqual(result["coverage"]["conditional_enable_state_unknown"], 1)


class PlacementTests(unittest.TestCase):
    null_fields = ("header_flags", "pos_x", "pos_y", "pos_z",
                   "rot_x", "rot_y", "rot_z", "scale")
    missing_scope_or_id = ("is_exterior=NULL", "worldspace_id=NULL",
                           "is_exterior=0,cell_id=NULL", "id=NULL")
    null_gap = "NULL placement flags, transforms, reference ID or selected scope"

    def prepare_assets(self, root):
        (root / "meshes").mkdir()
        document, binary = fixture([(0,0,0), (1,0,0), (0,1,0)])
        write_glb(root / "meshes/test.glb", document, binary)
        create_database(root / "skyrim_world.db")

    def run_cli(self, root):
        report = root / "report.json"
        process = subprocess.run([sys.executable, str(SPEC.origin), str(root),
            "--out", str(report)], capture_output=True, text=True)
        self.assertNotIn("Traceback", process.stderr)
        return process.returncode, json.loads(report.read_text())

    def test_each_null_field_is_counted_and_excluded_from_duplicates(self):
        for field in self.null_fields:
            with self.subTest(field=field), tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "world.db"
                create_database(path)
                with sqlite3.connect(path) as db:
                    for ident in (1, 2, 3, 4):
                        insert_reference(db, ident)
                    db.execute(f'UPDATE "references" SET {field}=NULL WHERE id IN (3,4)')
                result = AUDIT.duplicate_placements(path, 10)
                self.assertEqual(result["coverage"]["model_references"], 4)
                self.assertEqual(result["coverage"]["null_placement_fields"], 2)
                self.assertEqual(result["duplicate_groups"], 1)
                self.assertEqual(result["extra_placements"], 1)
                self.assertEqual(result["examples"][0]["reference_ids"], [1, 2])

    def test_multiple_null_fields_count_one_reference(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "world.db"
            create_database(path)
            with sqlite3.connect(path) as db:
                insert_reference(db, 1, transform=(None,) * 7)
                insert_reference(db, 2, flags=None, transform=(None,) * 7)
            result = AUDIT.duplicate_placements(path, 10)
            self.assertEqual(result["coverage"]["null_placement_fields"], 2)
            self.assertEqual(result["duplicate_groups"], 0)

    def test_exclusions_precede_null_transform_checks(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "world.db"
            create_database(path)
            with sqlite3.connect(path) as db:
                for ident, flags, parent in ((1, 0x20, None), (2, 0x800, None),
                        (3, 0x20, 42), (4, 0x800, 42), (5, 0, 42), (6, None, 42)):
                    insert_reference(db, ident, flags, parent, (None,) * 7)
                insert_reference(db, 7)
                insert_reference(db, 8)
            result = AUDIT.duplicate_placements(path, 10)
            self.assertEqual(result["coverage"]["model_references"], 8)
            self.assertEqual(result["coverage"]["deleted_or_initially_disabled"], 4)
            self.assertEqual(result["coverage"]["conditional_enable_state_unknown"], 1)
            self.assertEqual(result["coverage"]["null_placement_fields"], 1)
            self.assertEqual(result["examples"][0]["reference_ids"], [7, 8])

    def test_cli_null_fields_report_gap_and_exit_two_even_with_risks(self):
        for field in self.null_fields:
            with self.subTest(field=field), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.prepare_assets(root)
                database = root / "skyrim_world.db"
                with sqlite3.connect(database) as db:
                    for ident in (1, 2, 3):
                        insert_reference(db, ident)
                    db.execute(f'UPDATE "references" SET {field}=NULL WHERE id=3')
                original_database = database.read_bytes()
                status, result = self.run_cli(root)
                self.assertEqual(status, 2)
                self.assertEqual(database.read_bytes(), original_database)
                self.assertEqual(result["placements"]["coverage"]["null_placement_fields"], 1)
                self.assertEqual(result["placements"]["duplicate_groups"], 1)
                self.assertEqual(result["errors"], [])
                self.assertIn(self.null_gap, result["coverage_gaps"])
                self.assertTrue(result["risks_found"])
                self.assertFalse(result["static_scope_passed"])

    def test_cli_excluded_null_transforms_preserve_existing_gate_priority(self):
        for conditional in (False, True):
            with self.subTest(conditional=conditional), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.prepare_assets(root)
                with sqlite3.connect(root / "skyrim_world.db") as db:
                    insert_reference(db, 1, 0x20, transform=(None,) * 7)
                    insert_reference(db, 2, 0x800, transform=(None,) * 7)
                    if conditional:
                        insert_reference(db, 3, parent=42, transform=(None,) * 7)
                status, result = self.run_cli(root)
                self.assertEqual(status, 2 if conditional else 0)
                self.assertEqual(result["placements"]["coverage"].get("null_placement_fields", 0), 0)
                self.assertNotIn(self.null_gap, result["coverage_gaps"])
                self.assertEqual(result["coverage_gaps"],
                    ["conditional placement enable states not evaluated"] if conditional else [])
                self.assertEqual(result["static_scope_passed"], not conditional)

    def test_cli_invalid_transform_type_reports_placement_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.prepare_assets(root)
            with sqlite3.connect(root / "skyrim_world.db") as db:
                insert_reference(db, 1, transform=("invalid", 0, 0, 0, 0, 0, 1))
            status, result = self.run_cli(root)
            self.assertEqual(status, 2)
            self.assertIsNone(result["placements"])
            self.assertEqual(result["errors"][0]["path"], "skyrim_world.db")
            self.assertIn("placement audit failed", result["coverage_gaps"])
            self.assertFalse(result["static_scope_passed"])

    def test_missing_scope_or_id_is_excluded_from_duplicate_grouping(self):
        for mutation in self.missing_scope_or_id:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "world.db"
                create_database(path)
                with sqlite3.connect(path) as db:
                    for ident in (1, 2, 3, 4):
                        insert_reference(db, ident)
                    db.execute(f'UPDATE "references" SET {mutation} WHERE id IN (3,4)')
                result = AUDIT.duplicate_placements(path, 10)
                self.assertEqual(result["coverage"]["model_references"], 4)
                self.assertEqual(result["coverage"]["null_placement_fields"], 2)
                self.assertEqual(result["duplicate_groups"], 1)
                self.assertEqual(result["extra_placements"], 1)
                self.assertEqual(result["examples"][0]["reference_ids"], [1, 2])

    def test_multiple_null_scope_flags_and_transforms_count_each_row_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "world.db"
            create_database(path)
            with sqlite3.connect(path) as db:
                for ident in (1, 2, 3, 4):
                    insert_reference(db, ident)
                db.execute('UPDATE "references" SET is_exterior=NULL,cell_id=NULL,worldspace_id=NULL,pos_x=NULL')
                db.execute('UPDATE "references" SET id=NULL WHERE id=3')
                db.execute('UPDATE "references" SET header_flags=NULL WHERE id=4')
            result = AUDIT.duplicate_placements(path, 10)
            self.assertEqual(result["coverage"]["model_references"], 4)
            self.assertEqual(result["coverage"]["null_placement_fields"], 4)
            self.assertEqual(result["duplicate_groups"], 0)

    def test_valid_unused_null_scope_fields_keep_distinct_scope_groups(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "world.db"
            create_database(path)
            with sqlite3.connect(path) as db:
                for ident in (1, 2, 3, 4):
                    insert_reference(db, ident)
                db.execute('UPDATE "references" SET cell_id=NULL WHERE id IN (1,2)')
                db.execute('UPDATE "references" SET is_exterior=0,worldspace_id=NULL WHERE id IN (3,4)')
            result = AUDIT.duplicate_placements(path, 10)
            self.assertEqual(result["coverage"].get("null_placement_fields", 0), 0)
            self.assertEqual(result["duplicate_groups"], 2)
            self.assertEqual(result["extra_placements"], 2)
            self.assertEqual({tuple(row["scope"]): row["reference_ids"] for row in result["examples"]},
                             {("worldspace", 60): [1, 2], ("interior", 10): [3, 4]})

    def test_cli_missing_scope_or_id_reports_gap_instead_of_a_false_pass(self):
        for mutation in self.missing_scope_or_id:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.prepare_assets(root)
                database = root / "skyrim_world.db"
                with sqlite3.connect(database) as db:
                    insert_reference(db, 1)
                    db.execute(f'UPDATE "references" SET {mutation}')
                original_database = database.read_bytes()
                status, result = self.run_cli(root)
                self.assertEqual(status, 2)
                self.assertEqual(database.read_bytes(), original_database)
                self.assertEqual(result["placements"]["coverage"]["null_placement_fields"], 1)
                self.assertEqual(result["placements"]["duplicate_groups"], 0)
                self.assertEqual(result["errors"], [])
                self.assertEqual(result["coverage_gaps"], [self.null_gap])
                self.assertFalse(result["risks_found"])
                self.assertFalse(result["static_scope_passed"])

    def test_cli_valid_unused_null_scope_fields_pass_without_changing_database(self):
        for mutation in ("cell_id=NULL", "is_exterior=0,worldspace_id=NULL"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.prepare_assets(root)
                database = root / "skyrim_world.db"
                with sqlite3.connect(database) as db:
                    insert_reference(db, 1)
                    db.execute(f'UPDATE "references" SET {mutation}')
                original_database = database.read_bytes()
                status, result = self.run_cli(root)
                self.assertEqual(status, 0)
                self.assertEqual(database.read_bytes(), original_database)
                self.assertEqual(result["placements"]["coverage"].get("null_placement_fields", 0), 0)
                self.assertEqual(result["coverage_gaps"], [])
                self.assertTrue(result["static_scope_passed"])

    def test_cli_excluded_missing_scopes_preserve_gate_priority(self):
        for conditional in (False, True):
            with self.subTest(conditional=conditional), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.prepare_assets(root)
                database = root / "skyrim_world.db"
                with sqlite3.connect(database) as db:
                    insert_reference(db, 1, 0x20)
                    insert_reference(db, 2, 0x800)
                    if conditional:
                        insert_reference(db, 3, parent=42)
                    db.execute('UPDATE "references" SET id=NULL,is_exterior=NULL,cell_id=NULL,worldspace_id=NULL')
                original_database = database.read_bytes()
                status, result = self.run_cli(root)
                self.assertEqual(status, 2 if conditional else 0)
                self.assertEqual(database.read_bytes(), original_database)
                coverage = result["placements"]["coverage"]
                self.assertEqual(coverage["deleted_or_initially_disabled"], 2)
                self.assertEqual(coverage.get("conditional_enable_state_unknown", 0), int(conditional))
                self.assertEqual(coverage.get("null_placement_fields", 0), 0)
                self.assertEqual(result["placements"]["duplicate_groups"], 0)
                self.assertEqual(result["coverage_gaps"],
                    ["conditional placement enable states not evaluated"] if conditional else [])
                self.assertEqual(result["static_scope_passed"], not conditional)

    def test_unexpected_audit_failures_report_gap_and_exit_two(self):
        for error in (TypeError("invalid placement"), RuntimeError("worker failed"),
                      sqlite3.OperationalError("database unavailable")):
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                report = root / "report.json"
                with mock.patch.object(AUDIT, "audit", side_effect=error):
                    self.assertEqual(AUDIT.main([str(root), "--out", str(report)]), 2)
                result = json.loads(report.read_text())
                self.assertEqual(result["coverage_gaps"],
                    [f"audit failed ({type(error).__name__}): {error}"])
                self.assertFalse(result["static_scope_passed"])
                self.assertFalse(result["rendered_z_fighting_verified"])

    def test_interrupt_and_system_exit_are_not_converted_to_audit_results(self):
        for error in (KeyboardInterrupt(), SystemExit(23)):
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                report = root / "report.json"
                with mock.patch.object(AUDIT, "audit", side_effect=error):
                    with self.assertRaises(type(error)) as caught:
                        AUDIT.main([str(root), "--out", str(report)])
                self.assertIs(caught.exception, error)
                self.assertFalse(report.exists())


if __name__ == "__main__":
    unittest.main()
