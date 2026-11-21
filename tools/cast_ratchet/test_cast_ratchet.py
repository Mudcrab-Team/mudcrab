"""Tests for the added-line lossy-cast gate; builds throwaway repositories."""

import contextlib
import importlib.util
import io
import json
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("cast_ratchet.py")
SPEC = importlib.util.spec_from_file_location("cast_ratchet", SCRIPT)
ratchet = importlib.util.module_from_spec(SPEC)
sys.modules["cast_ratchet"] = ratchet
SPEC.loader.exec_module(ratchet)

WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/run_tests.yml"
CLIPPY_STEP = "Clippy check and added-line cast gate"


def run_git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        text=True,
        capture_output=True,
        check=True,
    )


def artifact(crate, kind="lib", name=None):
    """A Cargo JSON record that shows a crate target was built."""
    return {
        "reason": "compiler-artifact",
        "package_id": f"path+file:///repo/crates/{crate}#{crate}@0.1.0",
        "manifest_path": f"/repo/crates/{crate}/Cargo.toml",
        "target": {"name": name or crate, "kind": [kind]},
    }


def required_artifacts():
    """Every artifact that the gate requires, one per record."""
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
    path=None,
    macro_call_line=None,
    target=True,
    spans=True,
):
    """A Clippy diagnostic with one primary span."""
    if path is None:
        path = f"crates/{crate}/src/lib.rs"
    span = {
        "file_name": path,
        "line_start": line,
        "column_start": 5,
        "is_primary": True,
    }
    if macro_call_line is not None:
        span["expansion"] = {
            "span": {
                "file_name": path,
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
            "spans": [span] if spans else [],
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


def workflow_job(name):
    """Return the block of one workflow job."""
    workflow = WORKFLOW.read_text(encoding="utf-8")
    start = workflow.index(f"\n  {name}:")
    following = re.search(r"\n  [a-z_]+:\n", workflow[start + 1 :])
    end = start + 1 + following.start() if following else len(workflow)
    return workflow[start:end]


def workflow_step_script(job, name):
    """Return the shell block of one workflow step."""
    start = job.index("run: |", job.index(f"name: {name}"))
    start = job.index("\n", start) + 1
    end = job.find("\n      - name:", start)
    if end < 0:
        end = len(job)
    return textwrap.dedent(job[start:end])


class Repository:
    """A throwaway git repository with the two crates' lib files."""

    def __init__(self, root):
        self.root = Path(root)
        run_git(self.root, "init", "-q")
        run_git(self.root, "config", "user.email", "test@example.invalid")
        run_git(self.root, "config", "user.name", "Cast Gate Test")
        self.write("crates/converter/src/lib.rs", "pub fn convert() {}\n")
        self.write("crates/shared/src/lib.rs", "pub fn shared() {}\n")
        self.commit("base")
        self.base = run_git(self.root, "rev-parse", "HEAD").stdout.strip()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def append(self, path, text):
        target = self.root / path
        target.write_text(target.read_text(encoding="utf-8") + text, encoding="utf-8")

    def commit(self, message):
        run_git(self.root, "add", "-A")
        run_git(self.root, "commit", "-q", "-m", message)

    def mark_base(self):
        self.base = run_git(self.root, "rev-parse", "HEAD").stdout.strip()


class GateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Repository(self.temporary.name)
        self.log = self.repo.root / "clippy.json"

    def run_gate(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchet.main(
                [
                    "--root",
                    str(self.repo.root),
                    "--base",
                    self.repo.base,
                    "--clippy-json",
                    str(self.log),
                    *extra,
                ]
            )
        return code, out.getvalue() + err.getvalue()

    def test_an_added_cast_fails_and_names_the_line(self):
        self.repo.append(
            "crates/converter/src/lib.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add a lossy cast")
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_possible_truncation", 2)],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/converter/src/lib.rs:2", output)
        self.assertIn("clippy::cast_possible_truncation", output)
        self.assertIn("1 new Clippy hit(s)", output)

    def test_an_added_cast_in_shared_fails(self):
        self.repo.append(
            "crates/shared/src/lib.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add a shared cast")
        write_log(self.log, [diagnostic("shared", "clippy::cast_possible_wrap", 2)])
        self.assertEqual(self.run_gate()[0], 1)

    def test_a_cast_on_an_unchanged_line_passes_and_is_counted(self):
        self.repo.append(
            "crates/converter/src/lib.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add the cast on main")
        self.repo.mark_base()
        self.repo.append("crates/converter/src/lib.rs", "pub fn other() {}\n")
        self.repo.commit("unrelated change")
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_possible_truncation", 2)],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0)
        self.assertIn("1 existing hit(s) on unchanged lines", output)

    def test_a_moved_line_is_not_new(self):
        self.repo.append(
            "crates/converter/src/lib.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add the cast on main")
        self.repo.mark_base()
        self.repo.write(
            "crates/converter/src/lib.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n"
            "pub fn convert() {}\n",
        )
        self.repo.commit("move the cast to the top")
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_possible_truncation", 1)],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0)
        self.assertIn("1 existing hit(s)", output)

    def test_a_duplicated_line_is_new(self):
        self.repo.append(
            "crates/converter/src/lib.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add the cast on main")
        self.repo.mark_base()
        self.repo.append(
            "crates/converter/src/lib.rs",
            "pub fn probe2(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("copy the cast")
        write_log(
            self.log,
            [
                diagnostic("converter", "clippy::cast_possible_truncation", 2),
                diagnostic("converter", "clippy::cast_possible_truncation", 3),
            ],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/converter/src/lib.rs:3", output)
        self.assertIn("1 new Clippy hit(s)", output)

    def test_a_cast_in_a_new_file_fails(self):
        self.repo.write(
            "crates/converter/src/probe.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add a module with a cast")
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    1,
                    path="crates/converter/src/probe.rs",
                )
            ],
        )
        self.assertEqual(self.run_gate()[0], 1)

    def test_a_pure_rename_is_not_new(self):
        self.repo.write(
            "crates/converter/src/old.rs",
            "pub fn convert() {}\n"
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add the old module")
        self.repo.mark_base()
        run_git(
            self.repo.root,
            "mv",
            "crates/converter/src/old.rs",
            "crates/converter/src/new.rs",
        )
        self.repo.commit("rename the module")
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    2,
                    path="crates/converter/src/new.rs",
                )
            ],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0)
        self.assertIn("1 existing hit(s)", output)

    def test_a_macro_call_site_on_an_added_line_fails(self):
        self.repo.append("crates/converter/src/lib.rs", "probe!(value);\n")
        self.repo.commit("use the macro")
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    10,
                    macro_call_line=2,
                )
            ],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/converter/src/lib.rs:2", output)

    def test_a_macro_use_on_an_unchanged_line_passes(self):
        self.repo.append("crates/converter/src/lib.rs", "probe!(value);\n")
        self.repo.commit("use the macro")
        self.repo.mark_base()
        self.repo.append("crates/converter/src/lib.rs", "pub fn other() {}\n")
        self.repo.commit("change the macro body")
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    2,
                    macro_call_line=1,
                )
            ],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0)
        self.assertIn("1 existing hit(s)", output)

    def test_a_bin_target_on_an_added_line_fails(self):
        self.repo.write(
            "crates/converter/src/bin/probe.rs",
            "fn main() { let value = 1_u32; let _ = value as u8; }\n",
        )
        self.repo.commit("add a bin with a cast")
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    1,
                    kind="bin",
                    name="probe",
                    path="crates/converter/src/bin/probe.rs",
                )
            ],
        )
        self.assertEqual(self.run_gate()[0], 1)

    def test_a_build_script_is_ignored(self):
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_sign_loss", 10, kind="custom-build")],
        )
        self.assertEqual(self.run_gate()[0], 0)

    def test_an_integration_test_target_is_ignored(self):
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    2,
                    kind="test",
                    path="crates/converter/tests/probe.rs",
                )
            ],
        )
        self.assertEqual(self.run_gate()[0], 0)

    def test_a_crate_outside_the_gate_is_ignored(self):
        write_log(
            self.log,
            [
                diagnostic(
                    "engine",
                    "clippy::cast_possible_truncation",
                    10,
                    path="crates/engine/src/lib.rs",
                )
            ],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0)
        self.assertIn("0 existing hit(s)", output)

    def test_the_same_span_under_lib_and_bin_is_one_hit(self):
        self.repo.append(
            "crates/converter/src/lib.rs",
            "pub fn probe(value: u32) -> u8 { value as u8 }\n",
        )
        self.repo.commit("add a lossy cast")
        record = diagnostic("converter", "clippy::cast_sign_loss", 2)
        other = diagnostic("converter", "clippy::cast_sign_loss", 2, kind="bin")
        write_log(self.log, [record, other])
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("1 new Clippy hit(s)", output)

    def test_a_warning_outside_the_four_lints_fails(self):
        write_log(
            self.log,
            [diagnostic("converter", "clippy::needless_return", 10, text="unneeded return")],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("outside the four gate lints", output)
        self.assertIn("converter: clippy::needless_return: unneeded return", output)

    def test_an_error_outside_the_four_lints_fails(self):
        write_log(
            self.log,
            [diagnostic("converter", None, 10, level="error", text="mismatched types")],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("no lint code", output)

    def test_a_repeated_warning_outside_the_four_lints_is_reported_once(self):
        record = diagnostic(
            "converter", "clippy::needless_return", 10, text="unneeded return"
        )
        write_log(self.log, [record, record])
        code, output = self.run_gate()
        self.assertEqual(code, 1)
        self.assertEqual(output.count("outside the four gate lints"), 1)
        self.assertEqual(output.count("converter: clippy::needless_return"), 1)

    def test_a_diagnostic_note_is_not_counted(self):
        write_log(
            self.log,
            [diagnostic("converter", None, 10, level="note", text="note")],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0)
        self.assertNotIn("outside the four gate lints", output)

    def test_an_incomplete_log_is_an_error(self):
        write_log(self.log, [], finish=False)
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("build-finished", output)

    def test_a_failed_build_is_an_error(self):
        write_log(self.log, [], success=False)
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("build-finished", output)

    def test_a_malformed_log_is_an_error(self):
        self.log.write_text("{not json}\n", encoding="utf-8")
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("not a JSON record", output)

    def test_a_log_line_that_is_not_an_object_is_an_error(self):
        self.log.write_text("[1, 2]\n", encoding="utf-8")
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("not a JSON object", output)

    def test_a_missing_log_is_an_error(self):
        self.log = self.repo.root / "missing.json"
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("cannot read", output)

    def test_a_log_without_a_gated_crate_is_an_error(self):
        write_log(self.log, [], artifacts=[artifact("shared")])
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("converter", output)
        self.assertIn("compiler-artifact", output)

    def test_a_log_without_a_converter_bin_is_an_error(self):
        write_log(self.log, [], artifacts=[artifact("converter"), artifact("shared")])
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("converter bin", output)
        self.assertIn("gated target", output)

    def test_a_log_without_a_converter_lib_is_an_error(self):
        write_log(
            self.log,
            [],
            artifacts=[artifact("converter", "bin"), artifact("shared")],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("converter lib", output)

    def test_a_log_without_a_shared_lib_is_an_error(self):
        write_log(
            self.log,
            [],
            artifacts=[artifact("converter"), artifact("converter", "bin")],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("shared lib", output)

    def test_a_gated_diagnostic_without_a_target_is_an_error(self):
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_sign_loss", 10, target=False)],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("no target kind", output)
        self.assertIn("converter", output)

    def test_a_gated_diagnostic_without_a_span_is_an_error(self):
        write_log(
            self.log,
            [diagnostic("converter", "clippy::cast_sign_loss", 10, spans=False)],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 2)
        self.assertIn("no span", output)

    def test_a_missing_base_is_a_usage_error(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            with self.assertRaises(SystemExit) as raised:
                ratchet.main(
                    ["--root", str(self.repo.root), "--clippy-json", str(self.log)]
                )
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("--base", err.getvalue())

    def test_an_unknown_base_is_an_error(self):
        write_log(self.log, [])
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchet.main(
                [
                    "--root",
                    str(self.repo.root),
                    "--base",
                    "not-a-commit",
                    "--clippy-json",
                    str(self.log),
                ]
            )
        self.assertEqual(code, 2)
        self.assertIn("cast gate:", err.getvalue())

    def test_the_diff_skips_an_unrelated_file(self):
        self.repo.append("crates/converter/src/lib.rs", "pub fn other() {}\n")
        self.repo.commit("change another file")
        write_log(
            self.log,
            [
                diagnostic(
                    "converter",
                    "clippy::cast_possible_truncation",
                    2,
                    path="crates/converter/src/other.rs",
                )
            ],
        )
        code, output = self.run_gate()
        self.assertEqual(code, 0)
        self.assertIn("1 existing hit(s)", output)

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
        script = workflow_step_script(workflow_job("clippy"), CLIPPY_STEP)
        match = re.search(r"lint_flags=\((.*?)\)", script, re.S)
        self.assertIsNotNone(match)
        workflow_flags = [
            line.strip() for line in match.group(1).splitlines() if line.strip()
        ]
        self.assertEqual(out.getvalue().strip(), " ".join(workflow_flags))
        self.assertEqual(len(ratchet.LINTS), 4)

    def test_the_workflow_does_not_deny_all_warnings(self):
        script = workflow_step_script(workflow_job("clippy"), CLIPPY_STEP)
        self.assertNotIn("-D warnings", script)
        self.assertNotIn("--force-warn", script)
        self.assertIn("--message-format=json", script)
        self.assertIn("--lint-flags", script)
        self.assertIn("--base", script)
        job = workflow_job("clippy")
        self.assertIn("fetch-depth: 0", job)

    def test_there_is_no_committed_count(self):
        self.assertFalse(SCRIPT.with_name("baseline.json").exists())


if __name__ == "__main__":
    unittest.main()
