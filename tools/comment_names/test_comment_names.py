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
        self.directory = Path(temporary.name)

    def tree(self, name, files):
        root = self.directory / name
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        subprocess.check_call(["git", "-C", str(root), "init", "-q"])
        return root

    def run_check(self, head, base):
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            code = comment_names.main(["--root", str(self.tree("head", head)),
                                       "--base-root", str(self.tree("base", base))])
        return code, output.getvalue()

    def test_the_four_stale_names_of_2026_11_25_fail(self):
        base = {"crates/engine/src/lib.rs": "fn real_thing() {}\n"}
        head = {
            **base,
            "crates/engine/src/shaders/grass.wgsl": "// (`grass_render.rs`, `grass_ambient`)\n"
                                                    "    // (`grass_sun_colour`)\n",
            "crates/engine/src/streaming.rs": "/// through [`spawn_resident_cell`].\n",
            "crates/converter/src/derive.rs": "/// What [`derive_into`] changed.\n",
        }
        code, output = self.run_check(head, base)
        self.assertEqual(code, 1, output)
        for name in ("grass_ambient", "grass_sun_colour", "spawn_resident_cell", "derive_into"):
            self.assertIn(f"`{name}` is not in the code", output)

    def test_a_removed_symbol_fails_although_the_comment_did_not_change(self):
        comment = "/// Calls `old_helper`.\n"
        code, output = self.run_check(
            {"crates/a/src/lib.rs": comment + "fn new_helper() {}\n"},
            {"crates/a/src/lib.rs": comment + "fn old_helper() {}\n"})
        self.assertEqual(code, 1, output)
        self.assertIn("old_helper", output)

    def test_present_old_marked_and_moved_names_pass(self):
        base = {"crates/a/src/lib.rs": "/// `already_gone`\n"}
        head = {
            "crates/a/src/moved.rs": "/// `already_gone`\n",
            "crates/a/src/lib.rs": (
                "/// `a::real_thing()`, `field_name` and `from_python`.\n"
                "// external-name: Bevy's render system\n"
                "/// Runs before `queue_shadows`.\n"
                '/// `plain`, `CamelCase_name`, queue_unquoted\n'
                'struct S { field_name: u32 }\nfn real_thing() {}\n'),
            "scripts/tool.py": "from_python = 1\n",
        }
        code, output = self.run_check(head, base)
        self.assertEqual(code, 0, output)

    def test_a_name_only_in_another_comment_does_not_count(self):
        code, output = self.run_check(
            {"crates/a/src/lib.rs": "/// `only_prose`\n", "scripts/t.py": "x = 1  # only_prose\n"},
            {"crates/a/src/lib.rs": "\n"})
        self.assertEqual(code, 1, output)

    def test_a_marker_without_a_source_is_no_exemption(self):
        code, output = self.run_check(
            {"crates/a/src/lib.rs": "// external-name:\n/// `queue_shadows`\n"},
            {"crates/a/src/lib.rs": "\n"})
        self.assertEqual(code, 1, output)


if __name__ == "__main__":
    unittest.main()
