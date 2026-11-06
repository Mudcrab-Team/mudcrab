"""Exercise the documentation link checker, including the live tree."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
CHECKER = "scripts/check-docs-links.py"


def load_checker():
    spec = importlib.util.spec_from_file_location("docs_link_checker", ROOT / CHECKER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class DocsLinkTests(unittest.TestCase):
    def setUp(self):
        self.checker = load_checker()
        self.directory = tempfile.TemporaryDirectory(prefix="mudcrab-docs-links-")
        self.root = Path(self.directory.name)
        (self.root / "docs").mkdir()

    def tearDown(self):
        self.directory.cleanup()

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def problems(self, allowed=None):
        return self.checker.check(self.root, self.checker.tracked_documents(self.root)
                                  if (self.root / ".git").is_dir() else
                                  sorted(str(p.relative_to(self.root))
                                         for p in self.root.rglob("*.md")),
                                  allowed or {})

    def test_missing_relative_target_is_reported(self):
        self.write("docs/page.md", "See [the spec](missing.md).\n")
        checked, problems, permitted = self.problems()
        self.assertEqual(checked, 1)
        self.assertEqual([(row[0], row[1], row[2], row[3]) for row in problems],
                         [("docs/page.md", 1, "missing.md", "no such file")])
        self.assertEqual(permitted, [])

    def test_existing_target_with_anchor_resolves(self):
        self.write("docs/page.md", "See [the spec](other.md#purpose).\n")
        self.write("docs/other.md", "# Purpose\n")
        checked, problems, permitted = self.problems()
        self.assertEqual((checked, problems, permitted), (1, [], []))

    def test_external_anchor_and_filesystem_targets_are_ignored(self):
        self.write("docs/page.md", "\n".join([
            "[web](https://example.com/x) [mail](mailto:a@example.com)",
            "[here](#purpose) [local](/Users/someone/file.json) [rel](//host/path)",
            "",
        ]))
        checked, problems, permitted = self.problems()
        self.assertEqual((checked, problems, permitted), (0, [], []))

    def test_code_blocks_and_inline_code_are_ignored(self):
        self.write("docs/page.md", "\n".join([
            "Inline `[x](missing-inline.md)` is an example.",
            "",
            "```sh",
            "[y](missing-fenced.md)",
            "```",
            "",
            "~~~",
            "[z](missing-tilde.md)",
            "~~~",
            "",
            "[real](existing.md)",
            "",
        ]))
        self.write("docs/existing.md", "# Present\n")
        checked, problems, permitted = self.problems()
        self.assertEqual((checked, problems), (1, []))

    def test_allowlisted_target_passes_with_its_reason(self):
        self.write("docs/page.md", "See [snapshot](missing.md).\n")
        allowed = {("docs/page.md", "missing.md"): "byte-pinned copy"}
        checked, problems, permitted = self.problems(allowed)
        self.assertEqual(problems, [])
        self.assertEqual([(row[0], row[1], row[2]) for row in permitted],
                         [("docs/page.md", 1, "missing.md")])
        self.assertEqual(permitted[0][3], "byte-pinned copy")

    def test_target_outside_the_repository_is_rejected(self):
        self.write("docs/page.md", "See [parent](../../outside.md).\n")
        checked, problems, permitted = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertEqual(problems[0][3], "escapes the repository")

    def test_reference_definitions_are_checked(self):
        self.write("docs/page.md", "[note]: missing.md\n\nSee [note].\n")
        checked, problems, permitted = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertEqual(problems[0][2], "missing.md")

    def test_percent_escaped_targets_resolve(self):
        self.write("docs/page.md", "See [space](my%20file.md).\n")
        self.write("docs/my file.md", "# Present\n")
        checked, problems, permitted = self.problems()
        self.assertEqual((checked, problems), (1, []))

    def test_this_repository_has_no_unlisted_broken_links(self):
        completed = subprocess.run([sys.executable, str(ROOT / CHECKER)], cwd=ROOT,
                                   capture_output=True, text=True)
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        self.assertIn("0 broken relative links", completed.stdout)

    def test_allowlist_entries_carry_a_reason(self):
        for (document, target), reason in self.checker.ALLOWED.items():
            self.assertTrue(document.endswith(".md"), document)
            self.assertTrue(target, document)
            self.assertGreaterEqual(len(reason), 20, (document, target, reason))


if __name__ == "__main__":
    unittest.main()
