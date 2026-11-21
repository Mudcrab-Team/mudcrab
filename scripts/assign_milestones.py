#!/usr/bin/env python3
"""Give every open issue and pull request a milestone.

``--item N`` handles one issue or pull request, for the event runs. ``--all``
scans every open issue and pull request, for the daily run. A pull request with
no milestone inherits the milestone of the issues it links with a closing
keyword or a ``Part of`` or ``Related to`` line. Inheritance happens only when
every linked issue that has a milestone has the same one. An item that still
has no milestone gets the ``needs milestone`` label and one comment, and the
label is removed when a milestone is set.

Exit codes: 0 means the run finished (an item may still need a maintainer's
milestone); 1 means ``--all`` found open items with no milestone; 2 means the
API or an argument failed. ``--dry-run`` reads the same data and prints the
changes without writing any.

The script talks to GitHub through ``gh api``, so the workflow token or a
maintainer's ``gh`` login is the only credential. It uses only the standard
library.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import Any, Dict, FrozenSet, List, Mapping, Optional, Sequence, Tuple
from urllib.parse import quote

LABEL = "needs milestone"
LABEL_COLOR = "d4c5f9"
LABEL_DESCRIPTION = (
    "Open issue or pull request with no milestone; a maintainer assigns one "
    "within one working day"
)
COMMENT_MARKER = "<!-- milestone-check: guidance -->"
DEFAULT_BRANCH = "main"

# GitHub closes an item when a closing keyword appears anywhere on the line, so
# the parser follows it there. "Part of" and "Related to" count at line start,
# as the issue asks, because GitHub does not treat them as closing keywords.
CLOSING_KEYWORD_RE = re.compile(
    r"\b(?:close[sd]?|fix(?:es|ed)?|resolve[sd]?)\b\s*:?",
    re.IGNORECASE,
)
REFERENCE_LINE_RE = re.compile(
    r"^\s*(?:[-*+]\s+|\d+[.)]\s+)?[*_]*"
    r"(?:part\s+of|related(?:\s+to)?)"
    r"[*_]*\s*:?\s*(?P<rest>.*)$",
    re.IGNORECASE,
)
# A bare reference. The lookbehind keeps "owner/repo#12" and a URL out.
ISSUE_REFERENCE_RE = re.compile(r"(?<![\w/])#(?P<number>[1-9]\d*)\b")
# A full URL to an issue. Only links to this repository pass a milestone.
ISSUE_URL_RE = re.compile(
    r"https?://github\.com/(?P<owner>[\w.-]+)/(?P<repository>[\w.-]+)"
    r"/(?:issues|pull)/(?P<number>[1-9]\d*)\b",
    re.IGNORECASE,
)
REFERENCE_TOKEN = (
    r"(?:https?://github\.com/[\w.-]+/[\w.-]+/(?:issues|pull)/[1-9]\d*"
    r"|(?<![\w/])#[1-9]\d*)"
)
# A closing keyword links only the references that follow it, not any later
# reference on the same line.
REFERENCE_LIST_RE = re.compile(
    r"^\s*"
    + REFERENCE_TOKEN
    + r"(?:\s*(?:,\s*(?:and\s+)?|\band\b\s*)"
    + REFERENCE_TOKEN
    + r")*",
    re.IGNORECASE,
)
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
HTML_COMMENT_RE = re.compile(r"<!--.*?-->", re.DOTALL)


class GitHubError(RuntimeError):
    """A GitHub API call or its response failed."""


class NotFound(GitHubError):
    """The requested object does not exist."""


class GhCli:
    """A small client for ``gh api``; the only credential is ``GH_TOKEN``."""

    def __init__(self, executable: str = "gh") -> None:
        self.executable = executable

    def _run(self, command: Sequence[str], payload: Optional[str] = None) -> str:
        try:
            result = subprocess.run(
                list(command),
                input=payload,
                text=True,
                capture_output=True,
                check=False,
            )
        except OSError as error:
            raise GitHubError(f"cannot run {self.executable}: {error}") from error
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip()
            if "HTTP 404" in detail:
                raise NotFound(detail)
            raise GitHubError(
                detail or f"{' '.join(command)} exited {result.returncode}"
            )
        return result.stdout

    def get(self, path: str, paginate: bool = False) -> Any:
        """Return one JSON object, or the flattened pages of a list endpoint."""
        command = [self.executable, "api"]
        if paginate:
            command.extend(["--paginate", "--slurp"])
        command.append(path)
        text = self._run(command)
        try:
            data = json.loads(text)
        except json.JSONDecodeError as error:
            raise GitHubError(f"gh api {path} returned invalid JSON: {error}") from error
        if not paginate:
            return data
        if not isinstance(data, list):
            raise GitHubError(f"gh api {path} did not return a list of pages")
        items: List[Any] = []
        for page in data:
            if not isinstance(page, list):
                raise GitHubError(f"gh api {path} returned a page that is not a list")
            items.extend(page)
        return items

    def send(self, method: str, path: str, payload: Optional[Any] = None) -> None:
        """Make a write request. The body travels as JSON on standard input."""
        command = [self.executable, "api", "--method", method, path]
        text = None
        if payload is not None:
            command.extend(["--input", "-"])
            text = json.dumps(payload)
        self._run(command, text)


@dataclass(frozen=True)
class Milestone:
    number: int
    title: str


@dataclass(frozen=True)
class Item:
    number: int
    kind: str
    state: str
    body: str
    milestone: Optional[Milestone]
    labels: FrozenSet[str]
    is_pull_request: bool


def _field_string(data: Mapping[str, Any], name: str, where: str) -> str:
    value = data.get(name)
    if not isinstance(value, str):
        raise GitHubError(f"{where} has no {name}")
    return value


def parse_milestone(value: Any, where: str) -> Optional[Milestone]:
    """Read a milestone object, or None when the item has no milestone."""
    if value is None:
        return None
    if not isinstance(value, dict):
        raise GitHubError(f"{where} has an invalid milestone")
    number = value.get("number")
    if isinstance(number, bool) or not isinstance(number, int) or number <= 0:
        raise GitHubError(f"{where} has a milestone without a valid number")
    title = value.get("title")
    if not isinstance(title, str) or not title:
        raise GitHubError(f"{where} has a milestone without a title")
    return Milestone(number=number, title=title)


def parse_item(data: Any) -> Item:
    """Turn one API issue or pull request object into an Item."""
    if not isinstance(data, dict):
        raise GitHubError("the issue response is not a JSON object")
    number = data.get("number")
    if isinstance(number, bool) or not isinstance(number, int) or number <= 0:
        raise GitHubError("an issue response has no valid number")
    where = f"item #{number}"
    state = _field_string(data, "state", where)
    body_value = data.get("body")
    if body_value is None:
        body = ""
    elif isinstance(body_value, str):
        body = body_value
    else:
        raise GitHubError(f"{where} has a body that is not text")
    labels_value = data.get("labels", [])
    if not isinstance(labels_value, list):
        raise GitHubError(f"{where} has a label list that is not a list")
    labels = set()
    for label in labels_value:
        if isinstance(label, str):
            labels.add(label)
        elif isinstance(label, dict) and isinstance(label.get("name"), str):
            labels.add(label["name"])
        else:
            raise GitHubError(f"{where} has a label without a name")
    is_pull_request = "pull_request" in data
    return Item(
        number=number,
        kind="pull request" if is_pull_request else "issue",
        state=state,
        body=body,
        milestone=parse_milestone(data.get("milestone"), where),
        labels=frozenset(labels),
        is_pull_request=is_pull_request,
    )


def _lines_outside_code(body: str) -> List[str]:
    """Drop fenced code and HTML comments, so a sample reference is not a link."""
    text = HTML_COMMENT_RE.sub("", body)
    lines: List[str] = []
    fence: Optional[Tuple[str, int]] = None
    for line in text.splitlines():
        match = FENCE_RE.match(line)
        if match:
            marker = match.group(1)
            character = marker[0]
            length = len(marker)
            if fence is None:
                fence = (character, length)
            elif (
                character == fence[0]
                and length >= fence[1]
                # A closing fence has no info string; a shorter or annotated
                # fence inside the block is content.
                and line[match.end() :].strip() == ""
            ):
                fence = None
            continue
        if fence is None:
            lines.append(line)
    return lines


def _references_in(
    text: str, offset: int, owner: str, name: str
) -> List[Tuple[int, int]]:
    references: List[Tuple[int, int]] = []
    for reference in ISSUE_URL_RE.finditer(text):
        if (
            reference.group("owner").lower() == owner.lower()
            and reference.group("repository").lower() == name.lower()
        ):
            references.append(
                (offset + reference.start(), int(reference.group("number")))
            )
    for reference in ISSUE_REFERENCE_RE.finditer(text):
        references.append((offset + reference.start(), int(reference.group("number"))))
    return references


def linked_item_numbers(body: str, repository: str) -> List[int]:
    """Return the issue numbers that a body links, in the order they appear.

    A closing keyword anywhere on a line counts, as it does on GitHub. A line
    that starts with ``Part of`` or ``Related to`` counts too. Cross-repository
    references are ignored, because a milestone from another repository cannot
    be set here. Fenced code and HTML comments do not count.
    """
    if repository.count("/") != 1:
        raise ValueError(f"invalid repository {repository!r}")
    owner, name = repository.split("/", 1)
    numbers: List[int] = []
    seen = set()
    for line in _lines_outside_code(body):
        references: List[Tuple[int, int]] = []
        match = REFERENCE_LINE_RE.match(line)
        if match is not None:
            listed = REFERENCE_LIST_RE.match(match.group("rest"))
            if listed is not None:
                references.extend(
                    _references_in(
                        listed.group(0), match.start("rest"), owner, name
                    )
                )
        for keyword in CLOSING_KEYWORD_RE.finditer(line):
            listed = REFERENCE_LIST_RE.match(line[keyword.end() :])
            if listed is not None:
                references.extend(
                    _references_in(listed.group(0), keyword.end(), owner, name)
                )
        for _, number in sorted(references):
            if number not in seen:
                seen.add(number)
                numbers.append(number)
    return numbers


class Manager:
    """Plan and apply the milestone changes for one item or the whole tracker."""

    def __init__(
        self,
        client: Any,
        repository: str,
        dry_run: bool = False,
        default_branch: str = DEFAULT_BRANCH,
        out: Optional[Any] = None,
    ) -> None:
        if repository.count("/") != 1:
            raise ValueError(f"invalid repository {repository!r}")
        self.client = client
        self.repository = repository
        self.dry_run = dry_run
        self.default_branch = default_branch
        self.out = out if out is not None else sys.stdout
        self._label_exists: Optional[bool] = None
        self._linked_items: Dict[int, Optional[Item]] = {}

    def _path(self, suffix: str) -> str:
        return f"/repos/{self.repository}{suffix}"

    def _say(self, message: str) -> None:
        prefix = "milestone: would " if self.dry_run else "milestone: "
        print(f"{prefix}{message}", file=self.out)

    def _notice(self, message: str) -> None:
        print(f"milestone: {message}", file=self.out)

    def _apply(
        self, description: str, method: str, path: str, payload: Optional[Any] = None
    ) -> None:
        # Print after the write, so a failed call never reads as a change made.
        if not self.dry_run:
            self.client.send(method, path, payload)
        self._say(description)

    def _read_item(self, number: int) -> Item:
        data = self.client.get(self._path(f"/issues/{number}"))
        item = parse_item(data)
        if item.number != number:
            raise GitHubError(
                f"asked for item #{number}, but the API returned #{item.number}"
            )
        return item

    def _get_item(self, number: int) -> Item:
        try:
            return self._read_item(number)
        except NotFound:
            raise GitHubError(f"item #{number} does not exist") from None

    def _linked_item(self, number: int) -> Optional[Item]:
        if number in self._linked_items:
            return self._linked_items[number]
        try:
            data = self.client.get(self._path(f"/issues/{number}"))
        except NotFound:
            self._notice(f"linked item #{number} does not exist; ignored")
            self._linked_items[number] = None
            return None
        item = parse_item(data)
        if item.number != number:
            raise GitHubError(
                f"asked for linked item #{number}, but the API returned #{item.number}"
            )
        self._linked_items[number] = item
        return item

    def _ensure_label(self) -> None:
        if self._label_exists is None:
            try:
                self.client.get(self._path(f"/labels/{quote(LABEL, safe='')}"))
                self._label_exists = True
            except NotFound:
                self._label_exists = False
        if self._label_exists:
            return
        if not self.dry_run:
            self.client.send(
                "POST",
                self._path("/labels"),
                {
                    "name": LABEL,
                    "color": LABEL_COLOR,
                    "description": LABEL_DESCRIPTION,
                },
            )
        self._say(f"create the {LABEL!r} label")
        self._label_exists = True

    def _add_label(self, item: Item) -> None:
        if LABEL in item.labels:
            return
        self._ensure_label()
        self._apply(
            f"#{item.number}: add the {LABEL!r} label",
            "POST",
            self._path(f"/issues/{item.number}/labels"),
            {"labels": [LABEL]},
        )

    def _remove_label(self, item: Item) -> None:
        if LABEL not in item.labels:
            return
        self._apply(
            f"#{item.number}: remove the {LABEL!r} label",
            "DELETE",
            self._path(f"/issues/{item.number}/labels/{quote(LABEL, safe='')}"),
        )

    def comment_body(self, item: Item) -> str:
        milestones_url = f"https://github.com/{self.repository}/milestones"
        workflow_url = (
            f"https://github.com/{self.repository}/blob/{self.default_branch}"
            "/docs/contributing/workflow.md#milestones"
        )
        if item.is_pull_request:
            link_rule = (
                "If this pull request closes or helps an issue, write "
                "`Closes #N`, `Part of #N` or `Related to #N` in the body. It "
                "inherits that issue's milestone when all linked issues with a "
                "milestone agree."
            )
        else:
            link_rule = (
                "A pull request that links this issue with `Closes #N`, "
                "`Part of #N` or `Related to #N` inherits its milestone."
            )
        return "\n".join(
            [
                COMMENT_MARKER,
                f"This {item.kind} has no milestone. Every Mudcrab issue and pull "
                f"request carries one, and [the milestone list]({milestones_url}) "
                "is the roadmap.",
                "",
                link_rule,
                "",
                "Only maintainers can set a milestone, so you do not need to act: "
                "a maintainer assigns one within one working day. This workflow "
                f"removes the `{LABEL}` label when the milestone is set.",
                "",
                f"The rule and the daily check are in [workflow.md]({workflow_url}).",
            ]
        )

    def _comment_once(self, item: Item) -> None:
        comments = self.client.get(
            self._path(f"/issues/{item.number}/comments?per_page=100"),
            paginate=True,
        )
        if not isinstance(comments, list):
            raise GitHubError(f"comments for #{item.number} are not a list")
        for comment in comments:
            if isinstance(comment, dict) and COMMENT_MARKER in str(comment.get("body")):
                return
        self._apply(
            f"#{item.number}: post the {LABEL!r} comment",
            "POST",
            self._path(f"/issues/{item.number}/comments"),
            {"body": self.comment_body(item)},
        )

    def _milestone_from_links(self, item: Item) -> Optional[Milestone]:
        """Return the inherited milestone, or None, and print the reason."""
        numbers = linked_item_numbers(item.body, self.repository)
        if not numbers:
            self._notice(
                f"#{item.number}: no linked issue, so there is no milestone to inherit"
            )
            return None
        found: Dict[int, Milestone] = {}
        sources: Dict[int, List[int]] = {}
        for number in numbers:
            linked = self._linked_item(number)
            if linked is None:
                continue
            if linked.is_pull_request:
                self._notice(
                    f"#{item.number}: linked item #{number} is a pull request; "
                    "only issues pass a milestone"
                )
                continue
            if linked.milestone is None:
                continue
            found[linked.milestone.number] = linked.milestone
            sources.setdefault(linked.milestone.number, []).append(number)
        if len(found) == 1:
            milestone = next(iter(found.values()))
            source = ", ".join(f"#{number}" for number in sources[milestone.number])
            self._notice(
                f"#{item.number}: linked {source} has milestone {milestone.number} "
                f"({milestone.title})"
            )
            return milestone
        if len(found) > 1:
            titles = "; ".join(
                f"#{', #'.join(str(number) for number in sources[milestone.number])} "
                f"has {milestone.title}"
                for milestone in found.values()
            )
            self._notice(
                f"#{item.number}: linked issues disagree on the milestone "
                f"({titles}); no milestone set"
            )
            return None
        self._notice(
            f"#{item.number}: no linked issue has a milestone, so there is none to inherit"
        )
        return None

    def _process(self, item: Item) -> str:
        if item.state != "open":
            self._notice(f"#{item.number}: {item.kind} is {item.state}; no change")
            return "skipped"
        if item.milestone is not None:
            self._remove_label(item)
            self._notice(
                f"#{item.number}: {item.kind} already has milestone "
                f"{item.milestone.number} ({item.milestone.title})"
            )
            return "has-milestone"
        milestone = None
        if item.is_pull_request:
            milestone = self._milestone_from_links(item)
            if milestone is not None:
                self._apply(
                    f"#{item.number}: set milestone {milestone.number} "
                    f"({milestone.title})",
                    "PATCH",
                    self._path(f"/issues/{item.number}"),
                    {"milestone": milestone.number},
                )
                self._remove_label(item)
                return "inherited"
        self._add_label(item)
        self._comment_once(item)
        return "labelled"

    def process_item(self, number: int) -> str:
        item = self._get_item(number)
        return self._process(item)

    def process_all(self) -> Tuple[int, List[int]]:
        entries = self.client.get(
            self._path("/issues?state=open&per_page=100"), paginate=True
        )
        if not isinstance(entries, list):
            raise GitHubError("the open item list is not a list")
        counts: Dict[str, int] = {}
        remaining: List[int] = []
        for entry in entries:
            item = parse_item(entry)
            if item.milestone is None:
                # The list is a snapshot taken before the loop. A maintainer can
                # assign a milestone while this run is in progress, and a PATCH
                # would overwrite it. Re-read the item before it is written.
                try:
                    item = self._read_item(item.number)
                except NotFound:
                    self._notice(f"#{item.number}: item disappeared; skipped")
                    counts["skipped"] = counts.get("skipped", 0) + 1
                    continue
            outcome = self._process(item)
            counts[outcome] = counts.get(outcome, 0) + 1
            if outcome == "labelled":
                remaining.append(item.number)
        words = {
            "has-milestone": "already had a milestone",
            "inherited": "inherited a milestone",
            "labelled": "need a milestone",
            "skipped": "skipped",
        }
        summary = ", ".join(
            f"{counts[name]} {words[name]}" for name in words if counts.get(name)
        )
        self._notice(f"{len(entries)} open items: {summary or 'none'}")
        return len(entries), remaining


def _number_list(numbers: Sequence[int], limit: int = 20) -> str:
    shown = ", ".join(f"#{number}" for number in numbers[:limit])
    if len(numbers) > limit:
        shown += f", and {len(numbers) - limit} more"
    return shown


def _repository(value: Optional[str]) -> Optional[str]:
    repository = value or os.environ.get("GITHUB_REPOSITORY", "")
    if repository.count("/") != 1:
        return None
    return repository


def main(argv: Optional[Sequence[str]] = None, client: Optional[Any] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--repository",
        default=os.environ.get("GITHUB_REPOSITORY"),
        help="owner/name; defaults to GITHUB_REPOSITORY",
    )
    parser.add_argument(
        "--default-branch",
        default=DEFAULT_BRANCH,
        help="branch that holds docs/contributing/workflow.md (default: main)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the changes without writing any",
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--item", type=int, help="one issue or pull request number")
    mode.add_argument(
        "--all",
        action="store_true",
        help="every open issue and pull request; exits 1 while one has no milestone",
    )
    args = parser.parse_args(argv)

    repository = _repository(args.repository)
    if repository is None:
        print(
            "milestone: error: pass --repository owner/name or set GITHUB_REPOSITORY",
            file=sys.stderr,
        )
        return 2
    if args.item is not None and args.item <= 0:
        print("milestone: error: --item needs a positive number", file=sys.stderr)
        return 2

    try:
        manager = Manager(
            client if client is not None else GhCli(),
            repository,
            dry_run=args.dry_run,
            default_branch=args.default_branch,
        )
        if args.item is not None:
            manager.process_item(args.item)
            # A contributor cannot set a milestone, so this run never fails for
            # one missing; the daily run reports it instead.
            return 0
        total, remaining = manager.process_all()
        if args.dry_run:
            if remaining:
                print(
                    f"milestone: {len(remaining)} of {total} open items would still "
                    f"have no milestone: {_number_list(remaining)}",
                    file=sys.stderr,
                )
            return 0
        if remaining:
            print(
                f"milestone: error: {len(remaining)} of {total} open items still have "
                f"no milestone: {_number_list(remaining)}",
                file=sys.stderr,
            )
            return 1
        return 0
    except GitHubError as error:
        print(f"milestone: error: {error}", file=sys.stderr)
        return 2
    except OSError as error:
        print(f"milestone: error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
