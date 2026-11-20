#!/usr/bin/env python3
"""Require a source on added comments and docs that state retail behaviour.

The check reads ``git diff -U0 <base>...<head>`` and inspects only
added lines.  A line in a Rust or WGSL ``//`` comment, or in Markdown under
``docs/`` or ``crates/``, that makes a retail claim must carry one of these in
the same comment block or Markdown paragraph:

* a retail-facts registry ID such as ``RF-TEX-004``;
* a link into ``docs/research/``;
* the word ``unverified``.

A line that says ``not retail``, ``differs from retail`` or ``Mudcrab-only``
is labelled only when it also states a reason.  Hyphenated compounds such as
``retail-parity`` are not claims.  The accepted reason markers are listed in
``REASON_RE`` and in ``docs/contributing/writing-prs.md``.

Exit 0 means no unlabelled claims, 1 means findings, and 2 means the diff or
one of its files could not be read.  Standard library only.
"""

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import sys


CLAIM_RE = re.compile(
    r"\bretail\b(?!-)"
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
NON_RETAIL_RE = re.compile(
    r"\bnot\s+(?:a\s+)?retail\b"
    r"|\bnon[-\s]?retail\b"
    r"|\bnever\s+retail\b"
    r"|\bdiffers?\s+from\s+retail\b"
    r"|\bdifferent\s+from\s+retail\b"
    r"|\bunlike\s+retail\b"
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
HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")
HEADING_RE = re.compile(r"^\s{0,3}#{1,6}\s")
TABLE_RE = re.compile(r"^\s*\|")
LIST_RE = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s+")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
BLOCKQUOTE_RE = re.compile(r"^\s*(?:>\s*)+")
RAW_STRING_RE = re.compile(r'r(#*)"')


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    text: str
    unreasoned_note: bool


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


def parse_added_lines(diff_text):
    """Map each changed path to its added line numbers and text."""
    files = {}
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
                continue
            elif raw.startswith("\\"):
                continue
            else:
                new_line += 1
    return files


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
    """Return block text and a line-to-block map for Markdown prose.

    Fenced code is ignored.  A heading joins the paragraph below it so a
    source in the first paragraph also labels the heading.  Table rows and
    list items are separate blocks.
    """
    blocks = []
    line_block = {}
    current = None
    heading_pending = False
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
            current = None
            heading_pending = False
            continue
        content = BLOCKQUOTE_RE.sub("", line).strip()
        if not content:
            if not heading_pending:
                current = None
            continue
        stripped = content
        if HEADING_RE.match(stripped):
            blocks.append(stripped)
            current = len(blocks) - 1
            line_block[number] = current
            heading_pending = True
            continue
        if TABLE_RE.match(stripped) or LIST_RE.match(stripped):
            blocks.append(stripped)
            current = len(blocks) - 1
            line_block[number] = current
            heading_pending = False
            continue
        if current is None:
            blocks.append(stripped)
            current = len(blocks) - 1
        else:
            blocks[current] += "\n" + stripped
        line_block[number] = current
        heading_pending = False
    return blocks, line_block


def is_labelled(block):
    """Return True when a block names a source or a reasoned non-retail note."""
    if RF_RE.search(block) or RESEARCH_RE.search(block) or UNVERIFIED_RE.search(block):
        return True
    return bool(NON_RETAIL_RE.search(block) and REASON_RE.search(block))


def check(repo, base, head):
    """Return (checked lines, claim lines, findings) for one revision range."""
    diff = git(repo, "-c", "diff.renames=false", "-c", "color.ui=false",
               "diff", "-U0", f"{base}...{head}")
    changed = parse_added_lines(diff)
    checked = 0
    claims = 0
    findings = []
    for path, added in changed.items():
        language = language_for(path)
        if language is None or not added:
            continue
        lines = read_file(repo, head, path)
        if language == "markdown":
            blocks, line_block = build_markdown_blocks(lines)
            texts = None
        else:
            texts = scan_comment_texts(lines)
            blocks, line_block = build_comment_blocks(lines, texts)
        for number in sorted(added):
            if number < 1 or number > len(lines):
                raise CheckError(f"{path}:{number}: line is outside the checked revision")
            if number not in line_block:
                continue
            text = added[number]
            claim = lines[number - 1].strip() if texts is None else texts[number - 1]
            if claim is None:
                continue
            checked += 1
            if not (CLAIM_RE.search(claim) or NON_RETAIL_RE.search(claim)):
                continue
            claims += 1
            block = blocks[line_block[number]]
            if is_labelled(block):
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
            "a docs/research/ link, or the word \"unverified\" on the same line, "
            "in the same comment block, or in the same paragraph.",
            file=sys.stderr,
        )
        return 1
    print(f"Retail claims: {claims} claim line(s) checked in {checked} added "
          "comment/doc line(s), all labelled.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
