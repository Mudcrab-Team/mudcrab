"""Deadline and report-preservation tests for the bounded source evidence script."""
import contextlib
import importlib.util
import io
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[2] / "docs/research/dynamic-schema/pilot_code_evidence.py"
SPEC = importlib.util.spec_from_file_location("pilot_code_evidence", SCRIPT)
pilot_code_evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pilot_code_evidence)


class PilotCodeEvidenceTests(unittest.TestCase):
    def test_v117_git_revision_timeout_is_bounded_and_names_inspected_revision(self):
        timeout = subprocess.TimeoutExpired("git", pilot_code_evidence.GIT_TIMEOUT_SECONDS)
        with mock.patch.object(pilot_code_evidence.subprocess, "check_output", side_effect=timeout) as run:
            with self.assertRaisesRegex(
                pilot_code_evidence.SourceInspectionTimeout,
                "resolving inspected revision base-ref",
            ):
                pilot_code_evidence.git(
                    Path("/synthetic/repo"), "rev-parse", "base-ref^{commit}",
                    context="resolving inspected revision base-ref",
                )
        self.assertEqual(run.call_args.kwargs["timeout"], pilot_code_evidence.GIT_TIMEOUT_SECONDS)

    def test_v117_git_file_timeout_names_revision_and_source_path(self):
        timeout = subprocess.TimeoutExpired("git", pilot_code_evidence.GIT_TIMEOUT_SECONDS)
        with mock.patch.object(pilot_code_evidence.subprocess, "check_output", side_effect=timeout) as run:
            with self.assertRaisesRegex(
                pilot_code_evidence.SourceInspectionTimeout,
                "reading crates/converter/src/esm/exporter.rs at inspected revision abc123",
            ):
                pilot_code_evidence.file_at(
                    Path("/synthetic/repo"), "abc123", "crates/converter/src/esm/exporter.rs"
                )
        self.assertEqual(run.call_args.kwargs["timeout"], pilot_code_evidence.GIT_TIMEOUT_SECONDS)

    def test_v117_timeout_returns_controlled_failure_without_replacing_report(self):
        base = {"commit": "resolved-base", "files": {}}
        output_args = [
            "--repo", "/synthetic/repo", "--base", "base-ref", "--current", "current-ref",
            "--output", "/unused/source-report.json",
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "source-report.json"
            output.write_text("existing report bytes\n", encoding="utf-8")
            output_args[output_args.index("/unused/source-report.json")] = str(output)
            stderr = io.StringIO()
            timeout = pilot_code_evidence.SourceInspectionTimeout(
                "Git timed out while reading a source at inspected revision current-ref"
            )
            with mock.patch.object(pilot_code_evidence, "describe_revision", side_effect=[base, timeout]), \
                 contextlib.redirect_stderr(stderr):
                status = pilot_code_evidence.main(output_args)
            self.assertEqual(status, 2)
            self.assertIn("current-ref", stderr.getvalue())
            self.assertEqual(output.read_text(encoding="utf-8"), "existing report bytes\n")


if __name__ == "__main__":
    unittest.main()
