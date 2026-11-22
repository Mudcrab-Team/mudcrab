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

Exit 0 means no unlabelled claims, 1 means findings, and 2 means the diff or
one of its files could not be read.  Standard library only.
"""

import argparse
from collections import Counter
from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import sys


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
HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")
HEADING_RE = re.compile(r"^\s{0,3}(#{1,6})\s")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
BLOCKQUOTE_RE = re.compile(r"^\s*(?:>\s*)+")
RAW_STRING_RE = re.compile(r'r(#*)"')


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    text: str
    unreasoned_note: bool
    kind_problem: str = ""


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


def skip_char_or_lifetime(line, start):
    """Skip a Rust character literal; leave a lifetime alone."""
    if start + 2 < len(line) and line[start + 1] == "\\":
        end = line.find("'", start + 2)
        return end + 1 if end >= 0 else start + 1
    if start + 2 < len(line) and line[start + 2] == "'":
        return start + 3
    return start + 1


def scan_comment_texts(lines):
    """Return the ``//`` text of each line, or None when the line has none.

    State carries across lines.  A ``//`` inside a ``/* ... */`` block comment
    or a multi-line string is not a line comment.
    """
    texts = [None] * len(lines)
    block_depth = 0
    string = None
    for number, line in enumerate(lines):
        index = 0
        while index < len(line):
            if block_depth:
                if line.startswith("/*", index):
                    block_depth += 1
                    index += 2
                elif line.startswith("*/", index):
                    block_depth -= 1
                    index += 2
                else:
                    index += 1
                continue
            if string is not None:
                if string[0] == "raw":
                    terminator = '"' + string[1]
                    end = line.find(terminator, index)
                    if end < 0:
                        index = len(line)
                    else:
                        index = end + len(terminator)
                        string = None
                else:
                    while index < len(line):
                        if line[index] == "\\":
                            index += 2
                        elif line[index] == '"':
                            index += 1
                            string = None
                            break
                        else:
                            index += 1
                    if string is not None and not line.endswith("\\"):
                        string = None
                continue
            if line.startswith("//", index):
                texts[number] = line[index + 2:]
                break
            if line.startswith("/*", index):
                block_depth = 1
                index += 2
                continue
            char = line[index]
            if char == '"':
                string = ("normal",)
                index += 1
            elif char == "'":
                index = skip_char_or_lifetime(line, index)
            elif char == "r":
                match = RAW_STRING_RE.match(line, index)
                if match is None:
                    index += 1
                else:
                    string = ("raw", match.group(1))
                    index = match.end()
            else:
                index += 1
    return texts


def build_comment_blocks(lines, texts=None):
    """Return block text and a line-to-block map for // comments."""
    if texts is None:
        texts = scan_comment_texts(lines)
    blocks = []
    line_block = {}
    index = 0
    while index < len(lines):
        if texts[index] is None:
            index += 1
            continue
        start = index
        parts = []
        while index < len(lines):
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


def module_doc_cites_registry(lines, texts=None):
    """Return True when a ``//!`` module doc in the file cites an RF ID."""
    if texts is None:
        texts = scan_comment_texts(lines)
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
    """Return (checked lines, claim lines, findings) for one revision range."""
    # A second -C is the expensive copy search: it finds a copy whose source
    # file did not change in the same diff.
    diff = git(repo, "-c", "color.ui=false", "diff", "-U0", "-M", "-C", "-C",
               f"{base}...{head}")
    changed, removed = parse_diff(diff)
    checked = 0
    claims = 0
    findings = []
    for path, added in changed.items():
        language = language_for(path)
        if language is None or not added:
            continue
        lines = read_file(repo, head, path)
        file_labelled = False
        if language == "markdown":
            blocks, line_block = build_markdown_blocks(lines)
            texts = None
        else:
            texts = scan_comment_texts(lines)
            blocks, line_block = build_comment_blocks(lines, texts)
            file_labelled = (language == "rust"
                             and module_doc_cites_registry(lines, texts))
        for number in sorted(added):
            if number < 1 or number > len(lines):
                raise CheckError(f"{path}:{number}: line is outside the checked revision")
            if number not in line_block:
                continue
            text = added[number]
            claim = lines[number - 1].strip() if texts is None else texts[number - 1]
            if claim is None:
                continue
            if removed[text] > 0:
                removed[text] -= 1
                continue
            checked += 1
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
            if not makes_claim(claim):
                continue
            claims += 1
            if file_labelled or is_labelled(blocks[line_block[number]]):
                continue
            findings.append(Finding(
                path=path,
                line=number,
                text=text.strip(),
                unreasoned_note=bool(NON_RETAIL_RE.search(claim)),
            ))
    findings.sort(key=lambda finding: (finding.path, finding.line))
    return checked, claims, findings


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Require a source on added comment and doc retail claims.",
    )
    parser.add_argument("--repo", default=".", help="repository to inspect (default: .)")
    parser.add_argument(
        "--base",
        default=os.environ.get("RETAIL_CLAIM_BASE", "origin/main"),
        help="base revision; the diff uses its merge base (default: origin/main)",
    )
    parser.add_argument("--head", default="HEAD", help="changed revision (default: HEAD)")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    repo = Path(args.repo)
    try:
        checked, claims, findings = check(repo, args.base, args.head)
    except (CheckError, OSError) as error:
        print(f"retail claim check: {error}", file=sys.stderr)
        return 2
    kind_findings = [finding for finding in findings if finding.kind_problem]
    findings = [finding for finding in findings if not finding.kind_problem]
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
    if findings or kind_findings:
        return 1
    print(f"Retail claims: {claims} claim line(s) checked in {checked} added "
          "comment/doc line(s), all labelled.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
