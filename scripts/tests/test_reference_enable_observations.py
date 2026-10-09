import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "reference-enable-observations.py"
SPEC = importlib.util.spec_from_file_location("reference_enable_observations", SCRIPT)
TOOL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOOL)


class ManifestSafetyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.database = self.root / "source.db"
        with sqlite3.connect(self.database) as database:
            database.executescript('''
                CREATE TABLE schema_info(version INTEGER);
                INSERT INTO schema_info VALUES(7);
                CREATE TABLE plugins(name TEXT, priority INTEGER, checksum BLOB);
                INSERT INTO plugins VALUES('Synthetic.esm', 0, X'1234');
                CREATE TABLE cells(id INTEGER, interior_name TEXT, grid_x INTEGER, grid_y INTEGER);
                INSERT INTO cells VALUES(10, 'SyntheticInterior', NULL, NULL);
                CREATE TABLE statics(id INTEGER, model_path TEXT);
                INSERT INTO statics VALUES(200, 'meshes/generated.glb');
                CREATE TABLE formid_map(form_id INTEGER, plugin_name TEXT, internal_id INTEGER);
                INSERT INTO formid_map VALUES(100, 'Synthetic.esm', 100);
                CREATE TABLE records(form_id INTEGER, record_type TEXT);
                INSERT INTO records VALUES(200, 'STAT');
                CREATE TABLE "references"(
                    id INTEGER, cell_id INTEGER, base_form_id INTEGER, header_flags INTEGER,
                    enable_parent_id INTEGER, enable_parent_flags INTEGER, is_exterior INTEGER,
                    worldspace_id INTEGER, pos_x REAL, pos_y REAL, pos_z REAL);
                INSERT INTO "references" VALUES(100, 10, 200, 0, 0, 0, 0, NULL, 0, 0, 0);
            ''')
        self.original_database = self.database.read_bytes()

    def run_tool(self, *arguments):
        return subprocess.run(
            [sys.executable, str(SCRIPT), self.database.name, *map(str, arguments)],
            cwd=self.root, capture_output=True, text=True, timeout=10,
        )

    def assert_preserved(self):
        self.assertEqual(self.database.read_bytes(), self.original_database)
        self.assertFalse(list(self.root.glob(".*.tmp")))

    def rejected_before_query(self, destination_flag, destination):
        error = io.StringIO()
        with mock.patch.object(TOOL.sqlite3, "connect") as connect:
            with contextlib.redirect_stderr(error), self.assertRaises(SystemExit) as raised:
                TOOL.main([str(self.database), destination_flag, str(destination)])
            connect.assert_not_called()
        self.assertEqual(raised.exception.code, 1)
        self.assert_preserved()
        return error.getvalue()

    def test_direct_and_relative_database_outputs_are_rejected(self):
        (self.root / "nested").mkdir()
        for output in [self.database, Path("nested/../source.db")]:
            with self.subTest(output=str(output)):
                run = self.run_tool("--output", output)
                self.assertEqual(run.returncode, 1, run.stderr)
                self.assertIn("must differ from the database", run.stderr)
                self.assert_preserved()

    def test_symlink_and_hardlink_database_outputs_are_rejected_before_query(self):
        symlink = self.root / "symlink.db"
        symlink.symlink_to(self.database)
        hardlink = self.root / "hardlink.db"
        os.link(self.database, hardlink)
        for output in [symlink, hardlink]:
            with self.subTest(output=output.name):
                self.assertIn("must differ from the database",
                              self.rejected_before_query("--output", output))
                self.assertEqual(output.read_bytes(), self.original_database)

    def test_verification_cannot_name_the_database_or_a_hardlink(self):
        hardlink = self.root / "verify.db"
        os.link(self.database, hardlink)
        for manifest in [self.database, hardlink]:
            with self.subTest(manifest=manifest.name):
                self.assertIn("must differ from the database",
                              self.rejected_before_query("--verify", manifest))

    def test_existing_manifest_is_not_overwritten(self):
        output = self.root / "manifest.json"
        output.write_bytes(b"existing observation evidence\n")
        self.assertIn("already exists", self.rejected_before_query("--output", output))
        self.assertEqual(output.read_bytes(), b"existing observation evidence\n")

    def test_dangling_output_symlink_is_not_replaced(self):
        output = self.root / "manifest.json"
        target = self.root / "absent.json"
        output.symlink_to(target)
        self.assertIn("already exists", self.rejected_before_query("--output", output))
        self.assertTrue(output.is_symlink())
        self.assertFalse(target.exists())

    def test_unknown_cli_option_is_rejected_before_query(self):
        with mock.patch.object(TOOL.sqlite3, "connect") as connect:
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                TOOL.main([str(self.database), "--output", str(self.root / "manifest.json"),
                           "--unknown-engine-option"])
            connect.assert_not_called()
        self.assertEqual(raised.exception.code, 2)
        self.assert_preserved()

    def test_new_manifest_is_complete_and_verification_preserves_both_inputs(self):
        output = self.root / "manifest.json"
        generated = self.run_tool("--output", output)
        self.assertEqual(generated.returncode, 0, generated.stderr)
        original_manifest = output.read_bytes()
        document = json.loads(original_manifest)
        self.assertEqual(document["status"], "retail_gate_pending")
        self.assertEqual(len(document["cases"]), len(TOOL.CASES) * 2)
        self.assertTrue(any(case["sample_status"] == "selected" for case in document["cases"]))
        verified = self.run_tool("--verify", output)
        self.assertEqual(verified.returncode, 0, verified.stderr)
        self.assertEqual(output.read_bytes(), original_manifest)
        self.assert_preserved()

    def test_changed_verification_manifest_is_rejected_without_modifying_it(self):
        output = self.root / "manifest.json"
        self.assertEqual(self.run_tool("--output", output).returncode, 0)
        document = json.loads(output.read_text())
        document["status"] = "unsupported_acceptance_claim"
        output.write_text(json.dumps(document), encoding="utf-8")
        altered = output.read_bytes()
        verified = self.run_tool("--verify", output)
        self.assertEqual(verified.returncode, 1, verified.stderr)
        self.assertIn("manifest differs", verified.stderr)
        self.assertEqual(output.read_bytes(), altered)
        self.assert_preserved()

    def test_destination_created_during_publication_is_preserved(self):
        for database_alias in [False, True]:
            with self.subTest(database_alias=database_alias):
                output = self.root / f"race-{database_alias}.json"
                link = os.link

                def raced_link(temporary, destination):
                    if database_alias:
                        link(self.database, destination)
                    else:
                        Path(destination).write_bytes(b"concurrent observation evidence")
                    return link(temporary, destination)

                with mock.patch.object(TOOL.os, "link", side_effect=raced_link):
                    with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                        TOOL.main([str(self.database), "--output", str(output)])
                self.assertEqual(raised.exception.code, 1)
                self.assertEqual(output.read_bytes(), self.original_database if database_alias
                                 else b"concurrent observation evidence")
                self.assert_preserved()

    def test_flush_failure_cleans_temporary_file_and_keeps_inputs(self):
        output = self.root / "manifest.json"
        with mock.patch.object(TOOL.os, "fsync", side_effect=OSError("injected flush failure")):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                TOOL.main([str(self.database), "--output", str(output)])
        self.assertEqual(raised.exception.code, 1)
        self.assertFalse(output.exists())
        self.assert_preserved()

    def test_interrupted_publication_cleans_temporary_file_and_keeps_inputs(self):
        output = self.root / "manifest.json"
        with mock.patch.object(TOOL.os, "link", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                TOOL.main([str(self.database), "--output", str(output)])
        self.assertFalse(output.exists())
        self.assert_preserved()

    def test_sqlite_deadline_stops_the_query_without_publishing(self):
        output = self.root / "manifest.json"

        def costly_query(database, path):
            return database.execute('''WITH RECURSIVE work(n) AS (
                VALUES(0) UNION ALL SELECT n+1 FROM work WHERE n<1000000
                ) SELECT sum(n) FROM work''').fetchone()

        with mock.patch.object(TOOL, "build", side_effect=costly_query):
            with mock.patch.object(TOOL.time, "monotonic", side_effect=[0, 121]):
                error = io.StringIO()
                with contextlib.redirect_stderr(error), self.assertRaises(SystemExit) as raised:
                    TOOL.main([str(self.database), "--output", str(output)])
        self.assertEqual(raised.exception.code, 1)
        self.assertIn("interrupted", error.getvalue())
        self.assertFalse(output.exists())
        self.assert_preserved()


if __name__ == "__main__":
    unittest.main()
