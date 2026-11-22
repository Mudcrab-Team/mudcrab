#!/usr/bin/env python3
"""Bounded world-data evidence inventory. No plugin decoding or game-data writes."""
import argparse
import hashlib
import json
import re
import sqlite3
import subprocess
import sys
from contextlib import closing
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
SURFACE_NAME = "reviewed-surface.json"
STORE_DIR = "stores"
STORE_FIELDS = ("format_version", "store", "reviewed_at_commit", "owner", "reason", "states", "evidence")
STORE_DISPOSITIONS = ("reviewed_at_commit", "owner", "reason", "states", "evidence")
TABLE_DIR = "tables"
TABLE_FIELDS = ("format_version", "table", "classification", "owner", "reason")
TABLE_DISPOSITIONS = ("classification", "owner", "reason")
COMMIT = re.compile(r"[0-9a-f]{40}")
# Derived files. Never commit them: they hold fingerprints of source files, so every unrelated source edit
# changes them and every open pull request then conflicts. CI and this tool regenerate them. The list matches
# .gitignore. `reviewed-sources.json` is the old committed fingerprint file; it stays here so a stale copy is found.
GENERATED_PATHS = ("docs/data-inventory/inventory.json", "docs/data-inventory/report.md",
                   "tools/world_data_inventory/reviewed-sources.json")
MIGRATION_RECIPE = ("git rm -qf --ignore-unmatch -- " + " ".join(GENERATED_PATHS) + " && git commit --no-edit")
METHODS = {"prepare", "prepare_cached", "query_row", "query_row_and_then", "execute", "execute_batch"}
STATES = ("decoded", "preserved", "projected", "query_dependent", "mapped", "semantic_use")
STATE_VALUES = {
    "decoded": {"unknown", "source_decoded", "opaque", "rejected"},
    "preserved": {"unknown", "source_preserved"},
    "projected": {"unknown", "source_projected"},
    "query_dependent": {"unknown", "source_reference"},
    "mapped": {"unknown", "source_mapping"},
    "semantic_use": {"unknown", "source_sink"},
}
RAW_STRING = re.compile(r'r(#+)?"')
CHARACTER = re.compile(r"'(?:\\.|[^'\\\n])'")
WORD = re.compile(r"[A-Za-z_][A-Za-z_0-9]*")


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def canonical(value):
    return json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def rust_tokens(source):
    """Small lexical scanner, not a Rust parser. Strings/comments never become code."""
    tokens = []
    i = 0
    while i < len(source):
        start = i
        if source[i].isspace():
            i += 1
            continue
        if source.startswith("//", i):
            end = source.find("\n", i)
            i = len(source) if end < 0 else end
            continue
        if source.startswith("/*", i):
            depth = 1
            i += 2
            while i < len(source) and depth:
                if source.startswith("/*", i):
                    depth += 1
                    i += 2
                elif source.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            if depth:
                raise ValueError("unterminated Rust comment")
            continue
        raw = RAW_STRING.match(source, i)
        if raw:
            hashes = raw.group(1) or ""
            body = i + len(raw.group())
            end = source.find('"' + hashes, body)
            if end < 0:
                raise ValueError("unterminated raw string")
            i = end + 1 + len(hashes)
            tokens.append(("string", source[body:end], start, i))
            continue
        if source[i] == '"':
            i += 1
            value = ""
            while i < len(source) and source[i] != '"':
                if source[i] != "\\":
                    value += source[i]
                    i += 1
                    continue
                i += 1
                if source[i] == "\n" or source[i:i + 2] == "\r\n":
                    while i < len(source) and source[i].isspace():
                        i += 1
                elif source[i] == "x":
                    value += chr(int(source[i + 1:i + 3], 16))
                    i += 3
                elif source[i:i + 2] == "u{":
                    end = source.index("}", i)
                    value += chr(int(source[i + 2:end].replace("_", ""), 16))
                    i = end + 1
                else:
                    escapes = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
                    if source[i] not in escapes:
                        raise ValueError("unsupported Rust string escape")
                    value += escapes[source[i]]
                    i += 1
            if i == len(source):
                raise ValueError("unterminated Rust string")
            i += 1
            tokens.append(("string", value, start, i))
            continue
        char = CHARACTER.match(source, i)
        word = WORD.match(source, i)
        part = char.group() if char else word.group() if word else source[i]
        i += len(part)
        tokens.append(("code", part, start, i))
    return tokens


def production_tokens(source):
    tokens = rust_tokens(source)
    result = []
    i = 0
    while i < len(tokens):
        # Skip complete #[cfg(test)] items, including inline modules and helper functions.
        if [t[1] for t in tokens[i:i + 7]] == ["#", "[", "cfg", "(", "test", ")", "]"]:
            i += 7
            while i < len(tokens) and tokens[i][1] not in ("{", ";"):
                i += 1
            if i < len(tokens) and tokens[i][1] == "{":
                depth = 1
                i += 1
                while i < len(tokens) and depth:
                    if tokens[i][0] == "code":
                        depth += (tokens[i][1] == "{") - (tokens[i][1] == "}")
                    i += 1
            else:
                i += 1
        else:
            result.append(tokens[i])
            i += 1
    return result


def write_text(path, text):
    """Write UTF-8 with LF newlines on every host, so reports and fingerprints do not depend on the OS."""
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)


def scan_queries(root):
    queries = []
    for path in sorted((root / "crates/engine/src").rglob("*.rs")):
        if path.stem == "tests" or path.stem.endswith("_tests") or "tests" in path.parts:
            continue
        source = path.read_text(encoding="utf-8")
        tokens = production_tokens(source)
        constants = {}
        for i, token in enumerate(tokens):
            if token[1] == "const" and i + 1 < len(tokens):
                j = i + 2
                while j < len(tokens) and tokens[j][1] not in ("=", ";", "{"):
                    j += 1
                if j + 2 < len(tokens) and tokens[j][1] == "=" and tokens[j + 1][0] == "string" and tokens[j + 2][1] == ";":
                    constants[tokens[i + 1][1]] = tokens[j + 1][1]
        occurrences = {}
        for i in range(1, len(tokens) - 2):
            method = tokens[i][1]
            if tokens[i - 1][1] != "." or method not in METHODS or tokens[i + 1][1] != "(":
                continue
            j = i + 2
            stack = []
            while j < len(tokens):
                kind, word, _, _ = tokens[j]
                if kind == "code":
                    if not stack and word in (",", ")"):
                        break
                    if word in ("(", "[", "{"):
                        stack.append(word)
                    elif word in (")", "]", "}"):
                        if stack:
                            stack.pop()
                j += 1
            arg = tokens[i + 2:j]
            sql = None
            if len(arg) == 1:
                sql = arg[0][1] if arg[0][0] == "string" else constants.get(arg[0][1])
            start = tokens[i + 2][2]
            end = tokens[j][2] if j < len(tokens) else len(source)
            expression = source[start:end].strip()
            # Same-file string constants that a dynamic call names or interpolates (`{COLS}`). Their text is
            # part of the reader's reviewed identity, so an edit to such a constant is reviewed too.
            names = {t[1] for t in arg if t[0] == "code"} | {n for t in arg if t[0] == "string"
                                                            for n in re.findall(r"\{([A-Za-z_][A-Za-z_0-9]*)\}", t[1])}
            referenced = [constants[name] for name in sorted(names) if name in constants]
            key = f"{path.relative_to(root).as_posix()}::{method}::{digest(expression)[:16]}"
            occurrences[key] = occurrences.get(key, 0) + 1
            queries.append({"id": f"{key}::{occurrences[key]}", "path": path.relative_to(root).as_posix(),
                            "line": source.count("\n", 0, tokens[i][2]) + 1,
                            "method": method, "expression": expression, "sql": sql, "constants": referenced,
                            "status": "literal" if sql is not None and method != "execute_batch" else "unknown",
                            "reason": "Bounded lexical candidate scan; receiver types are not resolved."})
    return queries


def open_schema(schema, changes=()):
    db = sqlite3.connect(":memory:", cached_statements=0)
    try:
        shadows = set(schema["shadow_tables"])
        # Virtual tables create their own shadow tables. Replaying those would corrupt the fixture.
        objects = sorted(schema["objects"], key=lambda x: (x["type"] != "table", x["name"]))
        for obj in objects:
            if obj["sql"] and obj["name"] not in shadows:
                db.execute(obj["sql"])
        for sql in changes:
            db.executescript(sql)
    except Exception:
        db.close()
        raise
    return db


def prepare_dependencies(db, sql):
    """EXPLAIN compiles SQL without stepping the original statement or reading result rows."""
    dependencies = set()

    def authorizer(action, table, column, database, trigger):
        if action == sqlite3.SQLITE_READ:
            dependencies.add((database or "main", table, column or ""))
        return sqlite3.SQLITE_OK

    db.set_authorizer(authorizer)
    try:
        # CPython exposes no prepare-only API or bind count. EXPLAIN steps only VM opcodes.
        # The first failed binding still prepares. A fresh connection/cache-disabled caller is required.
        try:
            cursor = db.execute("EXPLAIN " + sql)
        except sqlite3.ProgrammingError as error:
            match = re.search(r"uses (\d+),", str(error))
            if not match:
                raise
            db.set_authorizer(allow_sql)
            db.set_authorizer(authorizer)
            cursor = db.execute("EXPLAIN " + sql, [None] * int(match.group(1)))
        cursor.close()
        return {"status": "prepared", "dependencies": [
            {"database": database, "table": table, "column": column,
             "scope": "table" if not column else "column"}
            for database, table, column in sorted(dependencies)],
            "executed_rows": "unknown", "mapped_rows": "unknown"}
    except sqlite3.Error as error:
        return {"status": "unknown", "error": str(error), "dependencies": [],
                "executed_rows": "unknown", "mapped_rows": "unknown"}
    finally:
        # Apple's Python 3.9 can install a rejecting callback for None. This disposable
        # connection has no caller-owned authorizer; an explicit allow callback also works there.
        db.set_authorizer(allow_sql)


def allow_sql(*_):
    return sqlite3.SQLITE_OK


def result_expressions(sql):
    """Bounded SELECT-list splitter for positional annotation checks, never a dependency oracle."""
    tokens = rust_tokens(sql)
    if not tokens or tokens[0][1].upper() != "SELECT":
        raise ValueError("mapping requires a simple SELECT list")
    expressions = []
    depth = 0
    start = tokens[0][3]
    for kind, word, begin, end in tokens[1:]:
        if kind == "code":
            if depth == 0 and (word == "," or word.upper() == "FROM"):
                expressions.append(sql[start:begin].strip())
                start = end
                if word.upper() == "FROM":
                    return expressions
            depth += (word == "(") - (word == ")")
    raise ValueError("unsupported SELECT mapping expression")


def source_files(root):
    paths = set()
    for directory in ("crates/converter/src", "crates/converter/records", "crates/shared/src"):
        paths.update(p for p in (root / directory).rglob("*") if p.suffix in (".rs", ".json"))
    paths.update(root / p for p in ("crates/converter/src/pipeline.rs", "crates/converter/src/cache.rs", "crates/converter/build.rs"))
    return {p.relative_to(root).as_posix(): digest(p.read_text(encoding="utf-8")) for p in sorted(paths) if p.is_file()}


def validate_refs(root, evidence):
    for ref in evidence:
        path = root / ref["path"]
        if not path.is_file() or ref["anchor"] not in path.read_text(encoding="utf-8"):
            raise ValueError(f"broken evidence reference: {ref}")


def store_paths(here):
    """The committed store files, sorted. Each store is its own file so store PRs never share a line."""
    directory = here / STORE_DIR
    if not directory.is_dir():
        raise ValueError(f"missing {STORE_DIR}/ directory; each external store needs its own JSON file")
    paths = sorted(directory.glob("*.json"))
    loaded = set(paths)
    for path in sorted(directory.iterdir()):
        if path.is_file() and path not in loaded:
            name = path.relative_to(here).as_posix()
            raise ValueError(f"unexpected file in store directory: {name}; only lowercase *.json store files are read")
    return paths


def load_stores(here):
    """Load one disposition per external store. Refuse a partial or mistyped file instead of dropping fields."""
    stores = {}
    sources = {}
    for path in store_paths(here):
        name = path.relative_to(here).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise ValueError(f"cannot read store file {name}: {error}") from None
        try:
            value = json.loads(text)
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid JSON in store file {name}: {error}") from None
        if not isinstance(value, dict):
            raise ValueError(f"store file is not an object: {name}")
        problems = []
        missing = sorted(set(STORE_FIELDS) - set(value))
        extra = sorted(set(value) - set(STORE_FIELDS))
        if missing:
            problems.append("missing " + ", ".join(missing))
        if extra:
            problems.append("unknown " + ", ".join(extra))
        if problems:
            raise ValueError(f"invalid store file {name}: " + "; ".join(problems))
        if value["format_version"] != 1:
            raise ValueError(f"unsupported store file version: {name}")
        key = value["store"]
        if not isinstance(key, str) or not key.strip():
            raise ValueError(f"missing store name: {name}")
        if key in stores:
            raise ValueError(f"duplicate store {key}: {sources[key]} and {name}")
        stores[key] = {field: value[field] for field in STORE_DISPOSITIONS}
        stores[key]["source"] = name
        sources[key] = name
    return dict(sorted(stores.items()))


def table_paths(here):
    """The committed table files, sorted. Each table is its own file so table PRs never share a line."""
    directory = here / TABLE_DIR
    if not directory.is_dir():
        raise ValueError(f"missing {TABLE_DIR}/ directory; each table needs its own JSON file")
    paths = sorted(directory.glob("*.json"))
    loaded = set(paths)
    for path in sorted(directory.iterdir()):
        if path.is_file() and path not in loaded:
            name = path.relative_to(here).as_posix()
            raise ValueError(f"unexpected file in table directory: {name}; only lowercase *.json table files are read")
    return paths


def load_tables(here):
    """Load one disposition per table. Refuse a partial or mistyped file instead of dropping fields."""
    tables = {}
    sources = {}
    for path in table_paths(here):
        name = path.relative_to(here).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise ValueError(f"cannot read table file {name}: {error}") from None
        try:
            value = json.loads(text)
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid JSON in table file {name}: {error}") from None
        if not isinstance(value, dict):
            raise ValueError(f"table file is not an object: {name}")
        problems = []
        missing = sorted(set(TABLE_FIELDS) - set(value))
        extra = sorted(set(value) - set(TABLE_FIELDS))
        if missing:
            problems.append("missing " + ", ".join(missing))
        if extra:
            problems.append("unknown " + ", ".join(extra))
        if problems:
            raise ValueError(f"invalid table file {name}: " + "; ".join(problems))
        if value["format_version"] != 1:
            raise ValueError(f"unsupported table file version: {name}")
        key = value["table"]
        if not isinstance(key, str) or not key.strip():
            raise ValueError(f"missing table name: {name}")
        if key in tables:
            raise ValueError(f"duplicate table {key}: {sources[key]} and {name}")
        tables[key] = {field: value[field] for field in TABLE_DISPOSITIONS}
        tables[key]["source"] = name
        sources[key] = name
    return dict(sorted(tables.items()))


def load_manual(here):
    """Load dispositions.json and join the per-store and per-table files.

    A legacy `stores` or `tables` block fails with the migration path, so no entry
    disappears when two pull requests edit the same shared object.
    """
    manual = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
    if "stores" in manual:
        raise ValueError(
            f"dispositions.json must not contain a stores block; each store has its own file under "
            f"tools/world_data_inventory/{STORE_DIR}/ with its own reviewed_at_commit. "
            "Move each entry to that directory and delete the block.")
    if "tables" in manual:
        raise ValueError(
            f"dispositions.json must not contain a tables block; each table has its own file under "
            f"tools/world_data_inventory/{TABLE_DIR}/. "
            "Move each entry to that directory and delete the block.")
    manual["stores"] = load_stores(here)
    manual["tables"] = load_tables(here)
    return manual


def validate_manual(root, manual):
    if manual.get("format_version", 1) != 1:
        raise ValueError("unsupported disposition version")
    for key, value in manual["fields"].items():
        if set(value["states"]) != set(STATES):
            raise ValueError(f"incomplete states: {key}")
        if any(value["states"][state] not in STATE_VALUES[state] for state in STATES):
            raise ValueError(f"invalid state: {key}")
        if not value["owner"] or not value["reason"]:
            raise ValueError(f"missing disposition owner/reason: {key}")
        validate_refs(root, value["evidence"])
        if value["states"]["semantic_use"] != "unknown" and not value.get("sink"):
            raise ValueError(f"semantic claim needs a sink: {key}")
        validate_refs(root, value.get("sink", []))
        validate_refs(root, value.get("tests", []))
    for key, value in manual["stores"].items():
        source = value.get("source", "unknown store file")
        if not isinstance(value.get("reviewed_at_commit"), str) or not COMMIT.fullmatch(value["reviewed_at_commit"]):
            raise ValueError(f"invalid store reviewed_at_commit: {key} ({source})")
        if not value.get("owner") or not value.get("reason"):
            raise ValueError(f"missing store owner/reason: {key} ({source})")
        if set(value["states"]) != set(STATES) or any(value["states"][state] not in STATE_VALUES[state] for state in STATES):
            raise ValueError(f"external store needs independent valid states: {key} ({source})")
        try:
            validate_refs(root, value["evidence"])
        except ValueError as error:
            raise ValueError(f"store {key} ({source}): {error}") from None
    for value in manual["legacy_shapes"]:
        validate_refs(root, value["evidence"])
    for key, value in manual["tables"].items():
        source = value.get("source", "unknown table file")
        if value["classification"] not in {"deferred", "provenance"} or not value["owner"] or not value["reason"]:
            raise ValueError(f"invalid table disposition: {key} ({source})")


def checked_at(root):
    """The commit under test, as a label only. Anchors are checked against the checked-out tree."""
    try:
        done = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True,
                              text=True, encoding="utf-8", check=True)
    except (OSError, subprocess.CalledProcessError):
        return "unknown"
    return done.stdout.strip() or "unknown"


def build(root, manual, extraction=None, queries=None):
    validate_manual(root, manual)
    queries = scan_queries(root) if queries is None else queries
    result = {"format_version": 1, "checked_at": checked_at(root),
              "schema_status": "emitted" if extraction else "not_extracted",
              "schemas": {}, "queries": queries, "variants": [], "fields": manual["fields"],
              "stores": {key: {field: value[field] for field in STORE_DISPOSITIONS}
                         for key, value in manual["stores"].items()},
              "limitations": manual["limitations"],
              "legacy_shapes": manual["legacy_shapes"],
              "denominators": {"rusqlite_candidates": len(queries), "emitted_schema_columns": 0 if extraction else None,
                               "manually_traced_fields": len(manual["fields"]),
                               "named_external_stores": len(manual["stores"])}}
    if not extraction:
        return result
    if extraction["format_version"] != 1:
        raise ValueError("unsupported extraction version")
    result["identity"] = {k: v for k, v in extraction.items() if k not in ("schemas", "variants")}
    result["identity"]["python_sqlite_version"] = sqlite3.sqlite_version
    for name, schema in sorted(extraction["schemas"].items()):
        with closing(open_schema(schema)) as db:
            tables = []
            for obj in schema["objects"]:
                if obj["type"] != "table" or obj["name"] in schema["shadow_tables"]:
                    continue
                table = obj["name"]
                if table not in manual["tables"]:
                    raise ValueError(f"unclassified emitted table: {table}")
                columns = []
                table_info = [list(row) for row in db.execute("SELECT * FROM pragma_table_info(?)", (table,))]
                if "table_info" in schema and table_info != schema["table_info"][table]:
                    raise ValueError(f"replayed SQLite shape differs from emitted metadata: {name}.{table}")
                for cid, column, kind, required, default, pk in table_info:
                    disposition = manual["fields"].get(f"{table}.{column}")
                    if disposition is None:
                        disposition = (manual["blob_default"] if "BLOB" in kind.upper()
                                       else {field: manual["tables"][table][field] for field in TABLE_DISPOSITIONS})
                    columns.append({"ordinal": cid, "name": column, "type": kind, "not_null": bool(required),
                                    "default": default, "primary_key": pk,
                                    "disposition": disposition,
                                    "population": "not_observed"})
                tables.append({"name": table, "columns": columns, "population": "not_observed"})
            result["schemas"][name] = sorted(tables, key=lambda x: x["name"])
            result["denominators"]["emitted_schema_columns"] += sum(len(t["columns"]) for t in tables)
            for query in queries:
                query.setdefault("preparations", {})[name] = (
                    prepare_dependencies(db, query["sql"]) if query["status"] == "literal"
                    else {"status": "unknown", "reason": "dynamic, batch, or unresolved receiver/argument"})
    emitted_fields = {f"{table['name']}.{column['name']}" for tables in result["schemas"].values()
                      for table in tables for column in table["columns"]}
    if set(manual["fields"]) - emitted_fields:
        raise ValueError(f"manual fields absent from emitted schemas: {sorted(set(manual['fields']) - emitted_fields)}")
    for variant in extraction["variants"]:
        entry = dict(variant)
        if variant["result"]["status"] == "prepared":
            with closing(open_schema(extraction["schemas"][variant["schema"]], variant["changes"])) as db:
                entry["preparations"] = {key: prepare_dependencies(db, sql)
                                         for key, sql in sorted(variant["result"]["statements"].items())}
                if any(p["status"] != "prepared" for p in entry["preparations"].values()):
                    raise ValueError(f"composed query did not prepare: {entry}")
        result["variants"].append(entry)
    dependencies = {}
    table_reads = {}
    def record_dependencies(shape, query_id, preparation):
        for dependency in preparation.get("dependencies", []):
            target = table_reads if dependency["scope"] == "table" else dependencies
            key = (shape, dependency["table"]) if dependency["scope"] == "table" else (shape, dependency["table"], dependency["column"])
            target.setdefault(key, set()).add(query_id)
    for query in queries:
        for shape, preparation in query["preparations"].items():
            record_dependencies(shape, f"{query['id']}@{shape}", preparation)
    for variant in result["variants"]:
        for statement, preparation in variant.get("preparations", {}).items():
            record_dependencies(variant["schema"], f"{variant['id']}::{statement}", preparation)
    for shape, tables in result["schemas"].items():
        for table in tables:
            table["prepare_table_reads"] = sorted(table_reads.get((shape, table["name"]), set()))
            for column in table["columns"]:
                column["prepare_dependencies"] = sorted(dependencies.get((shape, table["name"], column["name"]), set()))
                column["executed_rows"] = "unknown"
                column["mapped_rows"] = "unknown"
    validate_mappings(result)
    return result


def validate_mappings(result):
    for key, field in result["fields"].items():
        if "mapping" not in field:
            continue
        table, column = key.split(".", 1)
        mapping = field["mapping"]
        if mapping["query"] == "reference":
            selected = [v for v in result["variants"] if v["id"] == "selectors-63"]
            candidates = [(v["preparations"]["interior"], v["result"]["statements"]["interior"]) for v in selected]
        else:
            candidates = [(q["preparations"]["inhouse"], q["sql"]) for q in result["queries"]
                          if q["sql"] and mapping["query"] in q["sql"] and "preparations" in q]
        def matches(preparation, sql):
            expressions = result_expressions(sql)
            return (0 <= mapping["ordinal"] < len(expressions)
                    and expressions[mapping["ordinal"]] == mapping["expression"]
                    and any(d["table"] == table and d["column"] == column for d in preparation["dependencies"]))

        if not candidates or not any(
                matches(p, sql)
                for p, sql in candidates if p["status"] == "prepared"):
            raise ValueError(f"mapping has no prepared dependency: {key}")


def report(result):
    lines = ["# World-data inventory", "", f"Schema status: **{result['schema_status']}**. Checked at: `{result['checked_at']}`.", "",
             "Prepare dependencies are not executed rows, mapped rows, semantic use, population, or retail parity.", "",
             "## Denominators", "", *[f"- {key}: {'unknown' if value is None else value}" for key, value in result["denominators"].items()], "",
             "## Traced examples", "", "| Field | Decoded | Preserved | Projected | Query-dependent | Mapped | Semantic use | Role / reason |",
             "|---|---|---|---|---|---|---|---|"]
    for key, field in sorted(result["fields"].items()):
        lines.append("| " + " | ".join([f"`{key}`", *[field["states"][s] for s in STATES], ", ".join(field["roles"]) + ": " + field["reason"]]) + " |")
    if "identity" in result:
        identity = result["identity"]
        lines += ["", "## Extraction identity", ""]
        for key in ("source_commit", "world_schema", "producer_schema", "cell_cache_version", "rusqlite_version", "python_sqlite_version", "selector_denominator"):
            if key in identity:
                lines.append(f"- {key}: `{identity[key]}`")
        lines.append("- Full reader identities and per-query dependencies: inventory.json.")
    lines += ["", "## External stores", ""]
    for key, value in sorted(result["stores"].items()):
        lines += [f"- `{key}`: reviewed at `{value['reviewed_at_commit']}`. {value['reason']}"]
    lines += ["", "## Evidence anchors", ""]
    for key, value in sorted(result["fields"].items()):
        refs = value["evidence"] + value.get("sink", []) + value.get("tests", [])
        paths = sorted({ref["path"] for ref in refs})
        lines.append(f"- `{key}`: " + ", ".join(f"[{path}](../../{path})" for path in paths) + ". Exact anchors in inventory.json.")
    lines += ["", "## Retained legacy dispositions", ""]
    for value in result["legacy_shapes"]:
        lines.append(f"- `{value['key']}`: {value['reason']}")
    for name, tables in sorted(result["schemas"].items()):
        lines += ["", f"## Emitted schema: {name}", "", "| Table.column | Type | Disposition | Prepare dependencies | Population |", "|---|---|---|---|---|"]
        for table in tables:
            for col in table["columns"]:
                disposition = col["disposition"]
                lines.append(f"| `{table['name']}.{col['name']}` | {col['type']} | {disposition.get('classification', 'traced')} | {len(col['prepare_dependencies'])} | not_observed |")
            if table["prepare_table_reads"]:
                lines.append(f"| `{table['name']}.*` (count/table scope) | | | {len(table['prepare_table_reads'])} | not_observed |")
    lines += ["", "## Query extraction", "", "| Candidate | Status |", "|---|---|"]
    for query in result["queries"]:
        statuses = ", ".join(f"{name}: {p['status']}" for name, p in sorted(query.get("preparations", {}).items()))
        lines.append(f"| `{query['path']}:{query['line']}` {query['method']} | {statuses or query['status']} |")
    if result["variants"]:
        lines += ["", "## Composed ReferenceQuery variants", "", "| Variant | Builder result |", "|---|---|"]
        for variant in result["variants"]:
            lines.append(f"| `{variant['id']}` | {variant['result']['status']} |")
    lines += ["", "## Limits", "", *[f"- {limit}" for limit in result["limitations"]], ""]
    return "\n".join(lines)


def source_guard(root, queries, manual):
    # Conservative file-level projection guard. Changes require review even when DDL is unchanged.
    evidence_paths = {ref["path"] for field in manual["fields"].values()
                      for group in ("evidence", "sink", "tests") for ref in field.get(group, [])}
    evidence_paths.update(ref["path"] for store in manual["stores"].values() for ref in store["evidence"])
    store_dir = root / "tools/world_data_inventory" / STORE_DIR
    table_dir = root / "tools/world_data_inventory" / TABLE_DIR
    tooling = ["tools/world_data_inventory/inventory.py", "tools/world_data_inventory/dispositions.json",
               "tools/world_data_inventory/dispositions.schema.json",
               "tools/world_data_inventory/store.schema.json",
               "tools/world_data_inventory/table.schema.json",
               "crates/engine/src/world/inventory_tests.rs"]
    tooling += sorted(path.relative_to(root).as_posix() for path in store_dir.glob("*.json")) if store_dir.is_dir() else []
    tooling += sorted(path.relative_to(root).as_posix() for path in table_dir.glob("*.json")) if table_dir.is_dir() else []
    return {"projection_sources": source_files(root), "query_candidates": [q["id"] for q in queries],
            "reader_sources": {p: digest((root / p).read_text(encoding="utf-8"))
                               for p in sorted({q["path"] for q in queries} | evidence_paths)},
            "tooling_sources": {p: digest((root / p).read_text(encoding="utf-8")) for p in tooling if (root / p).is_file()}}


def reader_surface(queries):
    """Stable identity of each record reader: the call site key plus a hash of the SQL it runs.

    The key holds the file path, method, a hash of the call expression and an occurrence number. It holds no
    line number and no hash of the whole file, so an unrelated edit leaves it unchanged. The SQL hash catches a
    changed constant or literal. For a dynamic query, the hash covers the same-file string constants it names or interpolates.
    `None` marks a dynamic query with no such constant: its SQL is not reviewed, only its call expression."""
    def identity(query):
        if query["sql"] is not None:
            return digest(query["sql"])[:16]
        return digest("\0".join(query["constants"]))[:16] if query["constants"] else None
    return {q["id"]: identity(q) for q in queries}


def emitted_columns(extraction):
    """Every `table.column` that any extracted schema shape declares, as a sorted list."""
    names = set()
    for schema in extraction["schemas"].values():
        with closing(open_schema(schema)) as db:
            for obj in schema["objects"]:
                if obj["type"] == "table" and obj["name"] not in schema["shadow_tables"]:
                    names.update(f"{obj['name']}.{row[1]}" for row in
                                 db.execute("SELECT * FROM pragma_table_info(?)", (obj["name"],)))
    return sorted(names)


def surface_text(surface):
    """One entry per line, so a review diff shows exactly which reader or column changed."""
    lines = ['{', '  "format_version": 1,', '  "readers": {']
    lines += [f'    {json.dumps(key)}: {json.dumps(value)}' + ("," if i + 1 < len(surface["readers"]) else "")
              for i, (key, value) in enumerate(sorted(surface["readers"].items()))]
    lines += ['  },', '  "columns": [']
    lines += [f'    {json.dumps(name)}' + ("," if i + 1 < len(surface["columns"]) else "")
              for i, name in enumerate(sorted(surface["columns"]))]
    lines += ['  ]', '}', '']
    return "\n".join(lines)


def load_surface(here):
    path = here / SURFACE_NAME
    if not path.is_file():
        raise ValueError(f"missing {SURFACE_NAME}; review the readers, then run inventory.py --review-sources")
    surface = json.loads(path.read_text(encoding="utf-8"))
    if surface.get("format_version") != 1:
        raise ValueError("unsupported reviewed-surface version")
    return surface


def surface_difference(kind, reviewed, current):
    added = sorted(set(current) - set(reviewed))
    removed = sorted(set(reviewed) - set(current))
    changed = sorted(key for key in set(current) & set(reviewed)
                     if isinstance(reviewed, dict) and reviewed[key] != current[key])
    lines = [f"  new {kind} without a reviewed disposition: {key}" for key in added]
    lines += [f"  changed {kind} (its SQL changed): {key}" for key in changed]
    lines += [f"  reviewed {kind} that no longer exists: {key}" for key in removed]
    return lines


def tracked_generated_files(root):
    """Generated files that git tracks. Returns nothing outside a git checkout."""
    try:
        done = subprocess.run(["git", "ls-files", "--", *GENERATED_PATHS], cwd=root, capture_output=True,
                              text=True, encoding="utf-8", check=True)
    except (OSError, subprocess.CalledProcessError):
        return []
    return done.stdout.split()


def run(argv, root=ROOT, here=HERE):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--extraction", type=Path, help="JSON emitted by world_data_inventory_extraction")
    parser.add_argument("--output", type=Path, default=root / "docs/data-inventory",
                        help="where to write the generated report and JSON (default is gitignored)")
    parser.add_argument("--check", action="store_true",
                        help="kept for existing commands: the committed reviewed surface is always enforced")
    parser.add_argument("--review-sources", action="store_true",
                        help=f"after review, rewrite the committed {SURFACE_NAME}; add --extraction to rewrite its columns too")
    parser.add_argument("--source-identity", action="store_true", help="print the source fingerprint for executable extraction")
    args = parser.parse_args(argv)
    if args.source_identity and (args.extraction or args.review_sources or args.check):
        parser.error("--source-identity cannot be combined with generation/check options")
    if args.review_sources and args.check:
        parser.error("--check cannot update the reviewed surface")
    manual = load_manual(here)
    extraction = json.loads(args.extraction.read_text(encoding="utf-8")) if args.extraction else None
    validate_manual(root, manual)
    queries = scan_queries(root)
    # Derived. Never compared with a committed copy, so no committed file holds a source hash.
    guard = source_guard(root, queries, manual)
    fingerprint = digest(canonical(guard))
    if args.source_identity:
        print(fingerprint)
        return
    tracked = tracked_generated_files(root)
    if tracked:
        raise ValueError("generated inventory files are committed: " + ", ".join(tracked)
                         + ". They are derived in CI and gitignored. Remove them with: " + MIGRATION_RECIPE)
    if extraction and not args.review_sources and extraction.get("reviewed_source_guard_sha256") != fingerprint:
        raise ValueError("extraction does not match the reviewed source fingerprint")
    surface_path = here / SURFACE_NAME
    if args.review_sources:
        if extraction:
            # Reviewing a column never replaces classifying its table under tables/.
            tables = sorted({name.split(".", 1)[0] for name in emitted_columns(extraction)} - set(manual["tables"]))
            if tables:
                raise ValueError("unclassified emitted table: " + ", ".join(tables)
                                 + ". Add a disposition under tools/world_data_inventory/tables/ first")
        previous = json.loads(surface_path.read_text(encoding="utf-8")) if surface_path.is_file() else {}
        columns = emitted_columns(extraction) if extraction else previous.get("columns", [])
        write_text(surface_path, surface_text({"readers": reader_surface(queries), "columns": columns}))
    surface = load_surface(here)
    problems = surface_difference("reader", surface["readers"], reader_surface(queries))
    if extraction:
        problems += surface_difference("column", surface["columns"], emitted_columns(extraction))
    if problems:
        raise ValueError("unreviewed change to the record readers or columns. Review the dispositions in "
                         "tables/ and dispositions.json, then run inventory.py --review-sources"
                         + (" --extraction <extraction.json>" if extraction else "")
                         + " and commit " + SURFACE_NAME + ":\n" + "\n".join(problems))
    result = build(root, manual, extraction, queries)
    result["reviewed_source_guard_sha256"] = fingerprint
    result["source_guard"] = guard
    outputs = {"inventory.json": canonical(result), "report.md": report(result)}
    # Always regenerate. A missing file is created and a stale one is overwritten, so no step trusts a copy on disk.
    args.output.mkdir(parents=True, exist_ok=True)
    for name, content in outputs.items():
        write_text(args.output / name, content)
    print(f"inventory: {result['schema_status']}; {len(result['queries'])} candidates; {len(result['variants'])} composed variants; "
          f"regenerated {', '.join(outputs)} in {args.output}")


def main():
    try:
        run(sys.argv[1:])
    except ValueError as error:
        sys.exit(f"error: {error}")


if __name__ == "__main__":
    main()
