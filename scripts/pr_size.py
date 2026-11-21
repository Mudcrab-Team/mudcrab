#!/usr/bin/env python3
"""Sum the changed lines of a pull request for the advisory PR size label.

Input: a JSON file with the array that `GET /repos/{owner}/{repo}/pulls/{number}/files`
returns, one object per file with `filename`, `additions` and `deletions`. The script
never contacts GitHub, so the same input can be tested without a repository.

Output: the number of changed hand-written lines and one of the labels `size: L`
(more than 2,000 lines) or `size: XL` (more than 5,000 lines), or no label.

Test and generated files do not count. Their full list is `EXCLUDED_PATTERNS` below;
keep the comment and the list together, because a new generated directory belongs here.
"""

import argparse
import fnmatch
import json
import sys
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from typing import Optional

SIZE_L_THRESHOLD = 2_000
SIZE_XL_THRESHOLD = 5_000
LABEL_L = "size: L"
LABEL_XL = "size: XL"

# GitHub returns at most 3,000 files for a pull request. A full page can be truncated,
# so the count can be short; the caller warns instead of presenting it as the total.
GITHUB_MAX_FILES = 3_000

# One place for every path that does not count towards review size.
# Tests and fixtures first, then generated, vendored or recorded files.
EXCLUDED_PATTERNS = (
    # Test code, test data and snapshots.
    "tests/**",
    "**/tests/**",
    "*_tests.rs",
    "test_*.py",
    "**/test_*.py",
    "snapshots/**",
    "**/snapshots/**",
    "**/fixtures/**",
    # Generated, vendored or recorded files that a reviewer does not read line by line.
    "Cargo.lock",
    "devenv.lock",
    "vendor/**",
    "fuzz/seeds/**",
    "docs/data-inventory/**",
    "docs/research/evidence/**",
    "docs/evidence/**",
    "tools/skyrim-recorder/reference/**",
)


@dataclass(frozen=True)
class PrSize:
    """The counted and excluded line totals of one pull request."""

    lines: int
    counted_files: int
    excluded_files: int
    excluded_lines: int
    label: Optional[str]


def label_for(lines: int) -> Optional[str]:
    """Return the size label for a line count, or None below the L threshold."""
    if lines > SIZE_XL_THRESHOLD:
        return LABEL_XL
    if lines > SIZE_L_THRESHOLD:
        return LABEL_L
    return None


def is_excluded(filename: str) -> bool:
    """Return True when a changed path is a test, generated or recorded file."""
    normalized = filename.strip("/")
    if not normalized:
        raise ValueError("file entry has an empty filename")
    return any(fnmatch.fnmatchcase(normalized, pattern) for pattern in EXCLUDED_PATTERNS)


def _line_count(entry: Mapping[str, object], key: str, filename: str) -> int:
    value = entry.get(key)
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"file entry for {filename!r} has no valid {key}")
    return value


def summarize(entries: Iterable[Mapping[str, object]]) -> PrSize:
    """Sum additions and deletions, skipping test and generated files."""
    lines = 0
    counted_files = 0
    excluded_files = 0
    excluded_lines = 0

    for entry in entries:
        if not isinstance(entry, Mapping):
            raise ValueError("each file entry must be a JSON object")
        filename = entry.get("filename")
        if not isinstance(filename, str) or not filename:
            raise ValueError("file entry has no filename")
        changed = _line_count(entry, "additions", filename) + _line_count(
            entry, "deletions", filename
        )
        if is_excluded(filename):
            excluded_files += 1
            excluded_lines += changed
        else:
            counted_files += 1
            lines += changed

    return PrSize(
        lines=lines,
        counted_files=counted_files,
        excluded_files=excluded_files,
        excluded_lines=excluded_lines,
        label=label_for(lines),
    )


def _read_entries(path: str) -> object:
    if path == "-":
        return json.load(sys.stdin)
    with open(path, encoding="utf-8") as source:
        return json.load(source)


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--files",
        default="-",
        help="JSON file list from the GitHub API; '-' reads standard input",
    )
    parser.add_argument(
        "--github-output",
        help="append lines and label to this file, for GitHub Actions $GITHUB_OUTPUT",
    )
    args = parser.parse_args(argv)

    try:
        entries = _read_entries(args.files)
        if not isinstance(entries, list):
            raise ValueError("the file list must be a JSON array")
        result = summarize(entries)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"pr_size: error: {error}", file=sys.stderr)
        return 2

    total_files = result.counted_files + result.excluded_files
    truncated = total_files >= GITHUB_MAX_FILES
    if truncated:
        print(
            f"pr_size: warning: the API returned its maximum of {GITHUB_MAX_FILES} "
            "files; the count may be short",
            file=sys.stderr,
        )

    print(
        f"pr_size: {result.lines} changed hand-written lines in "
        f"{result.counted_files} files; excluded {result.excluded_files} test or "
        f"generated files ({result.excluded_lines} lines); "
        f"label: {result.label or 'none'}"
    )

    if args.github_output:
        values = (
            ("lines", str(result.lines)),
            ("counted_files", str(result.counted_files)),
            ("excluded_files", str(result.excluded_files)),
            ("excluded_lines", str(result.excluded_lines)),
            ("label", result.label or ""),
            ("truncated", "true" if truncated else "false"),
        )
        try:
            with open(args.github_output, "a", encoding="utf-8") as output:
                for name, value in values:
                    output.write(f"{name}={value}\n")
        except OSError as error:
            print(
                f"pr_size: error: cannot write {args.github_output}: {error}",
                file=sys.stderr,
            )
            return 2

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
