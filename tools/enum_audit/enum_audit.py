#!/usr/bin/env python3
"""Check retail enum decision sites, ratchet bug count, and guard added Rust lines.

This tool enforces three invariants on retail enum handling:
1. Every site in `tools/enum_audit/sites.json` keeps a valid status (`exhaustive`,
   `catch-all-allowed` with `reason` and `reference`, or `catch-all-bug` with
   `issue` and `reason`) and a unique non-line-number `anchor` present in `file`.
2. The count of `catch-all-bug` sites may only go down relative to `baseline.json`
   and relative to `--base` when `sites.json` exists there.
3. Added non-test Rust lines in the scoped reader/shared paths are scanned against
   a heuristic for silent catch-alls (`_ =>` / binding arms on integer matches,
   integer `.min(N)` / `.clamp`, small `& mask` extractions, and enum-like
   `.unwrap_or(`) unless the line or the line above carries
   `// retail-enum: <id> <reason>`.

Usage:
  python3 -B tools/enum_audit/enum_audit.py --check
  python3 -B tools/enum_audit/enum_audit.py --check --base origin/main
  python3 -B tools/enum_audit/enum_audit.py --audit-tree
  python3 -B tools/enum_audit/enum_audit.py --update-baseline

Exit codes: 0 = pass, 1 = check/ratchet/heuristic failure, 2 = usage/git/schema error.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

VALID_STATUSES = {"exhaustive", "catch-all-allowed", "catch-all-bug"}
ALLOWED_KEYS = {"file", "anchor", "enum_id", "status", "reason", "reference", "issue"}
ISSUE_RE = re.compile(r"^#\d+$")
HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")

# Scoped production Rust paths for the diff and tree catch-all heuristic.
SCOPED_ROOTS = (
    "crates/converter/src/esm",
    "crates/converter/src/records",
    "crates/converter/src/collision.rs",
    "crates/converter/src/animation",
    "crates/converter/src/mesh.rs",
    "crates/converter/src/mesh",
    "crates/shared/src",
    "vendor/project-wormhole-nif/src",
)

# `// retail-enum: <id> <reason>` requires both an identifier token and a reason.
EXEMPTION_RE = re.compile(r"//\s*retail-enum:\s*\S+\s+\S")

INT_ARM_RE = re.compile(
    r"^(?:-?\d+|0x[0-9a-fA-F_]+|\d+\.\.=?=?\d+)"
    r"(?:\s*\|\s*(?:-?\d+|0x[0-9a-fA-F_]+|\d+\.\.=?=?\d+))*"
    r"\s*(?:if\b.*?)?=>"
)
CATCH_ARM_RE = re.compile(r"^(?:_|([a-z_][a-z0-9_]*))\s*(?:if\b.*?)?=>\s*(.*)$")
NON_BINDING_KEYWORDS = frozenset(
    {"true", "false", "return", "break", "continue", "let", "if", "else", "match"}
)
EXPLICIT_REJECT_RE = re.compile(
    r"\b(?:Err\b|bail\s*!|ensure\s*!|unreachable\s*!|panic\s*!|nom::Err\b|ok_or\b)"
)
INT_MIN_CLAMP_RE = re.compile(
    r"\.(?:min|clamp)\(\s*(?:-?\d+|0x[0-9a-fA-F_]+)(?!\.)\s*(?:,\s*(?:-?\d+|0x[0-9a-fA-F_]+)(?!\.)\s*)?(?:,\s*)?\)"
)
SMALL_MASK_RE = re.compile(r"&\s*(0x[0-9a-fA-F_]+|\d+)\b(?!\s*[!=]=)")
UNWRAP_STR_DEFAULT_RE = re.compile(r'\.unwrap_or\([^)]*"(?:unknown|unused|default)"')
UNWRAP_INT_OR_ENUM_RE = re.compile(
    r"\.unwrap_or\(\s*(?:-?\d+(?!\.)\b|[A-Z][A-Za-z0-9_]*::)"
)
NON_ENUM_UNWRAP_RE = re.compile(
    r"\b(?:form_id|cell_form_id|offset|stride|size|len|count|capacity|depth|"
    r"width|height|mip|index)\b|from_utf8|as_bytes|u64|usize|f32|f64|Path::|Vec3::"
)


class AuditConfigError(Exception):
    """Raised when sites.json, baseline.json, or git inputs are malformed."""


def _load_unwrap_helpers():
    """Reuse mask_with_comments and cfg_test_spans from tools/unwrap_ratchet."""
    path = Path(__file__).resolve().parent.parent / "unwrap_ratchet" / "unwrap_ratchet.py"
    spec = importlib.util.spec_from_file_location("unwrap_ratchet_enum_shared", path)
    if spec is None or spec.loader is None:
        raise AuditConfigError(f"cannot load unwrap_ratchet helpers from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.mask_with_comments, module.cfg_test_spans


mask_with_comments, cfg_test_spans = _load_unwrap_helpers()


def is_scoped_rust_file(rel_path: str) -> bool:
    """True when `rel_path` is a non-test `.rs` file inside SCOPED_ROOTS."""
    if not rel_path.endswith(".rs"):
        return False
    name = Path(rel_path).name
    if name == "tests.rs" or name.endswith("_tests.rs"):
        return False
    for prefix in SCOPED_ROOTS:
        if rel_path == prefix or rel_path.startswith(prefix + "/"):
            return True
    # Also match crates/converter/src/collision*.rs or mesh*.rs if added later.
    if rel_path.startswith("crates/converter/src/collision") or rel_path.startswith(
        "crates/converter/src/mesh"
    ):
        return True
    return False


def scoped_rust_files(root: Path) -> list[str]:
    """Return sorted repo-relative POSIX paths of all scoped production `.rs` files."""
    found = set()
    conv_src = root / "crates" / "converter" / "src"
    if conv_src.is_dir():
        for p in conv_src.iterdir():
            if p.name.startswith(("collision", "mesh")):
                if p.is_file():
                    rel = p.relative_to(root).as_posix()
                    if is_scoped_rust_file(rel):
                        found.add(rel)
                elif p.is_dir():
                    for sub in p.rglob("*.rs"):
                        rel = sub.relative_to(root).as_posix()
                        if is_scoped_rust_file(rel):
                            found.add(rel)
    for rel_root in SCOPED_ROOTS:
        target = root / rel_root
        if target.is_file():
            rel = target.relative_to(root).as_posix()
            if is_scoped_rust_file(rel):
                found.add(rel)
        elif target.is_dir():
            for sub in target.rglob("*.rs"):
                rel = sub.relative_to(root).as_posix()
                if is_scoped_rust_file(rel):
                    found.add(rel)
    return sorted(found)


def load_json(path: Path, label: str) -> dict:
    """Load a JSON object or raise AuditConfigError."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise AuditConfigError(f"cannot read {label} {path}: {exc}") from exc
    try:
        data = json.loads(text)
    except ValueError as exc:
        raise AuditConfigError(f"invalid JSON in {label} {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise AuditConfigError(f"{label} {path} must be a JSON object")
    return data


def validate_sites(root: Path, data: dict) -> tuple[list[str], dict[str, int]]:
    """Validate schema, anchors, and per-status metadata. Returns (errors, status_counts)."""
    errors = []
    if data.get("format_version") != 1:
        errors.append(f"sites.json: unsupported format_version {data.get('format_version')!r}")
        return errors, {}
    sites = data.get("sites")
    if not isinstance(sites, list) or not sites:
        errors.append("sites.json: 'sites' must be a non-empty list")
        return errors, {}

    file_cache: dict[str, str] = {}
    seen_anchors: set[tuple[str, str]] = set()
    counts: Counter[str] = Counter()

    for idx, entry in enumerate(sites):
        if not isinstance(entry, dict):
            errors.append(f"sites[{idx}]: entry must be a JSON object")
            continue
        unknown_keys = sorted(set(entry) - ALLOWED_KEYS)
        if unknown_keys:
            errors.append(f"sites[{idx}]: unknown key(s): {', '.join(unknown_keys)}")

        rel = entry.get("file")
        anchor = entry.get("anchor")
        enum_id = entry.get("enum_id")
        status = entry.get("status")

        if not isinstance(rel, str) or not rel.strip():
            errors.append(f"sites[{idx}]: 'file' must be a non-empty string")
            continue
        if rel.startswith("/") or "\\" in rel or ".." in Path(rel).parts:
            errors.append(f"sites[{idx}]: 'file' must be a clean repo-relative POSIX path: {rel!r}")
            continue

        if not isinstance(enum_id, str) or not enum_id.strip():
            errors.append(f"sites[{idx}] ({rel}): 'enum_id' must be a non-empty string")

        if not isinstance(anchor, str) or not anchor.strip():
            errors.append(f"sites[{idx}] ({rel}): 'anchor' must be a non-empty string")
            continue
        if anchor.strip().isdigit():
            errors.append(
                f"sites[{idx}] ({rel}): 'anchor' must be a code anchor string, not a line number ({anchor!r})"
            )
            continue

        if (rel, anchor) in seen_anchors:
            errors.append(f"sites[{idx}] ({rel}): duplicate (file, anchor) entry: {anchor!r}")
        seen_anchors.add((rel, anchor))

        if status not in VALID_STATUSES:
            errors.append(
                f"sites[{idx}] ({rel}): invalid status {status!r}; expected one of {sorted(VALID_STATUSES)}"
            )
            continue
        counts[status] += 1

        reason = entry.get("reason")
        reference = entry.get("reference")
        has_issue = "issue" in entry
        issue = entry.get("issue")

        if status == "exhaustive":
            if has_issue and issue not in (None, ""):
                errors.append(f"sites[{idx}] ({rel}): 'exhaustive' site must not set 'issue'")
        elif status == "catch-all-allowed":
            if not isinstance(reason, str) or not reason.strip():
                errors.append(f"sites[{idx}] ({rel}): 'catch-all-allowed' requires a non-empty 'reason'")
            if not isinstance(reference, str) or not reference.strip():
                errors.append(f"sites[{idx}] ({rel}): 'catch-all-allowed' requires a non-empty 'reference'")
            if has_issue and issue not in (None, ""):
                errors.append(f"sites[{idx}] ({rel}): 'catch-all-allowed' site must not set 'issue'")
        elif status == "catch-all-bug":
            if not has_issue:
                errors.append(f"sites[{idx}] ({rel}): 'catch-all-bug' requires an 'issue' field (use \"\" if unfiled)")
            elif isinstance(issue, int):
                if issue <= 0:
                    errors.append(f"sites[{idx}] ({rel}): invalid issue number {issue!r}")
            elif isinstance(issue, str):
                if issue != "" and not ISSUE_RE.match(issue):
                    errors.append(
                        f"sites[{idx}] ({rel}): 'issue' must be \"\" or #<number>, got {issue!r}"
                    )
            else:
                errors.append(f"sites[{idx}] ({rel}): invalid 'issue' value {issue!r}")
            if not isinstance(reason, str) or not reason.strip():
                errors.append(f"sites[{idx}] ({rel}): 'catch-all-bug' requires a non-empty 'reason'")

        if rel not in file_cache:
            target = root / rel
            if not target.is_file():
                errors.append(f"sites[{idx}]: file does not exist: {rel}")
                file_cache[rel] = None
                continue
            file_cache[rel] = target.read_text(encoding="utf-8", errors="replace")

        text = file_cache[rel]
        # An empty file that exists is still checked: its anchors are missing.
        if text is not None:
            occurrences = text.count(anchor)
            if occurrences == 0:
                errors.append(
                    f"sites[{idx}] ({rel}): anchor missing (removed or moved to another file): {anchor!r}"
                )
            elif occurrences > 1:
                errors.append(
                    f"sites[{idx}] ({rel}): anchor matches {occurrences} places (must be unique): {anchor!r}"
                )

    return errors, dict(counts)


def catch_arm_text(masked_lines: list[str], line_no: int) -> str:
    """Text of the match arm that starts on `line_no` (1-based): its line, plus a block or
    the next line when the arm body continues past the `=>`."""
    first = masked_lines[line_no - 1]
    body = first.split("=>", 1)[1] if "=>" in first else ""
    parts = [first]
    balance = body.count("{") - body.count("}")
    idx = line_no
    if not body.strip() and idx < len(masked_lines):
        nxt = masked_lines[idx]
        parts.append(nxt)
        balance = nxt.count("{") - nxt.count("}")
        idx += 1
    while balance > 0 and idx < len(masked_lines):
        line = masked_lines[idx]
        parts.append(line)
        balance += line.count("{") - line.count("}")
        idx += 1
    return "\n".join(parts)


def scan_file_for_catch_alls(rel_path: str, raw_text: str) -> list[tuple[str, int, str, str]]:
    """Return `(rel_path, line_num, category, snippet)` for heuristic hits in non-test code."""
    masked, _ = mask_with_comments(raw_text)
    test_spans = cfg_test_spans(masked)
    masked_lines = masked.splitlines()
    raw_lines = raw_text.splitlines()

    in_test_line = []
    pos = 0
    for ml in masked_lines:
        in_test_line.append(any(start <= pos < end for start, end in test_spans))
        pos += len(ml) + 1

    def has_exemption(line_no: int) -> bool:
        cur = raw_lines[line_no - 1]
        prev = raw_lines[line_no - 2] if line_no >= 2 else ""
        return bool(EXEMPTION_RE.search(cur) or EXEMPTION_RE.search(prev))

    findings = []
    depth = 0
    match_stack = []  # [body_depth, has_int_arm, [(line_no, raw_line_stripped)]]
    pending_match = False

    for line_no, (ml, rl) in enumerate(zip(masked_lines, raw_lines), 1):
        stripped = ml.strip()
        if not in_test_line[line_no - 1]:
            if re.search(r"\bmatch\b", stripped) and "matches!" not in stripped:
                pending_match = True

            if match_stack and depth == match_stack[-1][0]:
                if INT_ARM_RE.match(stripped):
                    match_stack[-1][1] = True
                else:
                    cm = CATCH_ARM_RE.match(stripped)
                    if cm and cm.group(1) not in NON_BINDING_KEYWORDS:
                        # Only the catch-all arm itself may show an explicit rejection; an
                        # unrelated `Err` after the match must not exempt it.
                        ctx = catch_arm_text(masked_lines, line_no)
                        if not EXPLICIT_REJECT_RE.search(ctx):
                            match_stack[-1][2].append((line_no, rl.strip()))

            if not has_exemption(line_no):
                if INT_MIN_CLAMP_RE.search(ml) and ".len().min(" not in ml:
                    findings.append((rel_path, line_no, "integer-clamp", rl.strip()))

                mm = SMALL_MASK_RE.search(ml)
                if mm:
                    val = int(mm.group(1).replace("_", ""), 0)
                    lower = ml.lower()
                    if 1 < val <= 0x7F and "flags" not in lower and "form_id" not in lower:
                        findings.append((rel_path, line_no, "small-bitmask", rl.strip()))

                if ".unwrap_or(" in ml:
                    if UNWRAP_STR_DEFAULT_RE.search(rl):
                        findings.append((rel_path, line_no, "unwrap-or-fallback", rl.strip()))
                    elif not NON_ENUM_UNWRAP_RE.search(ml) and UNWRAP_INT_OR_ENUM_RE.search(ml):
                        findings.append((rel_path, line_no, "unwrap-or-fallback", rl.strip()))

        for ch in ml:
            if ch == "{":
                depth += 1
                if pending_match:
                    match_stack.append([depth, False, []])
                    pending_match = False
            elif ch == "}":
                if match_stack and depth == match_stack[-1][0]:
                    _, has_int_arm, catch_arms = match_stack.pop()
                    if has_int_arm:
                        for c_line, c_snippet in catch_arms:
                            if not has_exemption(c_line):
                                findings.append((rel_path, c_line, "match-catch-all", c_snippet))
                depth = max(0, depth - 1)

    # A clamp call split across lines (for example `.clamp(\n 0,\n 7,\n)`) is matched on the
    # whole masked text. Every line it spans is reported, so an added line anywhere in it counts.
    line_starts = [0]
    for ml in masked_lines:
        line_starts.append(line_starts[-1] + len(ml) + 1)
    seen = {(f[1], f[2]) for f in findings}
    for m in INT_MIN_CLAMP_RE.finditer(masked):
        if "\n" not in m.group(0):
            continue  # single-line calls are reported by the line scan
        first = next(n for n in range(len(line_starts) - 1) if line_starts[n + 1] > m.start()) + 1
        last = next(n for n in range(len(line_starts) - 1) if line_starts[n + 1] > m.end() - 1) + 1
        if in_test_line[first - 1] or has_exemption(first):
            continue
        for line_no in range(first, last + 1):
            if (line_no, "integer-clamp") not in seen:
                findings.append((rel_path, line_no, "integer-clamp", raw_lines[first - 1].strip()))
                seen.add((line_no, "integer-clamp"))
    return findings


def git(root: Path, *args: str) -> str:
    """Run git in `root` and return stdout, or raise AuditConfigError."""
    cmd = ["git", "-C", str(root), "-c", "core.quotePath=false", *args]
    res = subprocess.run(cmd, text=True, encoding="utf-8", errors="replace", capture_output=True, check=False)
    if res.returncode != 0:
        msg = res.stderr.strip() or res.stdout.strip()
        raise AuditConfigError(f"{' '.join(cmd)}: {msg}")
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
            m = HUNK_RE.match(raw)
            if m is None:
                raise AuditConfigError(f"cannot parse diff hunk: {raw}")
            new_line = int(m.group(1))
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


def read_base_sites_bug_count(root: Path, merge_base: str, sites_rel: str) -> int | None:
    """Return `catch-all-bug` count from `sites.json` at `merge_base`, or None if absent."""
    res = subprocess.run(
        ["git", "-C", str(root), "show", f"{merge_base}:{sites_rel}"],
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
    )
    if res.returncode != 0:
        return None
    try:
        base_data = json.loads(res.stdout)
    except ValueError as exc:
        raise AuditConfigError(f"invalid base {sites_rel} at {merge_base}: {exc}") from exc
    sites = base_data.get("sites")
    if not isinstance(sites, list):
        raise AuditConfigError(f"base {sites_rel} at {merge_base} has non-list 'sites'")
    return sum(1 for s in sites if isinstance(s, dict) and s.get("status") == "catch-all-bug")


def scan_diff(root: Path, base: str) -> tuple[str, list[tuple[str, int, str, str]]]:
    """Return `(merge_base, findings)` for added lines in scoped files since `merge_base`."""
    mb_lines = git(root, "merge-base", base, "HEAD").splitlines()
    if not mb_lines or not mb_lines[0].strip():
        raise AuditConfigError(f"{base} and HEAD have no merge base")
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

    findings = []
    for rel_path, line_numbers in sorted(new_by_file.items()):
        if not is_scoped_rust_file(rel_path):
            continue
        target = root / rel_path
        if not target.is_file():
            continue
        raw_text = target.read_text(encoding="utf-8", errors="replace")
        for hit in scan_file_for_catch_alls(rel_path, raw_text):
            if hit[1] in line_numbers:
                findings.append(hit)
    return merge_base, findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument("--sites", type=Path, help="path to sites.json")
    parser.add_argument("--baseline", type=Path, help="path to baseline.json")
    parser.add_argument("--base", help="git revision to diff and ratchet against")
    parser.add_argument("--check", action="store_true", help="validate sites, ratchet, and diff")
    parser.add_argument(
        "--audit-tree",
        action="store_true",
        help="scan all scoped production Rust files and print heuristic hits",
    )
    parser.add_argument(
        "--update-baseline",
        action="store_true",
        help="write current catch-all-bug count to baseline.json",
    )
    args = parser.parse_args(argv)

    root = args.root.resolve()
    sites_path = args.sites if args.sites else root / "tools" / "enum_audit" / "sites.json"
    baseline_path = args.baseline if args.baseline else root / "tools" / "enum_audit" / "baseline.json"
    if not sites_path.is_absolute():
        sites_path = root / sites_path
    if not baseline_path.is_absolute():
        baseline_path = root / baseline_path

    if args.audit_tree:
        all_hits = []
        for rel in scoped_rust_files(root):
            text = (root / rel).read_text(encoding="utf-8", errors="replace")
            all_hits.extend(scan_file_for_catch_alls(rel, text))
        for rel, line_no, kind, snippet in all_hits:
            print(f"{rel}:{line_no}: [{kind}] {snippet}")
        print(f"enum audit tree scan: {len(all_hits)} heuristic hit(s) across {len(scoped_rust_files(root))} files")
        return 0

    try:
        sites_data = load_json(sites_path, "sites.json")
    except AuditConfigError as exc:
        print(f"enum audit: {exc}", file=sys.stderr)
        return 2

    errors, counts = validate_sites(root, sites_data)
    if errors:
        for err in errors:
            print(f"enum audit: {err}", file=sys.stderr)
        return 1

    bug_count = counts.get("catch-all-bug", 0)

    if args.update_baseline:
        baseline_path.write_text(
            json.dumps({"format_version": 1, "catch_all_bug_count": bug_count}, indent=2) + "\n",
            encoding="utf-8",
        )
        print(f"enum audit: updated {baseline_path} with catch_all_bug_count={bug_count}")
        return 0

    try:
        baseline_data = load_json(baseline_path, "baseline.json")
    except AuditConfigError as exc:
        print(f"enum audit: {exc}", file=sys.stderr)
        return 2

    if baseline_data.get("format_version") != 1 or not isinstance(
        baseline_data.get("catch_all_bug_count"), int
    ):
        print(f"enum audit: invalid baseline format in {baseline_path}", file=sys.stderr)
        return 2

    baseline_bugs = baseline_data["catch_all_bug_count"]
    failed = False
    if bug_count > baseline_bugs:
        print(
            f"enum audit: catch-all-bug count rose from baseline {baseline_bugs} to {bug_count}",
            file=sys.stderr,
        )
        failed = True

    if args.base:
        try:
            merge_base, diff_hits = scan_diff(root, args.base)
            try:
                sites_rel = sites_path.relative_to(root).as_posix()
            except ValueError:
                sites_rel = "tools/enum_audit/sites.json"
            base_bugs = read_base_sites_bug_count(root, merge_base, sites_rel)
        except AuditConfigError as exc:
            print(f"enum audit: {exc}", file=sys.stderr)
            return 2

        if base_bugs is not None and bug_count > base_bugs:
            print(
                f"enum audit: catch-all-bug count rose above base {merge_base[:12]} ({base_bugs} -> {bug_count})",
                file=sys.stderr,
            )
            failed = True

        for rel, line_no, kind, snippet in diff_hits:
            print(
                f"{rel}:{line_no}: [{kind}] possible silent catch-all on retail data: {snippet}",
                file=sys.stderr,
            )
        if diff_hits:
            print(
                f"enum audit: {len(diff_hits)} added line(s) match silent catch-all patterns. "
                "Handle all variants explicitly, reject invalid values, or justify with "
                "`// retail-enum: <id> <reason>` on the line or line above.",
                file=sys.stderr,
            )
            failed = True

    if failed:
        return 1

    print(
        "enum audit: ok "
        f"(exhaustive={counts.get('exhaustive', 0)}, "
        f"catch-all-allowed={counts.get('catch-all-allowed', 0)}, "
        f"catch-all-bug={bug_count}/{baseline_bugs})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
