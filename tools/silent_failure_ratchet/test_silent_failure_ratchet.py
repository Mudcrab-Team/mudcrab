"""Tests for the silent-failure ratchet. They start no process but the script itself."""

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("silent_failure_ratchet.py")
SPEC = importlib.util.spec_from_file_location("silent_failure_ratchet", SCRIPT)
ratchet = importlib.util.module_from_spec(SPEC)
sys.modules["silent_failure_ratchet"] = ratchet
SPEC.loader.exec_module(ratchet)


def sites(source):
    """Sites of one production source text."""
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "lib.rs"
        path.write_text(textwrap.dedent(source), encoding="utf-8")
        return ratchet.sites_in_file(path)


def count(source):
    return len(sites(source))


class CountTests(unittest.TestCase):
    def test_counts_a_discarded_propagated_error(self):
        found = sites(
            """
            fn f() -> Result<(), Error> {
                let _ = g()?;
                let _: usize = h()?;
                Ok(())
            }
            """
        )
        self.assertEqual(
            [(label, line) for label, line, _ in found],
            [("discarded-try", 3), ("discarded-try", 4)],
        )

    def test_an_if_let_condition_is_not_a_discarded_value(self):
        self.assertEqual(
            count(
                """
                fn f() -> Result<(), Error> {
                    if let _ = g()? {}
                    Ok(())
                }
                """
            ),
            0,
        )

    def test_a_nested_discarded_let_counts_once(self):
        self.assertEqual(
            count(
                """
                fn f() -> Result<(), Error> {
                    let _ = { let _ = g()?; };
                    Ok(())
                }
                """
            ),
            1,
        )

    def test_a_question_in_a_closure_is_not_a_discarded_try(self):
        self.assertEqual(
            count(
                """
                fn f(items: Vec<u8>) {
                    let _ = items.iter().map(|x| h(x)?).count();
                    let _ = items.iter().map(|x| { h(x)? }).count();
                }
                """
            ),
            0,
        )

    def test_a_question_after_a_closure_is_a_discarded_try(self):
        self.assertEqual(
            count(
                """
                fn f(items: Vec<u8>) -> Result<(), Error> {
                    let _ = items.iter().map(|x| f(x)).collect::<Result<Vec<_>, _>>()?;
                    Ok(())
                }
                """
            ),
            1,
        )

    def test_counts_every_ok_conversion(self):
        self.assertEqual(
            count(
                """
                fn f() -> Option<u8> {
                    let a = g().ok();
                    let b = g().ok()?;
                    if let Some(c) = g().ok() { return Some(c); }
                    g().and_then(|value| value.parse().ok())
                }
                """
            ),
            4,
        )

    def test_counts_invented_values(self):
        self.assertEqual(
            count(
                """
                fn f() -> String {
                    let a = g().unwrap_or_default();
                    let b = g().unwrap_or("");
                    let c = g().unwrap_or_else(|_| String::new());
                    let d = g().map_err(|_| "");
                    format!("{a}{b}{c}{d}")
                }
                """
            ),
            4,
        )

    def test_counts_filter_map_dropping_failures(self):
        self.assertEqual(
            count(
                """
                fn f(xs: Vec<Result<u8, Error>>) {
                    let a: Vec<u8> = xs.iter().filter_map(Result::ok).copied().collect();
                    let b: Vec<u8> = xs.into_iter().filter_map(|item| item.ok()).collect();
                    drop((a, b));
                }
                """
            ),
            2,
        )

    def test_ignores_comments_and_strings(self):
        self.assertEqual(
            count(
                r'''
                // let _ = g()?;
                /* let _ = h()?; /* .ok() */ */
                fn f() -> &'static str {
                    let _ = '"';
                    let _ = "let _ = g()?; .ok()";
                    let _ = r#"let _ = g()?; .expect("x")"#;
                    "ok"
                }
                '''
            ),
            0,
        )

    def test_a_lifetime_does_not_open_a_char_literal(self):
        self.assertEqual(
            count(
                """
                fn f<'a>(x: &'a Option<u8>) -> u8 { let _ = x.ok()?; 1 }
                """
            ),
            1,
        )


class TestCodeTests(unittest.TestCase):
    def test_skips_a_cfg_test_module(self):
        self.assertEqual(
            count(
                """
                fn f() { let _ = g().ok(); }
                #[cfg(test)]
                mod tests {
                    fn t() { let _ = g()?; }
                }
                """
            ),
            1,
        )

    def test_skips_test_functions(self):
        self.assertEqual(
            count(
                """
                #[test]
                fn a() { let _ = g()?; }
                #[tokio::test(flavor = "current_thread")]
                async fn b() { let _ = g().ok(); }
                fn kept() { let _ = g().ok(); }
                """
            ),
            1,
        )


class MarkerTests(unittest.TestCase):
    def test_a_marker_on_the_same_line_is_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    let _ = g().ok(); // silent-ok: the cache may be absent
                }
                """
            ),
            0,
        )

    def test_a_marker_on_the_previous_line_is_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    // silent-ok: a missing optional tag is not an error
                    let _ = g().ok();
                }
                """
            ),
            0,
        )

    def test_a_block_comment_marker_is_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    let _ = g().ok(); /* silent-ok: best effort */
                }
                """
            ),
            0,
        )

    def test_a_marker_without_a_reason_is_not_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    let _ = g().ok(); // silent-ok:
                }
                """
            ),
            1,
        )

    def test_a_block_comment_marker_without_a_reason_is_not_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    let _ = g().ok(); /* silent-ok: */
                }
                """
            ),
            1,
        )

    def test_a_block_comment_marker_without_a_reason_on_the_previous_line_is_not_an_exception(
        self,
    ):
        self.assertEqual(
            count(
                """
                fn f() {
                    /* silent-ok: */
                    let _ = g().ok();
                }
                """
            ),
            1,
        )

    def test_a_marker_above_a_multi_line_statement_is_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    // silent-ok: the optional tag may be absent
                    let tag = record
                        .tag()
                        .ok();
                }
                """
            ),
            0,
        )

    def test_a_marker_above_a_multi_line_let_does_not_exempt_a_later_statement(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    // silent-ok: only the first lookup is optional
                    let first = record
                        .tag()
                        .ok();
                    let second = record.name().ok();
                }
                """
            ),
            1,
        )

    def test_a_marker_two_lines_above_is_not_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    // silent-ok: too far away
                    let x = 1;
                    let _ = g().ok();
                }
                """
            ),
            1,
        )

    def test_a_marker_inside_a_string_is_not_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    let note = "// silent-ok: this is only text";
                    let _ = g().ok();
                }
                """
            ),
            1,
        )

    def test_a_marker_word_that_ends_with_silent_ok_is_not_an_exception(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    let _ = g().ok(); // not-silent-ok: this is not the marker
                }
                """
            ),
            1,
        )

    def test_a_marker_exempts_only_its_own_site(self):
        self.assertEqual(
            count(
                """
                fn f() {
                    let _ = g().ok(); // silent-ok: absence is expected here
                    let x = 1;
                    let _ = h().ok();
                }
                """
            ),
            1,
        )


class CrateWalkTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(text))

    def test_skips_test_directories_and_test_modules(self):
        self.write(
            "crates/demo/src/lib.rs",
            """
            mod real;
            #[cfg(test)]
            mod demo_tests;
            mod tests;
            """,
        )
        self.write("crates/demo/src/real.rs", "fn a() { let _ = f().ok(); }\n")
        self.write("crates/demo/src/demo_tests.rs", "fn t() { let _ = f()?; }\n")
        self.write("crates/demo/src/tests.rs", "fn t() { let _ = f().ok(); }\n")
        self.write("crates/demo/tests/it.rs", "fn t() { let _ = f().ok(); }\n")
        self.write("crates/demo/benches/b.rs", "fn t() { let _ = f().ok(); }\n")
        self.assertEqual(ratchet.count_crate(self.root.resolve(), "demo"), 1)

    def test_skips_a_cfg_test_file_reached_through_a_path_attribute(self):
        self.write(
            "crates/demo/src/bin/tool.rs",
            '#[cfg(test)]\n#[path = "../tool_tests.rs"]\nmod tests;\n',
        )
        self.write("crates/demo/src/tool_tests.rs", "fn t() { let _ = f().ok(); }\n")
        self.assertEqual(ratchet.count_crate(self.root.resolve(), "demo"), 0)


class CommandTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)
        (self.root / "crates/demo/src").mkdir(parents=True)
        (self.root / "tools/silent_failure_ratchet").mkdir(parents=True)
        self.source = self.root / "crates/demo/src/lib.rs"
        self.source.write_text("fn f() { let _ = g().ok(); }\n")
        self.baseline = self.root / "tools/silent_failure_ratchet/baseline.json"
        self.baseline.write_text(json.dumps({"counts": {"demo": 1}}))

    def run_ratchet(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchet.main(["--root", str(self.root), *extra])
        return code, out.getvalue() + err.getvalue()

    def test_baseline_passes(self):
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_an_added_discarded_try_fails(self):
        self.source.write_text(
            "fn f() -> Result<(), Error> { let _ = g()?; Ok(()) }\n", encoding="utf-8"
        )
        self.baseline.write_text(json.dumps({"counts": {"demo": 0}}), encoding="utf-8")
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("RAISED", output)
        self.assertIn("silent failure ratchet", output)
        self.assertIn("silent-ok:", output)

    def test_a_marked_site_passes(self):
        self.source.write_text(
            "fn f() { let _ = g().ok(); // silent-ok: the cache may be absent\n}",
            encoding="utf-8",
        )
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_test_code_is_ignored(self):
        self.source.write_text(
            "fn f() {}\n#[cfg(test)]\nmod tests { fn t() { let _ = g()?; } }\n",
            encoding="utf-8",
        )
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_a_lower_count_passes_with_a_reminder_and_fails_when_strict(self):
        self.source.write_text("fn f() {}\n", encoding="utf-8")
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("Lower the baseline", output)
        self.assertEqual(self.run_ratchet("--strict")[0], 1)

    def test_update_writes_the_measured_count_and_raises_the_revision(self):
        self.source.write_text("fn f() {}\n", encoding="utf-8")
        self.assertEqual(self.run_ratchet("--update")[0], 0)
        document = json.loads(self.baseline.read_text())
        self.assertEqual(document["counts"]["demo"], 0)
        self.assertEqual(document["revision"], 1)
        self.assertEqual(self.run_ratchet("--strict")[0], 0)

    def test_a_crate_without_a_baseline_entry_passes_with_a_notice(self):
        self.baseline.write_text(json.dumps({"counts": {}}), encoding="utf-8")
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("no baseline entry", output)
        self.assertIn(" 1 ", output)
        self.assertEqual(json.loads(self.baseline.read_text()), {"counts": {}})

    def test_an_unknown_crate_is_a_usage_error(self):
        self.assertEqual(self.run_ratchet("--crate", "nope")[0], 2)

    def test_a_missing_baseline_is_a_usage_error(self):
        self.baseline.unlink()
        self.assertEqual(self.run_ratchet()[0], 2)

    def test_update_writes_lf_newlines(self):
        self.assertEqual(self.run_ratchet("--update")[0], 0)
        self.assertNotIn(b"\r", self.baseline.read_bytes())


class BaseCompareTests(unittest.TestCase):
    """`--base-root`: a pull request may not raise a count above the base commit's."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "head"
        self.base = Path(self.directory.name) / "base"
        for tree in (self.root, self.base):
            (tree / "crates/demo/src").mkdir(parents=True)
            (tree / "crates/demo/src/lib.rs").write_text("fn f() { let _ = g().ok(); }\n", encoding="utf-8")
            (tree / "tools/silent_failure_ratchet").mkdir(parents=True)
        self.set_baseline(self.root, 1)
        self.set_baseline(self.base, 1)

    def set_baseline(self, tree, value):
        (tree / "tools/silent_failure_ratchet/baseline.json").write_text(
            json.dumps({"counts": {"demo": value}}), encoding="utf-8"
        )

    def run_ratchet(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchet.main(["--root", str(self.root), "--base-root", str(self.base), *extra])
        return code, out.getvalue() + err.getvalue()

    def add(self, text):
        source = self.root / "crates/demo/src/lib.rs"
        source.write_text(source.read_text(encoding="utf-8") + text, encoding="utf-8")

    def test_the_same_tree_passes(self):
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_a_new_discarded_try_fails_against_the_base(self):
        self.add("fn h() -> Result<(), Error> { let _ = g()?; Ok(()) }\n")
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("RAISED", output)
        self.assertIn("above its base", output)
        self.assertIn("crates/demo/src/lib.rs:2: discarded-try:", output)

    def test_a_new_marked_site_passes(self):
        self.add("fn h() { let _ = g().ok(); // silent-ok: absence is expected\n}\n")
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_new_test_code_passes(self):
        self.add("#[cfg(test)]\nmod tests { fn t() { let _ = g()?; } }\n")
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_a_stale_baseline_neither_hides_a_rise_nor_fails_an_unrelated_pull_request(self):
        self.set_baseline(self.root, 99)
        self.set_baseline(self.base, 99)
        self.add("fn h() { let _ = g().ok(); }\n")
        self.assertEqual(self.run_ratchet()[0], 1)
        self.set_baseline(self.root, 0)
        self.set_baseline(self.base, 0)
        (self.root / "crates/demo/src/lib.rs").write_text("fn f() { let _ = g().ok(); }\n", encoding="utf-8")
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_a_changed_baseline_entry_accepts_the_added_sites(self):
        self.add("fn h() { let _ = g().ok(); }\n")
        self.assertEqual(ratchet.main(["--root", str(self.root), "--update"]), 0)
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_a_revision_change_does_not_approve_a_stale_entry_that_misses_the_count(self):
        self.set_baseline(self.root, 5)
        self.set_baseline(self.base, 5)
        (self.root / "tools/silent_failure_ratchet/baseline.json").write_text(
            json.dumps({"counts": {"demo": 5}, "revision": 1}), encoding="utf-8"
        )
        self.add("fn h() { let _ = g().ok(); }\n")
        self.assertEqual(self.run_ratchet()[0], 1)

    def test_a_base_root_without_crates_is_a_usage_error(self):
        self.base = Path(self.directory.name) / "missing"
        self.assertEqual(self.run_ratchet()[0], 2)

    def test_a_custom_baseline_is_read_with_a_base_root(self):
        relative = "tools/silent_failure_ratchet/other.json"
        (self.root / relative).write_text(
            json.dumps({"counts": {"demo": 2}}), encoding="utf-8"
        )
        (self.base / relative).write_text(
            json.dumps({"counts": {"demo": 1}}), encoding="utf-8"
        )
        self.add("fn h() { let _ = g().ok(); }\n")
        self.assertEqual(self.run_ratchet("--baseline", str(self.root / relative))[0], 0)

    def test_a_baseline_outside_the_root_is_a_usage_error_with_a_base_root(self):
        outside = Path(self.directory.name) / "outside.json"
        outside.write_text(json.dumps({"counts": {"demo": 1}}), encoding="utf-8")
        code, output = self.run_ratchet("--baseline", str(outside))
        self.assertEqual(code, 2)
        self.assertIn("--baseline must be inside --root", output)


if __name__ == "__main__":
    unittest.main()
