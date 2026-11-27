#!/usr/bin/env python3
"""Flag new unmarked sRGB conversions of retail record colours and ratchet existing sites.

Under the `skyrim` visual profile (SPEC-1 section 1, `docs/specs/engine/near-lighting.md`),
retail record RGB bytes are read as raw `byte / 255.0` without sRGB decoding. Decoding them
through `Color::srgb*`, `.to_linear()`, `LinearRgba::from()`, WGSL `srgb_to_linear()`, or an
sRGB GPU texture format darkens midtones and causes colour shifts (such as the red-windows bug).

This tool checks scoped Rust and WGSL files listed in `tools/colour_space/scope.json`:
1. Added-line gate (`--base <rev>`): flags new hits on added non-moved lines in scope unless
   marked with `// colour-space: <spec ref or "mudcrab-only" or "ui">` on the line or the
   line immediately above it.
2. Full-tree ratchet (`--ratchet`): counts unmarked hits in production code in scope across
   the tree and fails if the total rises above `tools/colour_space/baseline.json` (or above
   the base commit's baseline when `--base` is also supplied).

Usage:
  python3 tools/colour_space/colour_space.py --base origin/main --ratchet
  python3 tools/colour_space/colour_space.py --ratchet
  python3 tools/colour_space/colour_space.py --audit
  python3 tools/colour_space/colour_space.py --update-baseline

Exit codes: 0 = pass, 1 = new unmarked hit or baseline exceeded, 2 = usage, config, or git error.
"""

import argparse
import importlib.util
import json
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

DEFAULT_SCOPE = Path("tools/colour_space/scope.json")
DEFAULT_BASELINE = Path("tools/colour_space/baseline.json")
HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")
MARKER_RE = re.compile(r"^//[^\n]*?(?<![A-Za-z0-9_-])colour-space:[ \t]*\S")
WGSL_FN_PREFIX_RE = re.compile(r"\bfn\s+$")

RUST_PATTERNS = (
    ("Color::srgb_u8(", re.compile(r"(?<![A-Za-z0-9_])Color\s*::\s*srgb_u8\s*\(")),
    ("Color::srgba(", re.compile(r"(?<![A-Za-z0-9_])Color\s*::\s*srgba\s*\(")),
    ("Color::srgb(", re.compile(r"(?<![A-Za-z0-9_])Color\s*::\s*srgb\s*\(")),
    (".to_linear()", re.compile(r"\.\s*to_linear\s*\(")),
    ("LinearRgba::from(", re.compile(r"(?<![A-Za-z0-9_])LinearRgba\s*::\s*from\s*\(")),
    (
        "Srgb texture format",
        re.compile(
            r"(?<![A-Za-z0-9_])(?:TextureFormat\s*::\s*[A-Za-z0-9_]*Srgb|[A-Za-z0-9_]*UnormSrgb)(?![A-Za-z0-9_])"
        ),
    ),
)

WGSL_PATTERNS = (
    ("srgb_to_linear(", re.compile(r"\bsrgb_to_linear\s*\(")),
    ("sky_srgb_to_linear(", re.compile(r"\bsky_srgb_to_linear\s*\(")),
)

TEST_PATH_PARTS = {"tests", "benches"}


def load_unwrap_ratchet():
    """Load `unwrap_ratchet.py` so comment/string masking and `cfg(test)` detection match."""
    path = Path(__file__).resolve().parent.parent / "unwrap_ratchet" / "unwrap_ratchet.py"
    spec = importlib.util.spec_from_file_location("unwrap_ratchet_shared", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"colour-space: cannot load unwrap_ratchet from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


unwrap_ratchet = load_unwrap_ratchet()
mask_with_comments = unwrap_ratchet.mask_with_comments
cfg_test_spans = unwrap_ratchet.cfg_test_spans
inner_cfg_requires_test = unwrap_ratchet.inner_cfg_requires_test


class ToolError(Exception):
    """Configuration, baseline, or git failure."""


@dataclass(frozen=True)
class Hit:
    """One unmarked sRGB conversion or format site in scope."""

    path: str
    line: int
    column: int
    pattern: str
    text: str


def glob_to_regex(pattern):
    """Convert a POSIX glob pattern into an anchored regex matching relative paths."""
    parts = ["^"]
    i = 0
    n = len(pattern)
    while i < n:
        if pattern.startswith("**/", i):
            parts.append("(?:.+/)?")
            i += 3
        elif pattern.startswith("**", i):
            parts.append(".*")
            i += 2
        elif pattern[i] == "*":
            parts.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            parts.append("[^/]")
            i += 1
        else:
            parts.append(re.escape(pattern[i]))
            i += 1
    parts.append("$")
    return re.compile("".join(parts))


def read_scope(path):
    """Load scope glob patterns from `scope.json`."""
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ToolError(f"cannot read scope config {path}: {error}") from error
    if not isinstance(data, dict) or not isinstance(data.get("paths"), list):
        raise ToolError(f"invalid scope config {path}: expected object with 'paths' list")
    patterns = [p for p in data["paths"] if isinstance(p, str) and p.strip()]
    if len(patterns) != len(data["paths"]) or not patterns:
        raise ToolError(f"invalid scope config {path}: 'paths' must be a non-empty list of strings")
    return patterns


def resolve_scope_path(root, explicit_scope):
    """Locate `scope.json` under `root`, or fall back to the bundled config."""
    if explicit_scope is not None:
        return explicit_scope
    candidate = root / DEFAULT_SCOPE
    if candidate.is_file():
        return candidate
    return Path(__file__).resolve().with_name("scope.json")


def is_test_rust_path(rel_path):
    """Skip dedicated Rust test files if a broad glob pulls them into scope."""
    parts = Path(rel_path).parts
    if any(part in TEST_PATH_PARTS for part in parts[:-1]):
        return True
    name = parts[-1]
    return name == "tests.rs" or name.endswith("_tests.rs")


def scoped_files(root, patterns):
    """Sorted list of existing files under `root` matching `patterns`."""
    regexes = [glob_to_regex(pat) for pat in patterns]
    matched = set()
    for pat in patterns:
        for path in root.glob(pat):
            if path.is_file():
                rel = path.relative_to(root).as_posix()
                if any(rx.match(rel) for rx in regexes):
                    matched.add(rel)
    return sorted(matched)


def line_offsets(text):
    """1-based line start offsets for `text`."""
    starts = [0]
    for line in text.splitlines(keepends=True):
        starts.append(starts[-1] + len(line))
    return starts


def marked_lines(text, comments):
    """1-based line numbers containing a valid `// colour-space: <reason>` comment."""
    starts = line_offsets(text)
    marked = set()
    for start, end in comments:
        snippet = text[start:end]
        if not snippet.startswith("//"):
            continue
        if MARKER_RE.match(snippet):
            # Map character offset `start` to 1-based line index.
            lo, hi = 0, len(starts) - 2
            while lo <= hi:
                mid = (lo + hi) // 2
                if starts[mid] <= start < starts[mid + 1]:
                    marked.add(mid + 1)
                    break
                if start < starts[mid]:
                    hi = mid - 1
                else:
                    lo = mid + 1
    return marked


def scan_file(root, rel_path):
    """Return all unmarked `Hit` instances in `rel_path`."""
    if rel_path.endswith(".rs"):
        if is_test_rust_path(rel_path):
            return []
        patterns = RUST_PATTERNS
        is_wgsl = False
    elif rel_path.endswith(".wgsl"):
        patterns = WGSL_PATTERNS
        is_wgsl = True
    else:
        return []

    path = root / rel_path
    if not path.is_file():
        return []
    text = path.read_text(encoding="utf-8", errors="replace")
    code, comments = mask_with_comments(text)

    if not is_wgsl:
        if inner_cfg_requires_test(code):
            return []
        skipped = bytearray(len(code))
        for start, end in cfg_test_spans(code):
            skipped[start : end + 1] = b"\x01" * (end + 1 - start)
    else:
        skipped = None

    marked = marked_lines(text, comments)
    raw_lines = text.splitlines()
    code_lines = code.splitlines()
    starts = line_offsets(code)
    hits = []

    for line_no, code_line in enumerate(code_lines, 1):
        if skipped is not None and starts[line_no - 1] < len(skipped) and skipped[starts[line_no - 1]]:
            continue
        if line_no in marked or (line_no - 1) in marked:
            continue
        line_hits = []
        occupied = []
        for label, regex in patterns:
            for match in regex.finditer(code_line):
                span = (match.start(), match.end())
                if any(not (span[1] <= s or span[0] >= e) for s, e in occupied):
                    continue
                if is_wgsl and WGSL_FN_PREFIX_RE.search(code_line[: match.start()]):
                    continue
                occupied.append(span)
                line_hits.append(
                    Hit(
                        path=rel_path,
                        line=line_no,
                        column=match.start() + 1,
                        pattern=label,
                        text=raw_lines[line_no - 1].strip(),
                    )
                )
        line_hits.sort(key=lambda item: item.column)
        hits.extend(line_hits)
    return hits


def git(root, *args):
    """Run git in `root` and return stdout, raising ToolError on failure."""
    command = ["git", "-C", str(root), "-c", "core.quotePath=false", *args]
    result = subprocess.run(
        command,
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
    )
    if result.returncode:
        message = result.stderr.strip() or result.stdout.strip()
        raise ToolError(f"{' '.join(command)}: {message}")
    return result.stdout


def parse_diff(text):
    """Parse `git diff -U0` output into added `{path: {line_no: stripped}}` and removed Counter."""
    added = {}
    removed = Counter()
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
                raise ToolError(f"cannot parse diff hunk: {raw}")
            new_line = int(match.group(1))
            in_hunk = True
        elif in_hunk and new_line is not None:
            if raw.startswith("+"):
                if path is not None:
                    added[path][new_line] = raw[1:].strip()
                new_line += 1
            elif raw.startswith("-"):
                removed[raw[1:].strip()] += 1
            elif raw.startswith("\\"):
                continue
            else:
                new_line += 1
    return added, removed


def added_lines(root, base):
    """Added `{path: set(line_numbers)}` since merge-base with `base`, excluding moved lines."""
    merge_base = git(root, "merge-base", base, "HEAD").strip()
    if not merge_base:
        raise ToolError(f"{base} and HEAD have no merge base")
    diff = git(
        root,
        "diff",
        "--unified=0",
        "-M",
        "-C",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        merge_base,
    )
    added, removed = parse_diff(diff)
    kept = {}
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
    return kept, merge_base


def read_baseline_text(raw, source):
    """Parse baseline JSON into `(unmarked_total, counts)`."""
    try:
        data = json.loads(raw)
    except ValueError as error:
        raise ToolError(f"invalid baseline JSON in {source}: {error}") from error
    if not isinstance(data, dict) or not isinstance(data.get("unmarked_total"), int):
        raise ToolError(f"invalid baseline {source}: expected integer 'unmarked_total'")
    total = data["unmarked_total"]
    if total < 0:
        raise ToolError(f"invalid baseline {source}: 'unmarked_total' must be non-negative")
    counts = data.get("counts", {})
    if not isinstance(counts, dict):
        raise ToolError(f"invalid baseline {source}: 'counts' must be an object")
    return total, counts


def read_baseline(path):
    """Read `baseline.json` from disk."""
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as error:
        raise ToolError(f"cannot read baseline {path}: {error}") from error
    return read_baseline_text(raw, path)


def read_base_baseline(root, merge_base, rel_baseline):
    """Read `baseline.json` from `merge_base` if it exists there, else None."""
    result = subprocess.run(
        ["git", "-C", str(root), "show", f"{merge_base}:{rel_baseline}"],
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    return read_baseline_text(result.stdout, f"{merge_base}:{rel_baseline}")[0]


def write_baseline(path, counts, total):
    """Write `baseline.json` with LF newlines."""
    document = {
        "format_version": 1,
        "unmarked_total": total,
        "counts": {key: counts[key] for key in sorted(counts) if counts[key] > 0},
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(json.dumps(document, indent=2) + "\n")


def scan_scope(root, patterns):
    """Return `(all_hits, per_file_counts, files)` across `patterns`."""
    files = scoped_files(root, patterns)
    all_hits = []
    counts = {}
    for rel_path in files:
        file_hits = scan_file(root, rel_path)
        all_hits.extend(file_hits)
        counts[rel_path] = len(file_hits)
    return all_hits, counts, files


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument("--scope", type=Path, help=f"scope config JSON (default: {DEFAULT_SCOPE})")
    parser.add_argument(
        "--baseline",
        type=Path,
        help=f"baseline JSON for full-tree ratchet (default: {DEFAULT_BASELINE})",
    )
    parser.add_argument(
        "--base",
        help="git revision to compare added lines against (merge-base with HEAD)",
    )
    parser.add_argument(
        "--ratchet",
        action="store_true",
        help="check full-tree unmarked count against baseline.json",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="with --ratchet, also fail when unmarked count fell below baseline.json",
    )
    parser.add_argument(
        "--audit",
        action="store_true",
        help="print every unmarked hit in scope and exit 0",
    )
    parser.add_argument(
        "--update-baseline",
        action="store_true",
        help="write current unmarked counts in scope to baseline.json",
    )
    args = parser.parse_args(argv)

    if not any((args.base, args.ratchet, args.audit, args.update_baseline)):
        args.ratchet = True

    root = args.root.resolve()
    scope_path = resolve_scope_path(root, args.scope)
    if args.baseline is not None:
        baseline_path = args.baseline
        rel_baseline = DEFAULT_BASELINE.as_posix()
    else:
        baseline_path = root / DEFAULT_BASELINE
        rel_baseline = DEFAULT_BASELINE.as_posix()

    try:
        patterns = read_scope(scope_path)
        all_hits, counts, files = scan_scope(root, patterns)
    except ToolError as error:
        print(f"colour-space: {error}", file=sys.stderr)
        return 2

    total = len(all_hits)

    if args.update_baseline:
        write_baseline(baseline_path, counts, total)
        print(f"colour-space: wrote {baseline_path} (unmarked_total={total}, {len(files)} files)")
        return 0

    if args.audit:
        for hit in all_hits:
            print(f"{hit.path}:{hit.line}:{hit.column}: [{hit.pattern}] {hit.text}")
        print(f"colour-space: {total} unmarked hit(s) across {len(files)} scoped file(s)")
        return 0

    failed = False
    merge_base = None

    if args.base:
        try:
            added, merge_base = added_lines(root, args.base)
        except ToolError as error:
            print(f"colour-space: {error}", file=sys.stderr)
            return 2

        regexes = [glob_to_regex(pat) for pat in patterns]
        added_hits = []
        for rel_path in sorted(added):
            if not any(rx.match(rel_path) for rx in regexes):
                continue
            line_set = added[rel_path]
            for hit in scan_file(root, rel_path):
                if hit.line in line_set:
                    added_hits.append(hit)

        if added_hits:
            failed = True
            for hit in added_hits:
                print(
                    f"{hit.path}:{hit.line}:{hit.column}: unmarked [{hit.pattern}] on added line: {hit.text}",
                    file=sys.stderr,
                )
            print(
                f"colour-space: {len(added_hits)} new unmarked sRGB conversion(s) on added lines. "
                "Record colours are raw byte/255 under `skyrim` (SPEC-1). Remove the decode, "
                "or justify it on the line or line above with "
                "`// colour-space: <spec ref or \"mudcrab-only\" or \"ui\">`.",
                file=sys.stderr,
            )
        else:
            print(
                f"colour-space: 0 new unmarked sRGB conversion(s) on added lines "
                f"({total} existing unmarked in scope)."
            )

    if args.ratchet:
        try:
            limit, _ = read_baseline(baseline_path)
        except ToolError as error:
            print(f"colour-space: {error}", file=sys.stderr)
            return 2

        if merge_base is not None:
            try:
                base_limit = read_base_baseline(root, merge_base, rel_baseline)
            except ToolError as error:
                print(f"colour-space: {error}", file=sys.stderr)
                return 2
            if base_limit is not None and limit > base_limit:
                failed = True
                print(
                    f"colour-space: baseline.json unmarked_total rose from {base_limit} to {limit}; "
                    "the ratchet count may only go down.",
                    file=sys.stderr,
                )

        if total > limit:
            failed = True
            print(
                f"colour-space: unmarked count {total} exceeds baseline {limit}. "
                "Mark justified sites with `// colour-space: ...` or remove the decode.",
                file=sys.stderr,
            )
        elif total < limit:
            msg = (
                f"colour-space: unmarked count fell from {limit} to {total}; "
                "lower the baseline with `python3 tools/colour_space/colour_space.py --update-baseline`."
            )
            if args.strict:
                failed = True
                print(msg, file=sys.stderr)
            else:
                print(msg)
        else:
            print(f"colour-space: ratchet ok ({total} / {limit} unmarked across {len(files)} files)")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
