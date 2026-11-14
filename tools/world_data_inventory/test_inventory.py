"""Disposable SQLite controls; these do not certify engine or retail semantics."""
import copy
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

import inventory as inv


def schema_from(db):
    return {"objects": [dict(zip(("type", "name", "table", "sql"), row)) for row in db.execute(
        "SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")],
        "shadow_tables": [row[0] for row in db.execute("SELECT name FROM pragma_table_list WHERE type='shadow'")],
        "population": "not_observed", "schema_stamp": 1}


def fixture():
    with closing(sqlite3.connect(":memory:", cached_statements=0)) as db:
        db.executescript('''CREATE TABLE lights(id INTEGER PRIMARY KEY,radius REAL,falloff REAL);
            CREATE TABLE placements(id INTEGER PRIMARY KEY,light_id INTEGER,enabled INTEGER);
            CREATE TABLE future_gameplay(id INTEGER PRIMARY KEY,opaque BLOB);
            CREATE VIRTUAL TABLE spatial USING rtree(id,min_x,max_x);''')
        return schema_from(db)


def manual():
    return {"reviewed_at_commit": "disposable-fixture", "fields": {}, "stores": {}, "legacy_shapes": [],
            "limitations": ["Synthetic fixture only."],
            "tables": {name: {"classification": "deferred", "owner": "fixture", "reason": "Future feature allowed."}
                       for name in ("lights", "placements", "future_gameplay", "spatial")},
            "blob_default": {"classification": "opaque", "owner": "fixture", "reason": "No interpretation proof."}}


def fixture_report():
    """Reproducible readable example with no attribution to the production schema."""
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        source = root / "crates/engine/src/disposable_fixture.rs"
        source.parent.mkdir(parents=True)
        source.write_text('''fn queries(db: &Connection, dynamic: &str) {
    db.prepare("SELECT l.radius AS size FROM placements p JOIN lights l ON l.id=p.light_id WHERE p.enabled=?1");
    db.prepare("SELECT COUNT(*) FROM lights");
    db.prepare(dynamic);
}''')
        extraction = {"format_version": 1, "source_commit": "disposable-fixture",
                      "schemas": {"disposable-fixture": fixture()}, "variants": []}
        result = inv.build(root, manual(), extraction)
        # The golden report illustrates schema/dependency behavior across host SQLite versions.
        # Real extraction artifacts keep the actual version in their identity.
        result.pop("identity")
        return inv.report(result)


class Dependencies(unittest.TestCase):
    def test_join_alias_filter_and_result_dependencies_on_empty_schema(self):
        with closing(inv.open_schema(fixture())) as db:
            result = inv.prepare_dependencies(db, 'SELECT l.radius AS size FROM placements p JOIN lights l ON l.id=p.light_id WHERE p.enabled=?1')
            dependencies = {(d["table"], d["column"]) for d in result["dependencies"]}
            self.assertEqual(result["status"], "prepared")
            self.assertEqual(dependencies, {("lights", "radius"), ("lights", "id"), ("placements", "light_id"), ("placements", "enabled")})
            self.assertEqual(db.execute("SELECT count(*) FROM placements").fetchone()[0], 0)
            self.assertEqual(result["executed_rows"], "unknown")
            self.assertNotIn(("lights", "falloff"), dependencies)

    def test_count_only_empty_column_is_table_dependency(self):
        with closing(inv.open_schema(fixture())) as db:
            result = inv.prepare_dependencies(db, "SELECT COUNT(*) FROM lights")
            self.assertEqual(result["dependencies"], [{"database": "main", "table": "lights", "column": "", "scope": "table"}])

    def test_cached_execution_does_not_create_a_new_prepare_event(self):
        with closing(sqlite3.connect(":memory:")) as db:
            db.execute("CREATE TABLE fixture(value INTEGER)")
            reads = []
            db.set_authorizer(lambda action, table, column, *_: (reads.append((table, column)) if action == sqlite3.SQLITE_READ else None) or sqlite3.SQLITE_OK)
            self.assertEqual(db.execute("SELECT value FROM fixture").fetchall(), [])
            first = list(reads)
            self.assertTrue(first)
            self.assertEqual(db.execute("SELECT value FROM fixture").fetchall(), [])
            self.assertEqual(reads, first)
            db.set_authorizer(None)

    def test_extraction_repeats_prepare_without_inferring_execution(self):
        with closing(inv.open_schema(fixture())) as db:
            a = inv.prepare_dependencies(db, "SELECT radius FROM lights WHERE id=?1")
            b = inv.prepare_dependencies(db, "SELECT radius FROM lights WHERE id=?1")
            self.assertEqual(a, b)
            self.assertEqual(a["mapped_rows"], "unknown")

    def test_optional_table_branch_and_unknown_query(self):
        with closing(inv.open_schema(fixture(), ["DROP TABLE lights"])) as db:
            old = inv.prepare_dependencies(db, "SELECT NULL FROM placements WHERE id=?1")
            unsupported = inv.prepare_dependencies(db, "SELECT radius FROM lights")
            self.assertEqual(old["status"], "prepared")
            self.assertNotIn("lights", {d["table"] for d in old["dependencies"]})
            self.assertEqual(unsupported["status"], "unknown")
            self.assertEqual(unsupported["dependencies"], [])

    def test_explain_does_not_execute_writes(self):
        with closing(inv.open_schema(fixture())) as db:
            self.assertEqual(inv.prepare_dependencies(db, "INSERT INTO lights VALUES(1,5,2)")["status"], "prepared")
            self.assertEqual(db.execute("SELECT count(*) FROM lights").fetchone()[0], 0)

    def test_rtree_shadows_are_not_replayed_as_authored_tables(self):
        schema = fixture()
        self.assertTrue(schema["shadow_tables"])
        with closing(inv.open_schema(schema)) as db:
            result = inv.prepare_dependencies(db, "SELECT id FROM spatial WHERE min_x>=?1")
            self.assertEqual(result["status"], "prepared")
            self.assertIn("spatial", {d["table"] for d in result["dependencies"]})


class SourceAndDrift(unittest.TestCase):
    def test_literal_constants_comments_tests_and_dynamic_macro(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "crates/engine/src/world.rs"
            path.parent.mkdir(parents=True)
            path.write_text('''const SQL: &str = r##"SELECT radius FROM lights"##;
                fn live(db: &Connection) {
                    // db.prepare("SELECT phantom FROM nowhere");
                    db.prepare(SQL); db.prepare("SELECT id,\\\n                        radius FROM lights");
                    db.prepare(&format!("SELECT {field} FROM lights"));
                    db.prepare(sql!("SELECT falloff FROM lights"));
                }
                #[cfg(test)] fn helper() { db.prepare("SELECT test_only FROM tests"); }
                #[cfg(test)] mod tests { fn fixture() { db.prepare("SELECT also_test FROM tests"); } }
                fn after_tests() { db.query_row("SELECT count(*) FROM lights", [], f); }
            ''')
            queries = inv.scan_queries(root)
            self.assertEqual(len(queries), 5)
            self.assertEqual([q["status"] for q in queries], ["literal", "literal", "unknown", "unknown", "literal"])
            self.assertEqual(queries[0]["sql"], "SELECT radius FROM lights")
            self.assertEqual(queries[1]["sql"], "SELECT id,radius FROM lights")
            self.assertFalse(any("phantom" in q["expression"] or "test_only" in q["expression"] for q in queries))

    def test_broken_anchor_and_invalid_state_fail(self):
        data = manual()
        data["fields"]["lights.radius"] = {"owner": "fixture", "reason": "trace", "roles": ["result"],
            "states": {state: "unknown" for state in inv.STATES},
            "evidence": [{"path": "missing.rs", "anchor": "source"}]}
        with self.assertRaisesRegex(ValueError, "broken evidence"):
            inv.validate_manual(inv.ROOT, data)
        data["fields"]["lights.radius"]["evidence"] = []
        data["fields"]["lights.radius"]["states"]["mapped"] = "source_sink"
        with self.assertRaisesRegex(ValueError, "invalid state"):
            inv.validate_manual(inv.ROOT, data)

    def test_schema_and_column_changes_trigger_source_guard(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/converter/src/esm/exporter.rs"
            source.parent.mkdir(parents=True)
            source.write_text('db.execute_batch("CREATE TABLE future(id INTEGER)");')
            baseline = inv.source_guard(root, [], manual())
            source.write_text('db.execute_batch("CREATE TABLE future(id INTEGER, new_field REAL)");')
            self.assertNotEqual(baseline, inv.source_guard(root, [], manual()))
            source.with_name("new_projection.rs").write_text("// new schema producer")
            self.assertIn("crates/converter/src/esm/new_projection.rs", inv.source_guard(root, [], manual())["projection_sources"])

    def test_positional_mapping_requires_same_dependency_and_expression(self):
        with closing(inv.open_schema(fixture())) as db:
            sql = "SELECT l.id,l.radius FROM lights l WHERE l.id=?1"
            prepared = inv.prepare_dependencies(db, sql)
            result = {"fields": {"lights.radius": {"mapping": {"query": "SELECT l.id", "ordinal": 1, "expression": "l.radius"}}},
                      "queries": [{"sql": sql, "preparations": {"inhouse": prepared}}], "variants": []}
            inv.validate_mappings(result)
            result["fields"]["lights.radius"]["mapping"]["ordinal"] = 0
            with self.assertRaisesRegex(ValueError, "mapping has no prepared dependency"):
                inv.validate_mappings(result)

    def test_emitted_report_allows_future_tables_and_preserves_unknown(self):
        data = manual()
        extraction = {"format_version": 1, "source_commit": "fixture", "schemas": {"fixture": fixture()}, "variants": []}
        result = inv.build(inv.ROOT, data, extraction)
        second = inv.build(inv.ROOT, data, copy.deepcopy(extraction))
        self.assertEqual(inv.canonical(result), inv.canonical(second))
        self.assertEqual(result["schema_status"], "emitted")
        future = next(t for t in result["schemas"]["fixture"] if t["name"] == "future_gameplay")
        self.assertEqual(future["population"], "not_observed")
        self.assertEqual(future["columns"][1]["disposition"]["classification"], "opaque")
        self.assertIn("future_gameplay.opaque", inv.report(result))
        data["tables"].pop("future_gameplay")
        with self.assertRaisesRegex(ValueError, "unclassified emitted table"):
            inv.build(inv.ROOT, data, extraction)

    def test_fixture_report_matches_disposable_schema(self):
        self.assertEqual((inv.ROOT / "docs/data-inventory/fixture-report.md").read_text(), fixture_report())

    def test_dependency_evidence_does_not_transfer_between_schema_shapes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/engine/src/query.rs"
            source.parent.mkdir(parents=True)
            source.write_text('fn query(db: &Connection) { db.prepare("SELECT radius FROM lights"); }')
            old = fixture()
            with closing(inv.open_schema(old, ["ALTER TABLE lights DROP COLUMN radius"])) as db:
                changed = schema_from(db)
            result = inv.build(root, manual(), {"format_version": 1, "variants": [], "schemas": {"old": old, "changed": changed}})
            self.assertEqual(result["queries"][0]["preparations"]["old"]["status"], "prepared")
            self.assertEqual(result["queries"][0]["preparations"]["changed"]["status"], "unknown")
            changed_lights = next(t for t in result["schemas"]["changed"] if t["name"] == "lights")
            self.assertFalse(any(c["prepare_dependencies"] for c in changed_lights["columns"]))

    def test_committed_manual_anchors(self):
        inv.validate_manual(inv.ROOT, json.loads((inv.HERE / "dispositions.json").read_text()))

    def test_cli_rejects_stale_extraction_before_replay(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "stale.json"
            path.write_text(json.dumps({"format_version": 1, "reviewed_source_guard_sha256": "old-source"}))
            result = subprocess.run([sys.executable, "-B", str(inv.HERE / "inventory.py"),
                                     "--extraction", str(path), "--output", str(Path(directory) / "report")],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("extraction does not match the reviewed source fingerprint", result.stderr)
            self.assertFalse((Path(directory) / "report").exists())


if __name__ == "__main__":
    unittest.main()
