import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "pr_size.py"
SPEC = importlib.util.spec_from_file_location("pr_size", SCRIPT)
PR_SIZE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(PR_SIZE)


def entry(filename, additions=0, deletions=0):
    return {"filename": filename, "additions": additions, "deletions": deletions}


class LabelThresholdTests(unittest.TestCase):
    def test_at_or_below_l_threshold_has_no_label(self):
        self.assertIsNone(PR_SIZE.label_for(0))
        self.assertIsNone(PR_SIZE.label_for(PR_SIZE.SIZE_L_THRESHOLD))

    def test_above_l_threshold_is_large(self):
        self.assertEqual(PR_SIZE.LABEL_L, PR_SIZE.label_for(PR_SIZE.SIZE_L_THRESHOLD + 1))
        self.assertEqual(PR_SIZE.LABEL_L, PR_SIZE.label_for(PR_SIZE.SIZE_XL_THRESHOLD))

    def test_above_xl_threshold_is_extra_large(self):
        self.assertEqual(
            PR_SIZE.LABEL_XL, PR_SIZE.label_for(PR_SIZE.SIZE_XL_THRESHOLD + 1)
        )


class SummarizeTests(unittest.TestCase):
    def test_additions_and_deletions_are_summed(self):
        result = PR_SIZE.summarize(
            [entry("crates/engine/src/app.rs", 120, 30), entry("README.md", 5, 1)]
        )
        self.assertEqual(156, result.lines)
        self.assertEqual(2, result.counted_files)
        self.assertEqual(0, result.excluded_files)
        self.assertIsNone(result.label)

    def test_empty_list(self):
        result = PR_SIZE.summarize([])
        self.assertEqual(0, result.lines)
        self.assertIsNone(result.label)

    def test_test_and_generated_paths_are_excluded(self):
        excluded = (
            "tests/helper.rs",
            "crates/engine/tests/draw.rs",
            "crates/converter/src/records/core_audit_regression_tests.rs",
            "test_tool.py",
            "scripts/tests/test_tool.py",
            "crates/converter/tests/snapshots/world.snap",
            "snapshots/world.snap",
            "Cargo.lock",
            "vendor/bevy_pbr/src/render/mesh.rs",
            "fuzz/seeds/nif/one.nif",
            "docs/data-inventory/inventory.json",
            "docs/research/evidence/metal-20261008/report.html",
            "tools/skyrim-recorder/reference/jump-v3/manifest.json",
            # Each nested pattern alone, outside any tests/ directory.
            "crates/engine/snapshots/world.snap",
            "scripts/test_tool.py",
            "tools/skyrim-recorder/fixtures/jump.json",
            "scripts/profiling/fixtures/sample.json",
            "docs/evidence/lighting-l1-20261003.json",
            "devenv.lock",
        )
        for filename in excluded:
            with self.subTest(filename=filename):
                result = PR_SIZE.summarize([entry(filename, 100, 50)])
                self.assertEqual(0, result.lines)
                self.assertEqual(0, result.counted_files)
                self.assertEqual(1, result.excluded_files)
                self.assertEqual(150, result.excluded_lines)

    def test_paths_that_only_look_excluded_are_counted(self):
        counted = (
            "src/tests.rs",
            "vendorish/mesh.rs",
            "Cargo.lock.backup",
            "docs/data-inventory.md",
            "crates/engine/src/testing_helpers.rs",
        )
        for filename in counted:
            with self.subTest(filename=filename):
                result = PR_SIZE.summarize([entry(filename, 10, 5)])
                self.assertEqual(15, result.lines)
                self.assertEqual(1, result.counted_files)

    def test_rename_uses_the_new_path(self):
        result = PR_SIZE.summarize(
            [
                {
                    "filename": "tests/moved.rs",
                    "previous_filename": "crates/engine/src/live.rs",
                    "additions": 40,
                    "deletions": 2,
                }
            ]
        )
        self.assertEqual(0, result.lines)
        self.assertEqual(1, result.excluded_files)

    def test_missing_filename_fails_closed(self):
        with self.assertRaisesRegex(ValueError, "filename"):
            PR_SIZE.summarize([{"additions": 1, "deletions": 0}])

    def test_missing_or_negative_line_counts_fail_closed(self):
        for bad in (
            {"filename": "src/main.rs", "deletions": 0},
            {"filename": "src/main.rs", "additions": 1},
            {"filename": "src/main.rs", "additions": -1, "deletions": 0},
            {"filename": "src/main.rs", "additions": "1", "deletions": 0},
        ):
            with self.subTest(entry=bad):
                with self.assertRaises(ValueError):
                    PR_SIZE.summarize([bad])

    def test_non_object_entry_fails_closed(self):
        with self.assertRaisesRegex(ValueError, "JSON object"):
            PR_SIZE.summarize([entry("src/main.rs", 1, 0), "src/main.rs"])


class CommandLineTests(unittest.TestCase):
    def run_cli(self, payload, output=None):
        with tempfile.TemporaryDirectory() as tmp:
            files = Path(tmp) / "files.json"
            files.write_text(json.dumps(payload), encoding="utf-8")
            args = ["--files", str(files)]
            if output is not None:
                args += ["--github-output", str(Path(tmp) / output)]
            stdout = io.StringIO()
            stderr = io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                code = PR_SIZE.main(args)
            output_text = None
            if output is not None:
                output_text = (Path(tmp) / output).read_text(encoding="utf-8")
            return code, stdout.getvalue(), stderr.getvalue(), output_text

    def test_summary_and_github_output(self):
        code, stdout, _, output = self.run_cli(
            [entry("crates/engine/src/app.rs", 3000, 100)],
            output="github-output",
        )
        self.assertEqual(0, code)
        self.assertIn("3100 changed hand-written lines", stdout)
        self.assertIn("label: size: L", stdout)
        self.assertEqual(
            "lines=3100\n"
            "counted_files=1\n"
            "excluded_files=0\n"
            "excluded_lines=0\n"
            "label=size: L\n"
            "truncated=false\n",
            output,
        )

    def test_small_pull_request_clears_the_label(self):
        code, stdout, _, output = self.run_cli(
            [entry("README.md", 10, 2)], output="github-output"
        )
        self.assertEqual(0, code)
        self.assertIn("label: none", stdout)
        self.assertIn("\nlabel=\n", "\n" + output)

    def test_malformed_json_exits_with_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            files = Path(tmp) / "files.json"
            files.write_text("{not json", encoding="utf-8")
            stdout = io.StringIO()
            stderr = io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                code = PR_SIZE.main(["--files", str(files)])
        self.assertEqual(2, code)
        self.assertIn("pr_size: error:", stderr.getvalue())

    def test_non_array_input_exits_with_an_error(self):
        code, _, stderr, _ = self.run_cli({"filename": "src/main.rs"})
        self.assertEqual(2, code)
        self.assertIn("JSON array", stderr)

    def test_missing_input_file_exits_with_an_error(self):
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            code = PR_SIZE.main(["--files", "/nonexistent/pr-files.json"])
        self.assertEqual(2, code)
        self.assertIn("pr_size: error:", stderr.getvalue())

    def test_truncated_file_list_warns(self):
        original = PR_SIZE.GITHUB_MAX_FILES
        PR_SIZE.GITHUB_MAX_FILES = 2
        try:
            code, _, stderr, output = self.run_cli(
                [entry("src/a.rs", 1, 0), entry("src/b.rs", 1, 0)],
                output="github-output",
            )
        finally:
            PR_SIZE.GITHUB_MAX_FILES = original
        self.assertEqual(0, code)
        self.assertIn("warning:", stderr)
        self.assertIn("truncated=true", output)


if __name__ == "__main__":
    unittest.main()
