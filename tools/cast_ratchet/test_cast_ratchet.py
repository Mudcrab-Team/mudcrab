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
    """A Cargo JSON record that shows a crate was built."""
    return {
        "reason": "compiler-artifact",
        "package_id": f"path+file:///repo/crates/{crate}#{crate}@0.1.0",
        "manifest_path": f"/repo/crates/{crate}/Cargo.toml",
        "target": {"name": name or crate, "kind": [kind]},
    }


def diagnostic(crate, lint, line, text="lossy cast", kind="lib", name=None, level="warning"):
    """A Clippy diagnostic with one primary span."""
    return {
        "reason": "compiler-message",
        "package_id": f"path+file:///repo/crates/{crate}#{crate}@0.1.0",
        "manifest_path": f"/repo/crates/{crate}/Cargo.toml",
        "target": {"name": name or crate, "kind": [kind]},
        "message": {
            "code": {"code": lint},
            "level": level,
            "message": text,
            "spans": [
                {
                    "file_name": f"crates/{crate}/src/lib.rs",
                    "line_start": line,
                    "column_start": 5,
                    "is_primary": True,
                }
            ],
        },
    }


def write_log(path, diagnostics, finish=True, artifacts=None):
    records = list(artifacts if artifacts is not None else [artifact(c) for c in ratchet.CRATES])
    records.extend(diagnostics)
    if finish:
        records.append({"reason": "build-finished", "success": True})
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

    def test_an_incomplete_log_is_an_error(self):
        write_log(self.log, [], finish=False)
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("build-finished", output)

    def test_a_log_without_a_ratcheted_crate_is_an_error(self):
        write_log(self.log, [], artifacts=[artifact("shared")])
        code, output = self.run_ratchet()
        self.assertEqual(code, 2)
        self.assertIn("converter", output)
        self.assertIn("compiler-artifact", output)

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


if __name__ == "__main__":
    unittest.main()
