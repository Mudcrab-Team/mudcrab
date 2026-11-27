#!/usr/bin/env python3
"""Unit tests for tools/defaults_ratchet/defaults_ratchet.py."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import defaults_ratchet  # noqa: E402

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


class DefaultsRatchetTests(unittest.TestCase):
    def _make_repo(self, tmp: Path) -> Path:
        root = tmp / "repo"
        (root / "crates" / "converter" / "src" / "esm").mkdir(parents=True)
        (root / "crates" / "engine" / "src" / "world").mkdir(parents=True)
        (root / "tools" / "defaults_ratchet").mkdir(parents=True)

        (root / "crates" / "converter" / "src" / "lib.rs").write_text(
            "pub mod actor_models;\npub mod esm;\n", encoding="utf-8"
        )
        (root / "crates" / "converter" / "src" / "actor_models.rs").write_text(
            "pub struct ActorModelRow {\n"
            "    pub id: u32,\n"
            "    pub race_id: Option<u32>,\n"
            "    pub sex: Option<&'static str>,\n"
            "}\n",
            encoding="utf-8",
        )
        (root / "crates" / "converter" / "src" / "esm" / "mod.rs").write_text(
            "pub mod exporter;\n", encoding="utf-8"
        )
        (root / "crates" / "converter" / "src" / "esm" / "exporter.rs").write_text(
            "pub fn schema() -> &'static str {\n"
            "    \"CREATE TABLE statics (id INTEGER PRIMARY KEY, bounds_min_x REAL);\"\n"
            "}\n",
            encoding="utf-8",
        )
        (root / "crates" / "engine" / "src" / "lib.rs").write_text(
            "pub mod color_pipeline;\npub mod world;\npub mod non_retail;\n",
            encoding="utf-8",
        )
        (root / "crates" / "engine" / "src" / "color_pipeline.rs").write_text(
            "pub fn sky(has: bool) -> Option<f32> {\n"
            "    if has { Some(1.0) } else { None }\n"
            "}\n",
            encoding="utf-8",
        )
        (root / "crates" / "engine" / "src" / "world" / "mod.rs").write_text(
            "pub mod database;\n", encoding="utf-8"
        )
        (root / "crates" / "engine" / "src" / "world" / "database.rs").write_text(
            "pub const SQL: &str = \"SELECT id, opacity FROM waters\";\n",
            encoding="utf-8",
        )
        (root / "crates" / "engine" / "src" / "non_retail.rs").write_text(
            "pub fn ui_pad(v: Option<u32>) -> u32 {\n    v.unwrap_or(8)\n}\n",
            encoding="utf-8",
        )

        config = {
            "format_version": 1,
            "crates": {
                "converter": "crates/converter",
                "engine": "crates/engine",
            },
            "retail_data_modules": [
                "crates/converter/src/actor_models.rs",
                "crates/converter/src/esm",
                "crates/engine/src/color_pipeline.rs",
                "crates/engine/src/world",
            ],
            "db_write_modules": [
                "crates/converter/src/actor_models.rs",
                "crates/converter/src/esm/exporter.rs",
                "crates/engine/src/world/database.rs",
            ],
        }
        (root / "tools" / "defaults_ratchet" / "config.json").write_text(
            json.dumps(config, indent=2) + "\n", encoding="utf-8"
        )
        (root / "tools" / "defaults_ratchet" / "baseline.json").write_text(
            json.dumps(
                {"format_version": 1, "counts": {"converter": 0, "engine": 0}, "revision": 1},
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        run_git(root, "init")
        run_git(root, "config", "user.email", "test@example.com")
        run_git(root, "config", "user.name", "Test")
        run_git(root, "add", ".")
        run_git(root, "commit", "-m", "base")
        return root

    def _commit_change(self, root: Path, rel_path: str, content: str, msg: str = "change") -> None:
        target = root / rel_path
        target.write_text(content, encoding="utf-8")
        run_git(root, "add", rel_path)
        run_git(root, "commit", "-m", msg)

    def test_real_repository_passes(self):
        rc = defaults_ratchet.main(["--root", str(REPO_ROOT), "--check"])
        self.assertEqual(rc, 0)

    def test_catches_cl01_unresolved_actor_fallback_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/converter/src/actor_models.rs",
                "pub struct ActorModelRow {\n"
                "    pub id: u32,\n"
                "    pub race_id: u32,\n"
                "    pub sex: &'static str,\n"
                "    pub resolution: &'static str,\n"
                "}\n"
                "fn unresolved_row(id: u32) -> ActorModelRow {\n"
                "    ActorModelRow {\n"
                "        id,\n"
                "        race_id: 0,\n"
                "        sex: \"male\",\n"
                "        resolution: \"unresolved\",\n"
                "    }\n"
                "}\n",
            )
            _, hits = defaults_ratchet.scan_diff(
                root, "HEAD~1", defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            )
            self.assertEqual(
                [(h.category, h.line) for h in hits],
                [("fallback-struct-write", 10), ("fallback-struct-write", 11)],
            )
            self.assertEqual(
                defaults_ratchet.main(["--root", str(root), "--check", "--base", "HEAD~1"]),
                1,
            )

    def test_catches_wthr_fog_missing_field_replaced_with_literal_or_skyrim_clear(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "pub struct SkyPalette { pub fog_far: f32 }\n"
                "impl SkyPalette { pub const SKYRIM_CLEAR_DAY: Self = Self { fog_far: 80_000.0 }; }\n"
                "pub fn resolve(weather: Option<SkyPalette>, fog_near: Option<f32>) -> (SkyPalette, f32) {\n"
                "    let pal = weather.unwrap_or(SkyPalette::SKYRIM_CLEAR_DAY);\n"
                "    let near = fog_near.unwrap_or(0.0);\n"
                "    (pal, near)\n"
                "}\n"
                "pub fn fallback_sky(preset: u8) -> SkyPalette {\n"
                "    match preset {\n"
                "        0 => SkyPalette { fog_far: 48_300.0 },\n"
                "        _ => SkyPalette::SKYRIM_CLEAR_DAY,\n"
                "    }\n"
                "}\n",
            )
            _, hits = defaults_ratchet.scan_diff(
                root, "HEAD~1", defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            )
            self.assertEqual(
                [(h.category, h.line) for h in hits],
                [
                    ("unwrap-or-literal", 4),
                    ("unwrap-or-literal", 5),
                    ("fallback-constant", 11),
                ],
            )
            self.assertEqual(
                defaults_ratchet.main(["--root", str(root), "--check", "--base", "HEAD~1"]),
                1,
            )

    def test_sql_not_null_default_and_coalesce_literal_patterns(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/converter/src/esm/exporter.rs",
                "pub fn schema() -> &'static str {\n"
                "    \"CREATE TABLE statics (id INTEGER PRIMARY KEY, bounds_min_x REAL NOT NULL DEFAULT -64, tag TEXT NOT NULL DEFAULT '');\"\n"
                "}\n",
            )
            self._commit_change(
                root,
                "crates/engine/src/world/database.rs",
                "pub const SQL: &str = \"SELECT COALESCE(opacity, 50), COALESCE(a.x, b.x), COALESCE(MAX(id), 0) FROM waters\";\n",
            )
            cfg = defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            _, hits = defaults_ratchet.scan_diff(root, "HEAD~2", cfg)
            self.assertEqual(
                [(Path(h.path).name, h.category) for h in hits],
                [
                    ("exporter.rs", "sql-not-null-default"),
                    ("exporter.rs", "sql-not-null-default"),
                    ("database.rs", "sql-coalesce-literal"),
                ],
            )

    def test_fallback_params_and_non_fallback_struct_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/converter/src/actor_models.rs",
                "pub struct RawRecord { pub flags: u32, pub load_order: u32 }\n"
                "pub struct WalkState { pub elapsed: f32 }\n"
                "pub fn ok_cases(opt: Option<u32>) -> WalkState {\n"
                "    let Some(v) = opt else { return WalkState { elapsed: 1.0 }; };\n"
                "    let _raw = RawRecord { flags: 0, load_order: 0 };\n"
                "    let _ = v;\n"
                "    WalkState { elapsed: 0.0 }\n"
                "}\n"
                "pub fn bad_params(opt: Option<u32>) -> i32 {\n"
                "    match opt {\n"
                "        Some(v) => v as i32,\n"
                "        None => { let _p = params![0, \"male\"]; 0 }\n"
                "    }\n"
                "}\n",
            )
            cfg = defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            _, hits = defaults_ratchet.scan_diff(root, "HEAD~1", cfg)
            self.assertEqual([(h.category, h.line) for h in hits], [("fallback-db-params", 12)])

    def test_map_or_unwrap_or_else_and_fallback_const_exclusions(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "use crate::sky::{\n"
                "    DEFAULT_FOO,\n"
                "};\n"
                "pub const DEFAULT_BAR: f32 = 1.0;\n"
                "pub fn check(id: u32, a: Option<f32>, b: Option<f32>, c: Option<f32>, fallback: f32) -> f32 {\n"
                "    if id == DEFAULT_FOO { return 0.0; }\n"
                "    let x = a.map_or(DEFAULT_BAR, |v| v);\n"
                "    let y = b.unwrap_or_else(|| 2.0);\n"
                "    let z = c.unwrap_or(fallback);\n"
                "    x + y + z + FALLBACK_EXTRA\n"
                "}\n",
            )
            cfg = defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            _, hits = defaults_ratchet.scan_diff(root, "HEAD~1", cfg)
            self.assertEqual(
                [(h.category, h.line) for h in hits],
                [
                    ("map-or-literal", 7),
                    ("unwrap-or-literal", 8),
                    ("fallback-constant", 10),
                ],
            )

    def test_exemptions_retail_default_and_stand_in(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "pub fn f(a: Option<f32>, b: Option<f32>, c: Option<f32>, d: Option<f32>, e: Option<f32>) -> f32 {\n"
                "    // retail-default: SPEC-1 section 2.6 absent fade is 1.0\n"
                "    let v1 = a.unwrap_or(1.0);\n"
                "    let v2 = b.unwrap_or(0.5); // retail-default: glTF alphaCutoff\n"
                "    // STAND-IN(#946): use static clear day palette until dynamic weather lands\n"
                "    let v3 = c\n"
                "        .unwrap_or(DEFAULT_SUNLIGHT_SCALE);\n"
                "    // STAND-IN(#946): too short\n"
                "    let v4 = d.unwrap_or(2.8);\n"
                "    // retail-default:\n"
                "    let v5 = e.unwrap_or(3.0);\n"
                "    v1 + v2 + v3 + v4 + v5\n"
                "}\n",
            )
            cfg = defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            _, hits = defaults_ratchet.scan_diff(root, "HEAD~1", cfg)
            self.assertEqual([(h.line, h.category) for h in hits], [(9, "unwrap-or-literal"), (11, "unwrap-or-literal")])
            self.assertIn("four words", hits[0].problem)

    def test_blank_line_breaks_exemption_and_test_code_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "pub fn f(a: Option<f32>) -> f32 {\n"
                "    // retail-default: SPEC-1 section 2.6\n"
                "\n"
                "    a.unwrap_or(1.0)\n"
                "}\n"
                "#[cfg(test)]\n"
                "mod tests {\n"
                "    fn helper(a: Option<f32>) -> f32 { a.unwrap_or(9.9) }\n"
                "}\n",
            )
            cfg = defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            _, hits = defaults_ratchet.scan_diff(root, "HEAD~1", cfg)
            self.assertEqual([(h.line, h.category) for h in hits], [(4, "unwrap-or-literal")])

    def test_moved_lines_and_non_scoped_module_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "pub fn sky(has: bool, v: Option<f32>) -> f32 {\n"
                "    if has { 1.0 } else { v.unwrap_or(4.0) }\n"
                "}\n",
            )
            # Move identical line and touch out-of-scope file.
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "// shifted\n"
                "pub fn sky(has: bool, v: Option<f32>) -> f32 {\n"
                "    if has { 1.0 } else { v.unwrap_or(4.0) }\n"
                "}\n",
            )
            self._commit_change(
                root,
                "crates/engine/src/non_retail.rs",
                "pub fn ui_pad(v: Option<u32>) -> u32 {\n    v.unwrap_or(16)\n}\n",
            )
            cfg = defaults_ratchet.load_config(root / "tools/defaults_ratchet/config.json")
            _, hits = defaults_ratchet.scan_diff(root, "HEAD~2", cfg)
            self.assertEqual(hits, [])

    def test_ratchet_baseline_may_only_go_down(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(Path(tmp))
            # Add an existing hit and record baseline = 1.
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "pub fn f(a: Option<f32>) -> f32 { a.unwrap_or(1.0) }\n",
                msg="add one",
            )
            self.assertEqual(
                defaults_ratchet.main(["--root", str(root), "--update-baseline"]),
                0,
            )
            run_git(root, "add", "tools/defaults_ratchet/baseline.json")
            run_git(root, "commit", "-m", "record baseline")

            # Rising above baseline fails.
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "pub fn f(a: Option<f32>, b: Option<f32>) -> f32 { a.unwrap_or(1.0) + b.unwrap_or(2.0) }\n",
                msg="add second",
            )
            self.assertEqual(defaults_ratchet.main(["--root", str(root), "--check"]), 1)

            # Trying to raise baseline.json above --base fails too.
            self.assertEqual(
                defaults_ratchet.main(["--root", str(root), "--update-baseline"]),
                0,
            )
            run_git(root, "add", "tools/defaults_ratchet/baseline.json")
            run_git(root, "commit", "-m", "try raise baseline")
            self.assertEqual(
                defaults_ratchet.main(["--root", str(root), "--check", "--base", "HEAD~2"]),
                1,
            )

            # Fixing the site drops count below baseline: passes non-strict, fails --strict.
            run_git(root, "reset", "--hard", "HEAD~2")
            self._commit_change(
                root,
                "crates/engine/src/color_pipeline.rs",
                "pub fn f(a: Option<f32>) -> Option<f32> { a }\n",
                msg="fix site",
            )
            self.assertEqual(defaults_ratchet.main(["--root", str(root), "--check"]), 0)
            self.assertEqual(defaults_ratchet.main(["--root", str(root), "--check", "--strict"]), 1)
            self.assertEqual(
                defaults_ratchet.main(["--root", str(root), "--update-baseline"]),
                0,
            )
            baseline_data = json.loads(
                (root / "tools/defaults_ratchet/baseline.json").read_text(encoding="utf-8")
            )
            self.assertEqual(baseline_data["counts"]["engine"], 0)
            self.assertEqual(baseline_data["revision"], 3)


if __name__ == "__main__":
    unittest.main()
