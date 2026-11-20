#!/usr/bin/env python3
"""Ratchet the count of silent error drops in production Rust code.

The count may not rise. CI compares the pull request's tree with its base commit
(`--base-root`), so a pull request is judged only by the sites it adds, and no committed
number goes stale. A justified rise is accepted when the same pull request raises that
crate's entry in `baseline.json` (`--update`, which also raises the file's `revision`).
Without `--base-root` the check compares with `baseline.json` alone, for a quick local run.

A site is one of these patterns in production code:

  let _ = ...?;            a propagated error whose value is dropped
  .ok()                    a Result error turned into None, including `.ok()?`
  .unwrap_or_default()     an error or absence replaced by a default value
  .unwrap_or("")           an error or absence replaced by an empty string
  filter_map(Result::ok)   failed items silently dropped from an iterator
  map_err(|_| "")          an error message replaced by an empty string

Each site is a candidate for review, not proof of a bug. An intended site is exempt when a
comment on the same or the previous line gives a reason after `silent-ok:`, for example:

  // silent-ok: an absent tag means this record has no tag
  let tag = record.tag().ok();

A marker with nothing after the colon is not an exemption. Comments and string literals are
blanked before matching, so the text inside them neither counts nor exempts.

Not counted, because a syntax check cannot tell the two apart: `.flatten()` on
`Result<Option<_>>` and on `Option<Option<_>>`; a bare `let _ = call();` whose call returns
`Result`; and an error replaced by a non-empty invented value.

Production code means the modules reachable from `lib.rs`, `main.rs` and `src/bin/*.rs` of
each `crates/<name>/src`, without `#[cfg(test)]` modules and items. `tests/`, `examples/`,
`benches/` and `build.rs` are not counted. The walk and the test-code exclusion are the ones
in `tools/unwrap_ratchet/unwrap_ratchet.py`, so both ratchets count the same code.

Usage:
  python3 tools/silent_failure_ratchet/silent_failure_ratchet.py --base-root ../base   # what CI runs
  python3 tools/silent_failure_ratchet/silent_failure_ratchet.py                       # check the baseline
  python3 tools/silent_failure_ratchet/silent_failure_ratchet.py --crate engine        # one crate
  python3 tools/silent_failure_ratchet/silent_failure_ratchet.py --update              # record the counts

Exit codes: 0 = at or below the limit, 1 = a count rose, 2 = usage or baseline error.
A count below the limit passes and prints a reminder (baseline mode only). Pass
`--strict` to fail instead, for a branch that must keep the baseline tight.
"""

import argparse
import importlib.util
import re
import sys
from bisect import bisect_right
from collections import Counter
from pathlib import Path

DEFAULT_BASELINE = Path("tools/silent_failure_ratchet/baseline.json")


def load_unwrap_ratchet():
    """The unwrap ratchet module, whose source walk this tool shares."""
    path = Path(__file__).resolve().parent.parent / "unwrap_ratchet" / "unwrap_ratchet.py"
    spec = importlib.util.spec_from_file_location("unwrap_ratchet_shared", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"silent failure ratchet: cannot load the source walk from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


unwrap_ratchet = load_unwrap_ratchet()
production_files = unwrap_ratchet.production_files
cfg_test_spans = unwrap_ratchet.cfg_test_spans
crate_names = unwrap_ratchet.crate_names
read_baseline = unwrap_ratchet.read_baseline
read_revision = unwrap_ratchet.read_revision
write_baseline = unwrap_ratchet.write_baseline

# `silent-ok:` must be followed by a reason on the same comment.
MARKER = re.compile(r"(?<![A-Za-z0-9_-])silent-ok:\s*\S")
# Patterns that turn a `Result` into `None`, a default value or an empty value.
PATTERNS = (
    ("ok", re.compile(r"\.\s*ok\s*\(\s*\)")),
    ("unwrap-or-default", re.compile(r"\.\s*unwrap_or_default\s*\(\s*\)")),
    (
        "empty-value",
        re.compile(
            r"\.\s*(?:unwrap_or|unwrap_or_else)\s*\(\s*(?:\|[^|]{0,80}\|\s*)?"
            r"(?:String\s*::\s*new\s*\(\s*\)|String\s*::\s*default\s*\(\s*\))\s*\)"
        ),
    ),
    (
        "filter-map-ok",
        re.compile(
            r"filter_map\s*\(\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*Result\s*::\s*ok\s*\)"
        ),
    ),
    (
        "empty-error",
        re.compile(
            r"map_err\s*\(\s*\|[^|]{0,80}\|\s*"
            r"(?:String\s*::\s*new\s*\(\s*\)|String\s*::\s*default\s*\(\s*\))\s*\)"
        ),
    ),
)
# The empty string has no characters left after masking, so these match the raw text.
# A match inside a comment or a string is rejected: the masked code has a space there.
RAW_PATTERNS = (
    ("empty-value", re.compile(r'\.\s*unwrap_or\s*\(\s*""\s*\)')),
    ("empty-value", re.compile(r'\.\s*unwrap_or_else\s*\(\s*\|[^|]{0,80}\|\s*""\s*\)')),
    ("empty-error", re.compile(r'map_err\s*\(\s*\|[^|]{0,80}\|\s*""\s*\)')),
)
# `let _ =` with an optional type annotation. The rest of the statement is checked for `?`.
DISCARDED_LET = re.compile(r"\blet\s+_\s*(?::[^=;{}]+)?=")


def identifier_char(text, index):
    return index >= 0 and (text[index].isalnum() or text[index] == "_")


def mask(text):
    """Return `(code, comments)`.

    `code` is `text` with comments, strings and char literals replaced by spaces.
    Newlines and column positions are kept, so line numbers stay valid. `comments` holds
    the `(start, end)` spans of the comments in `text`, for the `silent-ok:` markers.
    This is the masking in `tools/unwrap_ratchet/unwrap_ratchet.py`, with the comment
    spans recorded.
    """
    out = list(text)
    comments = []
    n = len(text)

    def blank(start, end):
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    i = 0
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            comments.append((i, end))
            blank(i, end)
            i = end
            continue
        if text.startswith("/*", i):
            depth, j = 0, i
            while j < n:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                    if depth == 0:
                        break
                else:
                    j += 1
            comments.append((i, j))
            blank(i, j)
            i = j
            continue
        raw = re.match(r'(?:b|c)?r(#*)"', text[i : i + 80])
        if raw and not identifier_char(text, i - 1):
            closer = '"' + raw.group(1)
            end = text.find(closer, i + raw.end())
            end = n if end < 0 else end + len(closer)
            blank(i, end)
            i = end
            continue
        if c == '"' or (
            c in "bc" and text[i + 1 : i + 2] == '"' and not identifier_char(text, i - 1)
        ):
            quote = i if c == '"' else i + 1
            j = quote + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                elif text[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            blank(quote, j)
            i = j
            continue
        if c == "'" or (
            c == "b" and text[i + 1 : i + 2] == "'" and not identifier_char(text, i - 1)
        ):
            quote = i if c == "'" else i + 1
            if text[quote + 1 : quote + 2] == "\\":
                j = quote + 2
                while j < n and text[j] not in "'\n":
                    j += 1
                if j < n and text[j] == "'":
                    blank(quote, j + 1)
                    i = j + 1
                    continue
            elif text[quote + 2 : quote + 3] == "'" and text[quote + 1 : quote + 2] != "\n":
                blank(quote, quote + 3)
                i = quote + 3
                continue
            i = quote + 1  # a lifetime such as `'a`
            continue
        i += 1
    return "".join(out), comments


def statement_start(code, index):
    """Index just after the `;`, `{` or closing bracket that starts the statement."""
    depth = 0
    for position in range(index - 1, -1, -1):
        char = code[position]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if depth == 0:
                return position + 1
            depth -= 1
        elif char == ";" and depth == 0:
            return position + 1
    return 0


def statement_end(code, index):
    """Index of the `;` that ends the statement, or of the bracket that closes its parent."""
    depth = 0
    for position in range(index, len(code)):
        char = code[position]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                return position
            depth -= 1
        elif char == ";" and depth == 0:
            return position
    return len(code)


def marker_lines(text, comments, line_of):
    """0-based line numbers of `silent-ok:` comments that give a reason."""
    lines = set()
    for start, end in comments:
        for match in MARKER.finditer(text, start, end):
            lines.add(line_of(match.start()))
    return lines


def sites_in_file(path):
    """The silent-failure sites in one file, as `(label, 1-based line, line text)`.

    Sites under a test-only `#[cfg(...)]` or a `#[test]` function are not sites.
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    code, comments = mask(text)
    line_starts = [0]
    for index, char in enumerate(text):
        if char == "\n":
            line_starts.append(index + 1)

    source_lines = text.splitlines()

    def line_of(position):
        return bisect_right(line_starts, position) - 1

    def line_text(line):
        return source_lines[line].strip()[:160] if line < len(source_lines) else ""

    marked = marker_lines(text, comments, line_of)
    skipped = bytearray(len(code))
    for start, end in cfg_test_spans(code):
        skipped[start : end + 1] = b"\x01" * (end + 1 - start)

    sites = []
    discarded = []
    for match in DISCARDED_LET.finditer(code):
        # `if let _ =` and `while let _ =` are conditions, not discarded values.
        if code[statement_start(code, match.start()) : match.start()].strip():
            continue
        end = statement_end(code, match.end())
        question = code.find("?", match.end(), end)
        if question >= 0:
            discarded.append((match.start(), end))

    def add(label, position, skip_discarded=False):
        line = line_of(position)
        if skipped[position] or line in marked or line - 1 in marked:
            return
        if skip_discarded and any(start <= position <= end for start, end in discarded):
            return
        sites.append((label, line + 1, line_text(line)))

    for label, pattern in PATTERNS:
        for match in pattern.finditer(code):
            add(label, match.start(), skip_discarded=True)
    for label, pattern in RAW_PATTERNS:
        for match in pattern.finditer(text):
            if code[match.start()] != " ":  # not in a comment or a string
                add(label, match.start(), skip_discarded=True)
    for start, end in discarded:
        question = code.find("?", start, end)
        if question >= 0:
            add("discarded-try", question)
    sites.sort()
    return sites


def count_file(path):
    return len(sites_in_file(path))


def count_crate(root, crate):
    return sum(count_file(path) for path in production_files(root / "crates" / crate))


def crate_sites(root, crate):
    """The sites of one crate as `(repository path, label, line, line text)`."""
    sites = []
    for path in production_files(root / "crates" / crate):
        try:
            relative = path.relative_to(root).as_posix()
        except ValueError:  # a `#[path]` outside the repository root
            relative = path.as_posix()
        for label, line, text in sites_in_file(path):
            sites.append((relative, label, line, text))
    return sites


def limits_from_base(root, base_root, names, measured):
    """Per crate, the count a pull request may not exceed: the count at its base commit.

    A crate that the base lacks has no limit and is reported as new. A pull request accepts
    more sites by running `--update`, which writes the measured counts and raises the
    file's `revision`. A crate's entry then raises its limit when the pull request changed
    the entry, or when the revision changed and the entry equals the measured count. An
    entry the pull request left alone never raises a limit, so a stale baseline cannot hide
    a rise or cause a false failure.
    """
    try:
        head = read_baseline(root / DEFAULT_BASELINE)
    except (OSError, ValueError, KeyError):
        head = {}
    try:
        base = read_baseline(base_root / DEFAULT_BASELINE)
    except (OSError, ValueError, KeyError):
        base = {}
    revised = read_revision(root / DEFAULT_BASELINE) != read_revision(base_root / DEFAULT_BASELINE)
    limits = {}
    for name in names:
        if not (base_root / "crates" / name / "src").is_dir():
            continue
        limit = count_crate(base_root, name)
        if name in head:
            changed = head[name] != base.get(name)
            if changed or (revised and head[name] == measured[name]):
                limit = max(limit, head[name])
        limits[name] = limit
    return limits


def added_sites(root, base_root, crate):
    """Sites in the head tree that are not in the base tree, as `(path, label, text)`.

    The line number is left out, so a shift does not report every site below it. A site
    that moved with the same text and label counts as unchanged.
    """

    def keyed(tree):
        return Counter(
            (relative, label, text) for relative, label, _, text in crate_sites(tree, crate)
        )

    extra = keyed(root) - keyed(base_root)
    return sorted(extra.elements())


def report_added(sites, limit=12):
    print("New sites:", file=sys.stderr)
    for relative, label, text in sites[:limit]:
        print(f"  {relative}: {label}: {text}", file=sys.stderr)
    if len(sites) > limit:
        print(f"  and {len(sites) - limit} more", file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--crate", help="check one crate; the default is every crate")
    parser.add_argument("--baseline", type=Path, help=f"default: {DEFAULT_BASELINE}")
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument(
        "--base-root",
        type=Path,
        help="a checkout of the base commit; a count may not rise above its count there",
    )
    parser.add_argument("--update", action="store_true", help="write the measured counts")
    parser.add_argument("--strict", action="store_true", help="also fail when a count fell")
    args = parser.parse_args(argv)

    root = args.root.resolve()
    baseline_path = args.baseline or root / DEFAULT_BASELINE
    names = [args.crate] if args.crate else crate_names(root)
    for name in names:
        if not (root / "crates" / name / "src").is_dir():
            print(f"silent failure ratchet: no crate named {name}", file=sys.stderr)
            return 2
    measured = {name: count_crate(root, name) for name in names}

    if args.update:
        try:
            counts = read_baseline(baseline_path)
        except (OSError, ValueError, KeyError):
            counts = {}
        counts.update(measured)
        write_baseline(baseline_path, counts, read_revision(baseline_path) + 1)
        print(f"silent failure ratchet: wrote {baseline_path}")
        return 0

    if args.base_root:
        base_root = args.base_root.resolve()
        if not (base_root / "crates").is_dir():
            print(f"silent failure ratchet: no crates directory in {base_root}", file=sys.stderr)
            return 2
        allowed = limits_from_base(root, base_root, names, measured)
    else:
        try:
            allowed = read_baseline(baseline_path)
        except (OSError, ValueError, KeyError) as error:
            print(
                f"silent failure ratchet: cannot read baseline {baseline_path}: {error}",
                file=sys.stderr,
            )
            return 2

    raised, lowered, unrecorded = [], [], []
    for name in names:
        limit = allowed.get(name)
        if limit is None:
            print(f"{name:16} {measured[name]:5} / {'-':>5}  new crate, no limit")
            unrecorded.append(name)
            continue
        state = "ok"
        if measured[name] > limit:
            state = "RAISED"
            raised.append(name)
        elif measured[name] < limit:
            state = "below base" if args.base_root else "below baseline"
            lowered.append(name)
        print(f"{name:16} {measured[name]:5} / {limit:5}  {state}")
    if unrecorded:
        print(
            "silent failure ratchet: no limit for "
            + ", ".join(unrecorded)
            + " (a new crate, or no baseline entry). The count is shown above and passes. "
            "Record it with `python3 tools/silent_failure_ratchet/silent_failure_ratchet.py --update`.",
            file=sys.stderr,
        )
    if raised:
        print(
            "silent failure ratchet: a silent-failure count rose in "
            + ", ".join(raised)
            + (" above its base" if args.base_root else " above the baseline")
            + ". Return the error with `?`, log it, or handle it. If the drop is intended, "
            "put `// silent-ok: <reason>` on the same or the previous line.",
            file=sys.stderr,
        )
        if args.base_root:
            report_added(
                [site for name in raised for site in added_sites(root, base_root, name)]
            )
        print(
            "To accept a rise that has no marker, run "
            "`python3 tools/silent_failure_ratchet/silent_failure_ratchet.py --update` "
            "(it raises that crate's entry in `baseline.json` and its `revision`) and explain "
            "the reason in the pull request.",
            file=sys.stderr,
        )
        return 1
    if lowered and not args.base_root:
        message = (
            "silent failure ratchet: the count fell in "
            + ", ".join(lowered)
            + ". Lower the baseline with "
            "`python3 tools/silent_failure_ratchet/silent_failure_ratchet.py --update`."
        )
        print(message, file=sys.stderr)
        if args.strict:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
