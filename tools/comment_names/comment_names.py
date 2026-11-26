#!/usr/bin/env python3
"""Ratchet comments that name a symbol the code does not have.

A comment in `crates/**/*.rs` or `*.wgsl` that writes a snake_case name in backticks, such as
`spawn_cell` or [`module::spawn_cell`], names a symbol. This check reports a name that occurs
nowhere in the tracked code, strings included. CI compares the tree with its base commit
(`--base-root`), so it reports a new comment and also an old comment whose symbol was renamed
or removed. It does not judge the names that the base already lacked.

A name from outside the repository (Bevy, a crate, a retail file) is exempt when a comment on
the same or the previous line says where it is from:

  // external-name: Bevy's render system
  /// Runs before `queue_shadows`.

Not checked: names without an underscore, CamelCase names, names without backticks, block
comments, and a stale name that still occurs somewhere else in the code.

Usage:
  python3 tools/comment_names/comment_names.py --base-root ../base   # what CI runs
  python3 tools/comment_names/comment_names.py                       # list every unresolved name

Exit codes: 0 = no new name (or a listing), 1 = new unresolved names, 2 = usage error.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

SOURCE_ROOTS = ("crates", "tools", "scripts", "vendor", "fuzz")
SOURCE_SUFFIXES = {".rs", ".wgsl", ".py", ".sql", ".sh", ".ps1", ".toml", ".json"}
MAX_BYTES = 3_000_000
# `//` for Rust and WGSL, `#` for the others. `#[` and `#!` are Rust attributes.
TRAILING_COMMENT = {
    True: re.compile(r"(^|\s)//.*$"),
    False: re.compile(r"(^|\s)#(?![\[!]).*$"),
}
COMMENT_LINE = re.compile(r"^\s*//(.*)$")
NAMED = re.compile(r"`(?:[A-Za-z_][A-Za-z0-9_]*::)*([a-z][a-z0-9]*(?:_[a-z0-9]+)+)(?:\(\))?`")
MARKER = re.compile(r"(?<![A-Za-z0-9_-])external-name:\s*\S")
WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def tracked_sources(root):
    """Tracked and new, not ignored, source files under `root`, as relative paths."""
    listed = subprocess.check_output(
        ["git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard",
         "--", *SOURCE_ROOTS], text=True)
    for relative in sorted(set(filter(None, listed.split("\0")))):
        path = root / relative
        if path.suffix in SOURCE_SUFFIXES and path.is_file() and path.stat().st_size < MAX_BYTES:
            yield relative


def unresolved(root):
    """`{(path, name): first line}` for every comment name that the code does not have."""
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
    found = {}
    for relative, name, number in mentions:
        if name not in words:
            found.setdefault((relative, name), number)
    return found


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument("--base-root", type=Path, help="a checkout of the base commit")
    args = parser.parse_args(argv)
    for root in filter(None, (args.root, args.base_root)):
        if not (root / "crates").is_dir():
            print(f"comment names: no crates directory in {root}", file=sys.stderr)
            return 2
    head = unresolved(args.root.resolve())
    if args.base_root:
        base = unresolved(args.base_root.resolve())
        # A file that moved keeps its names, so compare by name too.
        known = {name for _, name in base}
        head = {key: line for key, line in head.items() if key not in base and key[1] not in known}
    for (relative, name), line in sorted(head.items()):
        print(f"{relative}:{line}: `{name}` is not in the code")
    if args.base_root and head:
        print("comment names: correct the name, or if it is from outside the repository, put "
              "`// external-name: <source>` on the line above it.", file=sys.stderr)
        return 1
    print(f"comment names: {len(head)} {'new ' if args.base_root else ''}unresolved")
    return 0


if __name__ == "__main__":
    sys.exit(main())
