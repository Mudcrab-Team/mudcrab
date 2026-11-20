"""Tests for the lossy-cast Clippy ratchet. They start no process but the script itself."""

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("cast_ratchet.py")
SPEC = importlib.util.spec_from_file_location("cast_ratchet", SCRIPT)
ratchet = importlib.util.module_from_spec(SPEC)
sys.modules["cast_ratchet"] = ratchet
SPEC.loader.exec_module(ratchet)


def artifact(crate, kind="lib", name=None):
    """A Cargo JSON record that shows a crate target was built."""
    return {
        "reason": "compiler-artifact",
        "package_id": f"path+file:///repo/crates/{crate}#{crate}@0.1.0",
        "manifest_path": f"/repo/crates/{crate}/Cargo.toml",
        "target": {"name": name or crate, "kind": [kind]},
    }


def required_artifacts():
    """Every artifact that the ratchet requires, one per record."""
    return [
        artifact(crate, kind)
        for crate in ratchet.CRATES
        for kind in ratchet.REQUIRED_KINDS[crate]
    ]


def diagnostic(
    crate,
    lint,
    line,
    text="lossy cast",
    kind="lib",
    name=None,
    level="warning",
    macro_call_line=None,
    target=True,
):
    """A Clippy diagnostic with one primary span."""
    span = {
        "file_name": f"crates/{crate}/src/lib.rs",
        "line_start": line,
        "column_start": 5,
        "is_primary": True,
    }
    if macro_call_line is not None:
        span["expansion"] = {
            "span": {
                "file_name": f"crates/{crate}/src/lib.rs",
                "line_start": macro_call_line,
                "column_start": 9,
            },
            "macro_decl_name": "lossy!",
        }
    record = {
        "reason": "compiler-message",
        "package_id": f"path+file:///repo/crates/{crate}#{crate}@0.1.0",
        "manifest_path": f"/repo/crates/{crate}/Cargo.toml",
        "message": {
            "code": {"code": lint} if lint else None,
            "level": level,
            "message": text,
            "spans": [span],
        },
    }
    if target:
        record["target"] = {"name": name or crate, "kind": [kind]}
    return record


def write_log(path, diagnostics, finish=True, success=True, artifacts=None):
    records = list(required_artifacts() if artifacts is None else artifacts)
    records.extend(diagnostics)
    if finish:
        records.append({"reason": "build-finished", "success": success})
    path.write_text(
        "\n".join(json.dumps(record) for record in records) + "\n", encoding="utf-8"
    )


class RatchetTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "tools/cast_ratchet").mkdir(parents=True)
        self.log = self.root / "clippy.json"
        self.baseline_path = self.root / ratchet.DEFAULT_BASELINE
        self.baseline_path.write_text(
            json.dumps(
                {
                    "counts": {
                        crate: {lint: 0 for lint in ratchet.LINTS}
                        for crate in ratchet.CRATES
                    }
                }
            ),
            encoding="utf-8",
        )

    def set_count(self, crate, lint, value):
        document = json.loads(self.baseline_path.read_text(encoding="utf-8"))
        document["counts"][crate][lint] = value
        self.baseline_path.write_text(json.dumps(document), encoding="utf-8")

    def run_ratchet(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchet.main(
                ["--root", str(self.root), "--clippy-json", str(self.log), *extra]
            )
        return code, out.getvalue() + err.getvalue()

    def test_a_rise_fails_and_names_the_crate_and_lint(self):
        self.set_count("converter", "clippy::cast_possible_truncation", 1)
        write_log(
            self.log,
            [
                diagnostic("converter", "clippy::cast_possible_truncation", 10),
                diagnostic("converter", "clippy::cast_possible_truncation", 20),
            ],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("converter", output)
        self.assertIn("clippy::cast_possible_truncation", output)
        self.assertIn("rose from 1 to 2", output)
        self.assertIn("RAISED", output)

    def test_an_equal_count_passes(self):
        self.set_count("shared", "clippy::cast_sign_loss", 2)
        write_log(
            self.log,
            [
                diagnostic("shared", "clippy::cast_sign_loss", 10),
                diagnostic("shared", "clippy::cast_sign_loss", 20),
            ],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("ok", output)
        self.assertNotIn("RAISED", output)

    def test_a_fall_prints_the_new_lower_number(self):
        self.set_count("converter", "clippy::cast_possible_wrap", 3)
        write_log(
            self.log,
            [
                diagnostic("converter", "clippy::cast_possible_wrap", 10),
                diagnostic("converter", "clippy::cast_possible_wrap", 20),
            ],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("fell from 3 to 2", output)
        self.assertIn("it writes 2", output)
        self.assertIn("Lower the baseline", output)

    def test_a_build_script_is_not_counted(self):
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_sign_loss", 10, kind="custom-build")],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertNotIn("RAISED", output)

    def test_a_bin_of_the_package_counts_as_the_crate(self):
        self.set_count("converter", "clippy::cast_possible_truncation", 0)
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    10,
                    kind="bin",
                    name="asset-closure",
                )
            ],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("rose from 0 to 1", output)

    def test_a_path_dependency_is_not_counted(self):
        record = diagnostic("project-wormhole-nif", "clippy::cast_possible_truncation", 10)
        record["manifest_path"] = "/repo/vendor/project-wormhole-nif/Cargo.toml"
        write_log(self.log, [record])
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertNotIn("RAISED", output)

    def test_a_duplicate_diagnostic_counts_once(self):
        self.set_count("converter", "clippy::float_cmp", 1)
        record = diagnostic("converter", "clippy::float_cmp", 10)
        write_log(self.log, [record, record])
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("ok", output)

    def test_the_same_span_under_lib_and_bin_counts_once(self):
        self.set_count("converter", "clippy::cast_sign_loss", 1)
        write_log(
            self.log,
            [
                diagnostic("converter", "clippy::cast_sign_loss", 10, kind="lib"),
                diagnostic("converter", "clippy::cast_sign_loss", 10, kind="bin"),
            ],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("ok", output)

    def test_two_uses_of_one_macro_count_twice(self):
        self.set_count("converter", "clippy::cast_possible_truncation", 1)
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    10,
                    macro_call_line=30,
                ),
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    10,
                    macro_call_line=40,
                ),
            ],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("rose from 1 to 2", output)

    def test_a_call_site_key_also_separates_macro_uses(self):
        def use(line):
            record = diagnostic("converter", "clippy::float_cmp", 10)
            record["message"]["spans"][0]["expansion"] = {
                "call_site": {
                    "file_name": "crates/converter/src/lib.rs",
                    "line_start": line,
                    "column_start": 9,
                }
            }
            return record

        self.set_count("converter", "clippy::float_cmp", 1)
        write_log(self.log, [use(30), use(40)])
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("rose from 1 to 2", output)

    def test_an_incomplete_log_is_an_error(self):
        write_log(self.log, [], finish=False)
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("build-finished", output)

    def test_a_failed_build_is_an_error(self):
        write_log(self.log, [], success=False)
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("build-finished", output)

    def test_an_incomplete_log_still_prints_the_table(self):
        self.set_count("shared", "clippy::float_cmp", 5)
        write_log(self.log, [], success=False)
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("clippy::float_cmp", output)
        self.assertIn("below baseline", output)
        self.assertIn("may be low", output)

    def test_update_refuses_an_incomplete_log(self):
        write_log(self.log, [], success=False)
        self.assertEqual(self.run_ratchet("--update")[0], 2)
        document = json.loads(self.baseline_path.read_text(encoding="utf-8"))
        self.assertEqual(document["counts"]["converter"]["clippy::float_cmp"], 0)

    def test_update_refuses_warnings_outside_the_four_lints(self):
        write_log(self.log, [diagnostic("converter", "clippy::needless_return", 10)])
        code, output = self.run_ratchet("--update")
        self.assertEqual(code, 1)
        self.assertIn("baseline not written", output)
        document = json.loads(self.baseline_path.read_text(encoding="utf-8"))
        self.assertEqual(document["counts"]["converter"]["clippy::float_cmp"], 0)

    def test_an_error_level_ratchet_diagnostic_counts(self):
        self.set_count("converter", "clippy::float_cmp", 0)
        write_log(
            self.log,
            [diagnostic("converter", "clippy::float_cmp", 10, level="error")],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("rose from 0 to 1", output)

    def test_a_log_without_a_ratcheted_crate_is_an_error(self):
        write_log(self.log, [], artifacts=[artifact("shared")])
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("converter", output)
        self.assertIn("compiler-artifact", output)

    def test_a_log_without_a_converter_bin_is_an_error(self):
        write_log(
            self.log,
            [],
            artifacts=[artifact("converter"), artifact("shared")],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("converter bin", output)
        self.assertIn("ratcheted target", output)

    def test_a_log_without_a_converter_lib_is_an_error(self):
        write_log(
            self.log,
            [],
            artifacts=[artifact("converter", "bin"), artifact("shared")],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("converter lib", output)

    def test_a_log_without_a_shared_lib_is_an_error(self):
        write_log(
            self.log,
            [],
            artifacts=[artifact("converter"), artifact("converter", "bin")],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("shared lib", output)

    def test_one_crate_check_needs_only_that_crates_targets(self):
        write_log(self.log, [], artifacts=[artifact("converter"), artifact("converter", "bin")])
        code, output = self.run_ratchet("--crate", "converter")
        self.assertEqual(code, 0)

    def test_a_ratchet_diagnostic_without_a_target_is_an_error(self):
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_sign_loss", 10, target=False)],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("no target kind", output)
        self.assertIn("converter", output)

    def test_an_unratcheted_crate_needs_no_target(self):
        record = diagnostic("dummy-content", "clippy::cast_sign_loss", 10, target=False)
        record["manifest_path"] = "/repo/crates/dummy-content/Cargo.toml"
        write_log(self.log, [record])
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertNotIn("no target kind", output)

    def test_a_warning_outside_the_four_lints_fails(self):
        write_log(
            self.log,
            [diagnostic("converter", "clippy::needless_return", 10, text="unneeded return")],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("outside the four ratchet lints", output)
        self.assertIn("converter: clippy::needless_return: unneeded return", output)

    def test_an_error_outside_the_four_lints_fails(self):
        write_log(
            self.log,
            [diagnostic("converter", None, 10, level="error", text="mismatched types")],
        )
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("no lint code", output)

    def test_a_repeated_warning_outside_the_four_lints_is_reported_once(self):
        record = diagnostic("converter", "clippy::needless_return", 10, text="unneeded return")
        write_log(self.log, [record, record])
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertEqual(output.count("outside the four ratchet lints"), 1)
        self.assertEqual(output.count("converter: clippy::needless_return"), 1)

    def test_a_diagnostic_note_is_not_counted_as_a_warning(self):
        write_log(self.log, [diagnostic("converter", None, 10, level="note", text="note")])
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertNotIn("outside the four ratchet lints", output)

    def test_a_missing_baseline_entry_is_an_error(self):
        document = json.loads(self.baseline_path.read_text(encoding="utf-8"))
        del document["counts"]["shared"]
        self.baseline_path.write_text(json.dumps(document), encoding="utf-8")
        write_log(self.log, [])
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("shared", output)
        self.assertIn("--update", output)

    def test_a_malformed_log_is_an_error(self):
        self.log.write_text("{not json}\n", encoding="utf-8")
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("not a JSON record", output)

    def test_a_missing_log_is_an_error(self):
        self.log = self.root / "missing.json"
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("cannot read", output)

    def test_an_unknown_baseline_lint_is_an_error(self):
        document = json.loads(self.baseline_path.read_text(encoding="utf-8"))
        document["counts"]["converter"]["clippy::cast_possible_truncation_typo"] = 0
        self.baseline_path.write_text(json.dumps(document), encoding="utf-8")
        write_log(self.log, [])
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("unknown lint", output)

    def test_a_non_integer_baseline_count_is_an_error(self):
        document = json.loads(self.baseline_path.read_text(encoding="utf-8"))
        document["counts"]["converter"]["clippy::float_cmp"] = "5"
        self.baseline_path.write_text(json.dumps(document), encoding="utf-8")
        write_log(self.log, [])
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("not an integer", output)

    def test_a_log_line_that_is_not_an_object_is_an_error(self):
        self.log.write_text("[1, 2]\n", encoding="utf-8")
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("not a JSON object", output)

    def test_update_writes_the_measured_counts(self):
        write_log(
            self.log,
            [
                diagnostic("converter", "clippy::cast_possible_truncation", 10),
                diagnostic("shared", "clippy::float_cmp", 20),
            ],
        )
        self.assertEqual(self.run_ratchet("--update")[0], 0)
        document = json.loads(self.baseline_path.read_text(encoding="utf-8"))
        self.assertEqual(
            document["counts"]["converter"]["clippy::cast_possible_truncation"], 1
        )
        self.assertEqual(document["counts"]["shared"]["clippy::float_cmp"], 1)
        self.assertEqual(self.run_ratchet()[0], 0)
        self.assertEqual(self.run_ratchet("--strict")[0], 0)

    def test_update_writes_lf_newlines(self):
        write_log(self.log, [])
        self.assertEqual(self.run_ratchet("--update")[0], 0)
        self.assertNotIn(b"\r", self.baseline_path.read_bytes())

    def test_strict_fails_on_a_fall(self):
        self.set_count("converter", "clippy::cast_sign_loss", 1)
        write_log(self.log, [])
        self.assertEqual(self.run_ratchet()[0], 0)
        self.assertEqual(self.run_ratchet("--strict")[0], 1)

    def test_lint_flags_select_every_lint(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ratchet.main(["--lint-flags"])
        self.assertEqual(code, 0)
        for lint in ratchet.LINTS:
            self.assertIn(f"-W {lint}", out.getvalue())

    def test_lint_flags_match_the_workflow_list(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            ratchet.main(["--lint-flags"])
        self.assertEqual(
            out.getvalue().strip(),
            " ".join(f"-W {lint}" for lint in ratchet.LINTS),
        )
        self.assertEqual(len(ratchet.LINTS), 4)


if __name__ == "__main__":
    unittest.main()
