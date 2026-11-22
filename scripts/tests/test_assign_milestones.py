import copy
import importlib.util
import io
import json
import os
import re
import subprocess
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "assign_milestones.py"
SPEC = importlib.util.spec_from_file_location("assign_milestones", SCRIPT)
ASSIGN = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(ASSIGN)

REPOSITORY = "Mudcrab-Team/mudcrab"
G5 = (46, "G5: Packaging and v1.0")
G4 = (10, "G4: v0.1.0 - Phase 1: Asset Pipeline")


def issue(number, milestone=None, labels=(), state="open"):
    return {
        "number": number,
        "state": state,
        "body": "",
        "milestone": None
        if milestone is None
        else {"number": milestone[0], "title": milestone[1]},
        "labels": [{"name": name} for name in labels],
    }


class FakeGitHub:
    """An in-memory GitHub that records every read and write."""

    def __init__(self, repository=REPOSITORY, label_exists=True):
        self.repository = repository
        self.items = {}
        self.comments = {}
        self.label_exists = label_exists
        self.calls = []

    def add(
        self, number, *, body="", milestone=None, labels=(), state="open", pull_request=False
    ):
        item = issue(number, milestone=milestone, labels=labels, state=state)
        item["body"] = body
        if pull_request:
            item["pull_request"] = {"url": "https://example.invalid/pull"}
        self.items[number] = item
        self.comments.setdefault(number, [])
        return self

    def label_names(self, number):
        return {label["name"] for label in self.items[number]["labels"]}

    def milestone_number(self, number):
        milestone = self.items[number]["milestone"]
        return None if milestone is None else milestone["number"]

    @property
    def writes(self):
        return [call for call in self.calls if call[0] != "GET"]

    def _prefix(self):
        return f"/repos/{self.repository}"

    def get(self, path, paginate=False):
        self.calls.append(("GET", path, None))
        prefix = re.escape(self._prefix())
        if path == f"{self._prefix()}/issues?state=open&per_page=100":
            return [
                copy.deepcopy(item)
                for item in self.items.values()
                if item["state"] == "open"
            ]
        match = re.fullmatch(prefix + r"/issues/(\d+)", path)
        if match:
            number = int(match.group(1))
            if number not in self.items:
                raise ASSIGN.NotFound(f"#{number}")
            return copy.deepcopy(self.items[number])
        match = re.fullmatch(prefix + r"/issues/(\d+)/comments\?per_page=100", path)
        if match:
            number = int(match.group(1))
            return [{"body": body} for body in self.comments.get(number, [])]
        if path == f"{self._prefix()}/labels/needs%20milestone":
            if self.label_exists:
                return {"name": ASSIGN.LABEL}
            raise ASSIGN.NotFound(path)
        raise AssertionError(f"unexpected GET {path}")

    def send(self, method, path, payload=None):
        self.calls.append((method, path, payload))
        prefix = re.escape(self._prefix())
        if method == "POST" and path == f"{self._prefix()}/labels":
            self.label_exists = True
            return
        match = re.fullmatch(prefix + r"/issues/(\d+)", path)
        if method == "PATCH" and match:
            number = int(match.group(1))
            number_of_milestone = payload["milestone"]
            titles = {G5[0]: G5[1], G4[0]: G4[1]}
            self.items[number]["milestone"] = {
                "number": number_of_milestone,
                "title": titles[number_of_milestone],
            }
            return
        match = re.fullmatch(prefix + r"/issues/(\d+)/labels", path)
        if method == "POST" and match:
            number = int(match.group(1))
            for name in payload["labels"]:
                if name not in self.label_names(number):
                    self.items[number]["labels"].append({"name": name})
            return
        match = re.fullmatch(
            prefix + r"/issues/(\d+)/labels/needs%20milestone", path
        )
        if method == "DELETE" and match:
            number = int(match.group(1))
            self.items[number]["labels"] = [
                label
                for label in self.items[number]["labels"]
                if label["name"] != ASSIGN.LABEL
            ]
            return
        match = re.fullmatch(prefix + r"/issues/(\d+)/comments", path)
        if method == "POST" and match:
            number = int(match.group(1))
            self.comments[number].append(payload["body"])
            return
        raise AssertionError(f"unexpected {method} {path}")


class ManagerTestCase(unittest.TestCase):
    def setUp(self):
        self.github = FakeGitHub()

    def manager(self, **kwargs):
        return ASSIGN.Manager(
            self.github, self.github.repository, out=io.StringIO(), **kwargs
        )

    def run_main(self, *arguments):
        stdout = io.StringIO()
        stderr = io.StringIO()
        code = None
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = ASSIGN.main(
                ["--repository", self.github.repository, *arguments],
                client=self.github,
            )
        return code, stdout.getvalue(), stderr.getvalue()


class LinkedItemNumberTests(unittest.TestCase):
    def numbers(self, body):
        return ASSIGN.linked_item_numbers(body, REPOSITORY)

    def test_closing_keywords_count_anywhere_on_the_line(self):
        self.assertEqual(
            [12, 13, 14, 15],
            self.numbers(
                "Closes #12\n"
                "Fixes #13, resolves #14\n"
                "This pull request closes #15."
            ),
        )

    def test_part_of_and_related_lines_count(self):
        self.assertEqual(
            [7, 8, 9, 10],
            self.numbers(
                "- Part of #7\n"
                "**Related to** #8\n"
                "Related: #9\n"
                "Related #10\n"
            ),
        )

    def test_same_repository_urls_count(self):
        self.assertEqual(
            [5, 6],
            self.numbers(
                "Closes https://github.com/Mudcrab-Team/mudcrab/issues/5\n"
                "Part of https://github.com/Mudcrab-Team/mudcrab/pull/6\n"
            ),
        )

    def test_other_repositories_are_ignored(self):
        self.assertEqual(
            [7],
            self.numbers(
                "Closes other/repo#5\n"
                "Related to https://github.com/other/repo/issues/6\n"
                "Closes https://github.com/Mudcrab-Team/mudcrab/issues/7\n"
            ),
        )

    def test_fenced_code_and_html_comments_are_ignored(self):
        self.assertEqual(
            [7],
            self.numbers(
                "```\nCloses #5\n```\n"
                "<!-- Closes #6 -->\n"
                "Closes #7\n"
            ),
        )

    def test_a_longer_fence_is_not_closed_by_a_shorter_one(self):
        self.assertEqual(
            [7],
            self.numbers(
                "````markdown\n"
                "```\n"
                "Closes #5\n"
                "```\n"
                "````\n"
                "Closes #7\n"
            ),
        )

    def test_a_closing_keyword_links_only_its_reference_list(self):
        self.assertEqual([5, 6], self.numbers("Closes #5 and #6\n"))
        self.assertEqual([5, 6, 7], self.numbers("Closes #5, #6, and #7\n"))
        self.assertEqual(
            [],
            self.numbers(
                "This fixes a typo in the README; see #10 for background.\n"
            ),
        )
        self.assertEqual([], self.numbers("Related to the work; see #10.\n"))

    def test_bare_references_are_not_links(self):
        self.assertEqual([], self.numbers("See #5 and #6.\n"))
        self.assertEqual([], self.numbers("**Merge after:** #9\n"))

    def test_order_and_duplicates(self):
        self.assertEqual([9, 3], self.numbers("Closes #9, #3 and #9\n"))


class ParsingTests(unittest.TestCase):
    def test_item_without_number_fails_closed(self):
        with self.assertRaisesRegex(ASSIGN.GitHubError, "number"):
            ASSIGN.parse_item({"state": "open"})

    def test_item_without_state_fails_closed(self):
        with self.assertRaisesRegex(ASSIGN.GitHubError, "state"):
            ASSIGN.parse_item({"number": 1})

    def test_milestone_without_number_fails_closed(self):
        with self.assertRaisesRegex(ASSIGN.GitHubError, "milestone"):
            ASSIGN.parse_item(
                {"number": 1, "state": "open", "milestone": {"title": "G5"}}
            )

    def test_body_may_be_null(self):
        item = ASSIGN.parse_item({"number": 1, "state": "open", "body": None})
        self.assertEqual("", item.body)
        self.assertFalse(item.is_pull_request)


class InheritanceTests(ManagerTestCase):
    def test_inherits_from_the_only_linked_issue_with_a_milestone(self):
        self.github.add(10, milestone=G5)
        self.github.add(11)
        self.github.add(20, body="Closes #10\nRelated to #11", pull_request=True)
        self.manager().process_item(20)
        self.assertEqual(G5[0], self.github.milestone_number(20))
        self.assertEqual([("PATCH", f"/repos/{REPOSITORY}/issues/20")], [
            call[:2] for call in self.github.writes
        ])

    def test_inherits_when_the_linked_milestones_agree(self):
        self.github.add(10, milestone=G5)
        self.github.add(11, milestone=G5)
        self.github.add(20, body="Closes #10\nPart of #11", pull_request=True)
        self.manager().process_item(20)
        self.assertEqual(G5[0], self.github.milestone_number(20))
        self.assertEqual(1, len(self.github.writes))

    def test_disagreeing_milestones_do_not_change_the_pull_request(self):
        self.github.add(10, milestone=G5)
        self.github.add(11, milestone=G4)
        self.github.add(20, body="Closes #10\nRelated to #11", pull_request=True)
        code, _, stderr = self.run_main("--item", "20")
        self.assertEqual(0, code)
        self.assertIsNone(self.github.milestone_number(20))
        self.assertIn(ASSIGN.LABEL, self.github.label_names(20))
        self.assertEqual(1, len(self.github.comments[20]))
        self.assertEqual("", stderr)

    def test_a_comma_and_list_checks_every_milestone(self):
        self.github.add(5, milestone=G5)
        self.github.add(6, milestone=G5)
        self.github.add(7, milestone=G4)
        self.github.add(20, body="Closes #5, #6, and #7", pull_request=True)
        self.run_main("--item", "20")
        self.assertIsNone(self.github.milestone_number(20))
        self.assertIn(ASSIGN.LABEL, self.github.label_names(20))

    def test_no_linked_issue_labels_only(self):
        self.github.add(20, body="A pull request with no link.", pull_request=True)
        code, stdout, _ = self.run_main("--item", "20")
        self.assertEqual(0, code)
        self.assertIsNone(self.github.milestone_number(20))
        self.assertIn(ASSIGN.LABEL, self.github.label_names(20))
        self.assertEqual(1, len(self.github.comments[20]))
        self.assertIn("no linked issue", stdout)

    def test_a_linked_pull_request_does_not_pass_a_milestone(self):
        self.github.add(10, milestone=G5, pull_request=True)
        self.github.add(20, body="Closes #10", pull_request=True)
        self.run_main("--item", "20")
        self.assertIsNone(self.github.milestone_number(20))
        self.assertIn(ASSIGN.LABEL, self.github.label_names(20))

    def test_unknown_linked_issue_is_ignored(self):
        self.github.add(20, body="Closes #999", pull_request=True)
        code, stdout, _ = self.run_main("--item", "20")
        self.assertEqual(0, code)
        self.assertIn("does not exist", stdout)
        self.assertIn(ASSIGN.LABEL, self.github.label_names(20))

    def test_an_issue_does_not_inherit(self):
        self.github.add(10, milestone=G5)
        self.github.add(20, body="Closes #10")
        self.run_main("--item", "20")
        self.assertIsNone(self.github.milestone_number(20))
        self.assertIn(ASSIGN.LABEL, self.github.label_names(20))


class LabelAndCommentTests(ManagerTestCase):
    def test_label_is_removed_when_a_milestone_is_set(self):
        self.github.add(30, milestone=G5, labels=(ASSIGN.LABEL,))
        self.manager().process_item(30)
        self.assertNotIn(ASSIGN.LABEL, self.github.label_names(30))
        self.assertEqual(
            [
                (
                    "DELETE",
                    f"/repos/{REPOSITORY}/issues/30/labels/needs%20milestone",
                )
            ],
            [call[:2] for call in self.github.writes],
        )

    def test_an_assigned_item_without_the_label_has_no_writes(self):
        self.github.add(30, milestone=G5)
        self.manager().process_item(30)
        self.assertEqual([], self.github.writes)

    def test_the_comment_is_posted_once(self):
        self.github.comments[30] = [f"{ASSIGN.COMMENT_MARKER}\nold text"]
        self.github.add(30, labels=(ASSIGN.LABEL,))
        self.manager().process_item(30)
        self.assertEqual([], self.github.writes)

    def test_the_missing_label_is_created_with_a_description(self):
        github = FakeGitHub(label_exists=False)
        github.add(30)
        manager = ASSIGN.Manager(github, REPOSITORY, out=io.StringIO())
        manager.process_item(30)
        create = github.writes[0]
        self.assertEqual(("POST", f"/repos/{REPOSITORY}/labels"), create[:2])
        self.assertEqual(ASSIGN.LABEL, create[2]["name"])
        self.assertEqual(ASSIGN.LABEL_COLOR, create[2]["color"])
        self.assertEqual(ASSIGN.LABEL_DESCRIPTION, create[2]["description"])
        self.assertIn(ASSIGN.LABEL, github.label_names(30))

    def test_an_existing_label_is_not_created_again(self):
        self.github.add(30)
        self.manager().process_item(30)
        self.assertFalse(
            any(call[:2] == ("POST", f"/repos/{REPOSITORY}/labels") for call in self.github.writes)
        )

    def test_comment_body_links_the_milestone_list_and_the_rule(self):
        item = ASSIGN.Item(
            number=30,
            kind="issue",
            state="open",
            body="",
            milestone=None,
            labels=frozenset(),
            is_pull_request=False,
        )
        body = self.manager(default_branch="develop").comment_body(item)
        self.assertIn(f"https://github.com/{REPOSITORY}/milestones", body)
        self.assertIn(
            f"https://github.com/{REPOSITORY}/blob/develop/docs/contributing/"
            "workflow.md#milestones",
            body,
        )
        self.assertIn(ASSIGN.LABEL, body)

    def test_a_closed_item_is_skipped(self):
        self.github.add(30, state="closed")
        code, stdout, _ = self.run_main("--item", "30")
        self.assertEqual(0, code)
        self.assertEqual([], self.github.writes)
        self.assertIn("closed", stdout)


class ModeTests(ManagerTestCase):
    def test_an_item_run_exits_zero_without_a_milestone(self):
        self.github.add(30)
        code, _, _ = self.run_main("--item", "30")
        self.assertEqual(0, code)
        self.assertIn(ASSIGN.LABEL, self.github.label_names(30))
        self.assertEqual(1, len(self.github.comments[30]))

    def test_a_pull_request_run_exits_zero_without_a_milestone(self):
        self.github.add(30, pull_request=True)
        code, _, _ = self.run_main("--item", "30")
        self.assertEqual(0, code)
        self.assertIn(ASSIGN.LABEL, self.github.label_names(30))

    def test_every_open_item_is_checked(self):
        self.github.add(1, milestone=G5)
        self.github.add(2, labels=(ASSIGN.LABEL,))
        code, _, stderr = self.run_main("--all")
        self.assertEqual(1, code)
        self.assertIn("#2", stderr)
        self.assertIn("still have no milestone", stderr)
        self.assertIn(ASSIGN.LABEL, self.github.label_names(2))

    def test_all_assigned_items_exit_zero(self):
        self.github.add(1, milestone=G5)
        self.github.add(2, milestone=G4, pull_request=True)
        code, stdout, _ = self.run_main("--all")
        self.assertEqual(0, code)
        self.assertEqual([], self.github.writes)
        self.assertIn("2 open items", stdout)

    def test_the_daily_run_also_inherits(self):
        self.github.add(1, milestone=G5)
        self.github.add(2, body="Closes #1", pull_request=True)
        code, _, _ = self.run_main("--all")
        self.assertEqual(0, code)
        self.assertEqual(G5[0], self.github.milestone_number(2))

    def test_dry_run_writes_nothing(self):
        self.github.add(30, labels=())
        code, stdout, _ = self.run_main("--item", "30", "--dry-run")
        self.assertEqual(0, code)
        self.assertEqual([], self.github.writes)
        self.assertIn("would #30: add", stdout)
        self.assertIn("would #30: post", stdout)

    def test_dry_run_all_reports_and_exits_zero(self):
        self.github.add(30)
        code, _, stderr = self.run_main("--all", "--dry-run")
        self.assertEqual(0, code)
        self.assertEqual([], self.github.writes)
        self.assertIn("#30", stderr)

    def test_the_daily_run_re_reads_an_unassigned_item(self):
        class StaleSnapshot(FakeGitHub):
            def get(inner_self, path, paginate=False):
                if path.endswith("/issues?state=open&per_page=100"):
                    snapshot = copy.deepcopy(inner_self.items[2])
                    inner_self.items[2]["milestone"] = {
                        "number": G5[0],
                        "title": G5[1],
                    }
                    return [snapshot]
                return FakeGitHub.get(inner_self, path, paginate=paginate)

        github = StaleSnapshot()
        github.add(2)
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            code = ASSIGN.main(
                ["--repository", REPOSITORY, "--all"], client=github
            )
        self.assertEqual(0, code)
        self.assertEqual([], github.writes)
        self.assertEqual(G5[0], github.milestone_number(2))

    def test_a_disappeared_item_is_skipped_in_the_daily_run(self):
        class Disappearing(FakeGitHub):
            def get(inner_self, path, paginate=False):
                if path.endswith("/issues?state=open&per_page=100"):
                    snapshot = [issue(2)]
                    del inner_self.items[2]
                    return snapshot
                return FakeGitHub.get(inner_self, path, paginate=paginate)

        github = Disappearing()
        github.add(2)
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            code = ASSIGN.main(
                ["--repository", REPOSITORY, "--all"], client=github
            )
        self.assertEqual(0, code)
        self.assertEqual([], github.writes)
        self.assertIn("disappeared", stdout.getvalue())

    def test_dry_run_inheritance_writes_nothing(self):
        self.github.add(1, milestone=G5)
        self.github.add(2, body="Closes #1", pull_request=True)
        code, stdout, _ = self.run_main("--item", "2", "--dry-run")
        self.assertEqual(0, code)
        self.assertEqual([], self.github.writes)
        self.assertIn("would #2: set milestone", stdout)

    def test_api_failure_exits_two(self):
        github = FakeGitHub()

        def broken_get(path, paginate=False):
            raise ASSIGN.GitHubError("the API is unavailable")

        github.get = broken_get
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            code = ASSIGN.main(
                ["--repository", REPOSITORY, "--item", "1"], client=github
            )
        self.assertEqual(2, code)
        self.assertIn("milestone: error: the API is unavailable", stderr.getvalue())

    def test_missing_repository_exits_two(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                code = ASSIGN.main(["--item", "1"], client=FakeGitHub())
        self.assertEqual(2, code)
        self.assertIn("--repository", stderr.getvalue())


class GhCliTests(unittest.TestCase):
    def run_with(self, result, function):
        with mock.patch.object(ASSIGN.subprocess, "run", return_value=result) as run:
            function()
        return run

    def test_404_is_reported_as_not_found(self):
        result = subprocess.CompletedProcess(
            [], 1, stdout="", stderr="gh: Not Found (HTTP 404)"
        )
        with self.assertRaises(ASSIGN.NotFound):
            self.run_with(result, lambda: ASSIGN.GhCli().get("/repos/o/r/labels/x"))

    def test_other_failures_are_errors(self):
        result = subprocess.CompletedProcess(
            [], 1, stdout="", stderr="gh: Server Error (HTTP 502)"
        )
        with self.assertRaises(ASSIGN.GitHubError):
            self.run_with(result, lambda: ASSIGN.GhCli().get("/repos/o/r"))

    def test_malformed_json_is_an_error(self):
        result = subprocess.CompletedProcess([], 0, stdout="{not json", stderr="")
        with self.assertRaisesRegex(ASSIGN.GitHubError, "invalid JSON"):
            self.run_with(result, lambda: ASSIGN.GhCli().get("/repos/o/r"))

    def test_paginated_pages_are_flattened(self):
        result = subprocess.CompletedProcess(
            [], 0, stdout=json.dumps([[{"number": 1}], [{"number": 2}]]), stderr=""
        )
        pages = []
        self.run_with(
            result,
            lambda: pages.append(
                ASSIGN.GhCli().get("/repos/o/r/issues?state=open", paginate=True)
            ),
        )
        self.assertEqual([{"number": 1}, {"number": 2}], pages[0])

    def test_write_sends_json_on_standard_input_without_a_shell(self):
        result = subprocess.CompletedProcess([], 0, stdout="", stderr="")
        run = self.run_with(
            result,
            lambda: ASSIGN.GhCli().send(
                "POST", "/repos/o/r/issues/1/labels", {"labels": [ASSIGN.LABEL]}
            ),
        )
        command, keywords = run.call_args
        self.assertIsInstance(command[0], list)
        self.assertNotIn("shell", keywords)
        self.assertIn("--input", command[0])
        self.assertEqual(
            {"labels": [ASSIGN.LABEL]}, json.loads(keywords["input"])
        )


if __name__ == "__main__":
    unittest.main()
