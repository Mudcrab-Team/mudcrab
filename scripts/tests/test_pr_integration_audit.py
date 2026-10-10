"""Exercise integration gates with real local Git histories; no remote calls."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "audit-pr-integration.py"
SPEC = importlib.util.spec_from_file_location("pr_integration_audit", SCRIPT)
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


class IntegrationAuditTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "-q", "--initial-branch=main")
        self.git("config", "user.name", "Integration fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        workflow = self.root / ".github/workflows/run_tests.yml"
        workflow.parent.mkdir(parents=True)
        workflow.write_text("on:\n  pull_request:\n  push:\n    branches: [main]\njobs: {}\n")
        (self.root / "shared.txt").write_text("".join(f"line {i}\n" for i in range(12)))
        self.initial = self.commit("initial")
        self.git("update-ref", "refs/remotes/origin/main", self.initial)
        self.inventory = []
        for number, line in ((1, 0), (2, 11)):
            self.git("checkout", "-q", "-b", f"feature-{number}", self.initial)
            path = self.root / "shared.txt"
            lines = path.read_text().splitlines(keepends=True)
            lines[line] = f"changed by PR {number}\n"
            path.write_text("".join(lines))
            (self.root / f"feature-{number}.txt").write_text(f"feature {number}\n")
            oid = self.commit(f"feature {number}")
            self.git("update-ref", f"refs/integration/test/pr-{number}", oid)
            self.inventory.append({"number": number, "headRefName": f"feature-{number}",
                                   "headRefOid": oid, "baseRefName": "main",
                                   "mergeBaseOid": self.initial,
                                   "files": ["shared.txt", f"feature-{number}.txt"]})
        self.git("checkout", "-q", "main")

    def git(self, *arguments):
        result = subprocess.run(["git", "-C", str(self.root), *arguments],
                                capture_output=True, text=True, check=True)
        return result.stdout.strip()

    def commit(self, message):
        self.git("add", ".")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def integrate(self):
        for number in (1, 2):
            self.git("merge", "-q", "--no-ff", f"feature-{number}", "-m", f"merge {number}")

    def audit(self):
        return AUDIT.audit(self.root, self.inventory, "refs/integration/test")

    def test_missing_commit_cannot_pass_even_with_matching_ref(self):
        self.git("merge", "-q", "--no-ff", "feature-1", "-m", "merge 1")
        result = self.audit()
        self.assertFalse(result["passed"])
        self.assertEqual(result["integrated_count"], 1)
        self.assertEqual([(error["kind"], error["number"]) for error in result["errors"]],
                         [("head_not_integrated", 2)])

    def test_integrated_heads_and_shared_paths_are_reported(self):
        self.integrate()
        result = self.audit()
        self.assertTrue(result["passed"], result["errors"])
        self.assertEqual(result["integrated_count"], 2)
        self.assertEqual(result["shared_paths"], [{"path": "shared.txt", "prs": [1, 2], "count": 2}])
        self.assertEqual(result["independent_shared_paths"],
                         [{"path": "shared.txt", "independent_pr_pairs": [[1, 2]], "count": 1}])
        self.assertEqual(result["integrationHeadOid"], self.git("rev-parse", "HEAD"))

    def test_moved_and_missing_refs_cannot_substitute_other_commits(self):
        self.integrate()
        self.git("update-ref", "refs/integration/test/pr-1", "HEAD")
        self.git("update-ref", "-d", "refs/integration/test/pr-2")
        result = self.audit()
        self.assertFalse(result["passed"])
        self.assertEqual([error["kind"] for error in result["errors"]],
                         ["ref_head_mismatch", "pr_audit_failed"])

    def test_stacked_diff_uses_captured_base_and_ci_gap_is_a_failure(self):
        self.integrate()
        self.git("checkout", "-q", "-b", "stacked", "feature-1")
        (self.root / "stacked.txt").write_text("stacked feature\n")
        (self.root / "feature-1.txt").write_text("refined stacked feature\n")
        oid = self.commit("stacked")
        self.git("update-ref", "refs/integration/test/pr-3", oid)
        self.inventory.append({"number": 3, "headRefName": "stacked", "headRefOid": oid,
                               "baseRefName": "feature-1", "mergeBaseOid": self.inventory[0]["headRefOid"]})
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "stacked", "-m", "merge stack")
        workflow = self.root / ".github/workflows/run_tests.yml"
        workflow.write_text("on:\n  pull_request:\n    branches: [main, develop]\njobs: {}\n")
        self.commit("limit workflow bases")
        result = self.audit()
        self.assertFalse(result["passed"])
        self.assertEqual(result["prs"][-1]["files"], ["feature-1.txt", "stacked.txt"])
        self.assertIn({"path": "feature-1.txt", "prs": [1, 3], "count": 2}, result["shared_paths"])
        self.assertNotIn("feature-1.txt", [row["path"] for row in result["independent_shared_paths"]])
        self.assertEqual(result["errors"], [{"kind": "ci_base_not_covered", "number": 3,
                                             "baseRefName": "feature-1"}])

    def test_stale_files_and_invalid_merge_base_cannot_hide_hotspots(self):
        self.integrate()
        self.inventory[0]["files"] = ["feature-1.txt"]
        self.inventory[1]["mergeBaseOid"] = self.inventory[0]["headRefOid"]
        result = self.audit()
        self.assertFalse(result["passed"])
        self.assertEqual([error["kind"] for error in result["errors"]],
                         ["captured_files_mismatch", "pr_audit_failed"])

    def test_empty_and_duplicate_inventory_are_rejected(self):
        for inventory in ([], [self.inventory[0], self.inventory[0]]):
            with self.subTest(inventory=inventory), self.assertRaises(ValueError):
                AUDIT.audit(self.root, inventory, "refs/integration/test")

    def test_cli_failure_writes_reviewable_json_and_returns_nonzero(self):
        inventory = self.root / "inventory.json"
        inventory.write_text(json.dumps(self.inventory))
        output = self.root / "audit.json"
        process = subprocess.run([sys.executable, str(SCRIPT), "--repository", str(self.root),
                                  "--inventory", str(inventory), "--ref-prefix", "refs/integration/test",
                                  "--output", str(output)], capture_output=True, text=True)
        self.assertEqual(process.returncode, 1, process.stderr)
        self.assertFalse(json.loads(output.read_text())["passed"])
        self.assertEqual(process.stdout, "")


class WorkflowPolicyTests(unittest.TestCase):
    def test_multiline_filters_and_ordered_negation(self):
        policy = AUDIT.pull_request_policy("on:\n  pull_request:\n    branches:\n    - main\n    - 'feat/**'\n    - '!feat/parked/**'\n    - 'feat/parked/ready'\njobs: {}\n")
        for base, expected in (("main", True), ("feat/a/b", True), ("feat/parked/a", False),
                               ("feat/parked/ready", True), ("develop", False)):
            with self.subTest(base=base):
                self.assertEqual(AUDIT.base_runs_ci(base, policy), expected)

    def test_single_star_does_not_cover_nested_branch_and_ignore_is_respected(self):
        policy = AUDIT.pull_request_policy("on:\n  pull_request:\n    branches: ['feat/*']\n")
        self.assertTrue(AUDIT.base_runs_ci("feat/one", policy))
        self.assertFalse(AUDIT.base_runs_ci("feat/one/two", policy))
        policy = AUDIT.pull_request_policy("on:\n  pull_request:\n    branches-ignore: ['parked/**']\n")
        self.assertTrue(AUDIT.base_runs_ci("feat/one/two", policy))
        self.assertFalse(AUDIT.base_runs_ci("parked/one", policy))

    def test_absent_pr_event_and_unsupported_syntax_fail_explicitly(self):
        self.assertFalse(AUDIT.base_runs_ci("main", AUDIT.pull_request_policy("on: [push]\n")))
        self.assertTrue(AUDIT.base_runs_ci("any/base", AUDIT.pull_request_policy("on: [pull_request, push]\n")))
        with self.assertRaises(ValueError):
            AUDIT.pull_request_policy("on: {pull_request: {}}\n")
        with self.assertRaises(ValueError):
            AUDIT.base_runs_ci("main", {"enabled": True, "branches": ["[md]*"], "branches_ignore": []})


if __name__ == "__main__":
    unittest.main()
