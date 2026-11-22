"""Regressions for the retail-claim source gate; builds throwaway repositories."""

import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


SPEC = importlib.util.spec_from_file_location(
    "check_retail_claims",
    Path(__file__).parents[1] / "check_retail_claims.py",
)
CHECK = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECK
SPEC.loader.exec_module(CHECK)

WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/run_tests.yml"


def run_git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        text=True,
        capture_output=True,
        check=True,
    )


def retail_claim_step_script():
    """Return the shell block of the retail-claim workflow step."""
    workflow = WORKFLOW.read_text(encoding="utf-8")
    start = workflow.index("run: |", workflow.index("name: Check retail claims cite a source"))
    start = workflow.index("\n", start) + 1
    end = workflow.index("\n      - name:", start)
    return textwrap.dedent(workflow[start:end])


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

    def mark_base(self):
        self.base = run_git(self.root, "rev-parse", "HEAD").stdout.strip()

    def run(self, *extra, env=None):
        return subprocess.run(
            [sys.executable, "-B", str(Path(CHECK.__file__)), "--repo", str(self.root),
             "--base", self.base, *extra],
            text=True,
            capture_output=True,
            check=False,
            env=dict(os.environ, **(env or {})),
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
        self.repo.mark_base()
        self.repo.write("docs/specs/example.md", "# Example\n\nRetail does X.\n\nMore text.\n")
        self.repo.commit("unrelated doc change")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_pure_rename_checks_no_line(self):
        self.repo.write(
            "crates/example/src/lib.rs",
            "// Retail does X.\nfn main() {}\n",
        )
        self.repo.commit("claim in the old path")
        self.repo.mark_base()
        run_git(self.repo.root, "mv", "crates/example/src/lib.rs",
                "crates/example/src/lib2.rs")
        self.repo.commit("pure rename")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_pure_copy_checks_no_line(self):
        self.repo.write(
            "crates/example/src/lib.rs",
            "// Retail does X.\nfn main() {}\n",
        )
        self.repo.commit("claim in the source path")
        self.repo.mark_base()
        self.repo.write(
            "crates/example/src/lib_copy.rs",
            "// Retail does X.\nfn main() {}\n",
        )
        self.repo.commit("pure copy")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_moved_claim_is_not_a_new_claim(self):
        self.repo.write(
            "crates/example/src/lib.rs",
            "fn a() {}\n// Retail does X.\nfn b() {}\nfn c() {}\nfn d() {}\n",
        )
        self.repo.commit("claim in the first position")
        self.repo.mark_base()
        self.repo.write(
            "crates/example/src/lib.rs",
            "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n// Retail does X.\n",
        )
        self.repo.commit("move the claim to the last position")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 claim line(s)", out)

    def test_moved_claim_with_a_second_copy_is_checked(self):
        self.repo.write(
            "crates/example/src/lib.rs",
            "fn a() {}\n// Retail does X.\nfn b() {}\n",
        )
        self.repo.commit("one claim")
        self.repo.mark_base()
        self.repo.write(
            "crates/example/src/lib.rs",
            "fn a() {}\nfn b() {}\n// Retail does X.\n// Retail does X.\n",
        )
        self.repo.commit("move and duplicate the claim")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("crates/example/src/lib.rs:4", err)

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

    def test_module_doc_with_registry_id_labels_the_file(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "//! The engine module (RF-ENGINE-001).\n\n"
            "fn main() {}\n\n"
            "/// Retail profiles hold zero speeds.\n"
            "fn load() {}\n",
        )
        self.repo.commit("file labelled by its module doc")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_module_doc_without_registry_id_does_not_label_the_file(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "//! The engine module.\n\n"
            "fn main() {}\n\n"
            "/// Retail profiles hold zero speeds.\n"
            "fn load() {}\n",
        )
        self.repo.commit("module doc without a source")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("crates/engine/src/lib.rs:5", err)

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
            'const NOTE: &str = r#"\n// Retail does X.\n"#;\nfn main() {}\n',
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
        self.repo.mark_base()
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

    def test_source_in_the_same_section_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\n## Evidence\n\n"
            "- [DoubleYou's investigation](https://example.invalid/defaults)\n\n"
            "In retail, the setting defaults to 3.0.\n",
        )
        self.repo.commit("claim and source in one section")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_source_in_a_different_section_fails(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\n## Evidence\n\n"
            "[notes](notes.md)\n\n"
            "## Claim\n\n"
            "In retail, the setting defaults to 3.0.\n",
        )
        self.repo.commit("source in another section")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("docs/specs/example.md:9", err)

    def test_repository_markdown_link_outside_research_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nIn retail, the setting defaults to 3.0. "
            "See [the record map](../player-movement-record-map.md).\n",
        )
        self.repo.commit("claim with a repository Markdown link")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_external_url_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nIn retail, the setting defaults to 3.0. "
            "See https://example.invalid/defaults.\n",
        )
        self.repo.commit("claim with an external URL")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_sha256_in_the_same_section_passes(self):
        digest = "a" * 64
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\n## Claim\n\n"
            f"Evidence: static analysis of game.exe (SHA-256 `{digest}`).\n\n"
            "In retail, the setting defaults to 3.0.\n",
        )
        self.repo.commit("claim with a binary hash")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_unreasoned_not_retail_note_fails(self):
        self.repo.write("docs/specs/example.md", "# Example\n\nThis is not retail behaviour.\n")
        self.repo.commit("bare non-retail note")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("gives no reason", err)
        self.assertIn("also needs a reason", err)
        self.assertIn("because", err)

    def test_reasoned_not_retail_note_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nThis is Mudcrab-only because retail has no such setting.\n",
        )
        self.repo.commit("reasoned non-retail note")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_reasoned_not_vanilla_note_passes(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\nThis is not vanilla because Mudcrab needs deterministic replay.\n",
        )
        self.repo.commit("reasoned not-vanilla note")
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


class StandInTests(unittest.TestCase):
    """The stand-in marker gate from section 6 of the stand-in audit."""

    MARKER = ("// STAND-IN(#900, @alice): terrain LOD mesh built from "
              "LAND heights\n")

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Repository(self.temporary.name)

    def check(self, result):
        return result.returncode, result.stdout, result.stderr

    def test_placeholder_comment_without_a_marker_fails(self):
        self.repo.write("crates/engine/src/x.rs", "// placeholder colour\nfn main() {}\n")
        self.repo.commit("bare stand-in")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("crates/engine/src/x.rs:1", err)
        self.assertIn("stand-in has no label", err)

    def test_marker_labels_a_trigger_in_the_same_block(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            self.MARKER + "// placeholder colour\nfn main() {}\n",
        )
        self.repo.commit("labelled stand-in")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("all labelled", out)

    def test_marker_without_an_owner_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// STAND-IN(#900): terrain LOD mesh built from heights\n"
            "// placeholder colour\nfn main() {}\n",
        )
        self.repo.commit("marker without an owner")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_marker_without_an_issue_fails(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// STAND-IN(@alice): terrain LOD mesh built from heights\n",
        )
        self.repo.commit("marker without an issue")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("needs an issue", err)

    def test_marker_with_an_optional_owner_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// STAND-IN(#900, @alice): terrain LOD mesh built from heights\n"
            "// placeholder colour\nfn main() {}\n",
        )
        self.repo.commit("marker with an owner")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_marker_with_a_full_issue_url_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// STAND-IN(https://github.com/Mudcrab-Team/mudcrab/issues/900, "
            "@alice): terrain LOD mesh built from LAND heights\n"
            "// placeholder colour\nfn main() {}\n",
        )
        self.repo.commit("marker with a URL")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_marker_with_too_few_words_fails(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// STAND-IN(#900): short text\n",
        )
        self.repo.commit("short marker")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("at least four words", err)

    def test_marker_with_no_retail_data_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// STAND-IN(#900): no retail data\n"
            "// placeholder colour\nfn main() {}\n",
        )
        self.repo.commit("marker for data retail never ships")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_lowercase_stand_in_prose_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// The LOD tile stands in for a gap in the terrain.\n"
            "// A stand-in shader validates the pipeline.\n"
            "fn main() {}\n",
        )
        self.repo.commit("prose about stand-ins")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 stand-in line(s)", out)

    def test_negated_trigger_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// No placeholder is used.\n"
            "// Nothing is invented and no provisional value remains.\n"
            "fn main() {}\n",
        )
        self.repo.commit("negated triggers")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 stand-in line(s)", out)

    def test_test_code_is_out_of_scope(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "fn main() {}\n\n#[cfg(test)]\nmod tests {\n"
            "    // placeholder colour\n}\n",
        )
        self.repo.write(
            "crates/engine/src/foo_tests.rs",
            "// placeholder colour\n",
        )
        self.repo.commit("trigger in test code")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 stand-in line(s)", out)

    def test_stand_in_identifier_without_a_comment_fails(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "pub const GENERATED_WHITE: [u8; 4] = [255, 255, 255, 255];\n",
        )
        self.repo.commit("bare stand-in identifier")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("stand-in identifier has no label", err)

    def test_generated_family_identifier_fails(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "const GENERATED_HEIGHT: f32 = 1.0;\n",
        )
        self.repo.commit("generated height stand-in")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("GENERATED_HEIGHT", err)
        self.assertIn("stand-in identifier has no label", err)

    def test_audit_lists_generated_family_identifier(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "const GENERATED_HEIGHT: f32 = 1.0;\n",
        )
        self.repo.commit("generated height stand-in")
        code, out, err = self.check(self.repo.run("--audit"))
        self.assertEqual(code, 0, err)
        self.assertIn("crates/engine/src/x.rs:1", out)
        self.assertIn("GENERATED_HEIGHT", out)
        self.assertIn("stand-in identifier has no label", out)

    def test_declaration_with_a_trailing_comment_fails_without_marker(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "pub const GENERATED_WHITE: [u8; 4] = [255; 4]; // RGBA bytes\n",
        )
        self.repo.commit("stand-in declaration with a trailing comment")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("stand-in identifier has no label", err)

    def test_declaration_with_a_trailing_comment_and_marker_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// STAND-IN(#900): generated white bytes for a missing texture\n"
            "pub const GENERATED_WHITE: [u8; 4] = [255; 4]; // RGBA bytes\n",
        )
        self.repo.commit("labelled stand-in declaration with a trailing comment")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_audit_finds_a_declaration_with_a_trailing_comment(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "pub const GENERATED_WHITE: [u8; 4] = [255; 4]; // RGBA bytes\n",
        )
        self.repo.commit("stand-in declaration with a trailing comment")
        code, out, err = self.check(self.repo.run("--audit"))
        self.assertEqual(code, 0, err)
        self.assertIn("crates/engine/src/x.rs:1", out)
        self.assertIn("stand-in identifier has no label", out)

    def test_declaration_inside_a_raw_string_is_ignored(self):
        hashes = "#" * 255
        self.repo.write(
            "crates/engine/src/x.rs",
            f'const NOTE: &str = r{hashes}"quoted "text\n'
            "const GENERATED_WHITE: [u8; 4] = [255; 4];\n"
            f'"{hashes};\nfn main() {{}}\n',
        )
        self.repo.commit("declaration inside a raw string")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 stand-in line(s)", out)

    def test_audit_ignores_a_declaration_inside_a_raw_string(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            'const NOTE: &str = r#"\n'
            "const GENERATED_WHITE: [u8; 4] = [255; 4];\n"
            '"#;\nfn main() {}\n',
        )
        self.repo.commit("declaration inside a raw string")
        code, out, err = self.check(self.repo.run("--audit"))
        self.assertEqual(code, 0, err)
        self.assertIn("0 unlabelled hit(s)", out)

    def test_multiline_string_does_not_supply_code_comments_or_test_attributes(self):
        path = "crates/engine/src/x.rs"
        self.repo.write(path, (
            'const NOTE: &str = "header\n'
            'continued\n'
            'const GENERATED_HEIGHT: f32 = 1.0;\n'
            '// STAND-IN(#900): only text inside the string\n'
            '#[cfg(test)]\n'
            '";\n'
            'const GENERATED_COLOUR: u8 = 1;\n'
            '// STAND-IN(#901): fill a missing terrain texture\n'
        ))
        self.repo.commit("multiline Rust string followed by a stand-in")

        diff = self.repo.run()
        audit = self.repo.run("--audit")
        with self.subTest(mode="diff"):
            self.assertEqual(diff.returncode, 1, diff.stderr)
            self.assertIn(f"{path}:7:", diff.stderr)
            self.assertNotIn(f"{path}:3:", diff.stderr)
        with self.subTest(mode="audit"):
            self.assertEqual(audit.returncode, 0, audit.stderr)
            self.assertIn(f"{path}:7:", audit.stdout)
            self.assertNotIn(f"{path}:3:", audit.stdout)
        with self.subTest(mode="issues"):
            issues = self.repo.run(
                "--check-issues", env=self.check_issues_env(self.gh_stub("CLOSED", []))
            )
            self.assertEqual(issues.returncode, 1, issues.stderr)
            self.assertIn("issue #901 is closed", issues.stderr)
            self.assertNotIn("issue #900", issues.stderr)

    def test_stand_in_identifier_with_a_marker_passes(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            self.MARKER
            + "pub const GENERATED_WHITE: [u8; 4] = [255, 255, 255, 255];\n",
        )
        self.repo.commit("labelled stand-in identifier")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_markdown_marker_labels_its_section(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\n## Stand-in\n\n"
            "// STAND-IN(#900, @alice): example note for this section\n\n"
            "This is a placeholder colour.\n",
        )
        self.repo.commit("marker in the same section")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)

    def test_markdown_marker_in_another_section_does_not_label(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\n## First\n\n"
            "// STAND-IN(#900, @alice): example note for the first section\n\n"
            "Nothing here.\n\n"
            "## Second\n\n"
            "This is a placeholder colour.\n",
        )
        self.repo.commit("marker in another section")
        code, _, err = self.check(self.repo.run())
        self.assertEqual(code, 1)
        self.assertIn("docs/specs/example.md:11", err)

    def test_moved_stand_in_is_not_a_new_finding(self):
        self.repo.write(
            "crates/engine/src/lib.rs",
            "fn a() {}\n// placeholder colour\nfn b() {}\nfn c() {}\n",
        )
        self.repo.commit("stand-in in the first position")
        self.repo.mark_base()
        self.repo.write(
            "crates/engine/src/lib.rs",
            "fn a() {}\nfn b() {}\nfn c() {}\n// placeholder colour\n",
        )
        self.repo.commit("move the stand-in")
        code, out, err = self.check(self.repo.run())
        self.assertEqual(code, 0, err)
        self.assertIn("0 stand-in line(s)", out)

    def test_audit_lists_hits_and_exits_zero(self):
        self.repo.write("crates/engine/src/x.rs", "// placeholder one\nfn x() {}\n")
        self.repo.write("crates/engine/src/y.rs", "// placeholder two\nfn y() {}\n")
        self.repo.commit("two stand-ins")
        code, out, err = self.check(self.repo.run("--audit"))
        self.assertEqual(code, 0, err)
        self.assertIn("crates/engine/src/x.rs:1", out)
        self.assertIn("crates/engine/src/y.rs:1", out)
        self.assertIn("2 unlabelled hit(s)", out)

    def test_audit_baseline_passes_at_the_count(self):
        self.repo.write("crates/engine/src/x.rs", "// placeholder one\nfn x() {}\n")
        self.repo.write("crates/engine/src/y.rs", "// placeholder two\nfn y() {}\n")
        baseline = self.repo.root / "baseline.json"
        baseline.write_text(json.dumps({
            "counts": {
                "crates/engine/src/x.rs": 1,
                "crates/engine/src/y.rs": 1,
            },
            "revision": 1,
        }), encoding="utf-8")
        self.repo.commit("two stand-ins and a baseline")
        code, out, err = self.check(
            self.repo.run("--audit", "--baseline", str(baseline))
        )
        self.assertEqual(code, 0, err)
        self.assertIn("at or below the baseline", out)

    def test_audit_baseline_rise_fails(self):
        self.repo.write(
            "crates/engine/src/x.rs",
            "// placeholder one\n// placeholder two\nfn x() {}\n",
        )
        baseline = self.repo.root / "baseline.json"
        baseline.write_text(
            '{"counts": {"crates/engine/src/x.rs": 1}, "revision": 1}\n',
            encoding="utf-8",
        )
        self.repo.commit("one more stand-in than the baseline")
        code, _, err = self.check(
            self.repo.run("--audit", "--baseline", str(baseline))
        )
        self.assertEqual(code, 1)
        self.assertIn("rose from 1 to 2", err)

    def test_audit_writes_the_baseline(self):
        self.repo.write("crates/engine/src/x.rs", "// placeholder one\nfn x() {}\n")
        baseline = self.repo.root / "baseline.json"
        self.repo.commit("one stand-in")
        code, out, err = self.check(
            self.repo.run("--audit", "--write-baseline", "--baseline", str(baseline))
        )
        self.assertEqual(code, 0, err)
        written = json.loads(baseline.read_text(encoding="utf-8"))
        self.assertEqual(written["counts"], {"crates/engine/src/x.rs": 1})
        self.assertEqual(written["revision"], 1)
        self.assertIn("Wrote", out)

    def test_audit_missing_baseline_fails_closed(self):
        self.repo.write("crates/engine/src/x.rs", "// placeholder one\nfn x() {}\n")
        self.repo.commit("one stand-in")
        code, _, err = self.check(
            self.repo.run("--audit", "--baseline", str(self.repo.root / "missing.json"))
        )
        self.assertEqual(code, 2)
        self.assertIn("cannot read baseline", err)

    def gh_stub(self, state, assignees=None, log=None):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        script = Path(directory.name) / "gh"
        payload = {"state": state}
        if assignees is not None:
            payload["assignees"] = [{"login": login} for login in assignees]
        lines = ["#!/bin/sh"]
        if log is not None:
            lines.append(f"printf '%s\\n' \"$*\" >> {shlex.quote(str(log))}")
        lines.append("cat <<'JSON'")
        lines.append(json.dumps(payload))
        lines.append("JSON")
        script.write_text("\n".join(lines) + "\n", encoding="utf-8")
        script.chmod(0o755)
        return directory.name

    def check_issues_env(self, directory, repository="Mudcrab-Team/mudcrab"):
        env = {"PATH": directory + os.pathsep + os.environ.get("PATH", "")}
        if repository:
            env["GITHUB_REPOSITORY"] = repository
        return env

    def test_check_issues_closed_issue_fails(self):
        self.repo.write("crates/engine/src/x.rs", self.MARKER + "fn main() {}\n")
        self.repo.commit("marker for a closed issue")
        directory = self.gh_stub("CLOSED", ["alice"])
        code, _, err = self.check(self.repo.run(
            "--check-issues", env=self.check_issues_env(directory)
        ))
        self.assertEqual(code, 1)
        self.assertIn("issue #900 is closed", err)

    def test_check_issues_open_issue_passes(self):
        self.repo.write("crates/engine/src/x.rs", self.MARKER + "fn main() {}\n")
        self.repo.commit("marker for an open issue")
        directory = self.gh_stub("OPEN", ["alice"])
        code, out, err = self.check(self.repo.run(
            "--check-issues", env=self.check_issues_env(directory)
        ))
        self.assertEqual(code, 0, err)
        self.assertIn("open issue", out)

    def test_check_issues_open_unassigned_issue_passes(self):
        self.repo.write("crates/engine/src/x.rs", self.MARKER + "fn main() {}\n")
        self.repo.commit("marker for an unassigned issue")
        directory = self.gh_stub("OPEN", [])
        code, out, err = self.check(self.repo.run(
            "--check-issues", env=self.check_issues_env(directory)
        ))
        self.assertEqual(code, 0, err)
        self.assertIn("open issue", out)

    def test_check_issues_shorthand_uses_the_inspected_repository(self):
        self.repo.write("crates/engine/src/x.rs", self.MARKER + "fn main() {}\n")
        self.repo.commit("marker for a shorthand issue")
        log = self.repo.root / "gh.log"
        directory = self.gh_stub("OPEN", log=log)
        code, out, err = self.check(self.repo.run(
            "--check-issues", env=self.check_issues_env(directory)
        ))
        self.assertEqual(code, 0, err)
        calls = log.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("issue view 900", calls[0])
        self.assertIn("-R Mudcrab-Team/mudcrab", calls[0])

    def test_check_issues_url_keeps_the_full_target(self):
        url = "https://github.com/other/project/issues/900"
        self.repo.write(
            "crates/engine/src/x.rs",
            f"// STAND-IN({url}): terrain LOD mesh built from LAND heights\n"
            "fn main() {}\n",
        )
        self.repo.commit("marker for a URL issue")
        log = self.repo.root / "gh.log"
        directory = self.gh_stub("OPEN", log=log)
        code, out, err = self.check(self.repo.run(
            "--check-issues", env=self.check_issues_env(directory)
        ))
        self.assertEqual(code, 0, err)
        calls = log.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn(url, calls[0])
        self.assertNotIn("-R", calls[0])

    def test_check_issues_distinct_repositories_do_not_share_the_cache(self):
        url = "https://github.com/other/project/issues/900"
        self.repo.write(
            "crates/engine/src/a.rs",
            f"// STAND-IN({url}): terrain LOD mesh built from LAND heights\n"
            "fn a() {}\n",
        )
        self.repo.write(
            "crates/engine/src/b.rs",
            self.MARKER + "fn b() {}\n",
        )
        self.repo.commit("markers for two repositories")
        log = self.repo.root / "gh.log"
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        script = Path(directory.name) / "gh"
        script.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$*\" >> {shlex.quote(str(log))}\n"
            "case \"$*\" in\n"
            "  *other/project/issues/900*) echo '{\"state\":\"CLOSED\"}' ;;\n"
            "  *) echo '{\"state\":\"OPEN\"}' ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        script.chmod(0o755)
        code, _, err = self.check(self.repo.run(
            "--check-issues", env=self.check_issues_env(directory.name)
        ))
        self.assertEqual(code, 1)
        self.assertIn(url, err)
        self.assertIn("is closed", err)
        calls = log.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(calls), 2)

    def test_check_issues_missing_gh_exits_two(self):
        self.repo.write("crates/engine/src/x.rs", self.MARKER + "fn main() {}\n")
        self.repo.commit("marker without a gh client")
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        git = shutil.which("git")
        self.assertIsNotNone(git)
        (Path(directory.name) / "git").symlink_to(git)
        code, _, err = self.check(self.repo.run(
            "--check-issues", env={"PATH": directory.name}
        ))
        self.assertEqual(code, 2)
        self.assertIn("gh is not installed", err)

    def test_check_issues_gh_error_exits_two(self):
        self.repo.write("crates/engine/src/x.rs", self.MARKER + "fn main() {}\n")
        self.repo.commit("marker with a failing gh client")
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        script = Path(directory.name) / "gh"
        script.write_text(
            "#!/bin/sh\necho network down >&2\nexit 1\n",
            encoding="utf-8",
        )
        script.chmod(0o755)
        code, _, err = self.check(self.repo.run(
            "--check-issues", env=self.check_issues_env(directory.name)
        ))
        self.assertEqual(code, 2)
        self.assertIn("network down", err)

    def test_workflow_runs_the_stand_in_baseline(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn(
            "name: Stand-in count does not rise above the baseline", workflow
        )
        self.assertIn(
            "scripts/check_retail_claims.py --audit --baseline "
            "scripts/stand_in_baseline.json",
            workflow,
        )


class ClaimRuleTests(unittest.TestCase):
    """The audit lines from PR #841 review F3."""

    ADJECTIVE_LINES = [
        "The opt-in test `default_movement_types_of_the_real_masters_survive_overrides` "
        "reads the retail masters.",
        "The retail reference scale from the stored percent: [`stored_reference_scale`] "
        "then [`reference_scale`].",
        "The retail constant, written as its bits, apart from `PERCENT_TO_SCALE`.",
        "Retail value tables: outputs that the retail game gives for chosen synthetic inputs.",
        "A row here shows that the code equals the retail game for one input, bit for bit.",
        "Each row names its registry ID. It names no address and no function from the "
        "retail game.",
    ]

    CLAIM_LINES = [
        "Retail movement types hold zero speeds where a creature cannot move.",
        "Retail movement types can hold zero speeds (a creature that cannot strafe has "
        "zero lateral speeds).",
        "Any MOVT exports with its own editor ID, including retail zero speeds (no strafing).",
        "Retail Update.esm holds hundreds of all-zero padding pairs.",
        "Retail profiles hold zero speeds in some directions.",
        "Retail heights are near 1.0.",
        "For retail data both slots resolve to `NPC_Default_MT`.",
        "In retail, `fLandTextureTilingMult:Landscape` defaults to 3.0.",
        "The unset race links resolve through the default objects, like the player race "
        "in Skyrim.esm.",
    ]

    def test_retail_as_an_adjective_or_label_is_not_a_claim(self):
        for line in self.ADJECTIVE_LINES:
            with self.subTest(line=line):
                self.assertFalse(CHECK.makes_claim(line), line)

    def test_retail_as_the_subject_is_a_claim(self):
        for line in self.CLAIM_LINES:
            with self.subTest(line=line):
                self.assertTrue(CHECK.makes_claim(line), line)


class BlockTests(unittest.TestCase):
    def test_markdown_heading_joins_its_first_paragraph(self):
        blocks, mapping = CHECK.build_markdown_blocks(
            ["## Retail behaviour", "", "It does X (RF-EX-001).", "More text."]
        )
        self.assertEqual(mapping[1], mapping[3])
        self.assertNotIn(2, mapping)
        self.assertIn("RF-EX-001", blocks[mapping[3]])

    def test_markdown_section_ends_at_the_next_same_level_heading(self):
        _, mapping = CHECK.build_markdown_blocks(
            ["## Evidence", "", "- Source: RF-EX-001.", "", "## Claim", "", "Retail does X."]
        )
        self.assertEqual(mapping[1], mapping[3])
        self.assertNotEqual(mapping[3], mapping[7])

    def test_markdown_subsection_is_its_own_block(self):
        _, mapping = CHECK.build_markdown_blocks(
            ["## Parent", "", "Text.", "", "### Child", "", "Retail does X."]
        )
        self.assertNotEqual(mapping[3], mapping[7])

    def test_markdown_list_item_shares_its_section(self):
        _, mapping = CHECK.build_markdown_blocks(
            ["## Evidence", "- Retail does X.", "- Source: RF-EX-001."]
        )
        self.assertEqual(mapping[2], mapping[3])

    def test_markdown_table_row_shares_its_section(self):
        _, mapping = CHECK.build_markdown_blocks(
            ["## Table", "", "| Claim | Source |", "| --- | --- |", "| Retail does X. | RF-EX-001 |"]
        )
        self.assertEqual(mapping[3], mapping[5])


class RegistryKindTests(unittest.TestCase):
    REGISTRY = "docs/research/retail/registry.md"
    HEADER = "| ID | Fact |\n|---|---|\n"

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Repository(self.temporary.name)

    def run_with_rows(self, rows):
        self.repo.write(self.REGISTRY, "# Registry\n\n" + self.HEADER + rows)
        self.repo.commit("registry rows")
        result = self.repo.run()
        return result.returncode, result.stdout, result.stderr

    def test_row_without_kind_is_a_rule_and_passes(self):
        code, out, err = self.run_with_rows("| RF-EX-001 | A formula. |\n")
        self.assertEqual(code, 0, err)
        self.assertIn("all labelled", out)

    def test_each_valid_kind_passes(self):
        rows = (
            "| RF-EX-001 | kind: rule. A formula. |\n"
            "| RF-EX-002 | kind: quirk. An odd test. |\n"
            "| RF-EX-003 | kind: defect. Reads undefined lanes. |\n"
            "| RF-EX-004 | Kind: Quirk. Capital letters pass. |\n"
        )
        code, _, err = self.run_with_rows(rows)
        self.assertEqual(code, 0, err)

    def test_invalid_kind_fails(self):
        code, _, err = self.run_with_rows("| RF-EX-001 | kind: oddity. A formula. |\n")
        self.assertEqual(code, 1)
        self.assertIn(f"{self.REGISTRY}:5", err)
        self.assertIn('invalid kind "oddity"', err)
        self.assertIn("1 registry row(s) have a kind problem", err)
        self.assertNotIn("need a source", err)

    def test_two_kind_fields_fail(self):
        code, _, err = self.run_with_rows(
            "| RF-EX-001 | kind: quirk. A test. kind: defect. |\n"
        )
        self.assertEqual(code, 1)
        self.assertIn("more than one kind field", err)

    def test_quirk_word_without_kind_fails(self):
        code, _, err = self.run_with_rows("| RF-EX-001 | The portal test is a quirk. |\n")
        self.assertEqual(code, 1)
        self.assertIn("has no kind field", err)

    def test_defect_word_without_kind_fails(self):
        code, _, err = self.run_with_rows("| RF-EX-001 | Unwritten lanes are a defect. |\n")
        self.assertEqual(code, 1)
        self.assertIn("has no kind field", err)

    def test_kind_rule_is_not_checked_outside_the_registry(self):
        self.repo.write(
            "docs/specs/example.md",
            "# Example\n\n| RF-EX-001 | kind: oddity. A defect. |\n",
        )
        self.repo.commit("row-like text elsewhere")
        code, _, err = self.check_result()
        self.assertEqual(code, 0, err)

    def test_unchanged_bad_row_is_ignored(self):
        self.repo.write(self.REGISTRY, "# Registry\n\n" + self.HEADER
                        + "| RF-EX-001 | kind: oddity. A formula. |\n")
        self.repo.commit("old bad row")
        self.repo.mark_base()
        self.repo.write(self.REGISTRY, "# Registry\n\n" + self.HEADER
                        + "| RF-EX-001 | kind: oddity. A formula. |\n"
                        + "| RF-EX-002 | A formula. |\n")
        self.repo.commit("add a good row")
        code, _, err = self.check_result()
        self.assertEqual(code, 0, err)

    def check_result(self):
        result = self.repo.run()
        return result.returncode, result.stdout, result.stderr


class WorkflowTests(unittest.TestCase):
    def test_push_without_a_base_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            run_git(repo, "init", "-q")
            result = subprocess.run(
                ["bash", "-c", retail_claim_step_script()],
                cwd=repo,
                text=True,
                capture_output=True,
                check=False,
                env=dict(
                    os.environ,
                    GITHUB_EVENT_NAME="push",
                    GITHUB_BASE_REF="",
                    BASE_REF="",
                    PUSH_BEFORE="",
                ),
            )
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("No base commit", result.stderr)


if __name__ == "__main__":
    unittest.main()
