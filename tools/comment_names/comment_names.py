#!/usr/bin/env python3
"""Check that a comment line a change adds names no symbol that the code does not have.

A comment in `crates/**/*.rs` or `*.wgsl` that writes a snake_case name in backticks, such as
`spawn_cell` or [`module::spawn_cell`], names a symbol. This check reports a name that occurs
nowhere in the tracked code, strings included. CI checks only the comment lines that the change
adds or edits since its merge base (`--base`). A line that only moved does not count.

A name from outside the repository (Bevy, a crate, a retail file) is exempt when a comment on
the same or the previous line says where it is from:

  // external-name: Bevy's render system
  /// Runs before `queue_shadows`.

Not checked: an unchanged comment whose symbol was renamed or removed (run `--audit` for
that), names without an underscore, CamelCase names, names without backticks, block comments,
untracked files under `--base`, and a stale name that still occurs somewhere else in the code.

Usage:
  python3 tools/comment_names/comment_names.py --base origin/main  # what CI runs
  python3 tools/comment_names/comment_names.py --audit             # every name in the tree

Exit codes: 0 = no finding (or an audit), 1 = an added line has a finding, 2 = usage or Git error.
"""

import argparse
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

SOURCE_ROOTS = ("crates", "tools", "scripts", "vendor", "fuzz")
SOURCE_SUFFIXES = {".rs", ".wgsl", ".py", ".sql", ".sh", ".ps1", ".toml", ".json"}
MAX_BYTES = 3_000_000
# This check's tests quote stale names. Counting them as code would hide those names.
OWN_SOURCES = "tools/comment_names/"
# `//` for Rust and WGSL, `#` for the others. `#[` and `#!` are Rust attributes.
TRAILING_COMMENT = {
    True: re.compile(r"(^|\s)//.*$"),
    False: re.compile(r"(^|\s)#(?![\[!]).*$"),
}
COMMENT_LINE = re.compile(r"^\s*//(.*)$")
NAMED = re.compile(r"`(?:[A-Za-z_][A-Za-z0-9_]*::)*([a-z][a-z0-9]*(?:_[a-z0-9]+)+)(?:\(\))?`")
MARKER = re.compile(r"(?<![A-Za-z0-9_-])external-name:\s*\S")
WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
HUNK = re.compile(r"^@@ -\S+ \+(\d+)")


def git(root, *args):
    return subprocess.check_output(
        ["git", "-C", str(root), "-c", "core.quotePath=false", *args],
        text=True, encoding="utf-8", errors="replace")


def tracked_sources(root):
    """Tracked and new, not ignored, source files under `root`, as relative paths."""
    listed = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard",
                 "--", *SOURCE_ROOTS)
    for relative in sorted(set(filter(None, listed.split("\0")))):
        if relative.startswith(OWN_SOURCES):
            continue
        path = root / relative
        if path.suffix in SOURCE_SUFFIXES and path.is_file() and path.stat().st_size < MAX_BYTES:
            yield relative


def added_lines(root, base):
    """`{(path, line number)}` of lines that the tree adds since its merge base with `base`.

    A line whose exact text the change also removes somewhere is a move, so it is left out.
    """
    diff = git(root, "diff", "-U0", "-M", "--no-color", "--no-ext-diff", "--src-prefix=a/",
               "--dst-prefix=b/", "--merge-base", base, "--", "crates")
    added = []
    removed = Counter()
    path = number = None
    for line in diff.split("\n"):
        if line.startswith("diff "):
            path = number = None
        elif number is None and line.startswith("+++ "):
            path = line[6:].rstrip("\t") if line.startswith("+++ b/") else None
        elif line.startswith("@@"):
            number = int(HUNK.match(line)[1])
        elif number is not None and line.startswith("+"):
            added.append((path, number, line[1:].strip()))
            number += 1
        elif number is not None and line.startswith("-"):
            removed[line[1:].strip()] += 1
    result = set()
    for path, number, text in added:
        if removed[text] > 0:
            removed[text] -= 1
        else:
            result.add((path, number))
    return result


def unresolved(root):
    """`[(path, line, name)]` for every comment name that the code does not have."""
    words = set()
    mentions = []
    for relative in tracked_sources(root):
        text = (root / relative).read_text(encoding="utf-8", errors="replace")
        slashes = relative.endswith((".rs", ".wgsl"))
        checked = slashes and relative.startswith("crates/")
        previous_marked = False
        for number, line in enumerate(text.splitlines(), 1):
            words.update(WORD.findall(TRAILING_COMMENT[slashes].sub("", line)))
            if not checked:
                continue
            comment = COMMENT_LINE.match(line)
            marked = bool(MARKER.search(line))
            if comment and not marked and not previous_marked:
                mentions.extend((relative, name, number) for name in NAMED.findall(comment[1]))
            previous_marked = marked
    return sorted({(relative, number, name) for relative, name, number in mentions
                   if name not in words})


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--base", help="check the lines added since the merge base with this revision")
    mode.add_argument("--audit", action="store_true", help="list every finding in the tree")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    if not (root / "crates").is_dir():
        print(f"comment names: no crates directory in {root}", file=sys.stderr)
        return 2
    try:
        found = unresolved(root)
        if args.base:
            added = added_lines(root, args.base)
            found = [row for row in found if row[:2] in added]
    except subprocess.CalledProcessError as error:
        print(f"comment names: git failed: {error}", file=sys.stderr)
        return 2
    for relative, number, name in found:
        print(f"{relative}:{number}: `{name}` is not in the code")
    if args.base and found:
        print("comment names: correct the name, or if it is from outside the repository, put "
              "`// external-name: <source>` on the line above it.", file=sys.stderr)
        return 1
    print(f"comment names: {len(found)} {'on added lines' if args.base else 'in the tree'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
