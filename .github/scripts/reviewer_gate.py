#!/usr/bin/env python3
"""Wait for the AI reviewers on a pull request head commit, failing closed.

The `reviewers_pass` job in `.github/workflows/run_tests.yml` runs this script.
It exists because a merge queue cannot require the `CodeRabbit` check itself:
CodeRabbit reviews a PR head commit and never reports on the queue's temporary
commit. The ruleset therefore requires only `tests_pass`, and `tests_pass`
depends on this gate.

Trust model. CodeRabbit reports as a GitHub check run. Only a GitHub App can
create a check run, so the app that owns a check run cannot be forged by a
person with Write access or by a workflow. A commit status is different: any
person with Write access, and any workflow with `statuses: write`, can post a
status under any name. This script therefore never reads commit statuses. It
accepts a check run only when it is named `CodeRabbit` and its `app.id` is the
CodeRabbit app id that the `main_protection` ruleset also pins. The slug and
the name alone are not trusted. The newest trusted check run decides:

* no trusted check run yet, or one that is not completed: wait
* `success`: pass
* `neutral` or `skipped` (CodeRabbit reports "Review rate limited" this way,
  and did not review): wait, unless `--accept-neutral` is set
* anything else, such as `failure`, `cancelled` or an unknown value: fail

An unreadable or malformed API answer never passes. The script waits for the
next poll, and the timeout fails the job. Greptile is logged and never blocks.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from typing import Any, Callable, Dict, List, NamedTuple, Optional

# The CodeRabbit GitHub App. This is the integration_id that the main_protection
# ruleset pins for the `CodeRabbit` required check.
CODERABBIT_APP_ID = 347564
CODERABBIT_CHECK_NAME = "CodeRabbit"
GREPTILE_APP_SLUG = "greptile-apps"
GREPTILE_CHECK_NAME = "Greptile Review"

PER_PAGE = 100
MAX_PAGES = 20

PASS = "pass"
PENDING = "pending"
FAIL = "fail"

RECOVERY = (
    "Recovery: comment `@coderabbitai review` on the pull request, wait for "
    "the CodeRabbit check to finish, then re-run this job."
)


class GateError(Exception):
    """The reviewer data could not be read or trusted. Never a pass."""


class Verdict(NamedTuple):
    outcome: str
    message: str


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def trusted_runs(
    check_runs: List[Any], name: str, app_id: Optional[int], app_slug: Optional[str]
) -> List[Dict[str, Any]]:
    """Return the check runs with this name that the given app owns."""
    found = []
    for run in check_runs:
        if not isinstance(run, dict) or run.get("name") != name:
            continue
        if not _is_int(run.get("id")):
            continue
        app = run.get("app")
        if not isinstance(app, dict):
            continue
        if app_id is not None and not (_is_int(app.get("id")) and app["id"] == app_id):
            continue
        if app_slug is not None and app.get("slug") != app_slug:
            continue
        found.append(run)
    return found


def newest(runs: List[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
    # Check run ids grow with creation time, so the highest id is the newest.
    return max(runs, key=lambda run: run["id"]) if runs else None


def evaluate_coderabbit(check_runs: List[Any], accept_neutral: bool = False) -> Verdict:
    run = newest(trusted_runs(check_runs, CODERABBIT_CHECK_NAME, CODERABBIT_APP_ID, None))
    if run is None:
        return Verdict(
            PENDING,
            f"no {CODERABBIT_CHECK_NAME} check run from app {CODERABBIT_APP_ID} yet",
        )
    label = f"{CODERABBIT_CHECK_NAME} check run {run['id']}"
    if run.get("status") != "completed":
        return Verdict(PENDING, f"{label} is {run.get('status')!r}, not completed")
    conclusion = run.get("conclusion")
    if conclusion == "success":
        return Verdict(PASS, f"{label} succeeded")
    if conclusion in ("neutral", "skipped"):
        output = run.get("output")
        title = output.get("title") if isinstance(output, dict) else None
        if accept_neutral:
            return Verdict(PASS, f"{label} is {conclusion} ({title!r}), accepted by configuration")
        return Verdict(
            PENDING,
            f"{label} is {conclusion} ({title!r}), so CodeRabbit did not review this commit. {RECOVERY}",
        )
    return Verdict(FAIL, f"{label} finished {conclusion!r}")


def describe_greptile(check_runs: List[Any]) -> str:
    run = newest(trusted_runs(check_runs, GREPTILE_CHECK_NAME, None, GREPTILE_APP_SLUG))
    if run is None:
        return "missing"
    if run.get("status") != "completed":
        return "pending"
    return str(run.get("conclusion"))


def collect_check_runs(
    fetch_page: Callable[[int], Any], per_page: int = PER_PAGE, max_pages: int = MAX_PAGES
) -> List[Any]:
    """Read every page of the check runs for a commit.

    Stops only at the first short page, and never trusts total_count to stop
    early, so a server that under-reports it cannot hide a later page. It
    raises GateError when the answer is malformed or has too many pages, so a
    CodeRabbit run beyond the last page we read can never be silently missed.
    """
    runs: List[Any] = []
    for page in range(1, max_pages + 1):
        payload = fetch_page(page)
        if not isinstance(payload, dict) or not isinstance(payload.get("check_runs"), list):
            raise GateError("check runs answer has no check_runs list")
        batch = payload["check_runs"]
        runs.extend(batch)
        if len(batch) < per_page:
            return runs
    raise GateError(f"more than {max_pages} pages of check runs")


def gh_fetch_page(repo: str, sha: str) -> Callable[[int], Any]:
    def fetch(page: int) -> Any:
        endpoint = f"repos/{repo}/commits/{sha}/check-runs?per_page={PER_PAGE}&page={page}"
        try:
            proc = subprocess.run(
                ["gh", "api", "-H", "Accept: application/vnd.github+json", endpoint],
                capture_output=True,
                text=True,
                timeout=120,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise GateError(f"gh api failed: {error}") from error
        if proc.returncode != 0:
            raise GateError(f"gh api exited {proc.returncode}: {proc.stderr.strip()[:300]}")
        try:
            return json.loads(proc.stdout)
        except ValueError as error:
            raise GateError(f"gh api returned invalid JSON: {error}") from error

    return fetch


def wait_for_reviewers(
    fetch_runs: Callable[[], List[Any]],
    *,
    timeout_seconds: float,
    interval_seconds: float,
    accept_neutral: bool = False,
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
    log: Callable[[str], None] = print,
) -> int:
    deadline = clock() + timeout_seconds
    last = "no answer yet"
    while True:
        try:
            runs = fetch_runs()
        except GateError as error:
            verdict = Verdict(PENDING, f"could not read check runs: {error}")
        else:
            verdict = evaluate_coderabbit(runs, accept_neutral)
        last = verdict.message
        log(f"{CODERABBIT_CHECK_NAME}: {verdict.outcome}: {verdict.message}")
        if verdict.outcome == PASS:
            log(f"{GREPTILE_CHECK_NAME} (advisory, not blocking): {describe_greptile(runs)}")
            return 0
        if verdict.outcome == FAIL:
            log(f"::error::{verdict.message}")
            return 1
        if clock() >= deadline:
            log(f"::error::Timed out waiting for {CODERABBIT_CHECK_NAME}: {last}")
            return 1
        sleep(interval_seconds)


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", required=True, help="owner/name")
    parser.add_argument("--sha", required=True, help="40-character head commit SHA")
    parser.add_argument("--timeout-seconds", type=float, default=25 * 60)
    parser.add_argument("--interval-seconds", type=float, default=30)
    parser.add_argument(
        "--accept-neutral",
        action="store_true",
        help="count a neutral or skipped CodeRabbit check run (for example 'Review rate limited') as a pass",
    )
    args = parser.parse_args(argv)

    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        parser.error("--repo must look like owner/name")
    if not re.fullmatch(r"[0-9a-f]{40}", args.sha):
        parser.error("--sha must be a 40-character lowercase hex commit SHA")

    fetch_page = gh_fetch_page(args.repo, args.sha)
    return wait_for_reviewers(
        lambda: collect_check_runs(fetch_page),
        timeout_seconds=args.timeout_seconds,
        interval_seconds=args.interval_seconds,
        accept_neutral=args.accept_neutral,
    )


if __name__ == "__main__":
    sys.exit(main())
