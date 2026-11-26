import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import comment_names  # noqa: E402


class CommentNameTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        subprocess.check_call(["git", "-C", str(self.root), "init", "-q"])

    def write(self, files):
        for relative, text in files.items():
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

    def git(self, *args):
        subprocess.check_call(["git", "-C", str(self.root), "-c", "user.name=Tests",
                               "-c", "user.email=tests@example.invalid", *args])

    def commit(self, files):
        self.write(files)
        self.git("add", "-A")
        self.git("commit", "-qm", "change", "--allow-empty")

    def run_main(self, *args):
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            code = comment_names.main(["--root", str(self.root), *args])
        return code, output.getvalue()

    def run_check(self, head, base):
        self.commit({"crates/a/src/keep.rs": "\n", **base})
        self.git("branch", "base")
        self.commit(head)
        return self.run_main("--base", "base")

    def test_the_four_stale_names_of_2026_11_25_fail(self):
        head = {
            "crates/engine/src/shaders/grass.wgsl": "// (`grass_render.rs`, `grass_ambient`)\n"
                                                    "    // (`grass_sun_colour`)\n",
            "crates/engine/src/streaming.rs": "/// through [`spawn_resident_cell`].\n",
            "crates/converter/src/derive.rs": "/// What [`derive_into`] changed.\n",
        }
        code, output = self.run_check(head, {"crates/engine/src/lib.rs": "fn real_thing() {}\n"})
        self.assertEqual(code, 1, output)
        for name in ("grass_ambient", "grass_sun_colour", "spawn_resident_cell", "derive_into"):
            self.assertIn(f"`{name}` is not in the code", output)

    def test_an_edited_comment_line_fails(self):
        code, output = self.run_check({"crates/a/src/lib.rs": "/// Now calls `gone_helper`.\n"},
                                      {"crates/a/src/lib.rs": "/// Calls `gone_helper`.\n"})
        self.assertEqual(code, 1, output)
        self.assertIn("crates/a/src/lib.rs:1", output)

    def test_unchanged_and_moved_lines_pass_and_the_audit_lists_them(self):
        comment = "/// Calls `old_helper`.\n"
        code, output = self.run_check(
            {"crates/a/src/lib.rs": "fn new_helper() {}\n", "crates/a/src/other.rs": comment,
             "crates/a/src/same.rs": comment + "fn added() {}\n"},
            {"crates/a/src/lib.rs": comment + "fn old_helper() {}\n",
             "crates/a/src/same.rs": comment})
        self.assertEqual(code, 0, output)
        code, output = self.run_main("--audit")
        self.assertEqual(code, 0, output)
        self.assertIn("2 in the tree", output)

    def test_a_change_on_the_base_branch_is_not_this_change(self):
        self.commit({"crates/a/src/lib.rs": "\n"})
        self.git("branch", "feature")
        self.commit({"crates/a/src/lib.rs": "/// `from_main`\n"})
        self.git("branch", "base")
        self.git("checkout", "-q", "feature")
        self.commit({"crates/a/src/mine.rs": "fn mine() {}\n"})
        code, output = self.run_main("--base", "base")
        self.assertEqual(code, 0, output)

    def test_present_and_marked_names_pass(self):
        head = {
            "crates/a/src/lib.rs": (
                "/// `a::real_thing()`, `field_name` and `from_python`.\n"
                "// external-name: Bevy's render system\n"
                "/// Runs before `queue_shadows`.\n"
                '/// `plain`, `CamelCase_name`, queue_unquoted\n'
                'struct S { field_name: u32 }\nfn real_thing() {}\n'),
            "scripts/tool.py": "from_python = 1\n",
        }
        code, output = self.run_check(head, {})
        self.assertEqual(code, 0, output)

    def test_a_name_only_in_another_comment_does_not_count(self):
        code, output = self.run_check(
            {"crates/a/src/lib.rs": "/// `only_prose`\n", "scripts/t.py": "x = 1  # only_prose\n"},
            {})
        self.assertEqual(code, 1, output)

    def test_the_checks_own_test_strings_are_not_code(self):
        code, output = self.run_check(
            {"crates/a/src/lib.rs": "/// `quoted_in_a_test`\n",
             "tools/comment_names/test_x.py": 'NAME = "quoted_in_a_test"\n'}, {})
        self.assertEqual(code, 1, output)

    def test_a_marker_without_a_source_is_no_exemption(self):
        code, output = self.run_check(
            {"crates/a/src/lib.rs": "// external-name:\n/// `queue_shadows`\n"}, {})
        self.assertEqual(code, 1, output)

    def test_an_unknown_base_is_an_error(self):
        self.commit({"crates/a/src/lib.rs": "\n"})
        code, output = self.run_main("--base", "no-such-revision")
        self.assertEqual(code, 2, output)


if __name__ == "__main__":
    unittest.main()
