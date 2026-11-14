"""Fixture tests for scripts/reviewer_gate.py, the `reviewers_pass` decision.

Every payload here is fake. The tests aim at the ways a gate like this can be
fooled: a status or check run posted by someone else under the CodeRabbit name,
a pending or missing run, an old success that a newer failure replaced, and a
CodeRabbit run that sits beyond the first page of check runs.
"""

import importlib.util
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "reviewer_gate.py"
SPEC = importlib.util.spec_from_file_location("reviewer_gate", SCRIPT)
GATE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(GATE)

CODERABBIT_APP = {"id": GATE.CODERABBIT_APP_ID, "slug": "coderabbitai"}
ACTIONS_APP = {"id": 15368, "slug": "github-actions"}
GREPTILE_APP = {"id": 867647, "slug": "greptile-apps"}
HEAD = "a" * 40


def run(run_id, *, name="CodeRabbit", app=CODERABBIT_APP, status="completed",
        conclusion="success", title="Finished"):
    return {
        "id": run_id,
        "name": name,
        "app": dict(app) if app is not None else None,
        "status": status,
        "conclusion": conclusion,
        "output": {"title": title},
    }


def decoys(count, first_id=1):
    """Check runs that belong to the Rust jobs, as a real head commit has many."""
    return [run(first_id + i, name=f"job-{i}", app=ACTIONS_APP) for i in range(count)]


def pages(runs, per_page=GATE.PER_PAGE):
    """Split runs into the paged answers the check runs endpoint gives."""
    chunks = [runs[i:i + per_page] for i in range(0, len(runs), per_page)] or [[]]
    return [{"total_count": len(runs), "check_runs": chunk} for chunk in chunks]


class Clock:
    """A fake clock, so a timeout takes no real time."""

    def __init__(self):
        self.now = 0.0
        self.sleeps = 0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds
        self.sleeps += 1


def gate(fetch_runs, *, accept_neutral=False, timeout=120, interval=30):
    clock = Clock()
    lines = []
    code = GATE.wait_for_reviewers(
        fetch_runs,
        timeout_seconds=timeout,
        interval_seconds=interval,
        accept_neutral=accept_neutral,
        clock=clock,
        sleep=clock.sleep,
        log=lines.append,
    )
    return code, lines, clock


class EvaluateCoderabbitTests(unittest.TestCase):
    def outcome(self, runs, **kwargs):
        return GATE.evaluate_coderabbit(runs, **kwargs).outcome

    def test_trusted_success_passes(self):
        self.assertEqual(self.outcome([run(10)]), GATE.PASS)

    def test_spoofed_publisher_never_passes(self):
        # A workflow, or a person, cannot own a check run as the CodeRabbit app.
        # GitHub Actions can create a check run under any name it likes.
        self.assertEqual(self.outcome([run(10, app=ACTIONS_APP)]), GATE.PENDING)

    def test_spoof_with_the_right_slug_but_another_app_id_is_rejected(self):
        fake = dict(CODERABBIT_APP, id=999)
        self.assertEqual(self.outcome([run(10, app=fake)]), GATE.PENDING)

    def test_spoof_with_a_string_or_boolean_app_id_is_rejected(self):
        self.assertEqual(
            self.outcome([run(10, app={"id": str(GATE.CODERABBIT_APP_ID), "slug": "coderabbitai"})]),
            GATE.PENDING,
        )
        self.assertEqual(self.outcome([run(10, app={"id": True, "slug": "coderabbitai"})]), GATE.PENDING)

    def test_missing_or_null_app_is_rejected(self):
        self.assertEqual(self.outcome([run(10, app=None)]), GATE.PENDING)
        no_app = run(10)
        del no_app["app"]
        self.assertEqual(self.outcome([no_app]), GATE.PENDING)

    def test_a_status_shaped_entry_is_ignored(self):
        # Commit statuses have "context" and "state", never "name" and "app".
        # The gate does not read them, so a faked status cannot count.
        fake_status = {"id": 5, "context": "CodeRabbit", "state": "success",
                       "creator": {"login": "coderabbitai[bot]"}}
        self.assertEqual(self.outcome([fake_status]), GATE.PENDING)

    def test_a_wrong_name_from_the_right_app_is_ignored(self):
        self.assertEqual(self.outcome([run(10, name="CodeRabbit Pro")]), GATE.PENDING)

    def test_missing_check_run_is_pending(self):
        self.assertEqual(self.outcome([]), GATE.PENDING)
        self.assertEqual(self.outcome(decoys(5)), GATE.PENDING)

    def test_unfinished_check_run_is_pending_even_with_a_success_conclusion(self):
        for status in ("queued", "in_progress", "waiting", "pending", None):
            with self.subTest(status=status):
                self.assertEqual(
                    self.outcome([run(10, status=status, conclusion=None)]), GATE.PENDING
                )
                self.assertEqual(
                    self.outcome([run(10, status=status, conclusion="success")]), GATE.PENDING
                )

    def test_newer_failure_supersedes_an_older_success(self):
        for failing in ("failure", "cancelled", "timed_out", "action_required", "stale"):
            with self.subTest(conclusion=failing):
                runs = [run(10, conclusion="success"), run(20, conclusion=failing)]
                self.assertEqual(self.outcome(runs), GATE.FAIL)
                self.assertEqual(self.outcome(list(reversed(runs))), GATE.FAIL)

    def test_newer_unfinished_run_supersedes_an_older_success(self):
        runs = [run(10, conclusion="success"), run(20, status="in_progress", conclusion=None)]
        self.assertEqual(self.outcome(runs), GATE.PENDING)

    def test_newer_success_supersedes_an_older_failure(self):
        runs = [run(10, conclusion="failure"), run(20, conclusion="success")]
        self.assertEqual(self.outcome(runs), GATE.PASS)

    def test_a_spoofed_newer_failure_does_not_hide_a_real_success(self):
        # Foreign runs are dropped before "newest" is chosen.
        runs = [run(10, conclusion="success"), run(99, conclusion="failure", app=ACTIONS_APP)]
        self.assertEqual(self.outcome(runs), GATE.PASS)

    def test_unknown_or_missing_conclusion_fails(self):
        self.assertEqual(self.outcome([run(10, conclusion="something_new")]), GATE.FAIL)
        self.assertEqual(self.outcome([run(10, conclusion=None)]), GATE.FAIL)

    def test_neutral_rate_limited_run_waits_with_recovery_text(self):
        verdict = GATE.evaluate_coderabbit([run(10, conclusion="neutral", title="Review rate limited")])
        self.assertEqual(verdict.outcome, GATE.PENDING)
        self.assertIn("Review rate limited", verdict.message)
        self.assertIn("@coderabbitai review", verdict.message)

    def test_neutral_and_skipped_pass_only_when_accepted(self):
        for conclusion in ("neutral", "skipped"):
            with self.subTest(conclusion=conclusion):
                runs = [run(10, conclusion=conclusion)]
                self.assertEqual(self.outcome(runs), GATE.PENDING)
                self.assertEqual(self.outcome(runs, accept_neutral=True), GATE.PASS)

    def test_accepting_neutral_does_not_accept_failure(self):
        self.assertEqual(self.outcome([run(10, conclusion="failure")], accept_neutral=True), GATE.FAIL)

    def test_malformed_entries_are_skipped_not_trusted(self):
        junk = [None, 7, "CodeRabbit", [], {"name": "CodeRabbit"}, run("10"), run(True)]
        self.assertEqual(self.outcome(junk), GATE.PENDING)
        self.assertEqual(self.outcome(junk + [run(11)]), GATE.PASS)


class CollectCheckRunsTests(unittest.TestCase):
    def fetcher(self, answers):
        requested = []

        def fetch(page):
            requested.append(page)
            return answers[page - 1]

        return fetch, requested

    def test_reads_every_page_beyond_the_first_thirty_entries(self):
        runs = decoys(230) + [run(1000)]
        fetch, requested = self.fetcher(pages(runs))
        collected = GATE.collect_check_runs(fetch)
        self.assertEqual(len(collected), 231)
        self.assertEqual(requested, [1, 2, 3])
        self.assertEqual(GATE.evaluate_coderabbit(collected).outcome, GATE.PASS)

    def test_a_failure_on_a_late_page_beats_a_success_on_page_one(self):
        runs = [run(5, conclusion="success")] + decoys(150, first_id=10) + [run(500, conclusion="failure")]
        fetch, _ = self.fetcher(pages(runs))
        self.assertEqual(GATE.evaluate_coderabbit(GATE.collect_check_runs(fetch)).outcome, GATE.FAIL)

    def test_a_full_last_page_costs_one_more_request_and_total_count_is_not_trusted(self):
        # A server that under-reports total_count must not hide a later page.
        answers = pages(decoys(GATE.PER_PAGE) + [run(500)])
        answers[0]["total_count"] = 1
        fetch, requested = self.fetcher(answers)
        collected = GATE.collect_check_runs(fetch)
        self.assertEqual(requested, [1, 2])
        self.assertEqual(GATE.evaluate_coderabbit(collected).outcome, GATE.PASS)

    def test_exactly_one_full_page_asks_for_a_second_empty_page(self):
        answers = pages(decoys(GATE.PER_PAGE)) + [{"total_count": GATE.PER_PAGE, "check_runs": []}]
        fetch, requested = self.fetcher(answers)
        GATE.collect_check_runs(fetch)
        self.assertEqual(requested, [1, 2])

    def test_too_many_pages_is_an_error_not_a_pass(self):
        def endless(page):
            return {"total_count": 10**6, "check_runs": decoys(GATE.PER_PAGE)}

        with self.assertRaises(GATE.GateError):
            GATE.collect_check_runs(endless, max_pages=3)

    def test_malformed_answers_raise(self):
        for payload in (None, [], "x", {}, {"check_runs": None}, {"check_runs": {}}):
            with self.subTest(payload=payload):
                with self.assertRaises(GATE.GateError):
                    GATE.collect_check_runs(lambda page, p=payload: p)


class WaitForReviewersTests(unittest.TestCase):
    def test_pass_exits_zero_and_logs_greptile(self):
        code, lines, _ = gate(lambda: [run(10), run(11, name="Greptile Review", app=GREPTILE_APP,
                                                   conclusion="failure")])
        self.assertEqual(code, 0)
        self.assertTrue(any("Greptile Review (advisory, not blocking): failure" in l for l in lines))

    def test_spoofed_publisher_times_out_and_fails(self):
        code, lines, clock = gate(lambda: [run(10, app=ACTIONS_APP)])
        self.assertEqual(code, 1)
        self.assertGreater(clock.sleeps, 1)
        self.assertTrue(any(l.startswith("::error::Timed out") for l in lines))

    def test_missing_status_times_out_and_fails(self):
        code, _, _ = gate(lambda: decoys(40))
        self.assertEqual(code, 1)

    def test_pending_then_success_passes(self):
        answers = iter([[run(10, status="in_progress", conclusion=None)], [run(10)]])
        code, _, clock = gate(lambda: next(answers))
        self.assertEqual(code, 0)
        self.assertEqual(clock.sleeps, 1)

    def test_pending_forever_fails_at_the_deadline(self):
        code, _, _ = gate(lambda: [run(10, status="queued", conclusion=None)])
        self.assertEqual(code, 1)

    def test_failure_fails_at_once_without_waiting(self):
        code, lines, clock = gate(lambda: [run(10, conclusion="failure")])
        self.assertEqual(code, 1)
        self.assertEqual(clock.sleeps, 0)
        self.assertTrue(any(l.startswith("::error::") for l in lines))

    def test_older_success_superseded_by_newer_failure_fails(self):
        code, _, _ = gate(lambda: [run(10), run(20, conclusion="failure")])
        self.assertEqual(code, 1)

    def test_api_errors_never_pass(self):
        def broken():
            raise GATE.GateError("boom")

        code, lines, _ = gate(broken)
        self.assertEqual(code, 1)
        self.assertTrue(any("could not read check runs: boom" in l for l in lines))

    def test_api_error_then_success_recovers(self):
        calls = iter([GATE.GateError("rate limit"), [run(10)]])

        def flaky():
            value = next(calls)
            if isinstance(value, Exception):
                raise value
            return value

        code, _, _ = gate(flaky)
        self.assertEqual(code, 0)

    def test_rate_limited_run_times_out_unless_neutral_is_accepted(self):
        neutral = [run(10, conclusion="neutral", title="Review rate limited")]
        code, lines, _ = gate(lambda: neutral)
        self.assertEqual(code, 1)
        self.assertTrue(any("@coderabbitai review" in l for l in lines))
        code, _, _ = gate(lambda: neutral, accept_neutral=True)
        self.assertEqual(code, 0)


class CommandLineTests(unittest.TestCase):
    """Run the real script against a fake `gh`, so the request plumbing is covered."""

    def run_cli(self, answers, extra=()):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            for index, answer in enumerate(answers, start=1):
                (tmp / f"page{index}.json").write_text(json.dumps(answer))
            fake = tmp / "gh"
            fake.write_text(
                f"#!{sys.executable}\n"
                "import os, sys\n"
                "args = sys.argv[1:]\n"
                "open(os.path.join(os.environ['FAKE_GH_DIR'], 'calls.txt'), 'a').write(' '.join(args) + '\\n')\n"
                "endpoint = args[-1]\n"
                "page = endpoint.rsplit('page=', 1)[1]\n"
                "path = os.path.join(os.environ['FAKE_GH_DIR'], 'page' + page + '.json')\n"
                "if not os.path.exists(path):\n"
                "    sys.stderr.write('HTTP 404')\n"
                "    sys.exit(1)\n"
                "sys.stdout.write(open(path).read())\n"
            )
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
            env = dict(os.environ, PATH=f"{tmp}{os.pathsep}{os.environ['PATH']}", FAKE_GH_DIR=str(tmp))
            proc = subprocess.run(
                [sys.executable, "-B", str(SCRIPT), "--repo", "Mudcrab-Team/mudcrab", "--sha", HEAD,
                 "--timeout-seconds", "0", "--interval-seconds", "0", *extra],
                capture_output=True, text=True, env=env, timeout=60,
            )
            calls = (tmp / "calls.txt").read_text().splitlines() if (tmp / "calls.txt").exists() else []
            return proc, calls

    def test_cli_passes_on_a_trusted_run_found_on_page_three(self):
        proc, calls = self.run_cli(pages(decoys(230) + [run(1000)]))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(len(calls), 3)
        self.assertIn(f"repos/Mudcrab-Team/mudcrab/commits/{HEAD}/check-runs?per_page=100&page=3", calls[-1])

    def test_cli_fails_on_a_spoofed_run(self):
        proc, _ = self.run_cli(pages(decoys(5) + [run(1000, app=ACTIONS_APP)]))
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertNotIn("CodeRabbit: pass", proc.stdout)

    def test_cli_fails_when_gh_fails(self):
        proc, _ = self.run_cli([])
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("could not read check runs", proc.stdout)

    def test_cli_fails_on_invalid_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp) / "gh"
            fake.write_text("#!/bin/sh\necho 'not json'\n")
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
            env = dict(os.environ, PATH=f"{tmp}{os.pathsep}{os.environ['PATH']}")
            proc = subprocess.run(
                [sys.executable, "-B", str(SCRIPT), "--repo", "Mudcrab-Team/mudcrab", "--sha", HEAD,
                 "--timeout-seconds", "0", "--interval-seconds", "0"],
                capture_output=True, text=True, env=env, timeout=60,
            )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("invalid JSON", proc.stdout)

    def test_cli_rejects_a_bad_sha_or_repo_before_any_request(self):
        for argv in (["--repo", "x", "--sha", HEAD], ["--repo", "a/b", "--sha", "main"],
                     ["--repo", "a/b", "--sha", HEAD + "&page=1"]):
            with self.subTest(argv=argv):
                proc = subprocess.run([sys.executable, "-B", str(SCRIPT), *argv],
                                      capture_output=True, text=True, timeout=60)
                self.assertEqual(proc.returncode, 2)


if __name__ == "__main__":
    unittest.main()
