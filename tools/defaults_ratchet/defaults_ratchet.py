#!/usr/bin/env python3
"""Ratchet invented defaults on retail Skyrim data and world-database boundaries.

This tool enforces two rules on production Rust code in configured modules:
1. SQLite boundary rule on ``db_write_modules``:
   - ``NOT NULL DEFAULT <non-null>`` in SQL DDL / migrations
   - ``COALESCE(..., <literal>)`` in SQL queries (excluding aggregate summaries
     such as ``COALESCE(MAX(id), 0)``)
   - Fallback struct writes (or fallback ``params![...]`` writes) setting
     non-counter fields to ``0``, ``0.0``, ``""``, ``"male"``, or ``"female"``
     when a source value is ``None`` / unresolved.
2. Retail-data module rule on ``retail_data_modules``:
   - ``.unwrap_or(<literal|CONST>)`` and ``.unwrap_or_else(|| <literal|CONST>)``
   - ``.map_or(<literal|CONST>, ...)``
   - Fallback returns or uses of ``SKYRIM_CLEAR_*``, ``DEFAULT_*``, or
     ``FALLBACK_*`` constants (excluding ``use`` imports, ``const``/``static``
     declarations, and ``==``/``!=`` comparisons).

A site is exempt when the line itself, its contiguous ``//`` comment block
immediately above, or the owning statement's first line / contiguous comment
block directly above carries either:
  // retail-default: <reference>
  // STAND-IN(#<issue>): <reason>   (validated by scripts/check_retail_claims.py)

Usage:
  python3 -B tools/defaults_ratchet/defaults_ratchet.py --check
  python3 -B tools/defaults_ratchet/defaults_ratchet.py --check --base origin/main
  python3 -B tools/defaults_ratchet/defaults_ratchet.py --audit-tree
  python3 -B tools/defaults_ratchet/defaults_ratchet.py --update-baseline

Exit codes: 0 = pass, 1 = ratchet or diff finding, 2 = config/git/baseline error.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import re
import subprocess
import sys
from bisect import bisect_right
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

DEFAULT_CONFIG = Path("tools/defaults_ratchet/config.json")
DEFAULT_BASELINE = Path("tools/defaults_ratchet/baseline.json")

HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")
TEST_PATH_RE = re.compile(r"(?:^|/)(?:tests|benches|examples)/")

# Exemption comments: `// retail-default: <reference>` or `STAND-IN(#N): <reason>`.
RETAIL_DEFAULT_RE = re.compile(r"//[!/]?\s*retail-default:\s*(?P<ref>\S.*)")

# Rule (a): SQLite boundary patterns.
SQL_NOT_NULL_DEFAULT_RE = re.compile(
    r"\bNOT\s+NULL\s+DEFAULT\s+(?!\(?\s*NULL\s*\)?\b)"
    r"(?:\(\s*(?:'(?:''|[^'])*'|\"[^\"]*\"|[-+]?(?:\d[\d_]*(?:\.\d[\d_]*)?|\.\d[\d_]*)|[A-Za-z_][A-Za-z0-9_]*)\s*\)"
    r"|'(?:''|[^'])*'|\"[^\"]*\"|[-+]?(?:\d[\d_]*(?:\.\d[\d_]*)?|\.\d[\d_]*)|[A-Za-z_][A-Za-z0-9_]*)",
    re.IGNORECASE,
)
SQL_COALESCE_RE = re.compile(r"\bCOALESCE\s*\(", re.IGNORECASE)
SQL_AGGREGATE_FIRST_ARG_RE = re.compile(
    r"^\s*(?P<fn>MAX|MIN|SUM|COUNT|TOTAL)\s*\(", re.IGNORECASE
)
SQL_LITERAL_RE = re.compile(
    r"^(?:[-+]?(?:\d+(?:\.\d*)?|\.\d+)|'(?:''|[^'])*'|TRUE|FALSE)$", re.IGNORECASE
)

# Rule (a): fallback struct writes with 0 / "" / "male" / "female".
STRUCT_OPEN_RE = re.compile(r"(?:[A-Za-z_][A-Za-z0-9_]*::)*([A-Za-z_][A-Za-z0-9_]*)\s*\{")
NON_STRUCT_KEYWORDS = frozenset(
    {
        "if",
        "else",
        "match",
        "for",
        "while",
        "loop",
        "fn",
        "struct",
        "enum",
        "union",
        "impl",
        "mod",
        "trait",
        "pub",
        "return",
        "unsafe",
        "async",
        "move",
        "const",
        "static",
        "type",
        "use",
        "let",
        "where",
        "in",
        "as",
    }
)
FALLBACK_FN_NAME_RE = re.compile(r"(?:unresolved|fallback|default|missing)", re.IGNORECASE)
FALLBACK_BRANCH_RE = re.compile(
    r"(?:\bNone\s*=>|\bErr\s*\([^)]*\)\s*=>|\belse\s*\{|\bunwrap_or_else\s*\()"
)
UNRESOLVED_MARKER_RE = re.compile(
    r'\b(?:resolution|status|state)\s*:\s*"(?:unresolved|fallback|default|missing)"'
)
STRUCT_FIELD_RE = re.compile(r"\b([a-z_][a-z0-9_]*)\s*:\s*([^,\n}]+)")
FALLBACK_LITERAL_VALUE_RE = re.compile(
    r'^(?:0(?:\.0+)?(?:[ui](?:8|16|32|64|128|size)|f(?:32|64))?|""|"male"|"female")$'
)
COUNTER_FIELDS = frozenset(
    {
        "count",
        "total",
        "skipped",
        "omitted",
        "errors",
        "warnings",
        "matched",
        "unresolved_count",
    }
)
PARAMS_MACRO_RE = re.compile(r"\b(?:rusqlite::)?params!\s*\[")

# Rule (b): retail-data module patterns.
UNWRAP_OR_RE = re.compile(r"\.\s*unwrap_or\s*\(")
UNWRAP_OR_ELSE_RE = re.compile(r"\.\s*unwrap_or_else\s*\(")
MAP_OR_RE = re.compile(r"\.\s*map_or\s*\(")
FALLBACK_CONST_RE = re.compile(
    r"\b(?:SKYRIM_CLEAR_[A-Z0-9_]+|DEFAULT_[A-Z0-9_]+|FALLBACK_[A-Z0-9_]+)\b"
)
CLOSURE_EMPTY_ARGS_RE = re.compile(r"^\|\s*\|\s*(?P<body>[\s\S]+)$")

NUMERIC_LIT_RE = re.compile(
    r"^-?(?:0b[01_]+(?:[ui](?:8|16|32|64|128|size))?"
    r"|0o[0-7_]+(?:[ui](?:8|16|32|64|128|size))?"
    r"|0x[0-9a-fA-F_]+(?:[ui](?:8|16|32|64|128|size))?"
    r"|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?(?:[ui](?:8|16|32|64|128|size)|f(?:32|64))?)$"
)
BOOL_LIT_RE = re.compile(r"^(?:true|false)$")
STRING_OR_CHAR_LIT_RE = re.compile(
    r'^(?:b?r#*"[\s\S]*"#*|b?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\])*\')$'
)
CONST_IDENT_RE = re.compile(
    r"^-?(?:(?:[A-Za-z_][A-Za-z0-9_]*::)+[A-Z][A-Z0-9_]*|[A-Z][A-Z0-9_]{1,})$"
)


class RatchetError(Exception):
    """Raised on invalid config, baseline, or git invocation."""


def _load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RatchetError(f"cannot load helper module from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_REPO_TOOLS_ROOT = Path(__file__).resolve().parents[2]
_unwrap = _load_module(
    "unwrap_ratchet_defaults_shared",
    _REPO_TOOLS_ROOT / "tools" / "unwrap_ratchet" / "unwrap_ratchet.py",
)
_silent = _load_module(
    "silent_failure_ratchet_defaults_shared",
    _REPO_TOOLS_ROOT / "tools" / "silent_failure_ratchet" / "silent_failure_ratchet.py",
)
_claims = _load_module(
    "check_retail_claims_defaults_shared",
    _REPO_TOOLS_ROOT / "scripts" / "check_retail_claims.py",
)

production_files = _unwrap.production_files
cfg_test_spans = _unwrap.cfg_test_spans
mask_with_comments = _unwrap.mask_with_comments
matching_bracket = _unwrap.matching_bracket
split_arguments = _unwrap.split_arguments
statement_start = _silent.statement_start
stand_in_marker = _claims.stand_in_marker
stand_in_marker_problem = _claims.stand_in_marker_problem


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    category: str
    snippet: str
    problem: str = ""


def is_test_path(rel_path: str) -> bool:
    """Return True for test/bench/example paths outside production scope."""
    if not rel_path.endswith(".rs"):
        return True
    name = Path(rel_path).name
    if name == "tests.rs" or name.endswith("_tests.rs"):
        return True
    return TEST_PATH_RE.search(rel_path) is not None


def load_json(path: Path, label: str) -> dict:
    """Load a JSON object or raise RatchetError."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise RatchetError(f"cannot read {label} {path}: {exc}") from exc
    try:
        data = json.loads(text)
    except ValueError as exc:
        raise RatchetError(f"invalid JSON in {label} {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise RatchetError(f"{label} {path} must be a JSON object")
    return data


def load_config(path: Path) -> dict:
    """Validate and return config.json."""
    data = load_json(path, "config.json")
    if data.get("format_version") != 1:
        raise RatchetError(
            f"config.json: unsupported format_version {data.get('format_version')!r}"
        )
    crates = data.get("crates")
    if not isinstance(crates, dict) or not crates:
        raise RatchetError("config.json: 'crates' must be a non-empty object")
    for name, crate_rel in crates.items():
        if not isinstance(name, str) or not isinstance(crate_rel, str) or not crate_rel:
            raise RatchetError(f"config.json: invalid crate mapping {name!r}: {crate_rel!r}")
    for key in ("retail_data_modules", "db_write_modules"):
        entries = data.get(key)
        if not isinstance(entries, list) or not all(
            isinstance(item, str) and item.strip() for item in entries
        ):
            raise RatchetError(f"config.json: '{key}' must be a list of non-empty paths")
    return data


def matches_module_scope(rel_path: str, entries: list[str]) -> bool:
    """True when `rel_path` matches an exact file or directory prefix in `entries`."""
    if is_test_path(rel_path):
        return False
    for entry in entries:
        prefix = entry.rstrip("/")
        if rel_path == prefix or rel_path.startswith(prefix + "/"):
            return True
    return False


def _line_starts(text: str) -> list[int]:
    starts = [0]
    for idx, ch in enumerate(text):
        if ch == "\n":
            starts.append(idx + 1)
    return starts


def _exempt_lines(
    raw_text: str, comments: list[tuple[int, int]], line_of
) -> tuple[set[int], dict[int, str]]:
    """Return `(exempt_1based_lines, stand_in_problems_by_1based_line)` from comment spans."""
    exempt: set[int] = set()
    problems: dict[int, str] = {}
    for start, end in comments:
        body = raw_text[start:end]
        if body.endswith("*/"):
            body = body[:-2]
        for seg_offset, segment in enumerate(body.split("\n")):
            ln = line_of(start) + seg_offset + 1
            if RETAIL_DEFAULT_RE.search(segment):
                exempt.add(ln)
                continue
            if "STAND-IN" in segment:
                if stand_in_marker(segment) is not None:
                    exempt.add(ln)
                else:
                    problem = stand_in_marker_problem(segment)
                    if problem and ln not in problems:
                        problems[ln] = problem
    return exempt, problems


def _comment_block_lines(
    raw_lines: list[str], target_line: int, include_block: bool = True
) -> list[int]:
    """Return 1-based line numbers for `target_line` and any contiguous `//` block above it."""
    lines = [target_line]
    idx = target_line - 1
    if include_block and idx >= 1 and raw_lines[idx - 1].strip().startswith("//"):
        while idx >= 1 and raw_lines[idx - 1].strip().startswith("//"):
            lines.append(idx)
            idx -= 1
    return lines


def _statement_first_line(code: str, position: int, line_of) -> int:
    """Return 1-based line number of the first non-whitespace token of the statement."""
    start = statement_start(code, position)
    n = len(code)
    while start < n and code[start].isspace():
        start += 1
    return line_of(min(start, max(position, 0))) + 1


def _has_exemption(
    raw_lines: list[str],
    exempt_lines: set[int],
    problems_by_line: dict[int, str],
    code: str,
    position: int,
    line_no: int,
    line_of,
) -> tuple[bool, str]:
    """Check current line, its comment block above, and the owning statement's header."""
    candidates: list[int] = []
    seen = set()
    stmt_line = _statement_first_line(code, position, line_of)
    for anchor in (line_no, stmt_line):
        for ln in _comment_block_lines(raw_lines, anchor):
            if ln not in seen:
                seen.add(ln)
                candidates.append(ln)

    problem = ""
    for ln in candidates:
        if ln in exempt_lines:
            return True, ""
        if ln in problems_by_line and not problem:
            problem = problems_by_line[ln]
    return False, problem


def _build_prod_raw(raw_text: str, code: str, comments: list[tuple[int, int]], test_spans: list[tuple[int, int]]) -> tuple[str, list[bool], list[bool]]:
    """Return `(prod_raw, skipped, in_comment)` preserving string literals but blanking comments and tests."""
    n = len(raw_text)
    in_comment = [False] * n
    skipped = [False] * n
    chars = list(raw_text)

    for start, end in comments:
        for idx in range(start, min(end, n)):
            in_comment[idx] = True
            if chars[idx] != "\n":
                chars[idx] = " "

    for start, end in test_spans:
        for idx in range(start, min(end + 1, n)):
            skipped[idx] = True
            if chars[idx] != "\n":
                chars[idx] = " "

    return "".join(chars), skipped, in_comment


def _is_in_string_literal(code: str, in_comment: list[bool], pos: int) -> bool:
    """True when `pos` is inside a string literal (blanked in `code`, not in a comment)."""
    return 0 <= pos < len(code) and code[pos] == " " and not in_comment[pos]


def _matching_sql_paren(text: str, open_pos: int) -> int:
    """Return index of matching `)` for SQL `(` at `open_pos`, respecting SQL single-quoted strings."""
    depth = 0
    in_quote = False
    i = open_pos
    n = len(text)
    while i < n:
        ch = text[i]
        if in_quote:
            if ch == "'" and i + 1 < n and text[i + 1] == "'":
                i += 2
                continue
            if ch == "'":
                in_quote = False
        else:
            if ch == "'":
                in_quote = True
            elif ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    return i
        i += 1
    return n - 1


def _split_sql_arguments(text: str) -> list[str]:
    """Split SQL function arguments on top-level commas outside parens and `'...'` literals."""
    parts = []
    depth = 0
    in_quote = False
    start = 0
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if in_quote:
            if ch == "'" and i + 1 < n and text[i + 1] == "'":
                i += 2
                continue
            if ch == "'":
                in_quote = False
        else:
            if ch == "'":
                in_quote = True
            elif ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
            elif ch == "," and depth == 0:
                parts.append(text[start:i].strip())
                start = i + 1
        i += 1
    parts.append(text[start:].strip())
    return [part for part in parts if part]


def is_literal_or_const(expr: str) -> bool:
    """True when a Rust expression is a literal, literal array/tuple, or uppercase constant."""
    expr = expr.strip()
    if not expr:
        return False
    # Strip outer parens if balanced.
    while expr.startswith("(") and expr.endswith(")"):
        inner = expr[1:-1].strip()
        if not inner:
            return False
        # Tuple like `(CONST_A, CONST_B)` vs grouped `(CONST_A)`.
        if matching_bracket(expr, 0) == len(expr) - 1:
            parts = _split_rust_top_level(inner)
            if len(parts) > 1:
                return all(is_literal_or_const(part) for part in parts)
            expr = inner
            continue
        break
    if NUMERIC_LIT_RE.match(expr) or BOOL_LIT_RE.match(expr) or STRING_OR_CHAR_LIT_RE.match(expr):
        return True
    if CONST_IDENT_RE.match(expr):
        return True
    if expr.startswith("[") and expr.endswith("]"):
        inner = expr[1:-1].strip()
        if not inner:
            return True
        if ";" in inner:
            val, count = inner.split(";", 1)
            return is_literal_or_const(val) and NUMERIC_LIT_RE.match(count.strip()) is not None
        return all(is_literal_or_const(part) for part in _split_rust_top_level(inner))
    return False


def _split_rust_top_level(text: str) -> list[str]:
    """Split Rust comma-separated items at depth 0 across `()`, `[]`, `{}`."""
    parts = []
    depth = 0
    start = 0
    in_str = False
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if in_str:
            if ch == "\\":
                i += 2
                continue
            if ch == '"':
                in_str = False
        else:
            if ch == '"':
                in_str = True
            elif ch in "([{":
                depth += 1
            elif ch in ")]}":
                depth -= 1
            elif ch == "," and depth == 0:
                parts.append(text[start:i].strip())
                start = i + 1
        i += 1
    tail = text[start:].strip()
    if tail:
        parts.append(tail)
    return parts


def _first_top_level_comma(code: str, start: int, end: int) -> int:
    """Return index of the first depth-0 `,` in `code[start:end]`, or -1."""
    depth = 0
    for i in range(start, end):
        ch = code[i]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == "," and depth == 0:
            return i
    return -1


def _use_statement_spans(code: str) -> list[tuple[int, int]]:
    """Character spans of `use ...;` items in masked `code`."""
    spans = []
    for match in re.finditer(r"\buse\b", code):
        prev = match.start() - 1
        while prev >= 0 and code[prev].isspace():
            prev -= 1
        # Ensure `use` starts an item (not `foo::use` or identifier suffix).
        if prev >= 0 and code[prev] not in ";{}":
            # Could follow `pub` / `pub(crate)` visibility.
            line_start = code.rfind("\n", 0, match.start()) + 1
            prefix = code[line_start : match.start()].strip()
            if not re.fullmatch(r"(?:pub(?:\([^)]*\))?)?", prefix):
                continue
        semi = code.find(";", match.end())
        if semi >= 0:
            spans.append((match.start(), semi))
    return spans


def _const_decl_name_positions(code: str) -> set[int]:
    """Start positions of declared constant/static names (`const NAME: ... = ...`)."""
    positions = set()
    for match in re.finditer(r"\b(?:const|static)\s+(?:mut\s+)?([A-Z_][A-Z0-9_]*)\b", code):
        positions.add(match.start(1))
    return positions


def _enclosing_fn_name(code: str, pos: int) -> str:
    """Return the nearest preceding `fn <name>` identifier before `pos`, or ''."""
    best = ""
    for match in re.finditer(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\b", code[:pos]):
        best = match.group(1)
    return best


def _last_arm_or_stmt_boundary(code: str, start: int, end: int) -> int:
    """Return index of the last depth-0 `,`, `;`, or `}` in `code[start:end]`, or `start - 1`."""
    boundary = start - 1
    depth = 0
    for i in range(start, end):
        ch = code[i]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            if depth == 0 and ch == "}":
                boundary = i
            depth = max(0, depth - 1)
        elif ch in ",;" and depth == 0:
            boundary = i
    return boundary


def _in_fallback_branch_context(code: str, open_brace: int) -> bool:
    """True when `open_brace` lies inside an active `None =>`, `Err(..) =>`, `else {`, or `unwrap_or_else(`."""
    depth = 0
    enclosing = -1
    for i in range(open_brace - 1, -1, -1):
        ch = code[i]
        if ch == "}":
            depth += 1
        elif ch == "{":
            if depth == 0:
                enclosing = i
                break
            depth -= 1
    # Bound the prefix to the active match arm or statement inside `enclosing`, so a closed
    # `let ... else { return; };` or an earlier `None => recover(),` arm never leaks forward.
    last_boundary = _last_arm_or_stmt_boundary(code, enclosing + 1, open_brace)
    prefix = code[last_boundary + 1 : open_brace]
    if FALLBACK_BRANCH_RE.search(prefix):
        return True
    if enclosing > 0:
        outer_depth = 0
        outer_enclosing = -1
        for i in range(enclosing - 1, -1, -1):
            ch = code[i]
            if ch == "}":
                outer_depth += 1
            elif ch == "{":
                if outer_depth == 0:
                    outer_enclosing = i
                    break
                outer_depth -= 1
        header_start = _last_arm_or_stmt_boundary(code, outer_enclosing + 1, enclosing) + 1
        header_start = max(header_start, enclosing - 200)
        if FALLBACK_BRANCH_RE.search(code[header_start : enclosing + 1]):
            return True
    return False


def _scan_db_rules(
    rel_path: str,
    raw_lines: list[str],
    exempt_lines: set[int],
    problems_by_line: dict[int, str],
    code: str,
    prod_raw: str,
    skipped: list[bool],
    in_comment: list[bool],
    line_of,
) -> list[Finding]:
    """Run Rule (a): SQLite boundary checks on a db_write_module."""
    findings: list[Finding] = []

    def emit(pos: int, category: str) -> None:
        if pos >= len(skipped) or skipped[pos]:
            return
        line_no = line_of(pos) + 1
        exempt, problem = _has_exemption(
            raw_lines, exempt_lines, problems_by_line, code, pos, line_no, line_of
        )
        if exempt:
            return
        snippet = raw_lines[line_no - 1].strip()[:160]
        findings.append(Finding(rel_path, line_no, category, snippet, problem))

    # 1. SQL DDL `NOT NULL DEFAULT <non-null>` inside string literals.
    for match in SQL_NOT_NULL_DEFAULT_RE.finditer(prod_raw):
        if _is_in_string_literal(code, in_comment, match.start()):
            emit(match.start(), "sql-not-null-default")

    # 2. SQL `COALESCE(..., <literal>)` inside string literals.
    for match in SQL_COALESCE_RE.finditer(prod_raw):
        if not _is_in_string_literal(code, in_comment, match.start()):
            continue
        open_paren = prod_raw.index("(", match.start())
        close_paren = _matching_sql_paren(prod_raw, open_paren)
        args = _split_sql_arguments(prod_raw[open_paren + 1 : close_paren])
        if len(args) < 2:
            continue
        agg_match = SQL_AGGREGATE_FIRST_ARG_RE.match(args[0])
        if agg_match:
            fn_name = agg_match.group("fn").upper()
            if fn_name in ("MAX", "MIN"):
                inner_open = args[0].index("(")
                inner_close = _matching_sql_paren(args[0], inner_open)
                inner_args = _split_sql_arguments(args[0][inner_open + 1 : inner_close])
                if len(inner_args) == 1:
                    continue
            else:
                continue
        if any(SQL_LITERAL_RE.match(arg) for arg in args[1:]):
            emit(match.start(), "sql-coalesce-literal")

    # 3. Fallback struct writes with `0` / `""` / `"male"` / `"female"` when unresolved/None.
    for match in STRUCT_OPEN_RE.finditer(code):
        pos = match.start()
        if skipped[pos]:
            continue
        short_name = match.group(1)
        if not short_name or short_name in NON_STRUCT_KEYWORDS or not short_name[0].isupper():
            continue
        if short_name == "RawRecord":
            continue
        open_brace = match.end() - 1
        close_brace = matching_bracket(code, open_brace)
        after = code[close_brace + 1 : close_brace + 20].lstrip()
        if after.startswith("=>"):
            continue
        # Ensure it isn't a `let Foo { ... } =` destructuring pattern.
        stmt_begin = statement_start(code, pos)
        before_in_stmt = code[stmt_begin:pos].strip()
        if re.match(r"^(?:if\s+let|while\s+let|let)\b", before_in_stmt) and "=" not in before_in_stmt:
            continue

        raw_body = prod_raw[open_brace + 1 : close_brace]
        is_row_struct = (
            short_name.endswith(("Row", "Record")) and short_name != "RawRecord"
        )
        has_unresolved_field = UNRESOLVED_MARKER_RE.search(raw_body) is not None
        fn_name = _enclosing_fn_name(code, pos)
        in_fallback_fn = FALLBACK_FN_NAME_RE.search(fn_name) is not None
        in_fallback_branch = _in_fallback_branch_context(code, open_brace)

        if not (is_row_struct or has_unresolved_field or in_fallback_fn or in_fallback_branch):
            continue

        # Scan direct fields at struct brace depth 0 (using `code` to skip nested `{}`).
        code_body = code[open_brace + 1 : close_brace]
        depth = 0
        top_chars = []
        for idx, ch in enumerate(code_body):
            if ch in "([{":
                depth += 1
                top_chars.append(" ")
            elif ch in ")]}":
                depth = max(0, depth - 1)
                top_chars.append(" ")
            elif depth == 0:
                top_chars.append(raw_body[idx])
            else:
                top_chars.append(" ")
        top_raw = "".join(top_chars)
        for fmatch in STRUCT_FIELD_RE.finditer(top_raw):
            field_name = fmatch.group(1)
            field_val = fmatch.group(2).strip()
            if field_name in COUNTER_FIELDS:
                continue
            if FALLBACK_LITERAL_VALUE_RE.match(field_val):
                emit(open_brace + 1 + fmatch.start(), "fallback-struct-write")

    # 4. Direct `params![...]` with 0 / "" / "male" / "female" in a fallback branch or fallback fn.
    for match in PARAMS_MACRO_RE.finditer(code):
        pos = match.start()
        if skipped[pos]:
            continue
        open_bracket = match.end() - 1
        close_bracket = matching_bracket(code, open_bracket)
        fn_name = _enclosing_fn_name(code, pos)
        if not (
            FALLBACK_FN_NAME_RE.search(fn_name)
            or _in_fallback_branch_context(code, pos)
        ):
            continue
        raw_inner = prod_raw[open_bracket + 1 : close_bracket]
        for arg in _split_rust_top_level(raw_inner):
            if FALLBACK_LITERAL_VALUE_RE.match(arg.strip()):
                emit(pos, "fallback-db-params")
                break

    return findings


def _scan_retail_data_rules(
    rel_path: str,
    raw_lines: list[str],
    exempt_lines: set[int],
    problems_by_line: dict[int, str],
    code: str,
    prod_raw: str,
    skipped: list[bool],
    line_of,
) -> list[Finding]:
    """Run Rule (b): `.unwrap_or`, `.map_or`, and fallback constant checks on retail_data_modules."""
    findings: list[Finding] = []
    covered_const_spans: list[tuple[int, int]] = []

    def emit(pos: int, category: str) -> None:
        if pos >= len(skipped) or skipped[pos]:
            return
        line_no = line_of(pos) + 1
        exempt, problem = _has_exemption(
            raw_lines, exempt_lines, problems_by_line, code, pos, line_no, line_of
        )
        if exempt:
            return
        snippet = raw_lines[line_no - 1].strip()[:160]
        findings.append(Finding(rel_path, line_no, category, snippet, problem))

    # 1. `.unwrap_or(<literal|CONST>)`
    for match in UNWRAP_OR_RE.finditer(code):
        if skipped[match.start()]:
            continue
        open_paren = match.end() - 1
        close_paren = matching_bracket(code, open_paren)
        args = _split_rust_top_level(prod_raw[open_paren + 1 : close_paren])
        if len(args) == 1 and is_literal_or_const(args[0]):
            covered_const_spans.append((open_paren, close_paren))
            emit(match.start(), "unwrap-or-literal")

    # 2. `.unwrap_or_else(|| <literal|CONST>)`
    for match in UNWRAP_OR_ELSE_RE.finditer(code):
        if skipped[match.start()]:
            continue
        open_paren = match.end() - 1
        close_paren = matching_bracket(code, open_paren)
        args = _split_rust_top_level(prod_raw[open_paren + 1 : close_paren])
        cm = CLOSURE_EMPTY_ARGS_RE.match(args[0]) if len(args) == 1 else None
        if cm and is_literal_or_const(cm.group("body").strip()):
            covered_const_spans.append((open_paren, close_paren))
            emit(match.start(), "unwrap-or-literal")

    # 3. `.map_or(<literal|CONST>, ...)`
    for match in MAP_OR_RE.finditer(code):
        if skipped[match.start()]:
            continue
        open_paren = match.end() - 1
        close_paren = matching_bracket(code, open_paren)
        raw_inner = prod_raw[open_paren + 1 : close_paren]
        args = _split_rust_top_level(raw_inner)
        if args and is_literal_or_const(args[0]):
            first_comma = _first_top_level_comma(code, open_paren + 1, close_paren)
            covered_const_spans.append(
                (open_paren, first_comma if first_comma >= 0 else close_paren)
            )
            emit(match.start(), "map-or-literal")

    # 4. Fallback returns/uses of `SKYRIM_CLEAR_*` / `DEFAULT_*` / `FALLBACK_*` constants.
    use_spans = _use_statement_spans(code)
    const_decls = _const_decl_name_positions(code)
    for match in FALLBACK_CONST_RE.finditer(code):
        pos = match.start()
        end = match.end()
        if skipped[pos]:
            continue
        if pos in const_decls:
            continue
        if any(s <= pos <= e for s, e in use_spans):
            continue
        if any(s <= pos <= e for s, e in covered_const_spans):
            continue
        # Skip equality/inequality checks (e.g. `id == DEFAULT_LAND_TEXTURE_FORM_ID`).
        before = code[max(0, pos - 64) : pos].rstrip()
        after = code[end : min(len(code), end + 32)].lstrip()
        # Walk back over path prefix (`shared::terrain_mesh::`).
        path_before = re.sub(r"(?:[A-Za-z_][A-Za-z0-9_]*::)+$", "", before).rstrip()
        if path_before.endswith(("==", "!=")) or after.startswith(("==", "!=")):
            continue
        emit(pos, "fallback-constant")

    return findings


def scan_file(rel_path: str, raw_text: str, config: dict) -> list[Finding]:
    """Scan one Rust file according to its membership in `db_write_modules` and `retail_data_modules`."""
    in_db = matches_module_scope(rel_path, config["db_write_modules"])
    in_retail = matches_module_scope(rel_path, config["retail_data_modules"])
    if not in_db and not in_retail:
        return []

    code, comments = mask_with_comments(raw_text)
    test_spans = cfg_test_spans(code)
    prod_raw, skipped, in_comment = _build_prod_raw(raw_text, code, comments, test_spans)
    raw_lines = raw_text.splitlines()
    starts = _line_starts(raw_text)

    def line_of(position: int) -> int:
        return bisect_right(starts, position) - 1

    exempt_lines, problems_by_line = _exempt_lines(raw_text, comments, line_of)
    findings: list[Finding] = []
    if in_db:
        findings.extend(
            _scan_db_rules(
                rel_path,
                raw_lines,
                exempt_lines,
                problems_by_line,
                code,
                prod_raw,
                skipped,
                in_comment,
                line_of,
            )
        )
    if in_retail:
        findings.extend(
            _scan_retail_data_rules(
                rel_path,
                raw_lines,
                exempt_lines,
                problems_by_line,
                code,
                prod_raw,
                skipped,
                line_of,
            )
        )
    findings.sort(key=lambda f: (f.path, f.line, f.category))
    return findings


def scan_crate(root: Path, crate_name: str, crate_rel: str, config: dict) -> list[Finding]:
    """Scan all production files of `crate_name` under `root / crate_rel`."""
    crate_dir = root / crate_rel
    if not (crate_dir / "src").is_dir():
        return []
    findings: list[Finding] = []
    for path in production_files(crate_dir):
        try:
            rel_path = path.relative_to(root).as_posix()
        except ValueError:
            rel_path = path.as_posix()
        raw_text = path.read_text(encoding="utf-8", errors="replace")
        findings.extend(scan_file(rel_path, raw_text, config))
    return findings


def count_crates(root: Path, config: dict) -> tuple[dict[str, int], list[Finding]]:
    """Return `(counts_per_crate, all_findings)` across configured crates."""
    counts: dict[str, int] = {}
    all_findings: list[Finding] = []
    for crate_name, crate_rel in sorted(config["crates"].items()):
        hits = scan_crate(root, crate_name, crate_rel, config)
        counts[crate_name] = len(hits)
        all_findings.extend(hits)
    return counts, all_findings


def git(root: Path, *args: str) -> str:
    """Run git in `root` and return stdout, or raise RatchetError."""
    cmd = ["git", "-C", str(root), "-c", "core.quotePath=false", *args]
    res = subprocess.run(
        cmd,
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
    )
    if res.returncode != 0:
        msg = res.stderr.strip() or res.stdout.strip()
        raise RatchetError(f"{' '.join(cmd)}: {msg}")
    return res.stdout


def parse_diff(text: str) -> tuple[dict[str, dict[int, str]], Counter[str]]:
    """Parse `git diff --unified=0` into added lines per path and deleted line multiset."""
    added: dict[str, dict[int, str]] = {}
    removed: Counter[str] = Counter()
    path = None
    new_line = None
    in_hunk = False
    for raw in text.splitlines():
        if raw.startswith("diff --git "):
            path = None
            new_line = None
            in_hunk = False
        elif not in_hunk and raw.startswith("+++ "):
            target = raw[4:]
            if target == "/dev/null":
                path = None
            else:
                path = target[2:] if target.startswith("b/") else target
                added.setdefault(path, {})
        elif raw.startswith("@@"):
            match = HUNK_RE.match(raw)
            if match is None:
                raise RatchetError(f"cannot parse diff hunk: {raw}")
            new_line = int(match.group(1))
            in_hunk = True
        elif in_hunk and new_line is not None:
            if raw.startswith("+"):
                if path is not None:
                    added[path][new_line] = raw[1:]
                new_line += 1
            elif raw.startswith("-"):
                removed[raw[1:]] += 1
            elif raw.startswith("\\"):
                continue
            else:
                new_line += 1
    return added, removed


def net_new_lines(added: dict[str, dict[int, str]], removed: Counter[str]) -> dict[str, set[int]]:
    """Drop added lines whose exact text was also deleted elsewhere in the diff."""
    kept: dict[str, set[int]] = {}
    for path in sorted(added):
        lines = set()
        for line_no in sorted(added[path]):
            text = added[path][line_no]
            if removed[text] > 0:
                removed[text] -= 1
                continue
            lines.add(line_no)
        if lines:
            kept[path] = lines
    return kept


def read_baseline(path: Path) -> tuple[dict[str, int], int]:
    """Load `(counts, revision)` from `baseline.json`."""
    data = load_json(path, "baseline.json")
    if data.get("format_version") != 1:
        raise RatchetError(
            f"baseline.json: unsupported format_version {data.get('format_version')!r}"
        )
    counts = data.get("counts")
    revision = data.get("revision", 1)
    if not isinstance(counts, dict):
        raise RatchetError(f"baseline.json {path} has no 'counts' object")
    if not isinstance(revision, int) or isinstance(revision, bool) or revision < 0:
        raise RatchetError(f"baseline.json {path} has invalid 'revision'")
    for key, value in counts.items():
        if not isinstance(key, str) or not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise RatchetError(f"baseline.json {path} has invalid count for {key!r}")
    return counts, revision


def write_baseline(path: Path, counts: dict[str, int], revision: int) -> None:
    """Write `baseline.json` with LF newlines."""
    doc = {"format_version": 1, "counts": dict(sorted(counts.items())), "revision": revision}
    text = json.dumps(doc, indent=2) + "\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)


def read_base_baseline(root: Path, merge_base: str, baseline_rel: str) -> dict[str, int] | None:
    """Return `counts` from `baseline.json` at `merge_base`, or None if absent."""
    res = subprocess.run(
        ["git", "-C", str(root), "show", f"{merge_base}:{baseline_rel}"],
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
    )
    if res.returncode != 0:
        return None
    try:
        data = json.loads(res.stdout)
    except ValueError as exc:
        raise RatchetError(f"invalid base {baseline_rel} at {merge_base}: {exc}") from exc
    if not isinstance(data, dict):
        raise RatchetError(f"base {baseline_rel} at {merge_base} must be a JSON object")
    counts = data.get("counts")
    if not isinstance(counts, dict):
        raise RatchetError(f"base {baseline_rel} at {merge_base} has non-object 'counts'")
    return counts


def scan_diff(root: Path, base: str, config: dict) -> tuple[str, list[Finding]]:
    """Return `(merge_base, findings)` for net-new added lines in scoped files since `merge_base`."""
    mb_lines = git(root, "merge-base", base, "HEAD").splitlines()
    if not mb_lines or not mb_lines[0].strip():
        raise RatchetError(f"{base} and HEAD have no merge base")
    merge_base = mb_lines[0].strip()
    diff_text = git(
        root,
        "diff",
        "--unified=0",
        "-M",
        "-C",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        f"{merge_base}...HEAD",
    )
    added, removed = parse_diff(diff_text)
    new_by_file = net_new_lines(added, removed)

    findings: list[Finding] = []
    for rel_path, line_numbers in sorted(new_by_file.items()):
        if not (
            matches_module_scope(rel_path, config["db_write_modules"])
            or matches_module_scope(rel_path, config["retail_data_modules"])
        ):
            continue
        raw_text = git(root, "show", f"HEAD:{rel_path}")
        for hit in scan_file(rel_path, raw_text, config):
            if hit.line in line_numbers:
                findings.append(hit)
    return merge_base, findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument("--config", type=Path, help=f"default: {DEFAULT_CONFIG}")
    parser.add_argument("--baseline", type=Path, help=f"default: {DEFAULT_BASELINE}")
    parser.add_argument("--base", help="git revision to diff and ratchet against")
    parser.add_argument("--check", action="store_true", help="check ratchet and optional diff")
    parser.add_argument(
        "--audit-tree",
        action="store_true",
        help="scan all scoped production files and print every hit",
    )
    parser.add_argument(
        "--update-baseline",
        action="store_true",
        help="write current per-crate counts to baseline.json",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="fail if any crate count is strictly below baseline.json",
    )
    args = parser.parse_args(argv)

    root = args.root.resolve()
    config_path = args.config if args.config else root / DEFAULT_CONFIG
    baseline_path = args.baseline if args.baseline else root / DEFAULT_BASELINE
    if not config_path.is_absolute():
        config_path = root / config_path
    if not baseline_path.is_absolute():
        baseline_path = root / baseline_path

    try:
        config = load_config(config_path)
    except RatchetError as exc:
        print(f"defaults ratchet: {exc}", file=sys.stderr)
        return 2

    measured, all_findings = count_crates(root, config)

    if args.audit_tree:
        for hit in all_findings:
            print(f"{hit.path}:{hit.line}: [{hit.category}] {hit.snippet}")
        by_cat = Counter(hit.category for hit in all_findings)
        summary = ", ".join(f"{k}={v}" for k, v in sorted(by_cat.items()))
        print(f"defaults ratchet tree scan: {len(all_findings)} hit(s) ({summary})")
        for name, count in sorted(measured.items()):
            print(f"  {name}: {count}")
        return 0

    if args.update_baseline:
        prev_rev = 0
        if baseline_path.is_file():
            try:
                _, prev_rev = read_baseline(baseline_path)
            except RatchetError:
                prev_rev = 0
        write_baseline(baseline_path, measured, prev_rev + 1)
        print(f"defaults ratchet: wrote {baseline_path} (revision {prev_rev + 1})")
        for name, count in sorted(measured.items()):
            print(f"  {name}: {count}")
        return 0

    try:
        baseline_counts, revision = read_baseline(baseline_path)
    except RatchetError as exc:
        print(f"defaults ratchet: {exc}", file=sys.stderr)
        return 2

    failed = False
    lowered = []
    for name in sorted(config["crates"]):
        count = measured[name]
        limit = baseline_counts.get(name)
        if limit is None:
            print(f"{name:22} {count:5} / {'-':>5}  MISSING FROM BASELINE", file=sys.stderr)
            failed = True
            continue
        state = "ok"
        if count > limit:
            state = "RAISED"
            failed = True
        elif count < limit:
            state = "below baseline"
            lowered.append(name)
        print(f"{name:22} {count:5} / {limit:5}  {state}")

    if args.base:
        try:
            merge_base, diff_hits = scan_diff(root, args.base, config)
            try:
                baseline_rel = baseline_path.relative_to(root).as_posix()
            except ValueError:
                baseline_rel = DEFAULT_BASELINE.as_posix()
            base_counts = read_base_baseline(root, merge_base, baseline_rel)
        except RatchetError as exc:
            print(f"defaults ratchet: {exc}", file=sys.stderr)
            return 2

        if base_counts is not None:
            for name, limit in sorted(baseline_counts.items()):
                base_limit = base_counts.get(name)
                if base_limit is not None and limit > base_limit:
                    print(
                        f"defaults ratchet: baseline for {name} rose above {merge_base[:12]} "
                        f"({base_limit} -> {limit}); baseline may only go down",
                        file=sys.stderr,
                    )
                    failed = True

        for hit in diff_hits:
            extra = f" ({hit.problem})" if hit.problem else ""
            print(
                f"{hit.path}:{hit.line}: [{hit.category}] possible invented default on retail data{extra}: {hit.snippet}",
                file=sys.stderr,
            )
        if diff_hits:
            print(
                f"defaults ratchet: {len(diff_hits)} added line(s) violate retail-default rules. "
                "Omit and report missing/unresolved data instead, or justify with "
                "`// retail-default: <reference>` or `// STAND-IN(#<issue>): <reason>` "
                "on the line or line above.",
                file=sys.stderr,
            )
            failed = True

    if failed:
        return 1

    if lowered:
        print(
            "defaults ratchet: count fell in "
            + ", ".join(lowered)
            + ". Lower the baseline with `python3 -B tools/defaults_ratchet/defaults_ratchet.py --update-baseline`.",
            file=sys.stderr,
        )
        if args.strict:
            return 1

    print(f"defaults ratchet: ok (revision {revision})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
