"""Tests for the production unwrap, expect and panic! ratchet. They start no process but the script itself."""

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("unwrap_ratchet.py")
SPEC = importlib.util.spec_from_file_location("unwrap_ratchet", SCRIPT)
ratchet = importlib.util.module_from_spec(SPEC)
sys.modules["unwrap_ratchet"] = ratchet
SPEC.loader.exec_module(ratchet)


def count(source):
    """Production count of one source text."""
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "lib.rs"
        path.write_text(textwrap.dedent(source), encoding="utf-8")
        return ratchet.count_file(path)


class CountTests(unittest.TestCase):
    def test_counts_the_three_forms(self):
        self.assertEqual(
            count(
                """
                fn f(x: Option<u8>) {
                    x.unwrap();
                    x.expect("why");
                    panic!("no");
                }
                """
            ),
            3,
        )

    def test_ignores_comments_and_strings(self):
        self.assertEqual(
            count(
                r'''
                // x.unwrap()
                /* y.expect("a") /* nested panic!("b") */ */
                fn f() -> &'static str {
                    let _ = '"';
                    let _ = "z.unwrap() \" still a string";
                    let _ = r#"w.expect("raw")"#;
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
                fn f<'a>(x: &'a Option<u8>) -> u8 { x.unwrap() }
                fn g<'a>(x: &'a Option<u8>) -> u8 { 1 }
                """
            ),
            1,
        )

    def test_skips_a_cfg_test_module(self):
        self.assertEqual(
            count(
                """
                fn f(x: Option<u8>) { x.unwrap(); }
                #[cfg(test)]
                mod tests {
                    fn t(x: Option<u8>) { x.unwrap(); x.unwrap(); }
                }
                """
            ),
            1,
        )

    def test_a_cfg_test_statement_does_not_hide_the_next_function(self):
        self.assertEqual(
            count(
                """
                #[cfg(test)]
                use std::collections::HashMap;
                #[cfg(test)]
                const TABLE: [u8; 2] = [1, 2];
                fn f(x: Option<u8>) { x.unwrap(); }
                #[cfg(test)]
                #[allow(dead_code)]
                fn helper(x: [u8; 4]) -> u8 { Some(x[0]).unwrap() }
                fn g(x: Option<u8>) { x.expect("kept"); }
                """
            ),
            2,
        )


    def test_skips_every_cfg_that_requires_test(self):
        self.assertEqual(
            count(
                """
                #[cfg(all(test, target_os = "macos"))]
                fn a(x: Option<u8>) { x.unwrap(); }
                #[cfg(all(unix, any(test, debug_assertions), test))]
                fn b(x: Option<u8>) { x.unwrap(); }
                #[cfg(any(test, feature = "x"))]
                fn c(x: Option<u8>) { x.unwrap(); }
                #[cfg(not(test))]
                fn d(x: Option<u8>) { x.unwrap(); }
                #[cfg(feature = "test")]
                fn e(x: Option<u8>) { x.unwrap(); }
                #[cfg(all(
                    test,
                    unix,
                ))]
                mod tests { fn t(x: Option<u8>) { x.unwrap(); } }
                """
            ),
            3,  # c (it also builds without `test`), d and e
        )

    def test_counts_calls_with_whitespace_or_a_comment_inside(self):
        self.assertEqual(
            count(
                """
                fn f(x: Option<u8>) {
                    x.unwrap(/* checked */);
                    x . unwrap ( );
                    x
                        .expect ("why");
                    panic ! ("no");
                }
                """
            ),
            4,
        )

    def test_skips_test_functions(self):
        self.assertEqual(
            count(
                """
                fn f(x: Option<u8>) { x.unwrap(); }
                #[test]
                fn a(x: Option<u8>) { x.unwrap(); }
                #[tokio::test(flavor = "current_thread")]
                async fn b(x: Option<u8>) { x.unwrap(); }
                #[test]
                #[should_panic]
                fn c() { panic!("expected"); }
                #[bench]
                fn d(x: Option<u8>) { x.unwrap(); }
                """
            ),
            1,
        )

    def test_a_cfg_test_field_or_variant_does_not_hide_the_next_item(self):
        self.assertEqual(
            count(
                """
                struct S {
                    #[cfg(test)]
                    marker: std::collections::HashMap<u8, u8>,
                    kept: u8,
                }
                enum E {
                    #[cfg(test)]
                    Probe(u8, u8),
                    Real,
                }
                fn f(x: Option<u8>) { x.unwrap(); }
                """
            ),
            1,
        )

    def test_a_cfg_test_statement_ends_at_its_semicolon(self):
        self.assertEqual(
            count(
                """
                fn f(a: bool, b: Option<u8>, c: Option<u8>) {
                    #[cfg(test)]
                    let x = if a { b.unwrap() } else { c.unwrap() };
                    b.unwrap();
                }
                """
            ),
            1,
        )

    def test_a_cfg_test_block_statement_covers_only_itself(self):
        for statement in (
            "#[cfg(test)] if c { x.unwrap(); }",
            "#[cfg(test)] if c { x.unwrap(); } else if d { x.unwrap(); } else { x.unwrap(); }",
            "#[cfg(test)] if let Some(v) = f(|| { 1 }) { x.unwrap(); }",
            "#[cfg(test)] match c { _ => x.unwrap() }",
            "#[cfg(test)] for _ in 0..2 { x.unwrap(); }",
            "#[cfg(test)] while c { x.unwrap(); }",
            "#[cfg(test)] loop { x.unwrap(); break; }",
            "#[cfg(test)] 'outer: loop { x.unwrap(); break 'outer; }",
            "#[cfg(test)] { x.unwrap(); }",
            "#[cfg(test)] unsafe { x.unwrap(); }",
        ):
            with self.subTest(statement=statement):
                self.assertEqual(
                    count(f"fn a(c: bool, d: bool, x: Option<u8>) {{ {statement} x.unwrap(); }}"),
                    1,
                )

    def test_a_cfg_test_macro_item_and_argument_end_where_they_end(self):
        self.assertEqual(
            count(
                """
                #[cfg(test)]
                thread_local! { static X: u8 = Some(1).unwrap(); }
                fn f(x: Option<u8>) { x.unwrap(); }
                fn g(x: Option<u8>) {
                    call(
                        #[cfg(test)]
                        probe,
                        x.unwrap(),
                    );
                }
                """
            ),
            2,
        )

    def test_a_nested_inner_cfg_test_attribute_hides_only_its_module(self):
        source = """
            fn f(x: Option<u8>) { x.unwrap(); }
            mod checks {
                #![cfg(test)]
                fn t(x: Option<u8>) { x.unwrap(); }
            }
            fn g(x: Option<u8>) { x.unwrap(); }
            """
        self.assertEqual(count(source), 2)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lib.rs"
            path.write_text(textwrap.dedent(source), encoding="utf-8")
            code = ratchet.mask(path.read_text(encoding="utf-8"))
            self.assertFalse(ratchet.inner_cfg_requires_test(code))

    def test_a_file_level_inner_cfg_test_attribute_hides_the_file(self):
        self.assertEqual(count("#![cfg(test)]\nfn t(x: Option<u8>) { x.unwrap(); }\n"), 0)


class RequiresTestTests(unittest.TestCase):
    def test_predicates(self):
        cases = {
            "test": True,
            "all(test, unix)": True,
            "all(unix, all(test))": True,
            "any(test, unix)": False,
            "any(test, test)": True,
            "not(test)": False,
            "unix": False,
            'feature = "test"': False,
            "target_os = \"macos\"": False,
        }
        for predicate, expected in cases.items():
            with self.subTest(predicate):
                self.assertEqual(ratchet.requires_test(predicate), expected)


class CrateWalkTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(text))

    def test_follows_nested_modules_and_skips_test_modules(self):
        self.write(
            "crates/demo/src/lib.rs",
            """
            mod outer;
            #[cfg(test)]
            mod outer_tests;
            """,
        )
        # `outer.rs` is not a `mod.rs`, so its child lives in `outer/inner.rs`.
        self.write("crates/demo/src/outer.rs", "mod inner;\nfn a(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/outer/inner.rs", "fn b(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/outer_tests.rs", "fn c(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 2)

    def test_skips_test_modules_in_separate_files_whatever_the_cfg_spelling(self):
        self.write(
            "crates/demo/src/lib.rs",
            """
            mod real;
            #[cfg(test)]
            mod tests;
            #[cfg(all(test, target_os = "macos"))]
            mod native_tests;
            #[cfg(all(test, unix))] #[allow(dead_code)] mod same_line;
            // A `tests.rs` with no cfg on its declaration is still test code.
            mod tests_by_name;
            """,
        )
        self.write("crates/demo/src/real.rs", "fn a(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/tests.rs", "mod nested;\nfn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/tests/nested.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/native_tests.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/same_line.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/tests_by_name.rs", "#![cfg(test)]\nfn t(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 1)

    def test_skips_files_under_tests_and_benches_directories(self):
        self.write(
            "crates/demo/src/lib.rs",
            '#[path = "tests/helpers.rs"]\nmod helpers;\n#[path = "benches/b.rs"]\nmod b;\nmod real;\n',
        )
        self.write("crates/demo/src/tests/helpers.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/benches/b.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/real.rs", "fn a(x: Option<u8>) { x.unwrap(); }\n")
        # Integration tests and benches outside `src` are never reached.
        self.write("crates/demo/tests/it.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/benches/b.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 1)

    def test_follows_path_attributes(self):
        self.write("crates/demo/src/lib.rs", '#[path = "elsewhere/real.rs"]\nmod fake;\n')
        self.write("crates/demo/src/elsewhere/real.rs", "fn a(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 1)


    def test_children_of_inline_modules_live_in_the_module_directories(self):
        self.write("crates/demo/src/lib.rs", "mod outer { mod mid { mod leaf; } mod side; }\nfn a(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/outer/mid/leaf.rs", "fn b(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/outer/side.rs", "fn c(x: Option<u8>) { x.unwrap(); }\n")
        # A file at the crate level with the child's name is not the child.
        self.write("crates/demo/src/leaf.rs", "fn d(x: Option<u8>) { x.unwrap(); x.unwrap(); x.unwrap(); x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 3)

    def test_an_inline_module_path_attribute_names_the_directory(self):
        self.write("crates/demo/src/lib.rs", '#[path = "elsewhere"]\nmod outer { mod inner; }\n')
        self.write("crates/demo/src/elsewhere/inner.rs", "fn a(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 1)

    def test_modules_declared_inside_a_test_only_module_are_skipped(self):
        self.write("crates/demo/src/lib.rs", "#[cfg(test)]\n#[path = \".\"]\nmod support { mod helper; }\nmod real;\n")
        self.write("crates/demo/src/helper.rs", "fn t(x: Option<u8>) { x.unwrap(); }\n")
        self.write("crates/demo/src/real.rs", "fn a(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 1)

    def test_a_nested_inner_cfg_test_attribute_keeps_the_file_and_its_children(self):
        self.write("crates/demo/src/lib.rs", "mod real;\nfn a(x: Option<u8>) { x.unwrap(); }\nmod checks { #![cfg(test)] fn t(x: Option<u8>) { x.unwrap(); } }\n")
        self.write("crates/demo/src/real.rs", "fn b(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 2)

    def test_a_path_outside_the_crate_does_not_stop_the_run(self):
        self.write("crates/demo/src/lib.rs", '#[path = "../../outside/shared.rs"]\nmod shared;\n')
        self.write("crates/outside/shared.rs", "fn a(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(ratchet.count_crate(self.root, "demo"), 1)


class CommandTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)
        (self.root / "crates/demo/src").mkdir(parents=True)
        (self.root / "tools/unwrap_ratchet").mkdir(parents=True)
        self.source = self.root / "crates/demo/src/lib.rs"
        self.source.write_text("fn f(x: Option<u8>) { x.unwrap(); }\n")
        (self.root / "tools/unwrap_ratchet/baseline.json").write_text(
            json.dumps({"counts": {"demo": 1}})
        )

    def run_ratchet(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchet.main(["--root", str(self.root), *extra])
        return code, out.getvalue() + err.getvalue()

    def test_baseline_passes(self):
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_an_added_production_unwrap_fails(self):
        self.source.write_text(self.source.read_text() + "fn g(x: Option<u8>) { x.unwrap(); }\n")
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("RAISED", output)

    def test_an_added_test_unwrap_passes(self):
        self.source.write_text(
            self.source.read_text()
            + "#[cfg(test)]\nmod tests { fn t(x: Option<u8>) { x.unwrap(); } }\n"
        )
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_a_lower_count_passes_with_a_reminder_and_fails_when_strict(self):
        self.source.write_text("fn f() {}\n")
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("Lower the baseline", output)
        self.assertEqual(self.run_ratchet("--strict")[0], 1)

    def test_update_writes_the_measured_count(self):
        self.source.write_text("fn f() {}\n")
        self.assertEqual(self.run_ratchet("--update")[0], 0)
        baseline = json.loads((self.root / "tools/unwrap_ratchet/baseline.json").read_text())
        self.assertEqual(baseline["counts"]["demo"], 0)
        self.assertEqual(self.run_ratchet("--strict")[0], 0)

    def test_a_crate_without_a_baseline_entry_passes_with_a_notice(self):
        (self.root / "tools/unwrap_ratchet/baseline.json").write_text(json.dumps({"counts": {}}))
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("no baseline entry", output)
        self.assertIn("demo", output)
        self.assertIn(" 1 ", output)  # the measured count is shown
        # The baseline file is not touched by a check.
        counts = json.loads((self.root / "tools/unwrap_ratchet/baseline.json").read_text())
        self.assertEqual(counts, {"counts": {}})

    def test_a_new_crate_does_not_hide_a_raised_one(self):
        (self.root / "crates/other/src").mkdir(parents=True)
        (self.root / "crates/other/src/lib.rs").write_text("fn f() {}\n")
        self.source.write_text(self.source.read_text() + "fn g(x: Option<u8>) { x.unwrap(); }\n")
        self.assertEqual(self.run_ratchet()[0], 1)

    def test_an_unknown_crate_is_a_usage_error(self):
        self.assertEqual(self.run_ratchet("--crate", "nope")[0], 2)

    def test_update_writes_lf_newlines(self):
        self.assertEqual(self.run_ratchet("--update")[0], 0)
        self.assertNotIn(b"\r", (self.root / "tools/unwrap_ratchet/baseline.json").read_bytes())


class BaseCompareTests(unittest.TestCase):
    """`--base-root`: a pull request may not raise a count above the base commit's."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "head"
        self.base = Path(self.directory.name) / "base"
        for tree in (self.root, self.base):
            (tree / "crates/demo/src").mkdir(parents=True)
            (tree / "crates/demo/src/lib.rs").write_text("fn f(x: Option<u8>) { x.unwrap(); }\n", encoding="utf-8")
            (tree / "tools/unwrap_ratchet").mkdir(parents=True)
        self.set_baseline(self.root, 1)
        self.set_baseline(self.base, 1)

    def set_baseline(self, tree, value):
        (tree / "tools/unwrap_ratchet/baseline.json").write_text(json.dumps({"counts": {"demo": value}}), encoding="utf-8")

    def run_ratchet(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchet.main(["--root", str(self.root), "--base-root", str(self.base), *extra])
        return code, out.getvalue() + err.getvalue()

    def add_unwrap(self):
        source = self.root / "crates/demo/src/lib.rs"
        source.write_text(source.read_text(encoding="utf-8") + "fn g(x: Option<u8>) { x.unwrap(); }\n", encoding="utf-8")

    def test_the_same_tree_passes(self):
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_an_added_unwrap_fails_against_the_base(self):
        self.add_unwrap()
        code, output = self.run_ratchet()
        self.assertEqual(code, 1)
        self.assertIn("RAISED", output)
        self.assertIn("above its base", output)

    def test_a_stale_baseline_neither_hides_a_rise_nor_fails_an_unrelated_pull_request(self):
        # Both trees carry a baseline that is far too high, and a far too low one.
        self.set_baseline(self.root, 99)
        self.set_baseline(self.base, 99)
        self.add_unwrap()
        self.assertEqual(self.run_ratchet()[0], 1)
        self.set_baseline(self.root, 0)
        self.set_baseline(self.base, 0)
        (self.root / "crates/demo/src/lib.rs").write_text("fn f(x: Option<u8>) { x.unwrap(); }\n", encoding="utf-8")
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_a_changed_baseline_entry_accepts_the_added_sites(self):
        self.add_unwrap()
        self.assertEqual(
            ratchet.main(["--root", str(self.root), "--update"]),
            0,
        )
        self.assertEqual(self.run_ratchet()[0], 0)

    def test_update_approves_a_rise_that_reaches_a_stale_entry(self):
        # The entry is stale (2) while the real count at the base is 1. A justified rise makes
        # the count 2, so `--update` writes the same number. The revision makes it visible.
        self.set_baseline(self.root, 2)
        self.set_baseline(self.base, 2)
        self.add_unwrap()
        self.assertEqual(self.run_ratchet()[0], 1)
        self.assertEqual(ratchet.main(["--root", str(self.root), "--update"]), 0)
        document = json.loads((self.root / "tools/unwrap_ratchet/baseline.json").read_text())
        self.assertEqual(document["counts"]["demo"], 2)
        self.assertEqual(document["revision"], 1)
        self.assertEqual(self.run_ratchet()[0], 0)
        # More sites after the update are not covered.
        self.add_unwrap()
        self.assertEqual(self.run_ratchet()[0], 1)

    def test_a_revision_change_does_not_approve_a_stale_entry_that_misses_the_count(self):
        # Entry 5 is stale and high. A raised revision with a different measured count must not
        # turn it into slack.
        self.set_baseline(self.root, 5)
        self.set_baseline(self.base, 5)
        (self.root / "tools/unwrap_ratchet/baseline.json").write_text(
            json.dumps({"counts": {"demo": 5}, "revision": 1}), encoding="utf-8"
        )
        self.add_unwrap()
        self.assertEqual(self.run_ratchet()[0], 1)

    def test_update_raises_the_revision_every_time(self):
        for expected in (1, 2):
            self.assertEqual(ratchet.main(["--root", str(self.root), "--update"]), 0)
            document = json.loads((self.root / "tools/unwrap_ratchet/baseline.json").read_text())
            self.assertEqual(document["revision"], expected)

    def test_a_lower_count_passes_without_a_reminder(self):
        (self.root / "crates/demo/src/lib.rs").write_text("fn f() {}\n", encoding="utf-8")
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("below base", output)
        self.assertNotIn("Lower the baseline", output)

    def test_a_crate_the_base_lacks_passes_with_a_notice(self):
        (self.root / "crates/fresh/src").mkdir(parents=True)
        (self.root / "crates/fresh/src/lib.rs").write_text("fn f(x: Option<u8>) { x.unwrap(); }\n", encoding="utf-8")
        code, output = self.run_ratchet()
        self.assertEqual(code, 0)
        self.assertIn("new crate", output)

    def test_a_base_root_without_crates_is_a_usage_error(self):
        self.base = Path(self.directory.name) / "missing"
        self.assertEqual(self.run_ratchet()[0], 2)


if __name__ == "__main__":
    unittest.main()
