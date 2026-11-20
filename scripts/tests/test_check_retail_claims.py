"""Regressions for the retail-claim source gate; builds throwaway repositories."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location(
    "check_retail_claims",
    Path(__file__).parents[1] / "check_retail_claims.py",
)
CHECK = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECK
SPEC.loader.exec_module(CHECK)


def run_git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        text=True,
        capture_output=True,
        check=True,
    )


class Repository:
    def __init__(self, root):
        self.root = Path(root)
        run_git(self.root, "init", "-q")
        run_git(self.root, "config", "user.email", "test@example.invalid")
        run_git(self.root, "config", "user.name", "Retail Claim Test")
        self.write("README.md", "fixture\n")
        self.commit("base")
        self.base = run_git(self.root, "rev-parse", "HEAD").stdout.strip()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message):
        run_git(self.root, "add", "-A")
        run_git(self.root, "commit", "-q", "-m", message)

    def run(self, *extra):
        return subprocess.run(
            [sys.executable, "-B", str(Path(CHECK.__file__)), "--repo", str(self.root),
             "--base", self.base, *extra],
            text=True,
            capture_output=True,
            check=False,
        )


class ClaimTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Repository(self.temporary.name)

    def check(self, result):
        return result.returncode, result.stdout, result.stderr

    def test_claim_with_registry_id_passes(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nRetail does X (RF-EX-001).\n")
        self.repo.commit("claim with source")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("all labelled", out)

    def test_claim_with_unverified_passes(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nRetail does X. UNVERIFIED.\n")
        self.repo.commit("claim marked unverified")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("all labelled", out)

    def test_claim_with_research_link_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nRetail does X. See [notes](../research/example.md).\n",
        )
        self.repo.commit("claim with research link")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_bare_claim_fails_with_file_and_line(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nRetail does X.\n")
        self.repo.commit("bare claim")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("docs/specs/example.md:3", err)
        self.assertIn("retail claim has no source", err)

    def test_old_unchanged_claim_is_ignored(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nRetail does X.\n")
        self.repo.commit("old bare claim")
        self.repo.base = run_git(self.repo.root, "rev-parse", "HEAD").stdout.strip()
        self.repo.write("docs/specs/example.md", "# Example\n\nRetail does X.\n\nMore text.\n")
        self.repo.commit("unrelated doc change")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_source_in_same_comment_block_passes(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "// Retail does X.\n// Source: RF-ENGINE-001.\nfn main() {}\n",
        )
        self.repo.commit("comment block with source")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_source_outside_the_comment_block_fails(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "// Retail does X.\n\nfn main() {}\n\n// RF-ENGINE-001.\n",
        )
        self.repo.commit("source in another block")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("crates/engine/src/lib.rs:1", err)

    def test_trailing_comment_is_checked(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "fn main() {} // Retail does X.\n",
        )
        self.repo.commit("trailing claim")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("crates/engine/src/lib.rs:1", err)

    def test_wgsl_comment_is_checked(self):
        self.repo.write(
            "crates/engine/shaders/example.wgsl",
            "// Retail does X.\nfn main() {}\n",
        )
        self.repo.commit("wgsl claim")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("crates/engine/shaders/example.wgsl:1", err)

    def test_string_literal_is_ignored(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            'pub const NOTE: &str = "Retail does X.";\n',
        )
        self.repo.commit("string literal only")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_multiline_raw_string_is_ignored(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            'const NOTE: &str = r#"\nRetail does X.\n"#;\nfn main() {}\n',
        )
        self.repo.commit("raw string only")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_block_comment_is_ignored(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "/*\n// Retail does X.\n*/\nfn main() {}\n",
        )
        self.repo.commit("block comment only")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_line_comment_after_block_comment_is_checked(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "/* note */ fn main() {} // Retail does X.\n",
        )
        self.repo.commit("comment after a block comment")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("crates/engine/src/lib.rs:1", err)

    def test_hyphenated_retail_compound_is_not_a_claim(self):
        self.repo.write(
            "docs/contributing/storage.md",
            "# Storage\n\n`reference/` holds third-party retail-parity research.\n",
        )
        self.repo.commit("retail compound")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_vendored_rust_is_ignored(self):
        self.repo.write(
            "vendor/example/src/lib.rs",
            "// Retail does X.\nfn main() {}\n",
        )
        self.repo.commit("vendored comment")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_deleted_file_hunk_is_ignored(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nRetail does X.\n")
        self.repo.commit("file to delete")
        self.repo.base = run_git(self.repo.root, "rev-parse", "HEAD").stdout.strip()
        (self.repo.root / "docs/specs/example.md").unlink()
        self.repo.commit("delete file")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_markdown_fenced_code_is_ignored(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\n```text\nRetail does X.\n```\n",
        )
        self.repo.commit("claim in a code fence")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_unreasoned_not_retail_note_fails(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nThis is not retail behaviour.\n")
        self.repo.commit("bare non-retail note")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("gives no reason", err)

    def test_reasoned_not_retail_note_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nThis is Mudcrab-only because retail has no such setting.\n",
        )
        self.repo.commit("reasoned non-retail note")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_bare_mudcrab_only_note_fails(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nThis path is Mudcrab-only.\n",
        )
        self.repo.commit("bare Mudcrab-only note")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("gives no reason", err)

    def test_reasoned_mudcrab_only_note_without_retail_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nThis path is Mudcrab-only because the engine has no save menu.\n",
        )
        self.repo.commit("reasoned Mudcrab-only note")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_reasoned_difference_from_retail_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nMudcrab clamps the step; this differs from retail since the "
            "engine has no havok.\n",
        )
        self.repo.commit("reasoned difference")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_reasoned_non_retail_note_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nThis non-retail path exists because the engine has no save menu.\n",
        )
        self.repo.commit("reasoned non-retail note")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_unreasoned_difference_from_retail_fails(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nThis differs from retail.\n")
        self.repo.commit("bare difference")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)

    def test_missing_base_fails_closed(self):
        result = subprocess.run(
            [sys.executable, "-B", str(Path(CHECK.__file__)), "--repo",
             str(self.repo.root), "--base", "does-not-exist"],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("retail claim check", result.stderr)


class BlockTests(unittest.TestCase):
    def test_markdown_heading_joins_its_first_paragraph(self):
        blocks, mapping = CHECK.build_markdown_blocks(
            ["## Retail behaviour", "", "It does X (RF-EX-001).", "More text."]
        )
        self.assertEqual(mapping[1], mapping[3])
        self.assertNotIn(2, mapping)
        self.assertIn("RF-EX-001", blocks[mapping[3]])

    def test_markdown_list_items_are_separate_blocks(self):
        _, mapping = CHECK.build_markdown_blocks(
            ["- Retail does X.", "- Source: RF-EX-001."]
        )
        self.assertNotEqual(mapping[1], mapping[2])

    def test_markdown_table_rows_are_separate_blocks(self):
        _, mapping = CHECK.build_markdown_blocks(
            ["| Claim | Source |", "| --- | --- |", "| Retail does X. | RF-EX-001 |"]
        )
        self.assertNotEqual(mapping[1], mapping[2])
        self.assertNotEqual(mapping[1], mapping[3])
        self.assertNotEqual(mapping[2], mapping[3])

    def test_markdown_blockquote_blank_line_is_a_boundary(self):
        _, mapping = CHECK.build_markdown_blocks(
            ["> Retail does X.", ">", "> RF-EX-001."]
        )
        self.assertNotEqual(mapping[1], mapping[3])


if __name__ == "__main__":
    unittest.main()
