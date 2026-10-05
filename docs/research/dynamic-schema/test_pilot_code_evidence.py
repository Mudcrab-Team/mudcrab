"""Deadline and report-preservation tests for the bounded source evidence script."""
import contextlib
import importlib.util
import io
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().with_name("pilot_code_evidence.py")
SPEC = importlib.util.spec_from_file_location("pilot_code_evidence", SCRIPT)
pilot_code_evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pilot_code_evidence)


class PilotCodeEvidenceTests(unittest.TestCase):
    def test_v134_lifetime_in_body_does_not_hide_nested_braces(self):
        source = """fn inspect() {
    let value: &'static str = "text";
    if !value.is_empty() {
        finish();
    }
}
fn following() {}
"""

        start, end = pilot_code_evidence.function_span(source, "inspect")
        body = source[start:end]

        self.assertIn("finish();", body)
        self.assertTrue(body.endswith("}"))
        self.assertNotIn("fn following", body)

    def test_v134_generic_lifetime_function_declaration_is_found(self):
        source = """fn inspect<'a>(value: &'a str) {
    if !value.is_empty() {
        finish();
    }
}
"""

        start, end = pilot_code_evidence.function_span(source, "inspect")
        self.assertIn("finish();", source[start:end])

    def test_v134_braces_inside_character_literals_are_ignored(self):
        source = """fn inspect() {
    let open = '{';
    let close = '}';
    if open != close {
        finish();
    }
}
"""

        start, end = pilot_code_evidence.function_span(source, "inspect")
        self.assertIn("finish();", source[start:end])

    def test_v155_git_revision_timeout_is_bounded_and_names_inspected_revision(self):
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

    def test_v155_git_file_timeout_names_revision_and_source_path(self):
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

    def test_v155_timeout_returns_controlled_failure_without_replacing_report(self):
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

    def test_v134_check_missing_or_invalid_report_returns_controlled_failure(self):
        revision = {
            "commit": "resolved-revision",
            "files": {path: {"sha256": "same"} for path in pilot_code_evidence.PILOT_FUNCTIONS},
        }
        with tempfile.TemporaryDirectory() as directory:
            for initial_content in (None, "{"):
                with self.subTest(initial_content=initial_content):
                    output = Path(directory) / "source-report.json"
                    output.unlink(missing_ok=True)
                    if initial_content is not None:
                        output.write_text(initial_content, encoding="utf-8")
                    stderr = io.StringIO()
                    with mock.patch.object(
                        pilot_code_evidence, "describe_revision", side_effect=[revision, revision]
                    ), mock.patch.object(
                        pilot_code_evidence, "function_comparison", return_value={}
                    ), contextlib.redirect_stderr(stderr):
                        status = pilot_code_evidence.main([
                            "--repo", "/synthetic/repo",
                            "--base", "base-ref",
                            "--current", "current-ref",
                            "--output", str(output),
                            "--check",
                        ])
                    self.assertEqual(status, 2)
                    self.assertIn("pilot code evidence failed", stderr.getvalue())
                    self.assertNotIn("Traceback", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
