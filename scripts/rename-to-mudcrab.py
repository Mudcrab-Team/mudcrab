#!/usr/bin/env python3
"""Rename OpenSkyrim / Wah Krah Jol to Mudcrab across every tracked text file.

Run from anywhere inside the repository. Re-runnable on a fresh checkout, so the
rename never needs hand-resolved merge conflicts: reset to fresh main, run this,
run `cargo fmt --all`, re-apply the small hand-fix commit.

    python scripts/rename-to-mudcrab.py            # apply, print per-file counts
    python scripts/rename-to-mudcrab.py check      # exit 1 and list files still to rename
    python scripts/rename-to-mudcrab.py self-test  # rules on inline samples, no files

The modes are words, not `--` options: the engine's tests read every script in
`scripts/` and check each double-dash option they find against the engine's parser.

Idempotent: a second run changes nothing, also after `cargo fmt --all` has
reflowed the output (chains split over lines, block closures and multi-line
args are all recognised). Line endings and encoding are kept (files are read as
bytes, only the matched text changes). Binary files (NUL byte), non-UTF-8 files,
`vendor/**` and this script are skipped.

Order of work per file: (1) mask complete "new name then old name" chains, (2)
PRE_RULES rewrite every remaining env read and the collision reader, (3) mask the
chains again plus PROTECTED, (4) RULES, (5) restore all masks.

EXCEPTIONS, left unchanged on purpose:
  * OPEN_SKYRIM_material (and any OPEN_SKYRIM token): the glTF extension name
    written into converted assets; assets converted earlier must keep loading.
  * The glTF extras key "openSkyrim" (JSON key and /openSkyrim/ pointer): the
    same converted-asset data format.
  * openskyrimdev@gmail.com: a real contact address.
  * ko_fi: wahkrahjol: an external account handle.
  * Complete fallback chains (CHAINS): env reads that try MUDCRAB_X and then
    OPENSKYRIM_X, the collision reader that tries mudcrabCollision and then
    openSkyrimCollision, and the texture helper's replace("MUDCRAB_",
    "OPENSKYRIM_"). The old names stay accepted next to the new ones.
  * Paths on a contributor's machine: a lowercase `openskyrim` right after
    a slash or backslash (e.g. /home/dev/.cache/openskyrim/...) and the folder
    name `OpenSkyrim-lod`.
  * OPENSKYRIM_* written with a literal asterisk and no name after it (docs
    saying the old names are still accepted). OPENSKYRIM_*_FIXTURE is renamed.

LOUD FAILURES AND HAND CHECKS:
  * A .rs file that still has `convert_installed_2d_fixture(` but no
    `legacy_var` after the rewrite means the helper changed shape: ERROR, exit
    code 2, nothing written. Fix the helper by hand or update the rule.
  * "Check by hand" lists every "OPENSKYRIM_X" / "MUDCRAB_X" string literal in
    .rs that is not the argument of env::var / env::var_os (a helper that takes
    a variable name needs the fallback added by hand), and every skipped
    binary or non-UTF-8 file that contains an old-name byte sequence.

The collision extras key the converter writes becomes mudcrabCollision; the
engine reader accepts both (see PRE_RULES).
"""

import argparse
from pathlib import Path
import re
import subprocess
import sys

SELF = "scripts/rename-to-mudcrab.py"

# Whitespace-tolerant fragments, so rustfmt reflowing cannot defeat a pattern.
_CALL_VAR_OS = r"(?:std::)?env::var_os"
_CALL_VAR = r"(?:std::)?env::var"

# Complete "new name then old name" chains. Masked before PRE_RULES (so they are
# not rewritten again) and after (so the rules do not touch the fallback).
CHAINS = [
    # env::var_os("MUDCRAB_X").or_else(|| env::var_os("OPENSKYRIM_X")), block closure ok
    rf'{_CALL_VAR_OS}\(\s*"MUDCRAB_(\w+)"\s*,?\s*\)\s*\.or_else\(\s*\|\|\s*\{{?\s*'
    rf'{_CALL_VAR_OS}\(\s*"OPENSKYRIM_\1"\s*,?\s*\)\s*;?\s*\}}?\s*,?\s*\)',
    # env::var("MUDCRAB_X").or_else(|_| env::var("OPENSKYRIM_X"))
    rf'{_CALL_VAR}\(\s*"MUDCRAB_(\w+)"\s*,?\s*\)\s*\.or_else\(\s*\|_\|\s*\{{?\s*'
    rf'{_CALL_VAR}\(\s*"OPENSKYRIM_\1"\s*,?\s*\)\s*;?\s*\}}?\s*,?\s*\)',
    # value.get("mudcrabCollision").or_else(|| value.get("openSkyrimCollision"))
    r'(\w+)\s*\.get\(\s*"mudcrabCollision"\s*\)\s*\.or_else\(\s*\|\|\s*\{?\s*'
    r'\1\s*\.get\(\s*"openSkyrimCollision"\s*\)\s*;?\s*\}?\s*,?\s*\)',
    # extras.get("mudcrabCollision", extras.get("openSkyrimCollision")) (Python)
    r'(\w+)\s*\.get\(\s*"mudcrabCollision"\s*,\s*\1\s*\.get\(\s*"openSkyrimCollision"\s*\)\s*,?\s*\)',
    # the texture helper's legacy name
    r'replace\(\s*"MUDCRAB_"\s*,\s*"OPENSKYRIM_"\s*\)',
]

# Never changed. Masked with placeholders before the rules, restored after.
PROTECTED = [
    r"OPEN_SKYRIM\w*",
    r'"openSkyrim"',
    r"/openSkyrim/",
    r"openskyrimdev@gmail\.com",
    r"ko_fi: wahkrahjol",
    r"(?<=[/\\])openskyrim",
    r"OpenSkyrim-lod",
    r"OPENSKYRIM_\*(?![\w*])",
]

# Masked only in Python files: a bare reader of the old collision key is left
# for the hand-fix commit, which turns it into a read of both keys. Renaming it
# here would make that commit conflict when it is re-applied on fresh main.
PROTECTED_PY = [
    r'\.get\(\s*"openSkyrimCollision"\s*\)',
]

# (pattern, replacement, note, only for paths ending with). Applied on the raw
# text after the complete chains are masked. Their output is masked again.
PRE_RULES = [
    (
        # Option-returning read: new name first, old name as a fallback inline
        # (rustfmt reflows the chain). Any remaining old read is converted.
        rf'({_CALL_VAR_OS})\(\s*"OPENSKYRIM_(\w+)"\s*,?\s*\)',
        r'\1("MUDCRAB_\2").or_else(|| \1("OPENSKYRIM_\2"))',
        "env var_os read: MUDCRAB_ first, OPENSKYRIM_ still accepted",
        ".rs",
    ),
    (
        # Result-returning read; works inside .ok()/.map chains too.
        rf'({_CALL_VAR})\(\s*"OPENSKYRIM_(\w+)"\s*,?\s*\)',
        r'\1("MUDCRAB_\2").or_else(|_| \1("OPENSKYRIM_\2"))',
        "env var read: MUDCRAB_ first, OPENSKYRIM_ still accepted",
        ".rs",
    ),
    (
        # Helper that takes the variable name as a parameter (texture.rs).
        # \2 is the whitespace before the first statement, \3 and \4 the
        # separators of the chain, so the original layout is kept.
        r'(fn\s+convert_installed_2d_fixture\s*\(\s*variable:\s*&str\s*,\s*'
        r'encoding:\s*TextureEncoding\s*,?\s*\)\s*\{)(\s*)'
        r'let\s+path\s*=\s*std::env::var_os\(\s*variable\s*\)(\s*)'
        r'\.map\(\s*std::path::PathBuf::from\s*\)(\s*)'
        r'\.unwrap_or_else\(\s*\|\|\s*panic!\(\s*"set \{variable\} to an installed DDS"\s*,?\s*\)\s*\)\s*;',
        r'\1\2let legacy_var = variable.replace("MUDCRAB_", "OPENSKYRIM_");\2'
        r'let path = std::env::var_os(variable)\3'
        r'.or_else(|| std::env::var_os(&legacy_var))\3'
        r'.map(std::path::PathBuf::from)\4'
        r'.unwrap_or_else(|| panic!("set {variable} or {legacy_var} to an installed DDS"));',
        "helper also accepts the OPENSKYRIM_ name",
        ".rs",
    ),
    (
        # Engine reader: new key first, old converted assets still load.
        r'(\w+)\.get\(\s*"openSkyrimCollision"\s*\)',
        r'\1.get("mudcrabCollision").or_else(|| \1.get("openSkyrimCollision"))',
        "collision reader accepts both keys",
        ".rs",
    ),
]

# (pattern, replacement, note). Applied in order on the masked text.
RULES = [
    # .gitignore: unused config file name, dropped (runs before the name rules)
    (r"^openskyrim\.cfg\r?\n", "", "unused .gitignore entry"),
    # 3. collision extras key (writer, docs, snapshot)
    (r"openSkyrimCollision", "mudcrabCollision", "collision extras key"),
    # 4. URLs and repository names
    (r"github\.com/realfakenerd/OpenSkyrim", "github.com/Mudcrab-Team/mudcrab", "repo URL"),
    (r"github\.com/realfakenerd/wah-krah-jol", "github.com/Mudcrab-Team/mudcrab", "repo URL"),
    (r"realfakenerd%2Fwah-krah-jol", "Mudcrab-Team%2Fmudcrab", "encoded repo name in badge URL"),
    (r"realfakenerd/wah-krah-jol", "Mudcrab-Team/mudcrab", "repo link text"),
    (r"<your-username>/wah-krah-jol\.git", "<your-username>/mudcrab.git", "clone URL"),
    (r"cd wah-krah-jol", "cd mudcrab", "clone directory"),
    (r"wah-krah-jol", "mudcrab", "remaining repo or folder name"),
    # 5. names
    (r"Wah Krah Jol", "Mudcrab", "project name"),
    (r"OpenSkyrim", "Mudcrab", "project name"),
    (r"OPENSKYRIM_", "MUDCRAB_", "env var prefix"),
    (r"openskyrim", "mudcrab", "lowercase name (temp dirs, thread, luarocks, types file)"),
]

OLD_NAME_BYTES = re.compile(rb"openskyrim|wah.?krah", re.IGNORECASE)
VAR_LITERAL = re.compile(r'"(?:OPENSKYRIM|MUDCRAB)_\w+"')
ENV_CALL_BEFORE = re.compile(r"(?:env::var(?:_os)?|expect)\(\s*$")


def _mask(text, patterns, masked):
    def sub(match):
        masked.append(match.group(0))
        return f"\x00{len(masked) - 1}\x00"

    # One pattern at a time: the chains use numbered backreferences.
    for pattern in patterns:
        text = re.sub(pattern, sub, text)
    return text


def _restore(text, masked):
    # Masks can nest (a later mask may contain an earlier placeholder).
    while "\x00" in text:
        text = re.sub(r"\x00(\d+)\x00", lambda m: masked[int(m.group(1))], text)
    return text


def rewrite(path, text):
    """Return (new text, number of replacements)."""
    count = 0
    masked = []
    text = _mask(text, CHAINS, masked)
    for pattern, replacement, _note, suffix in PRE_RULES:
        if path.endswith(suffix):
            text, n = re.subn(pattern, replacement, text)
            count += n
    protected = CHAINS + PROTECTED + (PROTECTED_PY if path.endswith(".py") else [])
    text = _mask(text, protected, masked)
    for pattern, replacement, _note in RULES:
        flags = re.MULTILINE if pattern.startswith("^") else 0
        text, n = re.subn(pattern, replacement, text, flags=flags)
        count += n
    return _restore(text, masked), count


def helper_error(path, new_text):
    """True if the texture helper is present but was not given its fallback."""
    return (
        path.endswith(".rs")
        and "fn convert_installed_2d_fixture(" in new_text
        and "legacy_var" not in new_text
    )


def hand_checks(path, new_text):
    """Variable-name literals in .rs that are not the argument of env::var(_os)."""
    found = []
    if not path.endswith(".rs"):
        return found
    for match in VAR_LITERAL.finditer(new_text):
        if not ENV_CALL_BEFORE.search(new_text[: match.start()]):
            line = new_text.count("\n", 0, match.start()) + 1
            found.append(f"{path}:{line}: {match.group(0)} is not an env::var/var_os argument")
    return found


# (name, path, input, expected). Each rule and each exception; the expected text
# must also be unchanged by a second run.
SELF_TESTS = [
    ("name", "a.md", "Wah Krah Jol and OpenSkyrim", "Mudcrab and Mudcrab"),
    ("lowercase", "a.rs", 'join("openskyrim-x")', 'join("mudcrab-x")'),
    ("env prefix in text", "a.md", "set OPENSKYRIM_SKYRIM_DATA", "set MUDCRAB_SKYRIM_DATA"),
    (
        "var_os read",
        "a.rs",
        'std::env::var_os("OPENSKYRIM_NIF_FIXTURE")\n    .map(f)',
        'std::env::var_os("MUDCRAB_NIF_FIXTURE").or_else(|| std::env::var_os("OPENSKYRIM_NIF_FIXTURE"))\n    .map(f)',
    ),
    (
        "var read in an or_else chain",
        "a.rs",
        'a.or_else(|| std::env::var("OPENSKYRIM_NIF_DIR").ok())',
        'a.or_else(|| std::env::var("MUDCRAB_NIF_DIR").or_else(|_| std::env::var("OPENSKYRIM_NIF_DIR")).ok())',
    ),
    (
        "var read without std::",
        "a.rs",
        'env::var("OPENSKYRIM_VALIDATE_LIMIT")',
        'env::var("MUDCRAB_VALIDATE_LIMIT").or_else(|_| env::var("OPENSKYRIM_VALIDATE_LIMIT"))',
    ),
    (
        "lone old read inside someone else's or_else",
        "a.rs",
        'other.or_else(|| env::var_os("OPENSKYRIM_X"))',
        'other.or_else(|| env::var_os("MUDCRAB_X").or_else(|| env::var_os("OPENSKYRIM_X")))',
    ),
    ("collision key", "a.md", "`openSkyrimCollision`", "`mudcrabCollision`"),
    (
        "collision reader",
        "a.rs",
        'value.get("openSkyrimCollision")',
        'value.get("mudcrabCollision").or_else(|| value.get("openSkyrimCollision"))',
    ),
    (
        "collision reader with another receiver",
        "a.rs",
        'extras.get("openSkyrimCollision")',
        'extras.get("mudcrabCollision").or_else(|| extras.get("openSkyrimCollision"))',
    ),
    (
        "a bare Python reader of the old key is left for the hand fix",
        "a.py",
        'extras.get("openSkyrimCollision")',
        'extras.get("openSkyrimCollision")',
    ),
    (
        "repo URL and clone",
        "a.md",
        "github.com/realfakenerd/OpenSkyrim, realfakenerd/wah-krah-jol, cd wah-krah-jol",
        "github.com/Mudcrab-Team/mudcrab, Mudcrab-Team/mudcrab, cd mudcrab",
    ),
    ("gitignore entry", ".gitignore", "a\nopenskyrim.cfg\nb\n", "a\nb\n"),
    ("keep OPEN_SKYRIM_material", "a.rs", '"OPEN_SKYRIM_material"', '"OPEN_SKYRIM_material"'),
    ("keep openSkyrim key", "a.rs", '"openSkyrim": 1, "/openSkyrim/x"', '"openSkyrim": 1, "/openSkyrim/x"'),
    ("keep contact", "a.md", "openskyrimdev@gmail.com", "openskyrimdev@gmail.com"),
    ("keep ko_fi", "a.yml", "ko_fi: wahkrahjol", "ko_fi: wahkrahjol"),
    ("keep contributor path", "a.md", "/home/dev/.cache/openskyrim/x", "/home/dev/.cache/openskyrim/x"),
    ("keep contributor folder", "a.md", "C:\\\\OpenSkyrim-lod", "C:\\\\OpenSkyrim-lod"),
    ("keep old-name docs", "a.md", "the old OPENSKYRIM_* names", "the old OPENSKYRIM_* names"),
    ("CRLF kept", "a.rs", 'x("openskyrim")\r\ny\r\n', 'x("mudcrab")\r\ny\r\n'),
]

HELPER_OLD = (
    "    fn convert_installed_2d_fixture(variable: &str, encoding: TextureEncoding) {\n"
    "        let path = std::env::var_os(variable)\n"
    "            .map(std::path::PathBuf::from)\n"
    '            .unwrap_or_else(|| panic!("set {variable} to an installed DDS"));\n'
    "        run(path);\n"
    "    }\n"
)
HELPER_NEW = (
    "    fn convert_installed_2d_fixture(variable: &str, encoding: TextureEncoding) {\n"
    '        let legacy_var = variable.replace("MUDCRAB_", "OPENSKYRIM_");\n'
    "        let path = std::env::var_os(variable)\n"
    "            .or_else(|| std::env::var_os(&legacy_var))\n"
    "            .map(std::path::PathBuf::from)\n"
    '            .unwrap_or_else(|| panic!("set {variable} or {legacy_var} to an installed DDS"));\n'
    "        run(path);\n"
    "    }\n"
)
SELF_TESTS.append(("texture helper", "a.rs", HELPER_OLD, HELPER_NEW))
# The same helper with a different body is not rewritten, so the loud failure fires.
HELPER_CHANGED = HELPER_OLD.replace("an installed DDS", "a DDS")

# Already-renamed code in rustfmt's expanded shapes: must not change.
STABLE_TESTS = [
    (
        "expanded var_os chain",
        'let path = std::env::var_os("MUDCRAB_X")\n    .or_else(|| std::env::var_os("OPENSKYRIM_X"))\n    .map(f);\n',
    ),
    (
        "block closure",
        'let p = std::env::var_os("MUDCRAB_X").or_else(|| {\n    std::env::var_os("OPENSKYRIM_X")\n});\n',
    ),
    (
        "multi-line args",
        'let p = std::env::var_os(\n    "MUDCRAB_X",\n)\n.or_else(|| {\n    std::env::var_os(\n        "OPENSKYRIM_X",\n    )\n});\n',
    ),
    (
        "expanded var chain with ok",
        'a.or_else(|| {\n    std::env::var("MUDCRAB_Y")\n        .or_else(|_| std::env::var("OPENSKYRIM_Y"))\n        .ok()\n})\n',
    ),
    (
        "expanded collision reader",
        'let c = value\n    .get("mudcrabCollision")\n    .or_else(|| value.get("openSkyrimCollision"));\n',
    ),
    ("texture helper after fmt", HELPER_NEW),
]
STABLE_PY = 'extras.get(\n    "mudcrabCollision",\n    extras.get("openSkyrimCollision"),\n)\n'


def self_test():
    failed = 0
    total = 0

    def fail(message):
        nonlocal failed
        failed += 1
        print(f"FAIL {message}", file=sys.stderr)

    for name, path, text, expected in SELF_TESTS:
        total += 1
        got, _count = rewrite(path, text)
        again, _count = rewrite(path, got)
        if got != expected or again != got:
            fail(f"{name}: got {got!r}, rerun {again!r}, expected {expected!r}")
    for name, text in STABLE_TESTS:
        total += 1
        got, _count = rewrite("a.rs", text)
        if got != text:
            fail(f"{name} (stable): got {got!r}")
    total += 1
    if rewrite("a.py", STABLE_PY)[0] != STABLE_PY:
        fail("expanded audit reader (stable)")
    total += 1
    if not helper_error("a.rs", rewrite("a.rs", HELPER_CHANGED)[0]):
        fail("changed texture helper: no loud failure")
    total += 1
    if helper_error("a.rs", rewrite("a.rs", HELPER_OLD)[0]):
        fail("texture helper: false loud failure")
    total += 1
    hand = hand_checks("a.rs", 'f("MUDCRAB_X");\nstd::env::var_os(\n  "MUDCRAB_Y")')
    if len(hand) != 1 or "a.rs:1" not in hand[0]:
        fail(f"hand checks: {hand}")
    print(f"{total - failed}/{total} self-tests passed")
    return 1 if failed else 0


def repo_root():
    # This file lives in `scripts/` at the repository root.
    return Path(__file__).resolve().parent.parent


def tracked_files(root):
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=root, capture_output=True, check=True
    ).stdout
    return [name for name in out.decode("utf-8").split("\0") if name]


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument(
        "mode",
        nargs="?",
        default="apply",
        choices=["apply", "check", "self-test"],
        help="apply (default); check: list files still to rename, change nothing, exit 1 if any; "
        "self-test: run the rules on inline samples, touch no files",
    )
    args = parser.parse_args()
    if args.mode == "self-test":
        return self_test()

    root = repo_root()
    pending = []  # (name, count, path, new bytes)
    errors = []
    by_hand = []
    skipped = []
    for name in tracked_files(root):
        if name == SELF or name.startswith("vendor/"):
            continue
        path = root / name
        if not path.is_file():
            continue
        data = path.read_bytes()
        try:
            if b"\x00" in data:
                raise UnicodeDecodeError("utf-8", b"", 0, 1, "NUL byte")
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            if OLD_NAME_BYTES.search(data):
                skipped.append(name)
            continue
        new_text, count = rewrite(name, text)
        if helper_error(name, new_text):
            errors.append(
                f"{name}: convert_installed_2d_fixture has no legacy_var fallback "
                "(the helper changed shape; fix it by hand or update the rule)"
            )
        by_hand.extend(hand_checks(name, new_text))
        if new_text != text:
            pending.append((name, count, path, new_text.encode("utf-8")))

    if errors:
        for line in errors:
            print(f"ERROR {line}", file=sys.stderr)
        print("nothing written", file=sys.stderr)
        return 2
    if args.mode != "check":
        for _name, _count, path, data in pending:
            path.write_bytes(data)
    for name, count, _path, _data in pending:
        print(f"{name}: {count}")
    if by_hand or skipped:
        print("check by hand:")
        for line in by_hand:
            print(f"  {line}")
        for name in skipped:
            print(f"  {name}: binary or non-UTF-8 file with an old-name byte sequence, skipped")
    if args.mode == "check":
        if pending:
            print(f"{len(pending)} file(s) still to rename", file=sys.stderr)
            return 1
        print("nothing left to rename")
        return 0
    print(f"{len(pending)} file(s) changed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
