#!/usr/bin/env python3
"""Ratchet the count of `.unwrap()`, `.expect(` and `panic!(` in production Rust code.

The count may not rise above the committed baseline unless a reviewer changes the
baseline file in the same pull request. It does not ask for zero: most existing sites
guard their own input, and a bulk rewrite would be riskier than the sites it removes.

Production code means the modules reachable from `lib.rs`, `main.rs` and `src/bin/*.rs`
of each `crates/<name>/src`, without `#[cfg(test)]` modules and items. `tests/`,
`examples/`, `benches/` and `build.rs` are not counted. Comments, string literals and
character literals are blanked before matching, so text inside them does not count.

Usage:
  python3 tools/unwrap_ratchet/unwrap_ratchet.py                 # check every crate against the baseline
  python3 tools/unwrap_ratchet/unwrap_ratchet.py --crate engine  # check one crate
  python3 tools/unwrap_ratchet/unwrap_ratchet.py --update        # write the measured counts to the baseline

Exit codes: 0 = at or below the baseline, 1 = a count rose, 2 = usage or baseline error.
A count below the baseline passes and prints a reminder to lower the baseline. Pass
`--strict` to fail instead, for a branch that must keep the baseline tight.
"""

import argparse
import json
import re
import sys
from pathlib import Path

PATTERN = re.compile(r"\.unwrap\(\)|\.expect\(|\bpanic!\(")
MOD_DECLARATION = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
PATH_ATTRIBUTE = re.compile(r'#\[path\s*=\s*"([^"]+)"\s*\]')
DEFAULT_BASELINE = Path("tools/unwrap_ratchet/baseline.json")


def identifier_char(text, index):
    return index >= 0 and (text[index].isalnum() or text[index] == "_")


def mask(text):
    """Return `text` with comments, strings and char literals replaced by spaces.

    Newlines and column positions are kept, so line numbers stay valid.
    """
    out = list(text)
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
    return "".join(out)


def matching_bracket(code, start):
    """Index of the bracket that closes the one at `start`, or the end of `code`."""
    pairs = {"(": ")", "[": "]", "{": "}"}
    stack = []
    for index in range(start, len(code)):
        char = code[index]
        if char in pairs:
            stack.append(pairs[char])
        elif stack and char == stack[-1]:
            stack.pop()
            if not stack:
                return index
    return len(code) - 1


def cfg_test_spans(code):
    """Character spans of items under `#[cfg(test)]` in masked `code`."""
    spans = []
    for match in re.finditer(r"#\[cfg\(test\)\]", code):
        index = match.end()
        while True:  # skip whitespace and further attributes on the same item
            while index < len(code) and code[index].isspace():
                index += 1
            if code.startswith("#[", index):
                index = matching_bracket(code, index + 1) + 1
            else:
                break
        depth = 0
        end = len(code) - 1
        while index < len(code):
            char = code[index]
            if char in "([":
                depth += 1
            elif char in ")]":
                depth -= 1
            elif char == "{" and depth == 0:
                end = matching_bracket(code, index)
                break
            elif char == ";" and depth == 0:
                end = index
                break
            index += 1
        spans.append((match.start(), end))
    return spans


def child_directory(path, root_files):
    """Directory that holds the out-of-line modules declared by `path`."""
    if path.name in root_files or path.name == "mod.rs":
        return path.parent
    return path.parent / path.stem


def production_files(crate_dir):
    """Source files of modules that are not declared under `#[cfg(test)]`."""
    src = crate_dir / "src"
    roots = [src / name for name in ("lib.rs", "main.rs") if (src / name).is_file()]
    roots += sorted((src / "bin").glob("*.rs")) if (src / "bin").is_dir() else []
    root_files = {"lib.rs", "main.rs"}
    resolved_roots = {root.resolve() for root in roots}
    seen, stack = set(), list(roots)
    while stack:
        path = stack.pop().resolve()
        if path in seen or not path.is_file():
            continue
        seen.add(path)
        is_root = path in resolved_roots
        base = path.parent if is_root else child_directory(path, root_files)
        pending_test, pending_path = False, None
        for line in path.read_text(encoding="utf-8", errors="replace").split("\n"):
            stripped = line.strip()
            if stripped.startswith("#[cfg(test)]"):
                pending_test = True
                continue
            attribute = PATH_ATTRIBUTE.search(line)
            if attribute:
                pending_path = attribute.group(1)
                continue
            declaration = MOD_DECLARATION.match(line)
            if declaration:
                if not pending_test:
                    if pending_path:
                        candidates = [path.parent / pending_path]
                    else:
                        name = declaration.group(1)
                        candidates = [base / f"{name}.rs", base / name / "mod.rs"]
                    stack.extend(candidate for candidate in candidates if candidate.is_file())
                pending_test, pending_path = False, None
            elif stripped and not stripped.startswith("#["):
                pending_test, pending_path = False, None
    return sorted(seen)


def count_file(path):
    text = path.read_text(encoding="utf-8", errors="replace")
    code = mask(text)
    skipped = bytearray(len(code))
    for start, end in cfg_test_spans(code):
        skipped[start : end + 1] = b"\x01" * (end + 1 - start)
    return sum(1 for match in PATTERN.finditer(code) if not skipped[match.start()])


def count_crate(root, crate):
    return sum(count_file(path) for path in production_files(root / "crates" / crate))


def crate_names(root):
    return sorted(
        entry.name
        for entry in (root / "crates").iterdir()
        if (entry / "src").is_dir()
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--crate", help="check one crate; the default is every crate")
    parser.add_argument("--baseline", type=Path, help=f"default: {DEFAULT_BASELINE}")
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument("--update", action="store_true", help="write the measured counts")
    parser.add_argument("--strict", action="store_true", help="also fail when a count fell")
    args = parser.parse_args(argv)

    root = args.root.resolve()
    baseline_path = args.baseline or root / DEFAULT_BASELINE
    names = [args.crate] if args.crate else crate_names(root)
    for name in names:
        if not (root / "crates" / name / "src").is_dir():
            print(f"ratchet: no crate named {name}", file=sys.stderr)
            return 2
    measured = {name: count_crate(root, name) for name in names}

    if args.update:
        try:
            counts = json.loads(baseline_path.read_text())["counts"]
        except (OSError, ValueError, KeyError):
            counts = {}
        counts.update(measured)
        baseline_path.write_text(
            json.dumps({"counts": dict(sorted(counts.items()))}, indent=2) + "\n"
        )
        print(f"ratchet: wrote {baseline_path}")
        return 0

    try:
        allowed = json.loads(baseline_path.read_text())["counts"]
    except (OSError, ValueError, KeyError) as error:
        print(f"ratchet: cannot read baseline {baseline_path}: {error}", file=sys.stderr)
        return 2

    raised, lowered = [], []
    for name in names:
        limit = allowed.get(name)
        if limit is None:
            print(f"ratchet: {name}: no baseline entry; run with --update", file=sys.stderr)
            return 2
        state = "ok"
        if measured[name] > limit:
            state = "RAISED"
            raised.append(name)
        elif measured[name] < limit:
            state = "below baseline"
            lowered.append(name)
        print(f"{name:16} {measured[name]:5} / {limit:5}  {state}")
    if raised:
        print(
            "ratchet: a production unwrap, expect or panic! count rose in "
            + ", ".join(raised)
            + ". Return the error with `?` instead. If the new call is justified, "
            "raise the baseline with `python3 tools/unwrap_ratchet/unwrap_ratchet.py --update` "
            "and explain it in the pull request.",
            file=sys.stderr,
        )
        return 1
    if lowered:
        message = (
            "ratchet: the count fell in "
            + ", ".join(lowered)
            + ". Lower the baseline with `python3 tools/unwrap_ratchet/unwrap_ratchet.py --update`."
        )
        print(message, file=sys.stderr)
        if args.strict:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
