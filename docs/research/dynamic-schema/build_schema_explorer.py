#!/usr/bin/env python3
"""Build the offline research explorer from pinned facts; Python stdlib only."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import sys
from urllib.parse import quote


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
# An evidence revision, never the viewer's eventual HEAD (which would self-reference).
CODE_PIN = "44499b22274e3e951f8aa5e630c833e39acf88cb"
XEDIT_PIN = "9fb016884bec138ea6c7b872cec831537d464c3e"
MUTAGEN_PIN = "4f533562ee0c70347d47c1979d5464d42b06ee6b"
CODE_HASHES = {
    "crates/converter/src/esm/exporter.rs": "cb3e83cc6c89d41666d6d21f471287e5bdf574d3a95c37fd2b35628b2361df4e",
    "crates/shared/src/lib.rs": "691bde5cb1ace0287992aafe75b35ba8b340de80b1fd15cd020c2223ca175050",
    "crates/converter/src/esm/records/mod.rs": "e4efc3290da5eb3e00794d20b8726763ef759353274bc67f05af69e5eba1eab6",
    "crates/converter/src/esm/mod.rs": "5b420c29a06888c571d491bd4e9a643a224d1e5b429e9a76f65b5b58f3366126",
    "crates/converter/src/esm/load_order.rs": "d99a576a6a89fdb640ac9f2942bb26bae96453f73f0d0f74ecd9db6c4b074057",
    "crates/converter/src/esm/extractors.rs": "3df9d88db6a3e591d0f1f3833bfc50c21b6c8892144e2da4ab2b9e0acacac4a2",
    "crates/converter/src/esm/cell_cache.rs": "b81172c62f74193a4e97167f9d742bb325a856d1e461face28173162c935bae7",
}
REPOS = {
    "project": ("Mudcrab-Team/mudcrab", CODE_PIN),
    "xedit": ("TES5Edit/TES5Edit", XEDIT_PIN),
    "mutagen": ("Mutagen-Modding/Mutagen", MUTAGEN_PIN),
}
HASH_NORMALIZATION = "SHA-256 of strict UTF-8 text with CRLF converted to LF; all other bytes retained."


def read_text_lf(path: Path) -> str:
    # Git may check text out with CRLF. Preserve lone CR, Unicode and whitespace.
    return path.read_bytes().decode("utf-8").replace("\r\n", "\n")


def text_digest(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def source_link(source: str, path: str, line: int, label: str = "") -> dict:
    """Construct links from trusted repository IDs and repository-relative paths."""
    if source not in REPOS or line < 1 or path.startswith(("/", "\\")):
        raise ValueError("Invalid source anchor")
    if ".." in Path(path).parts or "\\" in path:
        raise ValueError("Source paths must be POSIX repository paths")
    repo, pin = REPOS[source]
    return {
        "source": source, "path": path, "line": line, "pin": pin,
        "label": label or f"{source}: {path}:{line}",
        "url": f"https://github.com/{repo}/blob/{pin}/{quote(path, safe='/')}#L{line}",
    }


def anchor(path: str, text: str, sources: dict[str, str], label: str = "") -> dict:
    matches = [i + 1 for i, row in enumerate(sources[path].splitlines()) if text in row]
    if not matches:
        raise ValueError(f"Missing code anchor {path}: {text}")
    return source_link("project", path, matches[0], label)


def extract_ddl(exporter: str) -> tuple[str, int]:
    # Strictly the first raw SQL batch of create_tables, not test fixture DDL.
    match = re.search(
        r'pub fn create_tables\(conn: &Connection\) -> Result<\(\)> \{\s*'
        r'conn\.execute_batch\(\s*r#"(.*?)"#\s*\)\?;', exporter, re.S,
    )
    if not match:
        raise ValueError("create_tables SQL batch changed; review and refresh the code pin")
    return match.group(1), exporter[:match.start(1)].count("\n") + 1


def sql_identifier(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def introspect_tables(ddl: str, first_line: int) -> tuple[list[dict], dict]:
    """Execute the actual DDL in an empty database; never read retail data."""
    declarations = list(re.finditer(
        r'CREATE\s+(VIRTUAL\s+)?TABLE\s+IF\s+NOT\s+EXISTS\s+(?:"([^"]+)"|(\w+))',
        ddl, re.I,
    ))
    tables = []
    with sqlite3.connect(":memory:") as conn:
        conn.executescript(ddl)
        for declaration in declarations:
            name = declaration.group(2) or declaration.group(3)
            quoted = sql_identifier(name)
            columns = [
                {"name": n, "type": t, "declared_not_null": bool(nn),
                 "default": default, "pk_position": pk}
                for _, n, t, nn, default, pk in conn.execute(f"PRAGMA table_info({quoted})")
            ]
            indexes = []
            for _, index_name, unique, origin, partial in conn.execute(f"PRAGMA index_list({quoted})"):
                index_sql = conn.execute(
                    "SELECT sql FROM sqlite_master WHERE type='index' AND name=?", (index_name,)
                ).fetchone()[0]
                indexes.append({
                    "name": index_name, "unique": bool(unique), "origin": origin,
                    "partial": bool(partial), "sql": index_sql,
                    "columns": [c for _, _, c in conn.execute(f"PRAGMA index_info({sql_identifier(index_name)})")],
                })
            tables.append({
                "name": name, "kind": "rtree" if declaration.group(1) else "table",
                "columns": columns, "primary_key": [c["name"] for c in sorted(columns, key=lambda c: c["pk_position"]) if c["pk_position"]],
                "indexes": sorted(indexes, key=lambda i: i["name"]),
                "foreign_keys": list(conn.execute(f"PRAGMA foreign_key_list({quoted})")),
                "sql": conn.execute("SELECT sql FROM sqlite_master WHERE name=?", (name,)).fetchone()[0],
                "source": source_link("project", "crates/converter/src/esm/exporter.rs",
                                      first_line + ddl[:declaration.start()].count("\n"), "Current table DDL"),
            })
        sqlite_tables = {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    logical_names = {t["name"] for t in tables}
    return tables, {
        "logical_tables": len(tables), "rtree_tables": sum(t["kind"] == "rtree" for t in tables),
        "sqlite_tables_with_shadows": len(sqlite_tables),
        "excluded_shadow_tables": sorted(sqlite_tables - logical_names),
        "foreign_key_count": sum(len(t["foreign_keys"]) for t in tables),
        "column_count": sum(len(t["columns"]) for t in tables),
    }


def constant(sources: dict, name: str) -> int:
    match = re.search(rf"pub const {name}: u32 = (\d+);", sources["crates/shared/src/lib.rs"])
    if not match:
        raise ValueError(f"Missing version constant {name}")
    return int(match.group(1))


def read_json(path: Path) -> dict:
    return json.loads(read_text_lf(path))


def build_data(root: Path = ROOT, here: Path = HERE) -> dict:
    sources = {}
    for path, expected in CODE_HASHES.items():
        text = read_text_lf(root / path)
        if text_digest(text) != expected:
            raise ValueError(f"Code drift at {path}; review facts and update the explicit evidence pin")
        sources[path] = text
    input_names = ("candidate-inventory.json", "native-field-ledger.md", "pilot-code-evidence.json",
                   "schema-explorer-annotations.json", "schema-explorer-re-evidence.json")
    input_texts = {name: read_text_lf(here / name) for name in input_names}
    inventory = json.loads(input_texts["candidate-inventory.json"])
    if inventory["sources"]["xedit"]["pin"] != XEDIT_PIN or inventory["sources"]["mutagen"]["pin"] != MUTAGEN_PIN:
        raise ValueError("Candidate inventory source pin changed")
    annotations = json.loads(input_texts["schema-explorer-annotations.json"])
    evidence = json.loads(input_texts["schema-explorer-re-evidence.json"])
    for path, expected in evidence["pins"]["mudcrab_evidence_worktree"]["evidence_file_sha256"].items():
        text = input_texts.get(Path(path).name)
        if text is None or text_digest(text) != expected:
            raise ValueError(f"MCP brief/project evidence mismatch: {path}")
    ddl, first_line = extract_ddl(sources["crates/converter/src/esm/exporter.rs"])
    tables, summary = introspect_tables(ddl, first_line)
    table_names = {t["name"] for t in tables}
    rows = []
    for entry in inventory["entries"]:
        signature = entry["signature"]
        if not re.fullmatch(r"[A-Z0-9_]{4}", signature):
            raise ValueError(f"Invalid record signature: {signature}")
        status = "header" if signature == "TES4" else "shared" if entry["mutagen_declarations"] else "xedit-only"
        declarations = []
        for source in ("xedit", "mutagen"):
            for declaration in entry[f"{source}_declarations"]:
                declarations.append({
                    "name": declaration.get("name", declaration.get("object_name")),
                    "link": source_link(source, declaration["path"], declaration["line"]),
                    "groups": [source_link(source, g["path"], g["line"], g["group_name"])
                               for g in declaration.get("model_group_registration", [])],
                })
        mappings = []
        for mapping in annotations["record_mappings"]:
            if signature in mapping["records"]:
                if mapping["table"] not in table_names:
                    raise ValueError(f"Mapping target disappeared: {mapping['table']}")
                mappings.append({"table": mapping["table"], "note": mapping["note"],
                                 "source": anchor(mapping["path"], mapping["anchor"], sources, "Projection code")})
        rows.append({
            "id": signature, "name": annotations.get("record_name_overrides", {}).get(signature, entry["xedit_declarations"][0]["name"]),
            "status": status, "declarations": declarations, "caveats": entry["caveats"], "mappings": mappings,
            "generic_projection": signature != "TES4",
        })
    summary.update({"records": len(rows), "shared": sum(r["status"] == "shared" for r in rows),
                    "xedit_only": sum(r["status"] == "xedit-only" for r in rows), "headers": sum(r["status"] == "header" for r in rows)})
    expected_counts = {"records": 134, "shared": 127, "xedit_only": 6, "headers": 1,
                       "logical_tables": 26, "rtree_tables": 2, "sqlite_tables_with_shadows": 32, "foreign_key_count": 0}
    if any(summary[k] != v for k, v in expected_counts.items()):
        raise ValueError(f"Catalog/table denominator changed: {summary}")
    record_ids = {r["id"] for r in rows}
    for table in tables:
        annotation = annotations["tables"][table["name"]]
        table.update(annotation)
        table["records"] = [r["id"] for r in rows if any(m["table"] == table["name"] for m in r["mappings"])]
        for col in table["columns"]:
            col["note"] = annotations["column_notes"].get(f"{table['name']}.{col['name']}", "")
    fields = annotations["fields"]
    for field in fields:
        if field["record"] not in record_ids:
            raise ValueError(f"Unknown field record {field['record']}")
        field["id"] = f"{field['record']}.{field['tag']}"
        field["sources"] = [source_link(**s) for s in field.pop("anchors")]
        field["sources"].append(source_link("project", "docs/research/dynamic-schema/native-field-ledger.md", field["ledger_line"], "Historical field ledger (2026-10-05)"))
        for target in field["columns"]:
            tab, col = target.split(".")
            if tab not in table_names or col not in {c["name"] for t in tables if t["name"] == tab for c in t["columns"]}:
                raise ValueError(f"Missing field projection column {target}")
    for link in annotations["relationships"]:
        for side in ("from", "to"):
            tab, col = link[side].split(".")
            if tab not in table_names or col not in {c["name"] for t in tables if t["name"] == tab for c in t["columns"]}:
                raise ValueError(f"Invalid relationship {link[side]}")
        link["source"] = anchor(link.pop("path"), link.pop("anchor"), sources, "Code relationship evidence")
    gates = [dict(a, link=source_link(source, a["path"], a["line"]))
             for source, anchors in inventory["source_gate_anchors"].items() for a in anchors]
    return {
        "title": "Mudcrab schema explorer", "summary": summary,
        "versions": {"world_database": constant(sources, "WORLD_DATABASE_SCHEMA_VERSION"),
                     "runtime_min": constant(sources, "MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION"),
                     "cell_cache": constant(sources, "CELL_CACHE_VERSION")},
        "pins": {"code": CODE_PIN, "xedit": XEDIT_PIN, "mutagen": MUTAGEN_PIN},
        "hash_normalization": HASH_NORMALIZATION,
        "source_files": [{"path": path, "sha256": digest, "link": source_link("project", path, 1)} for path, digest in CODE_HASHES.items()],
        "input_hashes": {name: text_digest(text) for name, text in input_texts.items()},
        "records": rows, "tables": tables, "fields": fields,
        "relationships": annotations["relationships"], "variants": annotations["variants"],
        "glossary": annotations["glossary"], "dataflow": annotations["dataflow"],
        "source_gates": gates, "catalog_caveats": inventory["interpretive_caveats"],
        "mcp": evidence,
        "links": {
            "inventory": source_link("project", "docs/research/dynamic-schema/candidate-inventory.json", 1, "Candidate inventory"),
            "ledger": source_link("project", "docs/research/dynamic-schema/native-field-ledger.md", 1, "Historical field/native ledger"),
            "pilot": source_link("project", "docs/research/dynamic-schema/pilot-code-evidence.json", 1, "Historical code comparison report"),
            "plan": source_link("project", "docs/roadmap/dynamic-schema-initiative.md", 14, "Ingestion initiative"),
            "versions": source_link("project", "crates/shared/src/lib.rs", 12, "Version constants"),
        },
    }


def json_for_script(value: object) -> str:
    # HTML parses script endings before JSON; escape every '<', including mixed-case endings.
    return (json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
            .replace("&", "\\u0026").replace("<", "\\u003c").replace(">", "\\u003e")
            .replace("\u2028", "\\u2028").replace("\u2029", "\\u2029"))


def render(data: dict, template: str) -> str:
    if template.count("@@SCHEMA_DATA@@") != 1:
        raise ValueError("HTML template needs exactly one schema-data marker")
    return template.replace("@@SCHEMA_DATA@@", json_for_script(data))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Fail if the committed HTML differs from its pinned inputs")
    args = parser.parse_args(argv)
    output = HERE / "schema-explorer.html"
    try:
        data = build_data()
        html = render(data, read_text_lf(HERE / "schema-explorer.template.html"))
        if args.check:
            if read_text_lf(output) != html:
                raise ValueError("Generated HTML drift; run build_schema_explorer.py after reviewing input changes")
        else:
            output.write_text(html, encoding="utf-8", newline="\n")
        s = data["summary"]
        print(f"{'Checked' if args.check else 'Built'} {output.name}: {s['records']} catalog entries "
              f"({s['records'] - s['headers']} candidates + TES4), "
              f"{s['logical_tables']} logical tables, {s['column_count']} columns, "
              f"{len(data['fields'])} curated fields; {s['foreign_key_count']} declared foreign keys")
        return 0
    except (OSError, ValueError, KeyError, sqlite3.Error) as error:
        print(f"Schema explorer: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
