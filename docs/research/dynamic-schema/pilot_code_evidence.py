#!/usr/bin/env python3
"""Reproduce the Mudcrab source hashes and selected P0 pilot anchors.

This is a bounded provenance check for the REFR/CELL/STAT field, identity,
SQL and cache discussion in native-field-ledger.md. It reports only the
named source anchors below; it does not discover every field read or establish
catalog completeness.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path


PILOT_FUNCTIONS = {
    "crates/converter/src/esm/exporter.rs": {
        "create_tables": [
            "CREATE TABLE IF NOT EXISTS records (",
            "CREATE VIRTUAL TABLE IF NOT EXISTS exterior_spatial USING rtree(",
            "CREATE TABLE IF NOT EXISTS formid_map (",
            "CREATE TABLE IF NOT EXISTS conversion_cache (",
        ],
        "export_records": [
            "let tx = conn.unchecked_transaction()?;",
            "let blob = serialize_subrecords(&record.subrecords);",
            'INSERT OR REPLACE INTO records(form_id, record_type, cell_id, worldspace_id, load_order, data)',
            '"STAT" | "MSTT" | "FURN" | "TREE"',
            'order.identity(form_id).map_err',
        ],
        "insert_reference": [
            'let transform = view.get_f32_slice(b"DATA").unwrap_or_default();',
            "let scale = view",
            'let base_form_id = view.get_form_id(b"NAME").unwrap_or(0);',
            'let blob = serialize_subrecords(subs);',
            "let radius_override = view",
            'INSERT OR REPLACE INTO \\"references\\"(id, cell_id, worldspace_id, base_form_id',
        ],
    },
    "crates/converter/src/esm/extractors.rs": {
        "extract_cell_info": [
            '"XCLC" if data.len() >= 8',
            '"EDID" =>',
        ],
        "serialize_subrecords": [
            "let record_data = ArchivedRecordData {",
            "subrecords: subs",
            "data: data.clone(),",
        ],
    },
    "crates/converter/src/esm/cell_cache.rs": {
        "write_cell_cache": [
            "let water_by_cell = water_by_cell(records);",
            "let cell_id = record.cell_form_id.unwrap_or(record.form_id);",
            "cells.sort_unstable_by_key(|cell| cell.cell_id);",
            "validate_cell_cache(path)?;",
        ],
        "water_by_cell": [
            '.find(b"DNAM")',
            '.get_form_id(b"NAM2")',
            '.find(b"XCLW")',
            '.get_form_id(b"XCWT")',
        ],
    },
    "crates/converter/src/esm/mod.rs": {
        "convert_plugins": [
            "Self::convert_plugins_with_records(plugin_paths, db_path).map(|_| ())",
        ],
        "convert_plugins_with_records": [
            "let order = load_order::LoadOrder::read(plugin_paths)?;",
            "let master = Self::merge_plugins_with_load_order(plugin_paths, &order)?;",
            "let conn = Connection::open(db_path)?;",
            "let checksum = Sha256::digest(std::fs::read(path)?);",
            "export_to_db_with_load_order(&conn, &master, &order)?;",
        ],
        "merge_plugins_with_load_order": [
            "for (priority, path) in plugin_paths.iter().enumerate() {",
            "remap_record_form_ids(",
            "let Some(canonical) =",
            "if record.is_deleted() {",
            "merged.remove(&record.form_id);",
            "merged.insert(record.form_id, record);",
        ],
        "resolve": [
            ".map(|name| name.to_ascii_lowercase());",
            "let known = self.canonical_ids.get(&editor_id).copied();",
            "let canonical = known.unwrap_or(source_id);",
            "self.aliases.insert(source_id, canonical);",
            "self.canonical_ids.insert(editor_id, canonical);",
        ],
        "remap_record_form_ids": [
            'b"GRAS" | b"LTEX" | b"TXST" | b"LAND" | b"CELL" | b"WRLD"',
            "return Ok(0xFE00_0000 | (index << 12) | (form_id & 0xFFF));",
            "Ok((index << 24) | (form_id & 0x00FF_FFFF))",
            "record.form_id = remap(record.form_id)?;",
            'if tag.as_slice() == b"VMAD" {',
        ],
    },
    "crates/converter/src/esm/load_order.rs": {
        "read": [
            'ensure!(!result.names.contains(&name), "duplicate plugin {name}");',
            '"{name}: master {master} must precede its dependent plugin"',
            "let light = name.ends_with(\".esl\") || metadata.flags & 0x200 != 0;",
            "let slot = result.light.len() as u32;",
            "let slot = result.normal.len() as u32;",
        ],
        "identity": [
            "if form_id >> 24 == 0xfe {",
            "let plugin = slots",
            "Ok(StableId { plugin, local_id })",
        ],
    },
}


GIT_TIMEOUT_SECONDS = 30


class SourceInspectionTimeout(RuntimeError):
    """A bounded Git read did not finish before its deadline."""


def git(repo: Path, *args: str, context: str) -> str:
    try:
        return subprocess.check_output(
            ["git", "-C", str(repo), *args], text=True,
            timeout=GIT_TIMEOUT_SECONDS,
        ).strip()
    except subprocess.TimeoutExpired as exc:
        raise SourceInspectionTimeout(
            f"Git timed out after {GIT_TIMEOUT_SECONDS}s while {context}"
        ) from exc


def file_at(repo: Path, revision: str, path: str) -> str:
    context = f"reading {path} at inspected revision {revision}"
    try:
        raw = subprocess.check_output(
            ["git", "-C", str(repo), "show", f"{revision}:{path}"],
            timeout=GIT_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as exc:
        raise SourceInspectionTimeout(
            f"Git timed out after {GIT_TIMEOUT_SECONDS}s while {context}"
        ) from exc
    return raw.decode("utf-8")


def _char_literal_end(text: str, start: int) -> int | None:
    """Return the end of a Rust character literal, or None for a lifetime."""
    if start >= len(text) or text[start] != "'":
        return None
    index = start + 1
    if index >= len(text) or text[index] in "\r\n":
        return None

    if text[index] == "\\":
        index += 1
        if index >= len(text) or text[index] in "\r\n":
            return None
        escape = text[index]
        if escape == "u" and index + 1 < len(text) and text[index + 1] == "{":
            close = text.find("}", index + 2)
            if close < 0:
                return None
            index = close + 1
        elif escape == "x":
            index += 3
        else:
            index += 1
    else:
        index += 1

    if index < len(text) and text[index] == "'":
        return index + 1
    return None


def function_span(text: str, name: str) -> tuple[int, int]:
    lines = text.splitlines(keepends=True)
    declaration = re.compile(
        rf"^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+{re.escape(name)}"
        r"\s*(?:<[^>\n]*>)?\s*\("
    )
    start_lines = [i for i, line in enumerate(lines) if declaration.search(line)]
    if len(start_lines) != 1:
        raise ValueError(f"expected one function declaration for {name}; found {start_lines}")
    start_line = start_lines[0]
    offset = sum(len(line) for line in lines[:start_line])
    body_start = text.find("{", offset)
    if body_start < 0:
        raise ValueError(f"function {name} has no body")

    depth = 0
    state = "normal"
    escaped = False
    index = body_start
    while index < len(text):
        char = text[index]
        next_char = text[index + 1] if index + 1 < len(text) else ""
        if state == "line_comment":
            if char == "\n":
                state = "normal"
        elif state == "block_comment":
            if char == "*" and next_char == "/":
                state = "normal"
                index += 1
        elif state == "string":
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif (state == "string" and char == '"') or (state == "char" and char == "'"):
                state = "normal"
        elif char == "/" and next_char == "/":
            state = "line_comment"
            index += 1
        elif char == "/" and next_char == "*":
            state = "block_comment"
            index += 1
        elif char == '"':
            state = "string"
        elif char == "'":
            char_end = _char_literal_end(text, index)
            if char_end is not None:
                index = char_end - 1
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return offset, index + 1
        index += 1
    raise ValueError(f"unterminated body for function {name}")


def line_for(text: str, anchor: str) -> int:
    matches = [index for index, line in enumerate(text.splitlines(), 1) if anchor in line]
    if len(matches) != 1:
        raise ValueError(f"expected one source line for {anchor!r}; found {matches}")
    return matches[0]


def describe_revision(repo: Path, revision: str) -> dict:
    commit = git(
        repo, "rev-parse", f"{revision}^{{commit}}",
        context=f"resolving inspected revision {revision}",
    )
    files = {}
    for path, functions in PILOT_FUNCTIONS.items():
        contents = file_at(repo, commit, path)
        function_rows = {}
        for name, anchors in functions.items():
            start, end = function_span(contents, name)
            block = contents[start:end]
            line_offset = contents[:start].count("\n")
            function_rows[name] = {
                "line_start": line_offset + 1,
                "line_end": contents[:end].count("\n") + 1,
                "sha256": hashlib.sha256(block.encode("utf-8")).hexdigest(),
                "selected_anchors": {
                    anchor: line_offset + line_for(block, anchor) for anchor in anchors
                },
            }
        files[path] = {
            "git_blob": git(
                repo, "rev-parse", f"{commit}:{path}",
                context=f"resolving source blob at inspected revision {commit}, path {path}",
            ),
            "sha256": hashlib.sha256(contents.encode("utf-8")).hexdigest(),
            "functions": function_rows,
        }
    return {"commit": commit, "files": files}


def function_comparison(base: dict, current: dict) -> dict:
    return {
        path: {
            name: base["files"][path]["functions"][name]["sha256"]
            == current["files"][path]["functions"][name]["sha256"]
            for name in functions
        }
        for path, functions in PILOT_FUNCTIONS.items()
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--base", required=True, help="Mudcrab base revision")
    parser.add_argument("--current", required=True, help="comparison revision")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--check",
        action="store_true",
        help="compare the generated report with --output without rewriting it",
    )
    args = parser.parse_args(argv)

    try:
        base = describe_revision(args.repo, args.base)
        current = describe_revision(args.repo, args.current)
    except SourceInspectionTimeout as exc:
        print(f"pilot code evidence failed: {exc}", file=sys.stderr)
        return 2
    output = {
        "artifact_kind": "bounded_P0_converter_projection_identity_sql_cache_source_anchor_check",
        "scope": "Named pilot anchors and source hashes only; no exhaustive field scan or catalog claim.",
        "base": base,
        "current": current,
        "same_function_bodies": function_comparison(base, current),
        "same_file_bytes": {
            path: base["files"][path]["sha256"] == current["files"][path]["sha256"]
            for path in PILOT_FUNCTIONS
        },
    }
    if args.check:
        try:
            existing = json.loads(args.output.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError, RecursionError) as exc:
            print(
                f"pilot code evidence failed: cannot read valid JSON from {args.output}: {exc}",
                file=sys.stderr,
            )
            return 2
        if existing != output:
            print(f"pilot code evidence failed: source evidence differs from {args.output}", file=sys.stderr)
            return 2
        print(json.dumps({"checked": str(args.output.resolve()), "matches": True}, indent=2))
        return 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(args.output.resolve()), "same_file_bytes": output["same_file_bytes"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
