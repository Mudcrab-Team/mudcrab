#!/usr/bin/env python3
"""Unit tests for tools/enum_audit/enum_audit.py."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import enum_audit  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]


def run_git(root: Path, *args: str) -> str:
    res = subprocess.run(
        ["git", "-C", str(root), *args],
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=True,
    )
    return res.stdout


class EnumAuditTests(unittest.TestCase):
    def _make_repo(self, tmp: Path) -> Path:
        root = tmp / "repo"
        (root / "crates" / "converter" / "src" / "esm").mkdir(parents=True)
        (root / "crates" / "shared" / "src").mkdir(parents=True)
        (root / "tools" / "enum_audit").mkdir(parents=True)
        (root / "crates" / "converter" / "src" / "esm" / "reader.rs").write_text(
            "pub fn parse(v: u8) -> Result<u8, String> {\n"
            "    match v {\n"
            "        0 => Ok(0),\n"
            "        _ => Err(format!(\"bad {v}\")),\n"
            "    }\n"
            "}\n",
            encoding="utf-8",
        )
        (root / "crates" / "shared" / "src" / "other.rs").write_text(
            "pub fn helper() {}\n",
            encoding="utf-8",
        )
        sites = {
            "format_version": 1,
            "sites": [
                {
                    "file": "crates/converter/src/esm/reader.rs",
                    "anchor": "_ => Err(format!(\"bad {v}\")),",
                    "enum_id": "GRAS.DATA.UnitsFromWaterType",
                    "status": "exhaustive",
                }
            ],
        }
        (root / "tools" / "enum_audit" / "sites.json").write_text(
            json.dumps(sites, indent=2) + "\n", encoding="utf-8"
        )
        (root / "tools" / "enum_audit" / "baseline.json").write_text(
            json.dumps({"format_version": 1, "catch_all_bug_count": 1}, indent=2) + "\n",
            encoding="utf-8",
        )
        run_git(root, "init")
        run_git(root, "config", "user.email", "test@example.com")
        run_git(root, "config", "user.name", "Test")
        run_git(root, "add", ".")
        run_git(root, "commit", "-m", "base")
        return root

    def test_real_repository_passes(self):
        rc = enum_audit.main(["--root", str(REPO_ROOT), "--check"])
        self.assertEqual(rc, 0)

    def test_missing_anchor_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            (root / "crates" / "converter" / "src" / "esm" / "reader.rs").write_text(
                "pub fn parse(v: u8) -> Result<u8, String> { Ok(v) }\n",
                encoding="utf-8",
            )
            rc = enum_audit.main(["--root", str(root), "--check"])
            self.assertEqual(rc, 1)

    def test_anchor_moved_to_different_file_without_update_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            (root / "crates" / "converter" / "src" / "esm" / "reader.rs").write_text(
                "pub fn parse(v: u8) -> Result<u8, String> { Ok(v) }\n",
                encoding="utf-8",
            )
            (root / "crates" / "shared" / "src" / "other.rs").write_text(
                "pub fn moved(v: u8) -> Result<u8, String> {\n"
                "    match v {\n"
                "        0 => Ok(0),\n"
                "        _ => Err(format!(\"bad {v}\")),\n"
                "    }\n"
                "}\n",
                encoding="utf-8",
            )
            rc = enum_audit.main(["--root", str(root), "--check"])
            self.assertEqual(rc, 1)

    def test_line_number_anchor_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            sites = {
                "format_version": 1,
                "sites": [
                    {
                        "file": "crates/converter/src/esm/reader.rs",
                        "anchor": "4",
                        "enum_id": "GRAS.DATA.UnitsFromWaterType",
                        "status": "exhaustive",
                    }
                ],
            }
            (root / "tools" / "enum_audit" / "sites.json").write_text(
                json.dumps(sites), encoding="utf-8"
            )
            rc = enum_audit.main(["--root", str(root), "--check"])
            self.assertEqual(rc, 1)

    def test_allowed_requires_reason_and_reference(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            for missing_field in ("reason", "reference"):
                entry = {
                    "file": "crates/converter/src/esm/reader.rs",
                    "anchor": "_ => Err(format!(\"bad {v}\")),",
                    "enum_id": "MGEF.DATA.Archetype",
                    "status": "catch-all-allowed",
                    "reason": "Shared unused slot.",
                    "reference": "xEdit wbDefinitionsTES5.pas:1146",
                }
                del entry[missing_field]
                (root / "tools" / "enum_audit" / "sites.json").write_text(
                    json.dumps({"format_version": 1, "sites": [entry]}), encoding="utf-8"
                )
                self.assertEqual(enum_audit.main(["--root", str(root), "--check"]), 1)

    def test_ratchet_fails_when_bug_count_rises_and_passes_when_it_drops(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            sites_path = root / "tools" / "enum_audit" / "sites.json"
            base_sites = {
                "format_version": 1,
                "sites": [
                    {
                        "file": "crates/converter/src/esm/reader.rs",
                        "anchor": "_ => Err(format!(\"bad {v}\")),",
                        "enum_id": "GRAS.DATA.UnitsFromWaterType",
                        "status": "catch-all-bug",
                        "issue": "#1061",
                        "reason": "Known grass bug.",
                    }
                ],
            }
            sites_path.write_text(json.dumps(base_sites), encoding="utf-8")
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "one bug")
            base_sha = run_git(root, "rev-parse", "HEAD").strip()

            # Adding a second bug fails both baseline and --base ratchet.
            two_bugs = {
                "format_version": 1,
                "sites": base_sites["sites"]
                + [
                    {
                        "file": "crates/shared/src/other.rs",
                        "anchor": "pub fn helper() {}",
                        "enum_id": "unnamed",
                        "status": "catch-all-bug",
                        "issue": "",
                        "reason": "Newly added unfiled bug.",
                    }
                ],
            }
            sites_path.write_text(json.dumps(two_bugs), encoding="utf-8")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 1
            )

            # Fixing the bug (dropping 1 -> 0) passes.
            zero_bugs = {
                "format_version": 1,
                "sites": [
                    {
                        "file": "crates/converter/src/esm/reader.rs",
                        "anchor": "_ => Err(format!(\"bad {v}\")),",
                        "enum_id": "GRAS.DATA.UnitsFromWaterType",
                        "status": "exhaustive",
                    }
                ],
            }
            sites_path.write_text(json.dumps(zero_bugs), encoding="utf-8")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 0
            )

    def test_diff_flags_new_wildcard_and_binding_catch_all(self):
        for arm in ("_ => true,", "other => true,"):
            with tempfile.TemporaryDirectory() as tmp:
                root = self._make_repo(Path(tmp))
                base_sha = run_git(root, "rev-parse", "HEAD").strip()
                target = root / "crates" / "converter" / "src" / "esm" / "new_mode.rs"
                target.write_text(
                    "pub fn check_mode(mode: u32) -> bool {\n"
                    "    match mode {\n"
                    "        0 => true,\n"
                    "        1 => false,\n"
                    f"        {arm}\n"
                    "    }\n"
                    "}\n",
                    encoding="utf-8",
                )
                run_git(root, "add", ".")
                run_git(root, "commit", "-m", "add catch-all")
                self.assertEqual(
                    enum_audit.main(["--root", str(root), "--check", "--base", base_sha]),
                    1,
                    msg=f"expected failure for {arm}",
                )

    def test_diff_allows_explicit_error_rejection_arm(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            base_sha = run_git(root, "rev-parse", "HEAD").strip()
            target = root / "crates" / "converter" / "src" / "esm" / "new_mode.rs"
            target.write_text(
                "pub fn check_mode(mode: u32) -> Result<bool, String> {\n"
                "    match mode {\n"
                "        0 => Ok(true),\n"
                "        1 => Ok(false),\n"
                "        other => Err(format!(\"invalid mode {other}\")),\n"
                "    }\n"
                "}\n",
                encoding="utf-8",
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "explicit error")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 0
            )

    def test_empty_registered_file_fails_anchor_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            (root / "crates" / "converter" / "src" / "esm" / "reader.rs").write_text(
                "", encoding="utf-8"
            )
            self.assertEqual(enum_audit.main(["--root", str(root), "--check"]), 1)

    def test_unrelated_error_after_match_does_not_exempt_catch_all(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            base_sha = run_git(root, "rev-parse", "HEAD").strip()
            target = root / "crates" / "converter" / "src" / "esm" / "new_mode.rs"
            target.write_text(
                "pub fn check_mode(mode: u32, ok: bool) -> Result<u32, String> {\n"
                "    let value = match mode {\n"
                "        0 => 4,\n"
                "        _ => 1,\n"
                "    };\n"
                "    if !ok {\n"
                "        return Err(String::from(\"bad\"));\n"
                "    }\n"
                "    Ok(value)\n"
                "}\n",
                encoding="utf-8",
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "catch-all near unrelated error")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 1
            )

    def test_multi_line_clamp_is_flagged(self):
        hits = enum_audit.scan_file_for_catch_alls(
            "crates/converter/src/esm/x.rs",
            "pub fn f(mode: u8) -> u8 {\n"
            "    mode.clamp(\n"
            "        0, // minimum\n"
            "        7, // maximum\n"
            "    )\n"
            "}\n",
        )
        self.assertTrue(any(h[2] == "integer-clamp" and h[1] in (2, 3, 4, 5) for h in hits), hits)

    def test_diff_flags_clamp_mask_and_unwrap_or(self):
        snippets = [
            "pub fn f(mode: u32) -> u32 { mode.min(7) }\n",
            "pub fn f(mode: u32) -> u32 { mode.clamp(0, 7) }\n",
            "pub fn f(clamp: u8) -> u8 { clamp & 3 }\n",
            "pub fn f(id: u16) -> &'static str { lookup(id).unwrap_or(\"unknown\") }\n",
            "pub fn f(mode: Option<u8>) -> u8 { mode.unwrap_or(0) }\n",
        ]
        for code in snippets:
            with tempfile.TemporaryDirectory() as tmp:
                root = self._make_repo(Path(tmp))
                base_sha = run_git(root, "rev-parse", "HEAD").strip()
                target = root / "crates" / "converter" / "src" / "esm" / "new_mode.rs"
                target.write_text(code, encoding="utf-8")
                run_git(root, "add", ".")
                run_git(root, "commit", "-m", "add pattern")
                self.assertEqual(
                    enum_audit.main(["--root", str(root), "--check", "--base", base_sha]),
                    1,
                    msg=f"expected failure for {code!r}",
                )

    def test_diff_exemption_comment_allows_with_reason_and_rejects_without_reason(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            base_sha = run_git(root, "rev-parse", "HEAD").strip()
            target = root / "crates" / "converter" / "src" / "esm" / "new_mode.rs"

            # Missing reason after id must fail.
            target.write_text(
                "// retail-enum: GRAS.DATA.UnitsFromWaterType\n"
                "pub fn f(mode: u32) -> u32 { mode.min(7) }\n",
                encoding="utf-8",
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "incomplete comment")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 1
            )

            # Valid comment on line above passes.
            target.write_text(
                "// retail-enum: GRAS.DATA.UnitsFromWaterType validated on previous line\n"
                "pub fn f(mode: u32) -> u32 { mode.min(7) }\n",
                encoding="utf-8",
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "valid exemption")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 0
            )

            # Valid comment on same line passes.
            target.write_text(
                "pub fn f(mode: u32) -> u32 { mode.min(7) } // retail-enum: GRAS.DATA.UnitsFromWaterType validated inline\n",
                encoding="utf-8",
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "valid inline exemption")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 0
            )

    def test_ambiguous_anchor_and_invalid_bug_metadata_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            (root / "crates" / "converter" / "src" / "esm" / "reader.rs").write_text(
                "pub fn a() { let _ = 1; }\npub fn b() { let _ = 1; }\n",
                encoding="utf-8",
            )
            sites = {
                "format_version": 1,
                "sites": [
                    {
                        "file": "crates/converter/src/esm/reader.rs",
                        "anchor": "let _ = 1;",
                        "enum_id": "unnamed",
                        "status": "exhaustive",
                    }
                ],
            }
            (root / "tools" / "enum_audit" / "sites.json").write_text(
                json.dumps(sites), encoding="utf-8"
            )
            self.assertEqual(enum_audit.main(["--root", str(root), "--check"]), 1)

            sites["sites"] = [
                {
                    "file": "crates/shared/src/other.rs",
                    "anchor": "pub fn helper() {}",
                    "enum_id": "unnamed",
                    "status": "catch-all-bug",
                    "issue": "bad-issue",
                    "reason": "",
                }
            ]
            (root / "tools" / "enum_audit" / "sites.json").write_text(
                json.dumps(sites), encoding="utf-8"
            )
            self.assertEqual(enum_audit.main(["--root", str(root), "--check"]), 1)

    def test_moved_line_is_not_flagged_as_new(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            (root / "crates" / "converter" / "src" / "esm" / "reader.rs").write_text(
                "pub fn f(mode: u32) -> u32 { mode.min(7) }\n",
                encoding="utf-8",
            )
            sites = {
                "format_version": 1,
                "sites": [
                    {
                        "file": "crates/converter/src/esm/reader.rs",
                        "anchor": "pub fn f(mode: u32) -> u32 { mode.min(7) }",
                        "enum_id": "GRAS.DATA.UnitsFromWaterType",
                        "status": "catch-all-bug",
                        "issue": "#1061",
                        "reason": "Pre-existing.",
                    }
                ],
            }
            (root / "tools" / "enum_audit" / "sites.json").write_text(
                json.dumps(sites), encoding="utf-8"
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "pre-existing min")
            base_sha = run_git(root, "rev-parse", "HEAD").strip()

            # Move the exact line to other.rs and update sites.json.
            (root / "crates" / "converter" / "src" / "esm" / "reader.rs").write_text(
                "pub fn empty() {}\n", encoding="utf-8"
            )
            (root / "crates" / "shared" / "src" / "other.rs").write_text(
                "pub fn f(mode: u32) -> u32 { mode.min(7) }\n",
                encoding="utf-8",
            )
            sites["sites"][0]["file"] = "crates/shared/src/other.rs"
            (root / "tools" / "enum_audit" / "sites.json").write_text(
                json.dumps(sites), encoding="utf-8"
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "move line")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 0
            )

    def test_diff_ignores_cfg_test_and_bitflag_tests(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            base_sha = run_git(root, "rev-parse", "HEAD").strip()
            target = root / "crates" / "converter" / "src" / "esm" / "new_mode.rs"
            target.write_text(
                "pub fn has_flag(flags: u32) -> bool {\n"
                "    flags & 0x20 != 0\n"
                "}\n\n"
                "#[cfg(test)]\n"
                "mod tests {\n"
                "    fn t(v: u32) -> u32 { v.min(7) }\n"
                "}\n",
                encoding="utf-8",
            )
            run_git(root, "add", ".")
            run_git(root, "commit", "-m", "bitflag and test")
            self.assertEqual(
                enum_audit.main(["--root", str(root), "--check", "--base", base_sha]), 0
            )


if __name__ == "__main__":
    unittest.main()
