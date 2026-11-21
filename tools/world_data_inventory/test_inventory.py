"""Disposable SQLite controls; these do not certify engine or retail semantics."""
import contextlib
import copy
import io
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
    return {"fields": {}, "stores": {}, "legacy_shapes": [],
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


READER = 'fn query(db: &Connection) { db.prepare("SELECT radius FROM lights"); }\n'


def store_document(name, anchor="fn query", commit="0" * 40):
    """One committed store file. Its own reviewed commit keeps store pull requests in separate files."""
    return {"format_version": 1, "store": name, "reviewed_at_commit": commit,
            "owner": "fixture", "reason": "Fixture external store.",
            "states": {state: "unknown" for state in inv.STATES},
            "evidence": [{"path": "crates/engine/src/world.rs", "anchor": anchor}]}


def table_document(name, classification="deferred", owner="fixture", reason="Future feature allowed."):
    """One committed table file. Its own file keeps table pull requests apart."""
    return {"classification": classification, "format_version": 1, "owner": owner,
            "reason": reason, "table": name}


def make_tree(directory, source=READER):
    """A disposable checkout: one reader, the fixture dispositions and the converter/shared source roots."""
    root = Path(directory)
    here = root / "tools/world_data_inventory"
    here.mkdir(parents=True)
    dispositions = manual()
    dispositions.pop("stores")
    tables = dispositions.pop("tables")
    (here / "stores").mkdir()
    (here / "tables").mkdir()
    for key, value in tables.items():
        (here / "tables" / (key + ".json")).write_text(
            json.dumps(table_document(key, **value), sort_keys=True) + "\n", encoding="utf-8")
    (here / "dispositions.json").write_text(json.dumps(dispositions), encoding="utf-8")
    for folder in ("crates/converter/src", "crates/converter/records", "crates/shared/src"):
        (root / folder).mkdir(parents=True)
    (root / "crates/shared/src/lib.rs").write_text("pub const VERSION: u32 = 1;\n", encoding="utf-8")
    reader = root / "crates/engine/src/world.rs"
    reader.parent.mkdir(parents=True)
    reader.write_text(source, encoding="utf-8")
    return root, here, reader


def run_tool(root, here, *argv):
    out = root / "docs/data-inventory"
    with contextlib.redirect_stdout(io.StringIO()):
        inv.run([*argv, "--output", str(out)], root=root, here=here)
    return out


def committed_bytes(root):
    """Bytes of every file that the inventory tool owns and a commit would hold: the tool folder and the
    docs folder, minus the gitignored generated files. Source files elsewhere are the developer's own edit."""
    generated = {root / path for path in inv.GENERATED_PATHS}
    owned = [root / "tools/world_data_inventory", root / "docs/data-inventory"]
    return {path.relative_to(root).as_posix(): path.read_bytes() for folder in owned if folder.is_dir()
            for path in sorted(folder.rglob("*")) if path.is_file() and path not in generated}


class CommittedSurface(unittest.TestCase):
    """The gate keeps its purpose, and a source edit with no new table, column or reader changes no committed file."""

    def test_reader_source_edit_without_schema_change_gives_no_committed_diff(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, reader = make_tree(directory)
            run_tool(root, here, "--review-sources")
            out = run_tool(root, here, "--check")
            before = committed_bytes(root)
            first = json.loads((out / "inventory.json").read_text(encoding="utf-8"))["reviewed_source_guard_sha256"]
            # Unrelated edits: a new function, a comment, moved lines, a changed constant and an edit in a shared file.
            reader.write_text("// header\n\n\nfn helper() -> u32 { 7 }\n\n" + READER + "fn tail() {}\n", encoding="utf-8")
            (root / "crates/shared/src/lib.rs").write_text("pub const VERSION: u32 = 2;\n", encoding="utf-8")
            out = run_tool(root, here, "--check")
            self.assertEqual(committed_bytes(root), before)
            # The derived fingerprint did change. It lives only in the generated file.
            second = json.loads((out / "inventory.json").read_text(encoding="utf-8"))["reviewed_source_guard_sha256"]
            self.assertNotEqual(first, second)

    def test_new_or_changed_reader_fails_until_it_is_reviewed(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, reader = make_tree(directory)
            run_tool(root, here, "--review-sources")
            reader.write_text(READER + 'fn more(db: &Connection) { db.prepare("SELECT falloff FROM lights"); }\n', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "new reader without a reviewed disposition"):
                run_tool(root, here, "--check")
            run_tool(root, here, "--review-sources")
            run_tool(root, here, "--check")
            # Same call expression, different SQL behind the constant: still a changed reader.
            reader.write_text('const SQL: &str = "SELECT radius FROM lights";\nfn a(db: &Connection) { db.prepare(SQL); }\n', encoding="utf-8")
            run_tool(root, here, "--review-sources")
            reader.write_text('const SQL: &str = "SELECT falloff FROM lights";\nfn a(db: &Connection) { db.prepare(SQL); }\n', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "changed reader"):
                run_tool(root, here, "--check")
            # A dynamic reader that interpolates a same-file constant: an edit of the constant is reviewed.
            reader.write_text('const COLS: &str = "radius";\nfn a(db: &Connection) { db.prepare(&format!("SELECT {COLS} FROM lights")); }\n', encoding="utf-8")
            run_tool(root, here, "--review-sources")
            run_tool(root, here, "--check")
            reader.write_text('const COLS: &str = "radius,falloff";\nfn a(db: &Connection) { db.prepare(&format!("SELECT {COLS} FROM lights")); }\n', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "changed reader"):
                run_tool(root, here, "--check")
            # A removed reader is a change too, so the committed list stays exact.
            reader.write_text("fn nothing() {}\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "no longer exists"):
                run_tool(root, here, "--check")

    def test_new_table_or_column_without_a_disposition_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            extraction = {"format_version": 1, "source_commit": "fixture", "variants": [], "schemas": {"fixture": fixture()}}
            path = root / "extraction.json"
            def write(value):
                guard = inv.source_guard(root, inv.scan_queries(root), manual())
                value["reviewed_source_guard_sha256"] = inv.digest(inv.canonical(guard))
                path.write_text(json.dumps(value), encoding="utf-8")
            write(extraction)
            run_tool(root, here, "--review-sources", "--extraction", str(path))
            run_tool(root, here, "--check", "--extraction", str(path))
            columns = json.loads((here / "reviewed-surface.json").read_text())["columns"]
            self.assertIn("lights.falloff", columns)
            # A new column in a table that already has a disposition.
            with closing(inv.open_schema(extraction["schemas"]["fixture"], ["ALTER TABLE lights ADD COLUMN brightness REAL"])) as db:
                widened = {"format_version": 1, "source_commit": "fixture", "variants": [], "schemas": {"fixture": schema_from(db)}}
            write(widened)
            with self.assertRaisesRegex(ValueError, "new column without a reviewed disposition: lights.brightness"):
                run_tool(root, here, "--check", "--extraction", str(path))
            # A removed column must also be reviewed.
            write(extraction)
            run_tool(root, here, "--review-sources", "--extraction", str(path))
            with closing(inv.open_schema(extraction["schemas"]["fixture"], ["ALTER TABLE lights DROP COLUMN falloff"])) as db:
                narrowed = {"format_version": 1, "source_commit": "fixture", "variants": [], "schemas": {"fixture": schema_from(db)}}
            write(narrowed)
            with self.assertRaisesRegex(ValueError, "no longer exists: lights.falloff"):
                run_tool(root, here, "--check", "--extraction", str(path))
            # A new table fails: unreviewed as a column set, and unclassified as a table.
            with closing(inv.open_schema(extraction["schemas"]["fixture"], ["CREATE TABLE brand_new(id INTEGER)"])) as db:
                grown = {"format_version": 1, "source_commit": "fixture", "variants": [], "schemas": {"fixture": schema_from(db)}}
            write(grown)
            with self.assertRaisesRegex(ValueError, "new column without a reviewed disposition: brand_new.id"):
                run_tool(root, here, "--check", "--extraction", str(path))
            before = committed_bytes(root)
            with self.assertRaisesRegex(ValueError, "unclassified emitted table: brand_new"):
                run_tool(root, here, "--review-sources", "--extraction", str(path))
            self.assertEqual(committed_bytes(root), before)
            (here / "tables" / "brand_new.json").write_text(
                json.dumps(table_document("brand_new", reason="Reviewed."), sort_keys=True) + "\n",
                encoding="utf-8")
            write(grown)
            run_tool(root, here, "--review-sources", "--extraction", str(path))
            run_tool(root, here, "--check", "--extraction", str(path))

    def test_missing_or_stale_generated_files_are_regenerated_not_skipped(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            run_tool(root, here, "--review-sources")
            out = run_tool(root, here, "--check")
            expected = {name: (out / name).read_bytes() for name in ("inventory.json", "report.md")}
            self.assertTrue(all(expected.values()))
            (out / "inventory.json").unlink()
            (out / "report.md").write_text("stale copy from another checkout", encoding="utf-8")
            run_tool(root, here, "--check")
            self.assertEqual({name: (out / name).read_bytes() for name in expected}, expected)

    def test_a_tracked_generated_file_fails_with_the_one_line_recipe(self):
        if subprocess.run(["git", "--version"], capture_output=True).returncode != 0:
            self.skipTest("git is not available")
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            run_tool(root, here, "--review-sources")
            out = run_tool(root, here, "--check")
            git = lambda *args: subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", *args],
                                               cwd=root, capture_output=True, text=True, check=True)
            git("init", "-q")
            (root / ".gitignore").write_text("\n".join("/" + path for path in inv.GENERATED_PATHS) + "\n", encoding="utf-8")
            git("add", "-A")
            # The ignore rules keep a stale local copy out of `git add -A`.
            self.assertEqual(git("ls-files", "--", *inv.GENERATED_PATHS).stdout, "")
            run_tool(root, here, "--check")
            git("add", "-f", str(out / "inventory.json"))
            with self.assertRaisesRegex(ValueError, "generated inventory files are committed.*git rm -qf --ignore-unmatch"):
                run_tool(root, here, "--check")
            # The recipe removes the file. It is safe to run when a file is absent or was never tracked.
            subprocess.run(inv.MIGRATION_RECIPE.replace("&& git commit --no-edit", ""), shell=True, cwd=root, check=True,
                           capture_output=True)
            run_tool(root, here, "--check")

    def test_repository_ignores_every_generated_file(self):
        ignored = (inv.ROOT / ".gitignore").read_text(encoding="utf-8").splitlines()
        for path in inv.GENERATED_PATHS:
            self.assertIn("/" + path, ignored)
        self.assertEqual(inv.tracked_generated_files(inv.ROOT), [])


class StoreFiles(unittest.TestCase):
    """Each store owns its file and reviewed commit, so two store PRs never share a line."""

    def write_store(self, here, filename, document):
        (here / "stores").mkdir(exist_ok=True)
        (here / "stores" / filename).write_text(json.dumps(document), encoding="utf-8")

    def test_two_branches_that_add_different_stores_merge_without_conflict(self):
        # Both merge orders. A shared `stores` block or a shared reviewed commit would conflict here.
        for first, second in (("alpha", "beta"), ("beta", "alpha")):
            with self.subTest(order=(first, second)), tempfile.TemporaryDirectory() as directory:
                root, here, _ = make_tree(directory)
                git = lambda *args: subprocess.run(
                    ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                     "-c", "commit.gpgsign=false", *args],
                    cwd=root, capture_output=True, text=True, check=True)
                git("init", "-q")
                git("symbolic-ref", "HEAD", "refs/heads/main")
                git("add", "-A")
                git("commit", "-q", "-m", "base")
                for name in (first, second):
                    git("checkout", "-q", "-b", f"store-{name}", "main")
                    self.write_store(here, f"{name}.json", store_document(name))
                    git("add", "tools/world_data_inventory/stores")
                    git("commit", "-q", "-m", f"add {name} store")
                git("checkout", "-q", "main")
                for name in (first, second):
                    merged = subprocess.run(
                        ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                         "-c", "commit.gpgsign=false", "merge", "--no-edit", f"store-{name}"],
                        cwd=root, capture_output=True, text=True)
                    self.assertEqual(merged.returncode, 0, merged.stdout + merged.stderr)
                self.assertEqual(sorted(inv.load_stores(here)), sorted([first, second]))
                inv.validate_manual(root, inv.load_manual(here))

    def test_each_store_keeps_its_own_reviewed_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            _, here, _ = make_tree(directory)
            self.write_store(here, "alpha.json", store_document("alpha", commit="1" * 40))
            self.write_store(here, "beta.json", store_document("beta", commit="2" * 40))
            stores = inv.load_stores(here)
            self.assertEqual(stores["alpha"]["reviewed_at_commit"], "1" * 40)
            self.assertEqual(stores["beta"]["reviewed_at_commit"], "2" * 40)

    def test_check_fails_closed_on_a_missing_store_anchor(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            self.write_store(here, "broken.json",
                             store_document("broken", anchor="anchor that is not in the file"))
            with self.assertRaisesRegex(ValueError, r"store broken \(stores/broken\.json\): broken evidence reference"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_missing_store_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            (here / "stores").rmdir()
            with self.assertRaisesRegex(ValueError, "missing stores/ directory"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_missing_store_field(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            document = store_document("partial")
            document.pop("owner")
            self.write_store(here, "partial.json", document)
            with self.assertRaisesRegex(ValueError, "invalid store file stores/partial.json: missing owner"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_an_unknown_store_field(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            document = store_document("extra")
            document["unexpected"] = 1
            self.write_store(here, "extra.json", document)
            with self.assertRaisesRegex(ValueError, "invalid store file stores/extra.json: unknown unexpected"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_non_object_store_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            self.write_store(here, "list.json", [])
            with self.assertRaisesRegex(ValueError, "store file is not an object: stores/list.json"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_an_unexpected_file_in_the_store_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            (here / "stores" / "conversion-manifest.JSON").write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(ValueError,
                                        "unexpected file in store directory: stores/conversion-manifest.JSON"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_store_file_that_is_not_utf8(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            (here / "stores" / "binary.json").write_bytes(b"\xff\xfe")
            with self.assertRaisesRegex(ValueError, "cannot read store file stores/binary.json"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_store_path_that_cannot_be_read(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            (here / "stores" / "unreadable.json").mkdir()
            with self.assertRaisesRegex(ValueError, "cannot read store file stores/unreadable.json"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_duplicate_store(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            self.write_store(here, "one.json", store_document("same"))
            self.write_store(here, "two.json", store_document("same"))
            with self.assertRaisesRegex(ValueError, "duplicate store same: stores/one.json and stores/two.json"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_malformed_store_json(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            (here / "stores" / "broken.json").write_text("{", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid JSON in store file stores/broken.json"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_malformed_store_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            self.write_store(here, "bad-commit.json", store_document("bad-commit", commit="HEAD"))
            with self.assertRaisesRegex(ValueError, "invalid store reviewed_at_commit: bad-commit"):
                run_tool(root, here, "--check")

    def test_a_legacy_stores_block_fails_with_the_migration_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            dispositions = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            dispositions["stores"] = {"legacy": store_document("legacy")}
            (here / "dispositions.json").write_text(json.dumps(dispositions), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"must not contain a stores block.*stores/"):
                run_tool(root, here, "--check")


class TableFiles(unittest.TestCase):
    """One table per file; a bad or old layout fails instead of dropping a classification."""

    def write_table(self, here, filename, document):
        (here / "tables" / filename).write_text(json.dumps(document, sort_keys=True) + "\n", encoding="utf-8")

    def test_check_fails_closed_on_malformed_table_json(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            (here / "tables" / "broken.json").write_text("{", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid JSON in table file tables/broken.json"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_a_duplicate_table(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            self.write_table(here, "one.json", table_document("same"))
            self.write_table(here, "two.json", table_document("same"))
            with self.assertRaisesRegex(ValueError, "duplicate table same: tables/one.json and tables/two.json"):
                run_tool(root, here, "--check")

    def test_check_fails_closed_on_an_unexpected_file_in_the_table_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            (here / "tables" / "table.JSON").write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(ValueError,
                                        "unexpected file in table directory: tables/table.JSON"):
                run_tool(root, here, "--check")

    def test_emitted_payload_uses_only_the_old_table_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            manual = inv.load_manual(here)
            extraction = {"format_version": 1, "source_commit": "fixture",
                          "schemas": {"fixture": fixture()}, "variants": []}
            loaded = inv.build(root, manual, copy.deepcopy(extraction))
            stripped = copy.deepcopy(manual)
            stripped["tables"] = {name: {field: value[field] for field in inv.TABLE_DISPOSITIONS}
                                  for name, value in stripped["tables"].items()}
            old = inv.build(root, stripped, copy.deepcopy(extraction))
            self.assertEqual(inv.canonical(loaded), inv.canonical(old))

    def test_a_legacy_tables_block_fails_with_the_migration_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_tree(directory)
            dispositions = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            dispositions["tables"] = {"legacy": {"classification": "deferred", "owner": "fixture", "reason": "Old."}}
            (here / "dispositions.json").write_text(json.dumps(dispositions), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"must not contain a tables block.*tables/"):
                run_tool(root, here, "--check")


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

    def test_a_tests_rs_file_is_not_a_production_reader(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root / "crates/engine/src/feature"
            folder.mkdir(parents=True)
            (folder / "tests.rs").write_text('fn t(db: &Connection) { db.prepare("SELECT only_test FROM lights"); }', encoding="utf-8")
            (folder / "mod.rs").write_text('fn p(db: &Connection) { db.prepare("SELECT radius FROM lights"); }', encoding="utf-8")
            queries = inv.scan_queries(root)
            self.assertEqual([q["path"] for q in queries], ["crates/engine/src/feature/mod.rs"])

    def test_sources_are_read_as_utf8_whatever_the_host_locale(self):
        # U+009D encodes as 0xC2 0x9D. The Windows cp1252 codec rejects the byte 0x9D, so a read with the
        # host default encoding would raise here on such a host; an explicit UTF-8 read must not.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "crates/engine/src/wide.rs"
            path.parent.mkdir(parents=True)
            path.write_bytes('// \u009d \u00e9\nfn p(db: &Connection) { db.prepare("SELECT radius FROM lights"); }\n'.encode("utf-8"))
            self.assertEqual(len(inv.scan_queries(root)), 1)
            written = root / "out.json"
            inv.write_text(written, "line one\nline two\n")
            self.assertEqual(written.read_bytes(), b"line one\nline two\n")

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
        self.assertEqual((inv.ROOT / "docs/data-inventory/fixture-report.md").read_text(encoding="utf-8"), fixture_report())

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
        inv.validate_manual(inv.ROOT, inv.load_manual(inv.HERE))

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
