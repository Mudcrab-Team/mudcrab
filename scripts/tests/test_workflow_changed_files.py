"""Run the workflow filter against captured PR bases and manual dispatches."""

import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/run_tests.yml"


def workflow_filter_shell():
    workflow = WORKFLOW.read_text()
    step = workflow.split("name: Filter changed files", 1)[1]
    shell = textwrap.dedent(step.split("run: |\n", 1)[1].split("\n    outputs:", 1)[0])
    return workflow, step, shell


class WorkflowChangedFilesTests(unittest.TestCase):
    def test_captured_base_keeps_pr_changes_when_moving_base_contains_them(self):
        # Use the actual workflow shell, so the fixture covers its comparison policy.
        workflow, step, shell = workflow_filter_shell()
        self.assertIn("PR_BASE_SHA: ${{ github.event.pull_request.base.sha }}", step)
        self.assertIn("fetch-depth: 0", workflow.split("- id: filter", 1)[0])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*arguments):
                return subprocess.run(["git", "-C", str(root), *arguments], capture_output=True,
                                      text=True, check=True).stdout.strip()

            def commit(message):
                git("add", ".")
                git("commit", "-q", "-m", message)
                return git("rev-parse", "HEAD")

            git("init", "-q", "--initial-branch=main")
            git("config", "user.name", "Workflow fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "base.txt").write_text("initial\n")
            base = commit("captured base")
            git("checkout", "-q", "-b", "feature")
            (root / "scripts").mkdir()
            (root / "scripts/check.py").write_text("feature\n")
            commit("script feature")
            git("checkout", "-q", "main")
            git("merge", "-q", "--no-ff", "feature", "-m", "captured PR merge")
            merge = git("rev-parse", "HEAD")
            # The moving base absorbs the feature after this event was captured.
            # Comparing to its new merge base would silently drop the PR's script.
            git("checkout", "-q", "-b", "advanced-base", "feature")
            source = root / "crates/shared/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("later base edit\n")
            advanced = commit("base advances")
            git("update-ref", "refs/remotes/origin/main", advanced)
            git("checkout", "-q", "--detach", merge)
            moving_base = git("merge-base", "origin/main", merge)
            self.assertEqual(git("diff", "--name-only", moving_base, merge), "")
            output = root / "outputs.txt"
            environment = dict(os.environ, GITHUB_BASE_REF="main", GITHUB_SHA=merge,
                               PR_BASE_SHA=base, GITHUB_OUTPUT=str(output),
                               GITHUB_EVENT_NAME="pull_request")
            result = subprocess.run(["bash", "-c", shell], cwd=root, env=environment,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            values = dict(line.split("=", 1) for line in output.read_text().splitlines())
            self.assertEqual(values["check_scripts"], "true")
            self.assertEqual(values["check_rust"], "false")
            self.assertEqual(values["check_schema_tools"], "false")

    def test_manual_dispatch_runs_full_gates_after_evidence_only_commit(self):
        _, _, shell = workflow_filter_shell()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*arguments):
                return subprocess.run(["git", "-C", str(root), *arguments], capture_output=True,
                                      text=True, check=True).stdout.strip()

            git("init", "-q", "--initial-branch=main")
            git("config", "user.name", "Workflow fixture")
            git("config", "user.email", "fixture@example.invalid")
            for relative in ("crates/shared/src/lib.rs", "scripts/profiling/metal_trace.m",
                             "docs/research/dynamic-schema/schema-explorer-annotations.json",
                             "scripts/reference-enable-observations.py", "scripts/audit-z-fighting.py"):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("tracked owning fixture\n")
            # Make the tracked-path list exceed common pipe buffers. grep -q may
            # exit on the first owning path; an echo producer must not turn that
            # successful match into a SIGPIPE failure under set -o pipefail.
            for index in range(1000):
                (root / "docs/research" / (f"non-owning-{index:04}-"+"x"*125+".txt")).write_text("fixture\n")
            git("add", ".")
            git("commit", "-q", "-m", "owning code")
            self.assertGreater(len(git("ls-files").encode()), 128*1024)
            evidence = root / "docs/research/integration-record.json"
            evidence.write_text("{}\n")
            git("add", ".")
            git("commit", "-q", "-m", "evidence only")
            head = git("rev-parse", "HEAD")
            self.assertEqual(git("diff", "--name-only", "HEAD~1", head),
                             "docs/research/integration-record.json")
            # A push may use its diff. A manual qualification must still visit every
            # owning path even when the latest commit only records earlier evidence.
            for event, expected in (("push", "false"), ("workflow_dispatch", "true")):
                with self.subTest(event=event):
                    output = root / (event+"-outputs.txt")
                    environment = dict(os.environ, GITHUB_BASE_REF="", PR_BASE_SHA="",
                                       GITHUB_SHA=head, GITHUB_EVENT_NAME=event,
                                       GITHUB_OUTPUT=str(output))
                    result = subprocess.run(["bash", "-c", shell], cwd=root, env=environment,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    values = dict(line.split("=", 1) for line in output.read_text().splitlines())
                    self.assertEqual(set(values), {"check_rust", "check_scripts", "check_schema_tools",
                                                   "check_enable_observations", "check_z_fighting_audit",
                                                   "check_native_profiling"})
                    self.assertEqual(set(values.values()), {expected})


if __name__ == "__main__":
    unittest.main()
