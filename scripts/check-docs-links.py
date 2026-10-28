#!/usr/bin/env python3
"""Check that relative Markdown links in the repository resolve.

Documentation here often records work done in a research snapshot. Pages that
describe it use code spans for targets that live only in that snapshot, so a
relative link is a claim that this checkout contains the file. A link that does
not resolve is either a typo or a page that claims something this tree does not
have.

Deliberate exceptions live in `ALLOWED`, each with the reason it is kept. Adding
one should be a reviewed decision, not a way to silence the check.

Absolute URLs, mail links and pure `#anchor` links are not checked. Targets that
start with `/` are filesystem or site-absolute paths rather than repository
links, which is how the private working notes cite their own machines. Links
inside fenced code blocks and inline code spans are ignored, because they are
examples. Angle-bracket autolinks and raw HTML are not part of this contract.

This script deliberately takes no options. `crates/engine` audits every flag a
script under `scripts/` names against the engine parser, so a repository-wide
read-only check has no reason to add one.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parents[1]

SNAPSHOT_COPY = (
    "docs/research/skyrim-render-map/research-notes/note-0003/review-snapshots/"
    "pre-attribution-correction/intake-summary.md"
)
SNAPSHOT_REASON = (
    "byte-pinned pre-correction copy; the link resolves one level up in note-0003/ "
    "(see the README beside it)"
)

# (document, target) pairs that are allowed to stay unresolved, with the reason.
ALLOWED: dict[tuple[str, str], str] = {
    (SNAPSHOT_COPY, target): SNAPSHOT_REASON
    for target in (
        "source-sections.json",
        "claims-native-lighting.json",
        "claims-mod-interventions.json",
        "input-receipt.json",
        "../world-model.md",
        "../corpus-receipt-20261010-02.json",
    )
}

FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
INLINE_CODE = re.compile(r"(`+)(?!`).*?\1")
INLINE_LINK = re.compile(r"!?\[[^\]]*\]\(\s*(<[^>\s]*>|[^)\s]*)(?:\s+[\"'][^)]*[\"'])?\s*\)")
REFERENCE_LINK = re.compile(r"^ {0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)")
EXTERNAL = re.compile(r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|//)")


def tracked_documents(root: Path) -> list[str]:
    """Return the repository's tracked Markdown files."""
    completed = subprocess.run(
        ["git", "ls-files", "-z", "*.md"], cwd=root, capture_output=True, check=True
    )
    return sorted(name for name in completed.stdout.decode("utf-8").split("\0") if name)


def relative_links(text: str):
    """Yield (line number, target) for relative links outside code."""
    fence = None
    for number, line in enumerate(text.splitlines(), 1):
        opening = FENCE.match(line)
        if opening:
            marker = opening.group(1)[0]
            if fence is None:
                fence = marker
            elif fence == marker:
                fence = None
            continue
        if fence is not None:
            continue
        code_free = INLINE_CODE.sub("", line)
        for match in INLINE_LINK.finditer(code_free):
            yield number, match.group(1).strip("<>")
        definition = REFERENCE_LINK.match(code_free)
        if definition:
            yield number, definition.group(1).strip("<>")


def destination(root: Path, document: Path, target: str) -> str:
    """Classify a relative target as ok, missing or escaping."""
    plain = unquote(target.split("#", 1)[0])
    if not plain:
        return "ok"
    candidate = (document.parent / plain).resolve()
    try:
        candidate.relative_to(root.resolve())
    except ValueError:
        return "escaping"
    return "ok" if candidate.exists() else "missing"


def check(root: Path, documents: list[str], allowed=ALLOWED):
    """Return (links checked, problems, allowed hits) for the given documents."""
    checked = 0
    problems = []
    permitted = []
    for name in documents:
        document = root / name
        if not document.is_file():
            problems.append((name, 0, name, "document is missing"))
            continue
        for number, target in relative_links(document.read_text(encoding="utf-8")):
            if EXTERNAL.match(target) or target.startswith(("/", "#")) or not target:
                continue
            checked += 1
            status = destination(root, document, target)
            if status == "ok":
                continue
            if (name, target) in allowed:
                permitted.append((name, number, target, allowed[(name, target)]))
                continue
            reason = "escapes the repository" if status == "escaping" else "no such file"
            problems.append((name, number, target, reason))
    return checked, problems, permitted


def main() -> int:
    documents = tracked_documents(ROOT)
    checked, problems, permitted = check(ROOT, documents)
    for name, number, target, reason in problems:
        print(f"{name}:{number}: {target} ({reason})")
    summary = (f"{len(problems)} broken relative link{'s' if len(problems) != 1 else ''} "
               f"in {len(documents)} files ({checked} links checked, "
               f"{len(permitted)} allowed)")
    print(("FAIL: " if problems else "PASS: ") + summary)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
