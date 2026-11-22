#!/usr/bin/env python3
"""Require a source on added comments and docs that state retail behaviour.

The check reads ``git diff -U0 <base>...<head>`` and inspects only added lines.
Rename and copy detection is on, and an added line whose exact text was also
deleted is a move, not a new claim.  A line in a Rust or WGSL ``//`` comment, or
in Markdown under ``docs/`` or ``crates/``, that makes a retail claim must carry
one of these in the same comment block or Markdown section:

* a retail-facts registry ID such as ``RF-TEX-004``;
* a link to any repository Markdown file, such as ``[notes](notes.md)``;
* an external ``http(s)`` URL;
* a SHA-256 hash of the analysed file;
* the word ``unverified``.

A Markdown section runs from a heading to the next heading of the same or a
higher level.  A ``//!`` module doc that cites an RF ID labels every comment in
that Rust file.  "Retail" used as an adjective or a label is not a claim: a
claim needs a behaviour verb, or the locative "in retail".

A line that says ``not retail``, ``not vanilla``, ``differs from retail`` or
``Mudcrab-only`` is labelled only when it also states a reason.  Hyphenated
compounds such as ``retail-parity`` are not claims.  The accepted reason
markers are listed in ``REASON_RE`` and in
``docs/contributing/writing-prs.md``.

An added row of ``docs/research/retail/registry.md`` also needs a valid kind.
A row may carry ``kind: rule``, ``kind: quirk`` or ``kind: defect``; a row
without the field is a rule.  A row that uses another kind value, or that says
"quirk" or "defect" in its text but has no ``kind:`` field, is a finding.  See
ADR-0015.

A stand-in is a value or an asset that Mudcrab makes where retail ships one.  A
comment or doc line that admits a stand-in needs the marker
``STAND-IN(#NNN): what it replaces`` in the same ``//`` block or Markdown
section.  An owner may be added as ``STAND-IN(#NNN, @owner): ...``.  An added
line with a narrow trigger word, or an added ``PLACEHOLDER``, ``GENERATED_*``
or ``LEGACY_*`` identifier, fails without that marker.  A malformed marker
fails too.  Test code is out of scope.

``--audit`` lists every stand-in in the tree.  ``--baseline`` compares the
per-file count with ``scripts/stand_in_baseline.json`` and fails only when a
count rises, so existing stand-ins need no marker until someone labels them.
``--check-issues`` reports markers whose issue is closed; it runs on a
schedule, not on pull requests.

Exit 0 means no findings, 1 means findings, and 2 means the diff, a file or the
baseline could not be read, or ``gh`` failed.  Standard library only.
"""

import argparse
from collections import Counter
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools" / "unwrap_ratchet"))
from unwrap_ratchet import mask_with_comments


CLAIM_RE = re.compile(
    # "In retail, X defaults to 3.0" locates the claim in the retail game.
    r"\bin\s+retail\b"
    # "including retail zero speeds" states that retail data holds them.
    r"|\b(?:including|includes)\s+retail\b"
    # "Retail profiles hold zero speeds": retail is the subject of a claim
    # verb.  A determiner before "retail" marks an adjective or a label
    # ("the retail constant"), which is not a claim.
    r"|(?<![\w-])(?<!the )(?<!a )(?<!an )(?<!this )(?<!that )(?<!its )"
    r"(?<!our )(?<!their )(?<!every )(?<!no )retail\b(?!-)"
    r"(?:\s+[A-Za-z0-9_.`'&/()×%-]+){0,4}\s+"
    r"(?:is|are|was|were|has|have|had|does|do|did|holds?|contains?|uses?|"
    r"can|cannot|will|would|should|must|gives?|returns?|stores?|reads?|"
    r"writes?|names?|keeps?|applies|comes?|defaults?|adds?|multiplies|"
    r"computes?|counts?|sets?|takes?|makes?|runs?|loads?|treats?|clamps?|"
    r"produces?|reports?|shows?|states?|records?|means?|requires?|allows?|"
    r"resolves?|exports?|skips?|drops?|merges?)\b"
    r"|\bvanilla\b"
    r"|\bthe game does\b"
    r"|\bskyrim does\b"
    r"|\bin skyrim\b",
    re.IGNORECASE,
)
RF_RE = re.compile(r"\bRF-[A-Z0-9]+-[0-9]+\b")
RESEARCH_RE = re.compile(
    r"\bdocs/research/[^\s)\]>]+"
    r"|(?:\.\./)+research/[^\s)\]>]+"
    r"|\(research/[^\s)\]>]+",
    re.IGNORECASE,
)
UNVERIFIED_RE = re.compile(r"\bunverified\b", re.IGNORECASE)
SHA256_RE = re.compile(r"\b[0-9a-f]{64}\b", re.IGNORECASE)
MARKDOWN_LINK_RE = re.compile(r"\[[^\]\n]+\]\(\s*<?([^)\s>]+)")
REFERENCE_LINK_RE = re.compile(
    r"^\s{0,3}\[[^\]\n]+\]:\s*<?([^\s>]+)", re.MULTILINE
)
URL_RE = re.compile(r"https?://[^\s)\]>]+", re.IGNORECASE)
NON_RETAIL_RE = re.compile(
    r"\bnot\s+(?:a\s+)?retail\b"
    r"|\bnon[-\s]?retail\b"
    r"|\bnever\s+retail\b"
    r"|\bdiffers?\s+from\s+retail\b"
    r"|\bdifferent\s+from\s+retail\b"
    r"|\bunlike\s+retail\b"
    r"|\bnot\s+(?:a\s+)?vanilla\b"
    r"|\bnon[-\s]?vanilla\b"
    r"|\bnot\s+in\s+skyrim\b"
    r"|\bdiffers?\s+from\s+vanilla\b"
    r"|\bmudcrab[-\s]only\b"
    r"|\bno\s+retail\s+(?:equivalent|counterpart|behaviou?r)\b",
    re.IGNORECASE,
)
REASON_RE = re.compile(
    r"\b(?:because|since|so\s+that|due\s+to)\b"
    r"|\bto\s+(?:avoid|keep|prevent|match|make|let|stop)\b"
    r"|\bmudcrab\s+(?:uses|needs|has|adds|requires|keeps|does)\b"
    r"|\bwe\s+(?:use|need|add|keep|do)\b",
    re.IGNORECASE,
)
REGISTRY_PATH = "docs/research/retail/registry.md"
REGISTRY_ROW_RE = re.compile(r"^\s*\|\s*RF-[A-Z0-9]+-[0-9]+\s*\|")
KIND_RE = re.compile(r"\bkind:\s*([A-Za-z-]+)", re.IGNORECASE)
KIND_WORD_RE = re.compile(r"\b(?:quirk|defect)s?\b", re.IGNORECASE)
KINDS = ("rule", "quirk", "defect")
STAND_IN_WORD_RE = re.compile(r"\bSTAND-IN\b")
STAND_IN_MARKER_RE = re.compile(
    r"\bSTAND-IN\(\s*"
    r"(?P<issue>#\d+|https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/issues/\d+)"
    r"(?:\s*,\s*@(?P<owner>[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?))?"
    r"\s*\)\s*:\s*(?P<what>[^\n]+)",
)
# The trigger list is narrow on purpose: bare "stand-in", "temporary", "fallback",
# "approximation" and "synthetic" are common engineering words.  See section 6 of
# the stand-in audit.
STAND_IN_TRIGGER_RE = re.compile(
    r"\bplaceholder\b"
    r"|\bprovisional\b"
    r"|\binterim\b"
    r"|\bfor\s+now\b"
    r"|\bmade\s+up\b"
    r"|\bnot\s+consumed\b"
    r"|\blegacy\s+fallback\b"
    r"|\blegacy\s+normalization\b"
    r"|\bengineering\s+approximation\b"
    r"|\beyeball(?:ed|ing)?\b"
    r"|\bfitted\b"
    r"|\bgenerated\s+white\b"
    r"|\binvented\s+(?:value|colour|color|constant|normal|height)\b"
    r"|\b(?:a|the)\s+stand-in\s+(?:for|of)\s+(?:(?:a|the)\s+)?"
    r"(?:retail|authored|record|texture|asset|value|colour|color)\b",
    re.IGNORECASE,
)
STAND_IN_NEGATION_RE = re.compile(
    r"\b(?:no|not|never|without|nothing)\b", re.IGNORECASE
)
STAND_IN_IDENT_RE = re.compile(
    r"\bPLACEHOLDER\w*\b"
    r"|\bGENERATED(?:[_-]\w+|(?:WHITE|COLOR|COLOUR|NORMAL)\w*)\b"
    r"|\bSYNTHETIC\w*\b"
    r"|\bDUMMY\w*\b"
    r"|\bFAKE\w*\b"
    r"|\bLEGACY_\w+\b",
    re.IGNORECASE,
)
STAND_IN_MIN_WORDS = 4
STAND_IN_NO_DATA = "no retail data"
DEFAULT_BASELINE = Path("scripts/stand_in_baseline.json")
TEST_PATH_RE = re.compile(r"(?:^|/)(?:tests|recovery_tests)/")
HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")
HEADING_RE = re.compile(r"^\s{0,3}(#{1,6})\s")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
BLOCKQUOTE_RE = re.compile(r"^\s*(?:>\s*)+")


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    text: str
    unreasoned_note: bool
    kind_problem: str = ""
    stand_in_problem: str = ""


class CheckError(RuntimeError):
    """The diff or a file it names cannot be checked."""


def git(repo, *args):
    """Run git and return stdout, or raise CheckError with its message."""
    command = ["git", "-C", str(repo), "-c", "core.quotePath=false", *args]
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
        raise CheckError(f"{' '.join(command)}: {message}")
    return result.stdout


def language_for(path):
    """Return the checker name for a path, or None when it is out of scope."""
    if path.startswith("crates/") and path.endswith(".rs"):
        return "rust"
    if path.startswith("crates/") and path.endswith(".wgsl"):
        return "wgsl"
    if path.endswith(".md") and (path.startswith("docs/") or path.startswith("crates/")):
        return "markdown"
    return None


def parse_diff(diff_text):
    """Return added lines per path and the texts of the deleted lines."""
    files = {}
    removed = Counter()
    path = None
    new_line = None
    in_hunk = False
    for raw in diff_text.splitlines():
        if raw.startswith("diff "):
            path = None
            new_line = None
            in_hunk = False
        elif not in_hunk and raw.startswith("+++ "):
            target = raw[4:]
            if target == "/dev/null":
                path = None
            else:
                path = target[2:] if target.startswith("b/") else target
                files.setdefault(path, {})
        elif raw.startswith("@@"):
            match = HUNK_RE.match(raw)
            if match is None:
                raise CheckError(f"cannot parse diff hunk: {raw}")
            new_line = int(match.group(1))
            in_hunk = path is not None
        elif in_hunk and new_line is not None:
            if raw.startswith("+"):
                files[path][new_line] = raw[1:]
                new_line += 1
            elif raw.startswith("-"):
                removed[raw[1:]] += 1
            elif raw.startswith("\\"):
                continue
            else:
                new_line += 1
    return files, removed


def read_file(repo, head, path):
    """Read one path at the checked revision, not from the working tree."""
    return git(repo, "show", f"{head}:{path}").split("\n")


def source_lines(lines, language):
    if language == "markdown":
        return None, None
    source = "\n".join(lines)
    code, spans = mask_with_comments(source)
    comments = [None] * len(lines)
    line = 0
    previous = 0
    for start, end in spans:
        if source.startswith("//", start):
            line += source.count("\n", previous, start)
            comments[line] = source[start + 2:end]
            previous = end
    return code.split("\n") if language == "rust" else None, comments


def build_comment_blocks(texts):
    blocks = []
    line_block = {}
    index = 0
    while index < len(texts):
        if texts[index] is None:
            index += 1
            continue
        start = index
        parts = []
        while index < len(texts):
            text = texts[index]
            if text is None:
                break
            parts.append(text)
            index += 1
        block = len(blocks)
        blocks.append("\n".join(parts))
        for number in range(start + 1, index + 1):
            line_block[number] = block
    return blocks, line_block


def build_markdown_blocks(lines):
    """Return block text and a line-to-section map for Markdown prose.

    A block is a section: its heading to the next heading of the same or a
    higher level.  A source anywhere in the section labels a claim in it.
    Fenced code is ignored.  Content before the first heading is its own
    block.
    """
    blocks = []
    line_block = {}
    open_sections = []
    fence = None
    for number, raw in enumerate(lines, 1):
        line = raw.rstrip("\r")
        if fence is not None:
            stripped = line.strip()
            if stripped.startswith(fence[0] * fence[1]):
                fence = None
            continue
        match = FENCE_RE.match(line)
        if match is not None:
            fence = (match.group(1)[0], len(match.group(1)))
            continue
        content = BLOCKQUOTE_RE.sub("", line).strip()
        if not content:
            continue
        heading = HEADING_RE.match(content)
        if heading is not None:
            level = len(heading.group(1))
            while open_sections and open_sections[-1][0] >= level:
                open_sections.pop()
            blocks.append(content)
            open_sections.append((level, len(blocks) - 1))
            line_block[number] = len(blocks) - 1
            continue
        if not open_sections:
            blocks.append("")
            open_sections.append((1, len(blocks) - 1))
        index = open_sections[-1][1]
        blocks[index] += "\n" + content
        line_block[number] = index
    return blocks, line_block


def has_markdown_source(text):
    """Return True when the text links to a Markdown file or an external URL."""
    if URL_RE.search(text):
        return True
    for pattern in (MARKDOWN_LINK_RE, REFERENCE_LINK_RE):
        for match in pattern.finditer(text):
            target = match.group(1).split("#", 1)[0].split("?", 1)[0]
            if target.lower().endswith(".md"):
                return True
    return False


def is_labelled(block):
    """Return True when a block names a source or a reasoned non-retail note."""
    if (RF_RE.search(block) or RESEARCH_RE.search(block)
            or UNVERIFIED_RE.search(block) or SHA256_RE.search(block)):
        return True
    if has_markdown_source(block):
        return True
    return bool(NON_RETAIL_RE.search(block) and REASON_RE.search(block))


def makes_claim(text):
    """Return True when the text states retail behaviour or a non-retail note."""
    return bool(CLAIM_RE.search(text) or NON_RETAIL_RE.search(text))


def stand_in_marker(line):
    """Return the marker match on a line, or None when it is absent or invalid."""
    if not STAND_IN_WORD_RE.search(line):
        return None
    match = STAND_IN_MARKER_RE.search(line)
    if match is None:
        return None
    what = match.group("what").strip()
    if what.rstrip(".").strip().lower() == STAND_IN_NO_DATA:
        return match
    if len(what.split()) < STAND_IN_MIN_WORDS:
        return None
    return match


def stand_in_marker_problem(line):
    """Return why a STAND-IN marker on this line is invalid, or an empty string."""
    if not STAND_IN_WORD_RE.search(line):
        return ""
    match = STAND_IN_MARKER_RE.search(line)
    if match is None:
        return ("STAND-IN needs an issue: "
                "STAND-IN(#N): <what it replaces>")
    what = match.group("what").strip()
    if what.rstrip(".").strip().lower() == STAND_IN_NO_DATA:
        return ""
    if len(what.split()) < STAND_IN_MIN_WORDS:
        return "STAND-IN needs at least four words after the colon"
    return ""


def is_stand_in_labelled(block):
    """Return True when a comment block or Markdown section holds a valid marker."""
    return any(
        STAND_IN_WORD_RE.search(line) and stand_in_marker(line) is not None
        for line in block.split("\n")
    )


def is_stand_in_trigger(text):
    """Return True when the text admits a stand-in, with the negation guard."""
    for match in STAND_IN_TRIGGER_RE.finditer(text):
        before = text[max(0, match.start() - 24):match.start()]
        if STAND_IN_NEGATION_RE.search(before):
            continue
        return True
    return False


def direct_type_member_lines(code_lines):
    """Return 1-based lines that are direct struct fields or enum variants."""
    members = set()
    stack = []
    pending_type = False
    for number, code in enumerate(code_lines, 1):
        if stack and stack[-1] == "type":
            members.add(number)
        if re.search(r"\b(?:struct|enum)\b", code):
            pending_type = True
        for char in code:
            if char == "{":
                stack.append("type" if pending_type else "other")
                pending_type = False
            elif char == "}":
                if stack:
                    stack.pop()
            elif char == ";" and not stack:
                pending_type = False
    return members


def stand_in_identifier(line, direct_member=False):
    """Return the declared stand-in name on a code line, or None.

    A declaration is a ``const``, ``static`` or ``fn`` item, a struct field, or
    an enum variant.  A use such as ``Entity::PLACEHOLDER`` is not a declaration,
    and a parameter or a struct-literal field is not a type member.
    """
    for match in STAND_IN_IDENT_RE.finditer(line):
        prefix = line[:match.start()]
        suffix = line[match.end():]
        if re.search(r"\b(?:const|static|fn)\s+(?:mut\s+)?$", prefix):
            return match.group(0)
        if (direct_member
                and re.fullmatch(r"\s*(?:pub(?:\([^)]*\))?\s*)?", prefix)
                and re.match(r"\s*(?::|,|\(|\{)", suffix)):
            return match.group(0)
    return None


def preceding_comment_block(number, lines, line_block):
    """Return the comment block directly above a 1-based code line, if any.

    Attribute lines between the comment and the item are skipped.  A blank line
    ends the search, so a file-level marker does not label a later item.
    """
    index = number - 1
    while index >= 1:
        stripped = lines[index - 1].strip()
        if not stripped:
            return None
        if stripped.startswith("#[") or stripped.startswith("#!["):
            index -= 1
            continue
        return line_block.get(index)
    return None


def declaration_is_labelled(number, lines, line_block, blocks):
    """Return True when a declaration's own line or the block above has a marker."""
    for block in (line_block.get(number),
                  preceding_comment_block(number, lines, line_block)):
        if block is not None and is_stand_in_labelled(blocks[block]):
            return True
    return False


def is_test_path(path):
    """Return True for a test file, whose stand-ins are out of scope."""
    name = Path(path).name
    return (name == "tests.rs" or name.endswith("_tests.rs")
            or TEST_PATH_RE.search(path) is not None)


def cfg_test_start(lines):
    """Return the 1-based line of the first ``#[cfg(test)]``, or None."""
    for number, line in enumerate(lines, 1):
        if line.strip().startswith("#[cfg(test)]"):
            return number
    return None


def module_doc_cites_registry(texts):
    return any(
        text is not None and text.startswith("!") and RF_RE.search(text)
        for text in texts
    )


def registry_kind_problem(text):
    """Return the kind problem of an added registry row, or an empty string."""
    if REGISTRY_ROW_RE.match(text) is None:
        return ""
    kinds = [value.lower() for value in KIND_RE.findall(text)]
    if len(kinds) > 1:
        return "registry row has more than one kind field"
    if kinds:
        if kinds[0] in KINDS:
            return ""
        return (f"registry row has the invalid kind \"{kinds[0]}\"; "
                "use rule, quirk or defect")
    if KIND_WORD_RE.search(text):
        return ("registry row says quirk or defect but has no kind field; "
                "add \"kind: quirk\" or \"kind: defect\"")
    return ""


def check(repo, base, head):
    """Return (checked, claims, stand-in lines, findings) for one revision range."""
    # A second -C is the expensive copy search: it finds a copy whose source
    # file did not change in the same diff.
    diff = git(repo, "-c", "color.ui=false", "diff", "-U0", "-M", "-C", "-C",
               f"{base}...{head}")
    changed, removed = parse_diff(diff)
    checked = 0
    claims = 0
    stand_ins = 0
    findings = []
    for path, added in changed.items():
        language = language_for(path)
        if language is None or not added:
            continue
        lines = read_file(repo, head, path)
        code_lines, texts = source_lines(lines, language)
        if texts is None:
            blocks, line_block = build_markdown_blocks(lines)
        else:
            blocks, line_block = build_comment_blocks(texts)
        file_labelled = language == "rust" and module_doc_cites_registry(texts)
        test_from = cfg_test_start(code_lines) if code_lines is not None else None
        type_members = (direct_type_member_lines(code_lines)
                        if code_lines is not None else set())
        for number in sorted(added):
            if number < 1 or number > len(lines):
                raise CheckError(f"{path}:{number}: line is outside the checked revision")
            text = added[number]
            if removed[text] > 0:
                removed[text] -= 1
                continue
            claim = (lines[number - 1].strip() if texts is None
                     else texts[number - 1])
            in_block = number in line_block
            if in_block:
                checked += 1
                if claim is not None:
                    if path == REGISTRY_PATH:
                        problem = registry_kind_problem(text)
                        if problem:
                            findings.append(Finding(
                                path=path,
                                line=number,
                                text=text.strip(),
                                unreasoned_note=False,
                                kind_problem=problem,
                            ))
                    if makes_claim(claim):
                        claims += 1
                        if not (file_labelled
                                or is_labelled(blocks[line_block[number]])):
                            findings.append(Finding(
                                path=path,
                                line=number,
                                text=text.strip(),
                                unreasoned_note=bool(NON_RETAIL_RE.search(claim)),
                            ))
            if is_test_path(path) or (test_from is not None and number >= test_from):
                continue
            if (code_lines is not None
                    and stand_in_identifier(code_lines[number - 1],
                                            number in type_members) is not None):
                stand_ins += 1
                if not declaration_is_labelled(number, lines, line_block, blocks):
                    findings.append(Finding(
                        path=path,
                        line=number,
                        text=text.strip(),
                        unreasoned_note=False,
                        stand_in_problem="stand-in identifier has no label",
                    ))
            if not in_block or claim is None:
                continue
            problem = stand_in_marker_problem(claim)
            if problem:
                findings.append(Finding(
                    path=path,
                    line=number,
                    text=text.strip(),
                    unreasoned_note=False,
                    stand_in_problem=problem,
                ))
                continue
            if STAND_IN_WORD_RE.search(claim):
                continue
            if is_stand_in_trigger(claim):
                stand_ins += 1
                if not is_stand_in_labelled(blocks[line_block[number]]):
                    findings.append(Finding(
                        path=path,
                        line=number,
                        text=text.strip(),
                        unreasoned_note=False,
                        stand_in_problem="stand-in has no label",
                    ))
    findings.sort(key=lambda finding: (finding.path, finding.line))
    return checked, claims, stand_ins, findings


def tracked_paths(repo):
    """Return the repository's tracked paths."""
    return [path for path in git(repo, "ls-files", "-z").split("\0") if path]


def read_worktree_file(repo, path):
    """Read a tracked path from the working tree for an audit."""
    try:
        return (Path(repo) / path).read_text(
            encoding="utf-8", errors="replace"
        ).split("\n")
    except OSError as error:
        raise CheckError(f"{path}: cannot read: {error}") from error


def stand_ins_in_lines(path, lines):
    """Return (path, line, problem, text) for every stand-in in one file."""
    language = language_for(path)
    hits = []
    if language is None or is_test_path(path):
        return hits
    code_lines, texts = source_lines(lines, language)
    if texts is None:
        blocks, line_block = build_markdown_blocks(lines)
    else:
        blocks, line_block = build_comment_blocks(texts)
    test_from = cfg_test_start(code_lines) if code_lines is not None else None
    type_members = (direct_type_member_lines(code_lines)
                    if code_lines is not None else set())
    for number, raw in enumerate(lines, 1):
        if test_from is not None and number >= test_from:
            break
        stripped = raw.strip()
        if (code_lines is not None
                and stand_in_identifier(code_lines[number - 1],
                                        number in type_members) is not None
                and not declaration_is_labelled(
                    number, lines, line_block, blocks)):
            hits.append(
                (path, number, "stand-in identifier has no label", stripped)
            )
        if texts is None:
            if number not in line_block:
                continue
            if STAND_IN_WORD_RE.search(raw):
                problem = stand_in_marker_problem(raw)
                if problem:
                    hits.append((path, number, problem, stripped))
                continue
            if (is_stand_in_trigger(stripped)
                    and not is_stand_in_labelled(blocks[line_block[number]])):
                hits.append((path, number, "stand-in has no label", stripped))
            continue
        comment = texts[number - 1]
        if comment is None:
            continue
        if STAND_IN_WORD_RE.search(comment):
            problem = stand_in_marker_problem(comment)
            if problem:
                hits.append((path, number, problem, stripped))
            continue
        if (is_stand_in_trigger(comment)
                and not is_stand_in_labelled(blocks[line_block[number]])):
            hits.append((path, number, "stand-in has no label", stripped))
    return hits


def audit(repo):
    """Return every unlabelled stand-in in the working tree."""
    hits = []
    for path in tracked_paths(repo):
        if language_for(path) is None or is_test_path(path):
            continue
        hits.extend(stand_ins_in_lines(path, read_worktree_file(repo, path)))
    hits.sort(key=lambda hit: (hit[0], hit[1]))
    return hits


def stand_in_counts(hits):
    """Return the number of hits per file."""
    return Counter(hit[0] for hit in hits)


def load_baseline(path):
    """Return (per-file counts, revision) from a baseline file."""
    try:
        data = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise CheckError(f"cannot read baseline {path}: {error}") from error
    counts = data.get("counts") if isinstance(data, dict) else None
    revision = data.get("revision", 1) if isinstance(data, dict) else None
    if not isinstance(counts, dict):
        raise CheckError(f"baseline {path} has no counts object")
    if not isinstance(revision, int) or isinstance(revision, bool):
        raise CheckError(f"baseline {path} has no integer revision")
    for key, value in counts.items():
        if (not isinstance(key, str) or not isinstance(value, int)
                or isinstance(value, bool) or value < 0):
            raise CheckError(f"baseline {path} has an invalid count for {key!r}")
    return counts, revision


def write_baseline(path, counts):
    """Write the current counts, raising the revision so the change is visible."""
    target = Path(path)
    revision = 1
    if target.exists():
        _, previous = load_baseline(target)
        revision = previous + 1
    document = {"counts": dict(counts), "revision": revision}
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(
            json.dumps(document, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    except OSError as error:
        raise CheckError(f"cannot write baseline {target}: {error}") from error


def run_audit(repo, baseline, write):
    """List every stand-in, optionally gate or write the baseline."""
    hits = audit(repo)
    for path, line, problem, text in hits:
        print(f"{path}:{line}: {problem}: {text}")
    counts = stand_in_counts(hits)
    print(f"Stand-ins: {len(hits)} unlabelled hit(s) in {len(counts)} file(s).")
    if write:
        target = baseline or str(DEFAULT_BASELINE)
        write_baseline(target, counts)
        print(f"Wrote {target} with {len(hits)} hit(s).")
        return 0
    if baseline is None:
        print("Audit only: pass --baseline or --write-baseline to gate a count.")
        return 0
    limits, revision = load_baseline(baseline)
    risen = [
        (path, limits.get(path, 0), count)
        for path, count in sorted(counts.items())
        if count > limits.get(path, 0)
    ]
    if risen:
        for path, limit, count in risen:
            print(f"{path}: stand-in count rose from {limit} to {count}",
                  file=sys.stderr)
        print(f"\n{len(risen)} file(s) hold more stand-ins than the baseline "
              f"({baseline}, revision {revision}). Add a marker with an open "
              "issue, or raise the baseline in a reviewed change.",
              file=sys.stderr)
        return 1
    print(f"Stand-in count is at or below the baseline ({baseline}, "
          f"revision {revision}).")
    return 0


def github_repository(repo):
    """Return owner/repo for the inspected repository, or an empty string."""
    configured = os.environ.get("GITHUB_REPOSITORY", "").strip()
    if re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", configured):
        return configured
    try:
        remote = git(repo, "remote", "get-url", "origin").strip()
    except CheckError:
        return ""
    match = re.search(
        r"github\.com[:/]([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+?)(?:\.git)?$",
        remote,
    )
    return match.group(1) if match else ""


def issue_target(issue, repository):
    """Return (cache key, gh query, repository argument) for a marker issue."""
    if issue.startswith("#"):
        number = str(int(issue[1:]))
        key = f"{repository}#{number}" if repository else f"#{number}"
        return key, number, repository
    return issue, issue, ""


def collect_markers(repo):
    """Return (path, line, issue) for every valid marker."""
    markers = []
    for path in tracked_paths(repo):
        language = language_for(path)
        if language is None or is_test_path(path):
            continue
        lines = read_worktree_file(repo, path)
        code_lines, texts = source_lines(lines, language)
        if texts is None:
            _, line_block = build_markdown_blocks(lines)
        else:
            _, line_block = build_comment_blocks(texts)
        test_from = cfg_test_start(code_lines) if code_lines is not None else None
        for number, raw in enumerate(lines, 1):
            if test_from is not None and number >= test_from:
                break
            if number not in line_block:
                continue
            line = raw if texts is None else texts[number - 1]
            if line is None:
                continue
            match = stand_in_marker(line)
            if match is None:
                continue
            markers.append((path, number, match.group("issue")))
    return markers


def review_issue(query, repository):
    """Return the state of one issue target, or raise CheckError."""
    command = ["gh", "issue", "view", query]
    if repository:
        command += ["-R", repository]
    command += ["--json", "state"]
    try:
        result = subprocess.run(
            command,
            text=True,
            capture_output=True,
            check=False,
        )
    except FileNotFoundError as error:
        raise CheckError(
            "gh is not installed; --check-issues needs the GitHub CLI"
        ) from error
    if result.returncode:
        message = (result.stderr.strip() or result.stdout.strip()
                   or f"exit {result.returncode}")
        raise CheckError(f"{' '.join(command)}: {message}")
    try:
        data = json.loads(result.stdout)
    except ValueError as error:
        raise CheckError(f"{' '.join(command)}: invalid JSON: {error}") from error
    state = data.get("state") if isinstance(data, dict) else None
    if not isinstance(state, str):
        raise CheckError(f"{' '.join(command)}: response has no state")
    return state


def check_issues(repo):
    """Report markers whose issue is closed."""
    markers = collect_markers(repo)
    repository = github_repository(repo)
    states = {}
    problems = []
    for path, line, issue in markers:
        key, query, target_repository = issue_target(issue, repository)
        if key not in states:
            states[key] = review_issue(query, target_repository)
        state = states[key]
        if state.upper() != "OPEN":
            problems.append((path, line, f"issue {issue} is {state.lower()}"))
    for path, line, problem in problems:
        print(f"{path}:{line}: {problem}", file=sys.stderr)
    if problems:
        print(f"\n{len(problems)} stand-in marker(s) name a closed issue.",
              file=sys.stderr)
        return 1
    print(f"Stand-in issues: {len(markers)} marker(s) name an open issue.")
    return 0


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Require a source on added comment and doc retail claims, "
                    "and a label on every stand-in for retail data.",
    )
    parser.add_argument("--repo", default=".", help="repository to inspect (default: .)")
    parser.add_argument(
        "--base",
        default=os.environ.get("RETAIL_CLAIM_BASE", "origin/main"),
        help="base revision; the diff uses its merge base (default: origin/main)",
    )
    parser.add_argument("--head", default="HEAD", help="changed revision (default: HEAD)")
    parser.add_argument(
        "--audit",
        action="store_true",
        help="list every stand-in in the tree instead of checking one diff",
    )
    parser.add_argument(
        "--baseline",
        nargs="?",
        const=str(DEFAULT_BASELINE),
        default=None,
        help="with --audit, fail when a file's count rises above this baseline "
             f"(default: {DEFAULT_BASELINE})",
    )
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="with --audit, write the current counts to the baseline",
    )
    parser.add_argument(
        "--check-issues",
        action="store_true",
        help="report markers whose issue is closed; runs on a schedule, "
             "not in pull-request CI",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    repo = Path(args.repo)
    try:
        if args.check_issues:
            return check_issues(repo)
        if args.audit or args.baseline is not None or args.write_baseline:
            return run_audit(repo, args.baseline, args.write_baseline)
        checked, claims, stand_ins, findings = check(repo, args.base, args.head)
    except (CheckError, OSError) as error:
        print(f"retail claim check: {error}", file=sys.stderr)
        return 2
    kind_findings = [finding for finding in findings if finding.kind_problem]
    stand_in_findings = [
        finding for finding in findings
        if not finding.kind_problem and finding.stand_in_problem
    ]
    findings = [
        finding for finding in findings
        if not finding.kind_problem and not finding.stand_in_problem
    ]
    for finding in kind_findings:
        print(f"{finding.path}:{finding.line}: {finding.kind_problem}: "
              f"{finding.text}", file=sys.stderr)
    if kind_findings:
        print(
            f"\n{len(kind_findings)} registry row(s) have a kind problem. "
            "Kinds are rule, quirk and defect; see docs/adr/"
            "0015-retail-rule-quirk-defect.md.",
            file=sys.stderr,
        )
    for finding in stand_in_findings:
        print(f"{finding.path}:{finding.line}: {finding.stand_in_problem}: "
              f"{finding.text}", file=sys.stderr)
    if stand_in_findings:
        print(
            f"\n{len(stand_in_findings)} stand-in line(s) need a label. Add "
            "\"STAND-IN(#NNN): <what it replaces>\" in the same comment "
            "block or Markdown section, with an open issue. An owner handle "
            "is optional. See ADR-0015.",
            file=sys.stderr,
        )
    if findings:
        for finding in findings:
            if finding.unreasoned_note:
                problem = ("retail claim has no source and its non-retail note "
                           "gives no reason")
            else:
                problem = "retail claim has no source"
            print(f"{finding.path}:{finding.line}: {problem}: {finding.text}",
                  file=sys.stderr)
        print(
            f"\n{len(findings)} retail claim line(s) need a source. Add an RF ID, "
            "a link to a repository Markdown file, an external source URL, or the "
            "word \"unverified\" on the same line, in the same comment block, or "
            "in the same section.",
            file=sys.stderr,
        )
        if any(finding.unreasoned_note for finding in findings):
            print(
                "A non-retail or Mudcrab-only note also needs a reason: "
                "\"because\", \"since\", \"so that\", \"due to\", \"to keep\", "
                "or what Mudcrab does instead.",
                file=sys.stderr,
            )
    if findings or kind_findings or stand_in_findings:
        return 1
    print(f"Retail claims: {claims} claim line(s) checked in {checked} added "
          "comment/doc line(s), all labelled.")
    print(f"Stand-ins: {stand_ins} stand-in line(s) checked in added lines, "
          "all labelled.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
