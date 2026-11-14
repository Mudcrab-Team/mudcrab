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
# Out-of-line module declarations with the attributes in front of them.
MOD_DECLARATION = re.compile(
    r"((?:#\[[^\]]*\]\s*)*)(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
ATTRIBUTE = re.compile(r"#\[([^\]]*)\]")
TEST_DIRECTORIES = {"tests", "benches"}
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


def split_arguments(text):
    """Split `text` at the commas that are not inside parentheses."""
    parts, depth, start = [], 0, 0
    for index, char in enumerate(text):
        if char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(text[start:index])
            start = index + 1
    parts.append(text[start:])
    return [part.strip() for part in parts if part.strip()]


def requires_test(predicate):
    """True when a `cfg` predicate can only hold in a test build.

    `test`, `all(test, ...)` and `all(..., any(test, ...))` qualify. `not(test)`,
    `any(test, other)` and predicates without `test` do not.
    """
    predicate = predicate.strip()
    call = re.match(r"([A-Za-z_][A-Za-z0-9_]*)\s*\((.*)\)$", predicate, re.S)
    if call:
        name, inner = call.group(1), call.group(2)
        arguments = split_arguments(inner)
        if name == "all":
            return any(requires_test(argument) for argument in arguments)
        if name == "any":
            return bool(arguments) and all(requires_test(argument) for argument in arguments)
        return False
    return predicate == "test"


def attributes_require_test(attributes):
    """Whether any `#[cfg(...)]` among `attributes` (text) requires a test build."""
    for match in re.finditer(r"#!?\[\s*cfg\s*\(", attributes):
        open_bracket = attributes.index("[", match.start())
        close = matching_bracket(attributes, open_bracket)
        body = attributes[open_bracket + 1 : close].strip()
        if requires_test(body[body.index("(") + 1 : body.rindex(")")]):
            return True
    return False


def cfg_test_spans(code):
    """Character spans of items under a test-only `#[cfg(...)]` in masked `code`."""
    spans = []
    for match in re.finditer(r"#\[\s*cfg\s*\(", code):
        close = matching_bracket(code, code.index("[", match.start()))
        if not attributes_require_test(code[match.start() : close + 1]):
            continue
        index = close + 1
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


def is_test_file(crate_dir, path):
    """A file under `tests/` or `benches/`, or one named `tests.rs`, holds test code."""
    relative = path.relative_to(crate_dir.resolve()).parts
    return bool(TEST_DIRECTORIES.intersection(relative[:-1])) or relative[-1] == "tests.rs"


def inner_cfg_requires_test(code):
    """Whether the file starts with a test-only inner attribute such as `#![cfg(test)]`."""
    return any(
        attributes_require_test(match.group(0))
        for match in re.finditer(r"#!\[\s*cfg\s*\([^\]]*\]", code)
    )


def child_directory(path, root_files):
    """Directory that holds the out-of-line modules declared by `path`."""
    if path.name in root_files or path.name == "mod.rs":
        return path.parent
    return path.parent / path.stem


def production_files(crate_dir):
    """Source files of modules that are not test-only.

    Modules declared under a test-only `cfg` (`#[cfg(test)]`, `#[cfg(all(test, ...))]`),
    files under `tests/` or `benches/`, files named `tests.rs`, and files that begin with a
    test-only inner attribute are left out, together with the modules they declare.
    """
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
        text = path.read_text(encoding="utf-8", errors="replace")
        code = mask(text)
        if is_test_file(crate_dir, path) or inner_cfg_requires_test(code):
            continue
        seen.add(path)
        base = path.parent if path in resolved_roots else child_directory(path, root_files)
        for declaration in MOD_DECLARATION.finditer(code):
            # Positions match between `code` and `text`; only `text` still has strings.
            attributes = text[declaration.start(1) : declaration.end(1)]
            if attributes_require_test(code[declaration.start(1) : declaration.end(1)]):
                continue
            custom = re.search(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]', attributes)
            if custom:
                candidates = [path.parent / custom.group(1)]
            else:
                name = declaration.group(2)
                candidates = [base / f"{name}.rs", base / name / "mod.rs"]
            stack.extend(candidate for candidate in candidates if candidate.is_file())
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

    raised, lowered, unrecorded = [], [], []
    for name in names:
        limit = allowed.get(name)
        if limit is None:
            print(f"{name:16} {measured[name]:5} / {'-':>5}  new crate, no baseline entry")
            unrecorded.append(name)
            continue
        state = "ok"
        if measured[name] > limit:
            state = "RAISED"
            raised.append(name)
        elif measured[name] < limit:
            state = "below baseline"
            lowered.append(name)
        print(f"{name:16} {measured[name]:5} / {limit:5}  {state}")
    if unrecorded:
        print(
            "ratchet: no baseline entry for "
            + ", ".join(unrecorded)
            + ". The count is shown above and passes. Record it with "
            "`python3 tools/unwrap_ratchet/unwrap_ratchet.py --update`.",
            file=sys.stderr,
        )
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
