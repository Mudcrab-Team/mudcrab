"""Portable semantic/serialization regressions for the human schema explorer."""

from html.parser import HTMLParser
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from urllib.parse import unquote, urlsplit

import build_schema_explorer as explorer


class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.scripts = []
        self.active_script = None
        self.external_resources = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "script":
            self.scripts.append({"attrs": attrs, "body": ""})
            self.active_script = self.scripts[-1]
        if tag in {"script", "img", "iframe", "link"} and (attrs.get("src") or attrs.get("href")):
            self.external_resources.append(attrs.get("src") or attrs.get("href"))

    def handle_endtag(self, tag):
        if tag == "script":
            self.active_script = None

    def handle_data(self, text):
        if self.active_script is not None:
            self.active_script["body"] += text


class TestV159_ExplorerFacts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.data = explorer.build_data()
        cls.tables = {t["name"]: t for t in cls.data["tables"]}
        cls.records = {r["id"]: r for r in cls.data["records"]}
        cls.ddl, _ = explorer.extract_ddl(explorer.read_text_lf(explorer.ROOT / "crates/converter/src/esm/exporter.rs"))

    def test_every_inventory_identity_retained_and_tes4_separate(self):
        inventory = explorer.read_json(explorer.HERE / "candidate-inventory.json")
        self.assertEqual(set(self.records), {e["signature"] for e in inventory["entries"]})
        self.assertEqual(len(self.records), 134)
        self.assertEqual(sum(r["status"] == "shared" for r in self.records.values()), 127)
        self.assertEqual({r["id"] for r in self.records.values() if r["status"] == "xedit-only"},
                         {"CLDC", "PLYR", "PWAT", "RGDL", "SCOL", "SCPT"})
        self.assertEqual(self.records["TES4"]["status"], "header")
        self.assertFalse(self.records["TES4"]["generic_projection"])
        self.assertEqual(len([d for d in self.records["GLOB"]["declarations"] if d["link"]["source"] == "mutagen"]), 5)
        self.assertEqual(len([d for d in self.records["GMST"]["declarations"] if d["link"]["source"] == "mutagen"]), 5)

    def test_logical_tables_exclude_only_rtree_shadows_and_have_no_fks(self):
        self.assertEqual(len(self.tables), 26)
        self.assertEqual({t["name"] for t in self.tables.values() if t["kind"] == "rtree"},
                         {"exterior_spatial", "lod_chunks_spatial"})
        self.assertEqual(set(self.data["summary"]["excluded_shadow_tables"]),
                         {f"{table}_{suffix}" for table in ("exterior_spatial", "lod_chunks_spatial")
                          for suffix in ("node", "parent", "rowid")})
        self.assertEqual(self.data["summary"]["sqlite_tables_with_shadows"], 32)
        with sqlite3.connect(":memory:") as conn:
            conn.executescript(self.ddl)
            for table in self.tables.values():
                self.assertEqual(table["foreign_keys"], [])
                self.assertEqual(list(conn.execute(f'PRAGMA foreign_key_list("{table["name"]}")')), [])
        self.assertTrue(self.data["relationships"])
        self.assertTrue(all(r["kind"] in {"Code relationship", "Semantic link"} for r in self.data["relationships"]))

    def test_versions_come_from_current_shared_contract(self):
        self.assertEqual(self.data["versions"], {"world_database": 9, "runtime_min": 3, "cell_cache": 3})
        self.assertEqual(self.data["pins"]["code"], explorer.CODE_PIN)
        self.assertEqual(self.data["summary"]["column_count"], 183)
        self.assertEqual(self.data["integration_evidence"]["contracts"]["producer"], 27)
        self.assertEqual(self.data["integration_evidence"]["code_commit"], explorer.CODE_PIN)

    def test_composite_keys_defaults_and_partial_index_preserved(self):
        self.assertEqual(self.tables["lod"]["primary_key"], ["cell_id", "lod_level"])
        self.assertEqual(self.tables["lod_chunks"]["primary_key"], ["worldspace_id", "tier", "anchor_x", "anchor_y"])
        self.assertEqual(self.tables["landscape_texture_grasses"]["primary_key"], ["ltex_id", "gras_id"])
        self.assertEqual(self.tables["scripts"]["primary_key"], ["form_id", "script_name"])
        columns = {c["name"]: c for c in self.tables["references"]["columns"]}
        self.assertEqual(columns["scale"]["default"], "1.0")
        self.assertEqual(columns["header_flags"]["default"], "0")
        partial = next(i for i in self.tables["records"]["indexes"] if i["name"] == "idx_records_cell_id")
        self.assertTrue(partial["partial"])
        self.assertEqual(partial["columns"], ["cell_id"])
        self.assertIn("WHERE cell_id IS NOT NULL", partial["sql"])

    def test_actual_sql_pk_nullability_defaults_and_unenforced_links(self):
        integer_key = self.tables["plugins"]["columns"][0]
        text_key = self.tables["conversion_cache"]["columns"][0]
        self.assertEqual(integer_key["pk_position"], 1)
        self.assertFalse(integer_key["declared_not_null"])
        self.assertFalse(text_key["declared_not_null"])
        with sqlite3.connect(":memory:") as conn:
            conn.executescript(self.ddl)
            conn.execute("INSERT INTO plugins(id,name,priority,checksum) VALUES(NULL,'fixture.esm',0,x'00')")
            self.assertEqual(conn.execute("SELECT id FROM plugins").fetchone()[0], 1)
            # TEXT PRIMARY KEY in a rowid table permits NULL unless explicitly NOT NULL.
            conn.execute("INSERT INTO conversion_cache VALUES(NULL,x'00',0)")
            self.assertIsNone(conn.execute("SELECT plugin_path FROM conversion_cache").fetchone()[0])
            with self.assertRaises(sqlite3.IntegrityError):
                conn.execute("INSERT INTO lod VALUES(NULL,0,x'00')")
            conn.execute("INSERT INTO lod VALUES(1,0,x'00')")
            conn.execute("INSERT INTO lod VALUES(1,1,x'00')")
            with self.assertRaises(sqlite3.IntegrityError):
                conn.execute("INSERT INTO lod VALUES(1,0,x'00')")
            # No FK requires these cell/base IDs to exist. SQL defaults are exercised.
            conn.execute('INSERT INTO "references"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z) '
                         'VALUES(1,999,888,0,0,0,0,0,0,0)')
            self.assertEqual(conn.execute('SELECT scale,header_flags FROM "references"').fetchone(), (1.0, 0))

    def test_partial_export_allowlists_and_flag_layers_remain_visible(self):
        movt = self.records["MOVT"]["mappings"][0]
        gmst = self.records["GMST"]["mappings"][0]
        self.assertIn("0x0003580D", movt["note"])
        self.assertIn("Only NPC_Default_MT", movt["note"])
        self.assertIn("fMoveCharWalkBase", gmst["note"])
        self.assertIn("fJumpHeightMin", gmst["note"])
        self.assertIn("Only", gmst["note"])
        columns = {t["name"]+'.'+c["name"]: c for t in self.tables.values() for c in t["columns"]}
        for key in ("cells.flags", "references.header_flags", "statics.flags", "worldspaces.flags", "npcs.flags"):
            self.assertIn("header", columns[key]["note"].lower())
        self.assertIn("DATA flags", columns["lights.flags"]["note"])
        self.assertIn("Only XESP byte 4", columns["references.enable_parent_flags"]["note"])
        self.assertIn("do not contribute", columns["references.enable_parent_flags"]["note"])
        self.assertIn("unresolved", columns["references.enable_parent_flags"]["note"])

    def test_current_xesp_and_armo_facts_are_separate_from_historical_evidence(self):
        fields = {field["id"]: field for field in self.data["fields"]}
        xesp = fields["REFR.XESP"]
        self.assertIn("only byte 4", xesp["projection"])
        self.assertIn("excluded from the flag column", xesp["projection"])
        self.assertTrue(any(source["path"].endswith("/achr.rs") for source in xesp["sources"]))
        self.assertIn("MOD4 with the in-house reader or MOD3 with the legacy reader", fields["STAT.MODL"]["projection"])
        self.assertIn("historical corpus", xesp["unknown"])
        self.assertIn("parent-chain effects remain unobserved", xesp["unknown"])

    def test_mcp_labels_keep_producer_and_runtime_limits(self):
        mcp = self.data["mcp"]
        self.assertEqual(mcp["mcp_receipts"]["workbench"]["called_tools"], ["ledger_search", "ledger_show", "lookup"])
        self.assertEqual(mcp["mcp_receipts"]["workbench"]["mutating_tools_called"], [])
        findings = {f["id"]: f for f in mcp["ledger_findings"]}
        self.assertIn("input-manifest hash", findings["F0010"]["limit"])
        self.assertIn("input/load-order hash", findings["F0011"]["limit"])
        self.assertIn("Superseded", findings["F0007"]["use"])
        self.assertIn("Producer converter commit absent", findings["F0008"]["limit"])
        self.assertIn("Excluded", findings["F0009"]["use"])
        self.assertIn("no VM/runtime observation", mcp["pins"]["native_target"]["evidence_tier"])
        self.assertEqual({f["record"] for f in self.data["fields"]}, {"REFR", "CELL", "STAT"})
        self.assertTrue(all(f["unknown"] for f in self.data["fields"]))

    def test_pin_links_encode_paths_and_anchor_actual_source_lines(self):
        links = []
        for record in self.records.values():
            links.extend(d["link"] for d in record["declarations"])
            links.extend(m["source"] for m in record["mappings"])
        links.extend(s for f in self.data["fields"] for s in f["sources"])
        links.extend(t["source"] for t in self.tables.values())
        links.extend(stage["source"] for stage in self.data["dataflow"]["current"])
        for link in links:
            split = urlsplit(link["url"])
            self.assertEqual((split.scheme, split.hostname), ("https", "github.com"))
            self.assertIn('/blob/'+link["pin"]+'/', split.path)
            self.assertEqual(split.fragment, 'L'+str(link["line"]))
            self.assertTrue(unquote(split.path).endswith('/'+link["path"]))
            self.assertNotIn(' ', link["url"])
            if link["source"] == "project":
                self.assertLessEqual(link["line"], len(explorer.read_text_lf(explorer.ROOT / link["path"]).splitlines()))
        with self.assertRaises(ValueError):
            explorer.source_link("project", "../outside", 1)
        with self.assertRaises(ValueError):
            explorer.source_link("project", "/absolute", 1)

    def test_reviewed_project_anchors_point_to_the_intended_source_text(self):
        annotations = explorer.read_json(explorer.HERE / "schema-explorer-annotations.json")
        fields = {field["id"]: field for field in self.data["fields"]}
        for field in annotations["fields"]:
            for expected in field["anchors"]:
                if expected["source"] != "project":
                    continue
                actual = next(source for source in fields[field["record"]+'.'+field["tag"]]["sources"]
                              if source["path"] == expected["path"] and source["label"] != "Historical field ledger (2026-10-05)"
                              and expected["anchor"] in explorer.read_text_lf(explorer.ROOT / source["path"]).splitlines()[source["line"]-1])
                self.assertEqual(actual["pin"], explorer.CODE_PIN)

    def test_missing_or_ambiguous_code_anchor_requires_review(self):
        path = "fixture.rs"
        self.assertEqual(explorer.anchor(path, "selected()", {path: "// leading line\nselected();\n"})["line"], 2)
        with self.assertRaisesRegex(ValueError, "Missing code anchor"):
            explorer.anchor(path, "selected()", {path: "different();\n"})
        with self.assertRaisesRegex(ValueError, "Ambiguous code anchor"):
            explorer.anchor(path, "selected()", {path: "selected();\nselected();\n"})


class TestV159_EmbeddedDocument(unittest.TestCase):
    def test_script_breakout_cannot_create_markup_or_resources(self):
        hostile = {'label': '</ScRiPt><img src="https://example.invalid/track"><script>alert(1)</script>&\u2028\u2029',
                   'nested': ['<script>', '<!--', '雪']}
        rendered = explorer.render(hostile, '<script id="schema-data" type="application/json">@@SCHEMA_DATA@@</script><script>/* app */</script>')
        parsed = Document()
        parsed.feed(rendered)
        self.assertEqual(len(parsed.scripts), 2)
        self.assertEqual(parsed.external_resources, [])
        self.assertEqual(json.loads(parsed.scripts[0]["body"]), hostile)
        self.assertNotIn('<', parsed.scripts[0]["body"])
        self.assertNotIn('\u2028', parsed.scripts[0]["body"])

    def test_generated_document_matches_reviewed_inputs_and_is_self_contained(self):
        data = explorer.build_data()
        generated = explorer.render(data, explorer.read_text_lf(explorer.HERE / "schema-explorer.template.html"))
        self.assertEqual(generated, explorer.read_text_lf(explorer.HERE / "schema-explorer.html"))
        parsed = Document()
        parsed.feed(generated)
        self.assertEqual(len(parsed.scripts), 2)
        self.assertEqual(parsed.external_resources, [])
        self.assertEqual(json.loads(parsed.scripts[0]["body"]), data)


class TestV160_TextIdentity(unittest.TestCase):
    def copy_checkout(self, root: Path, crlf: bool):
        relative_docs = explorer.HERE.relative_to(explorer.ROOT)
        paths = list(explorer.CODE_HASHES) + [str(relative_docs / name) for name in (
            "candidate-inventory.json", "native-field-ledger.md", "pilot-code-evidence.json",
            "schema-explorer-annotations.json", "schema-explorer-re-evidence.json", "schema-explorer.template.html",
            "schema-explorer-integration-evidence.json")]
        for relative in paths:
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            text = explorer.read_text_lf(explorer.ROOT / relative)
            target.write_bytes((text.replace('\n', '\r\n') if crlf else text).encode('utf-8'))
        return root / relative_docs

    def test_equivalent_lf_crlf_checkouts_have_identical_complete_data_and_html(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lf, crlf = root / "lf", root / "crlf"
            lf_docs = self.copy_checkout(lf, False)
            crlf_docs = self.copy_checkout(crlf, True)
            lf_data, crlf_data = explorer.build_data(lf, lf_docs), explorer.build_data(crlf, crlf_docs)
            self.assertEqual(lf_data, crlf_data)
            lf_html = explorer.render(lf_data, explorer.read_text_lf(lf_docs / "schema-explorer.template.html"))
            crlf_html = explorer.render(crlf_data, explorer.read_text_lf(crlf_docs / "schema-explorer.template.html"))
            self.assertEqual(lf_html, crlf_html)

    def test_normalization_preserves_lone_cr_unicode_and_other_whitespace(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'source'
            path.write_bytes('A\r\nB\rC\t 雪\u00a0\n'.encode('utf-8'))
            self.assertEqual(explorer.read_text_lf(path), 'A\nB\rC\t 雪\u00a0\n')
            path.write_bytes(b'\xff')
            with self.assertRaises(UnicodeDecodeError):
                explorer.read_text_lf(path)

    def test_semantic_or_nonnewline_source_drift_still_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            docs = self.copy_checkout(root, True)
            path = root / 'crates/converter/src/esm/exporter.rs'
            path.write_bytes(path.read_bytes().replace(b'DEFAULT 1.0', b'DEFAULT 2.0', 1))
            with self.assertRaisesRegex(ValueError, 'Code drift'):
                explorer.build_data(root, docs)

    def test_stale_integration_evidence_cannot_claim_new_contracts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            docs = self.copy_checkout(root, False)
            path = docs / "schema-explorer-integration-evidence.json"
            evidence = explorer.read_json(path)
            evidence["contracts"]["world_database"] = 7
            path.write_text(json.dumps(evidence))
            with self.assertRaisesRegex(ValueError, "contracts/projection changed"):
                explorer.build_data(root, docs)


if __name__ == "__main__":
    unittest.main()
