#!/usr/bin/env python3
"""Ratchet the count of `.unwrap()`, `.expect(` and `panic!(` in production Rust code.

The count may not rise. CI compares the pull request's tree with its base commit
(`--base-root`), so a pull request is judged only by the sites it adds, and no committed
number goes stale. A justified rise is accepted when the same pull request raises that
crate's entry in `baseline.json` (`--update`, which also raises the file's `revision`). Without `--base-root` the check compares
with `baseline.json` alone, for a quick local run. It does not ask for zero: most existing
sites guard their own input, and a bulk rewrite would be riskier than the sites it removes.

Production code means the modules reachable from `lib.rs`, `main.rs` and `src/bin/*.rs`
of each `crates/<name>/src`, without `#[cfg(test)]` modules and items. `tests/`,
`examples/`, `benches/` and `build.rs` are not counted. Comments, string literals and
character literals are blanked before matching, so text inside them does not count.

Usage:
  python3 tools/unwrap_ratchet/unwrap_ratchet.py --base-root ../base   # what CI runs: compare with the base tree
  python3 tools/unwrap_ratchet/unwrap_ratchet.py                       # check every crate against the baseline
  python3 tools/unwrap_ratchet/unwrap_ratchet.py --crate engine        # check one crate
  python3 tools/unwrap_ratchet/unwrap_ratchet.py --update              # write the measured counts to the baseline

Exit codes: 0 = at or below the limit, 1 = a count rose, 2 = usage or baseline error.
A count below the limit passes and prints a reminder (baseline mode only). Pass
`--strict` to fail instead, for a branch that must keep the baseline tight.
"""

import argparse
import json
import re
import sys
from pathlib import Path

# Rust allows whitespace (and, after masking, comments) between the tokens of a call.
PATTERN = re.compile(r"\.\s*unwrap\s*\(\s*\)|\.\s*expect\s*\(|\bpanic\s*!\s*\(")
# `#[test]`, `#[bench]` and runtime variants such as `#[tokio::test(...)]` compile only for tests.
TEST_FUNCTION_ATTRIBUTE = re.compile(r"#\[\s*(?:[A-Za-z_][A-Za-z0-9_]*::)*(?:test|bench)\s*[(\]]")
# An inline module: attributes, optional visibility, `mod name {`.
INLINE_MODULE = re.compile(
    r"((?:#\[[^\]]*\]\s*)*)(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{"
)
# Out-of-line module declarations with the attributes in front of them.
MOD_DECLARATION = re.compile(
    r"((?:#\[[^\]]*\]\s*)*)(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
ATTRIBUTE = re.compile(r"#\[([^\]]*)\]")
RAW_STRING = re.compile(r'(?:b|c)?r(#*)"')
TEST_DIRECTORIES = {"tests", "benches"}
DEFAULT_BASELINE = Path("tools/unwrap_ratchet/baseline.json")


def identifier_char(text, index):
    return index >= 0 and (text[index].isalnum() or text[index] == "_")


def mask(text):
    """Return `text` with comments, strings and char literals replaced by spaces.

    Newlines and column positions are kept, so line numbers stay valid.
    """
    return mask_with_comments(text)[0]


def mask_with_comments(text):
    """Return `(code, comments)`.

    `code` is `mask(text)`. `comments` holds the `(start, end)` spans of the comments in
    `text`, so a caller can read a marker from a comment. The silent-failure ratchet calls
    this function, so both ratchets agree about what is code.
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
        raw = RAW_STRING.match(text, i) if c in "bcr" else None
        if raw and not identifier_char(text, i - 1):
            closer = '"' + raw.group(1)
            end = text.find(closer, raw.end())
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
                j = quote + 3
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


def enclosing_open(code, position):
    """Index of the `{` that encloses `position`, or -1 at the top level."""
    depth = 0
    for index in range(position - 1, -1, -1):
        char = code[index]
        if char == "}":
            depth += 1
        elif char == "{":
            if depth == 0:
                return index
            depth -= 1
    return -1


def in_type_body(code, open_brace):
    """Whether the block opened at `open_brace` is the body of a struct, enum or union."""
    boundary = max(code.rfind(";", 0, open_brace), code.rfind("{", 0, open_brace), code.rfind("}", 0, open_brace))
    return re.search(r"\b(?:struct|enum|union)\b", code[boundary + 1 : open_brace]) is not None


def skip_attributes(code, index):
    """Index of the first item token after whitespace and further attributes."""
    while True:
        while index < len(code) and code[index].isspace():
            index += 1
        if code.startswith("#[", index):
            index = matching_bracket(code, index + 1) + 1
        else:
            return index


def field_end(code, index, limit):
    """End of a struct field or enum variant that starts at `index`.

    It stops at the comma that ends the field, or just before the closing brace `limit`
    of the type. Angle brackets nest, so `HashMap<K, V>` does not end the field.
    """
    depth = 0
    while index < limit:
        char = code[index]
        if char in "([{<":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == ">" and code[index - 1 : index] != "-":
            depth -= 1
        elif char == "," and depth <= 0:
            return index
        index += 1
    return limit - 1


ITEM_START = re.compile(
    r"(?:(?:pub|fn|mod|use|const|static|struct|enum|union|impl|trait|type|extern|unsafe|async|macro_rules|let)\b"
    r"|[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*\s*!)"  # a macro invocation such as `thread_local! { ... }`
)


BLOCK_START = re.compile(r"(?:'[A-Za-z_][A-Za-z0-9_]*\s*:\s*)?(?:if|match|for|while|loop)\b|\{")
ELSE = re.compile(r"\s*else\b")


def block_statement_end(code, index):
    """End of a block-like statement (`if`, `match`, `for`, `while`, `loop` or a bare block).

    The statement ends at the closing brace of its block, or of the last block of an
    `else` chain, so the next statement is not part of it.
    """
    end = None
    depth = 0
    while index < len(code):
        char = code[index]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif char == "{" and depth == 0:
            end = matching_bracket(code, index)
            index = end + 1
            chain = ELSE.match(code, index)
            if chain is None:
                return end
            index = chain.end()
            continue
        if depth < 0:  # the bracket that closes the parent
            return index - 1
        index += 1
    return len(code) - 1 if end is None else end


def item_end(code, index, attribute_start):
    """Last character of the item, field, argument or statement that an attribute applies to.

    An item ends at its block or at its `;`. A block-like statement (`if`, `match`, `for`,
    `while`, `loop`, `{}`) ends at its closing brace and `else` chain. A `let` statement ends
    at its `;`, whatever blocks it holds. A field, variant, argument or other expression ends at the next
    comma or `;` outside brackets, or just before the bracket that closes its parent.
    """
    index = skip_attributes(code, index)
    opening = enclosing_open(code, attribute_start)
    if opening >= 0 and in_type_body(code, opening):
        return field_end(code, index, matching_bracket(code, opening))
    if BLOCK_START.match(code, index):
        return block_statement_end(code, index)
    starts = ITEM_START.match(code, index)
    statement = starts is not None and starts.group(0) == "let"
    expression = starts is None
    depth = 0
    while index < len(code):
        char = code[index]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif char == "{":
            if depth == 0 and not statement and not expression:
                return matching_bracket(code, index)
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == ";" and depth == 0:
            return index
        elif char == "," and depth == 0 and expression:
            return index
        if depth < 0:  # the bracket that closes the parent
            return index - 1
        index += 1
    return len(code) - 1


def cfg_test_spans(code):
    """Character spans of code that exists only in a test build, in masked `code`.

    That is every item, field or statement under a test-only `#[cfg(...)]`, every
    `#[test]` function, and every block that starts with a test-only inner attribute.
    """
    spans = []
    for match in re.finditer(r"#\[\s*cfg\s*\(", code):
        close = matching_bracket(code, code.index("[", match.start()))
        if attributes_require_test(code[match.start() : close + 1]):
            spans.append((match.start(), item_end(code, close + 1, match.start())))
    for match in TEST_FUNCTION_ATTRIBUTE.finditer(code):
        close = matching_bracket(code, code.index("[", match.start()))
        spans.append((match.start(), item_end(code, close + 1, match.start())))
    for match in re.finditer(r"#!\[\s*cfg\s*\(", code):
        close = matching_bracket(code, code.index("[", match.start()))
        if not attributes_require_test(code[match.start() : close + 1]):
            continue
        opening = enclosing_open(code, match.start())
        if opening < 0:
            spans.append((0, len(code) - 1))  # a file-level attribute
        else:
            spans.append((opening, matching_bracket(code, opening)))
    return spans


def is_test_file(crate_dir, path):
    """A file under `tests/` or `benches/`, or one named `tests.rs`, holds test code."""
    try:
        relative = path.relative_to(crate_dir.resolve()).parts
    except ValueError:  # a `#[path]` that points outside the crate
        return False
    return bool(TEST_DIRECTORIES.intersection(relative[:-1])) or relative[-1] == "tests.rs"


def inner_cfg_requires_test(code):
    """Whether the file starts with a test-only inner attribute such as `#![cfg(test)]`.

    Only the inner attributes at the top of the file apply to the whole file. One inside an
    inline module applies to that module, and `cfg_test_spans` handles it.
    """
    index = 0
    while True:
        while index < len(code) and code[index].isspace():
            index += 1
        if not code.startswith("#![", index):
            return False
        close = matching_bracket(code, index + 2)
        if attributes_require_test(code[index : close + 1]):
            return True
        index = close + 1


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
        test_spans = cfg_test_spans(code)
        inline = []
        for module in INLINE_MODULE.finditer(code):
            opening = module.end() - 1
            custom = re.search(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]', text[module.start(1) : module.end(1)])
            inline.append((opening, matching_bracket(code, opening), custom.group(1) if custom else module.group(2)))
        for declaration in MOD_DECLARATION.finditer(code):
            # A declaration inside a test-only item, module or block is test code too.
            if any(start <= declaration.start() <= end for start, end in test_spans):
                continue
            # Positions match between `code` and `text`; only `text` still has strings.
            attributes = text[declaration.start(1) : declaration.end(1)]
            # Out-of-line modules inside inline modules live in the inline modules' directories.
            parts = [name for opening, closing, name in inline if opening < declaration.start() < closing]
            custom = re.search(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]', attributes)
            if custom:
                candidates = [path.parent.joinpath(*parts) / custom.group(1)]
            else:
                name = declaration.group(2)
                directory = base.joinpath(*parts)
                candidates = [directory / f"{name}.rs", directory / name / "mod.rs"]
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


def read_baseline(path):
    """The `counts` of a baseline file as a dict. Raises OSError, ValueError or KeyError."""
    return json.loads(path.read_text(encoding="utf-8"))["counts"]


def read_revision(path):
    """The `revision` of a baseline file; 0 when the file or the key is missing."""
    try:
        return int(json.loads(path.read_text(encoding="utf-8")).get("revision", 0))
    except (OSError, ValueError, AttributeError):
        return 0


def write_baseline(path, counts, revision):
    """Write LF newlines on every host, so `--update` on Windows does not rewrite the file."""
    document = {"counts": dict(sorted(counts.items())), "revision": revision}
    text = json.dumps(document, indent=2) + "\n"
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)


def limits_from_base(root, base_root, names, measured):
    """Per crate, the count a pull request may not exceed: the count at its base commit.

    A crate that the base lacks has no limit and is reported as new. A pull request accepts
    more sites by running `--update`, which writes the measured counts and raises the
    file's `revision`. A crate's entry then raises its limit when the pull request changed
    the entry, or when the revision changed and the entry equals the measured count. The
    revision makes the approval visible in the diff even when the number is the same as a
    stale entry's. An entry the pull request left alone otherwise never raises a limit,
    so a stale baseline cannot hide a rise or cause a false failure.
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
            print(f"ratchet: no crate named {name}", file=sys.stderr)
            return 2
    measured = {name: count_crate(root, name) for name in names}

    if args.update:
        try:
            counts = read_baseline(baseline_path)
        except (OSError, ValueError, KeyError):
            counts = {}
        counts.update(measured)
        write_baseline(baseline_path, counts, read_revision(baseline_path) + 1)
        print(f"ratchet: wrote {baseline_path}")
        return 0

    if args.base_root:
        base_root = args.base_root.resolve()
        if not (base_root / "crates").is_dir():
            print(f"ratchet: no crates directory in {base_root}", file=sys.stderr)
            return 2
        allowed = limits_from_base(root, base_root, names, measured)
    else:
        try:
            allowed = read_baseline(baseline_path)
        except (OSError, ValueError, KeyError) as error:
            print(f"ratchet: cannot read baseline {baseline_path}: {error}", file=sys.stderr)
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
            "ratchet: no limit for "
            + ", ".join(unrecorded)
            + " (a new crate, or no baseline entry). The count is shown above and passes. Record it with "
            "`python3 tools/unwrap_ratchet/unwrap_ratchet.py --update`.",
            file=sys.stderr,
        )
    if raised:
        print(
            "ratchet: a production unwrap, expect or panic! count rose in "
            + ", ".join(raised)
            + (" above its base" if args.base_root else " above the baseline")
            + ". Return the error with `?` instead. If the new call is justified, "
            "accept it with `python3 tools/unwrap_ratchet/unwrap_ratchet.py --update` "
            "(it raises that crate's entry in `baseline.json`) and explain it in the pull request.",
            file=sys.stderr,
        )
        return 1
    if lowered and not args.base_root:
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
