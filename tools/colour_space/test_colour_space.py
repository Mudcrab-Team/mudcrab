import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import colour_space  # noqa: E402


DEFAULT_SCOPE_JSON = json.dumps(
    {
        "format_version": 1,
        "paths": [
            "crates/engine/src/emittance.rs",
            "crates/engine/src/underwater*.rs",
            "crates/engine/src/weather_sky.rs",
            "crates/engine/src/retail_lighting.rs",
            "crates/engine/src/sky.rs",
            "crates/engine/src/render.rs",
            "crates/engine/src/world/environment/interior.rs",
            "crates/engine/src/lights.rs",
            "crates/engine/src/grass_render.rs",
            "crates/engine/src/shaders/*.wgsl",
        ],
    }
)


class ColourSpaceTests(unittest.TestCase):
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
        subprocess.check_call(
            [
                "git",
                "-C",
                str(self.root),
                "-c",
                "user.name=Tests",
                "-c",
                "user.email=tests@example.invalid",
                *args,
            ]
        )

    def commit(self, files):
        self.write(files)
        self.git("add", "-A")
        self.git("commit", "-qm", "change", "--allow-empty")

    def run_main(self, *args):
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            code = colour_space.main(["--root", str(self.root), *args])
        return code, output.getvalue()

    def run_diff_check(self, head, base=None, extra_args=()):
        base_files = {
            "tools/colour_space/scope.json": DEFAULT_SCOPE_JSON,
            "crates/engine/src/sky.rs": "pub fn keep() {}\n",
            **(base or {}),
        }
        self.commit(base_files)
        self.git("branch", "base")
        self.commit(head)
        return self.run_main("--base", "base", *extra_args)

    def test_flags_each_rust_and_wgsl_pattern_on_added_lines(self):
        cases = [
            ("crates/engine/src/sky.rs", "fn a() { let _ = Color::srgb(0.1, 0.2, 0.3); }\n", "Color::srgb("),
            ("crates/engine/src/sky.rs", "fn a() { let _ = Color::srgb_u8(1, 2, 3); }\n", "Color::srgb_u8("),
            ("crates/engine/src/sky.rs", "fn a() { let _ = Color::srgba(0.1, 0.2, 0.3, 1.0); }\n", "Color::srgba("),
            ("crates/engine/src/render.rs", "fn a(c: Color) { let _ = c.to_linear(); }\n", ".to_linear()"),
            ("crates/engine/src/lights.rs", "fn a(c: Color) { let _ = LinearRgba::from(c); }\n", "LinearRgba::from("),
            (
                "crates/engine/src/grass_render.rs",
                "fn a() { let _ = TextureFormat::Rgba8UnormSrgb; }\n",
                "Srgb texture format",
            ),
            (
                "crates/engine/src/underwater_pass.rs",
                "fn a() { let _ = Bc3RgbaUnormSrgb; }\n",
                "Srgb texture format",
            ),
            (
                "crates/engine/src/shaders/water.wgsl",
                "fn f(c: vec3<f32>) -> vec3<f32> { return srgb_to_linear(c); }\n",
                "srgb_to_linear(",
            ),
            (
                "crates/engine/src/shaders/water.wgsl",
                "fn f(c: vec3<f32>) -> vec3<f32> { return sky_srgb_to_linear(c); }\n",
                "sky_srgb_to_linear(",
            ),
        ]
        for rel_path, snippet, expected_label in cases:
            with self.subTest(pattern=expected_label, path=rel_path):
                sub = tempfile.TemporaryDirectory()
                self.addCleanup(sub.cleanup)
                sub_root = Path(sub.name)
                subprocess.check_call(["git", "-C", str(sub_root), "init", "-q"])
                (sub_root / "tools/colour_space").mkdir(parents=True)
                (sub_root / "tools/colour_space/scope.json").write_text(DEFAULT_SCOPE_JSON, encoding="utf-8")
                (sub_root / "crates/engine/src/shaders").mkdir(parents=True)
                (sub_root / "crates/engine/src/sky.rs").write_text("// base\n", encoding="utf-8")
                subprocess.check_call(
                    [
                        "git",
                        "-C",
                        str(sub_root),
                        "-c",
                        "user.name=Tests",
                        "-c",
                        "user.email=tests@example.invalid",
                        "add",
                        "-A",
                    ]
                )
                subprocess.check_call(
                    [
                        "git",
                        "-C",
                        str(sub_root),
                        "-c",
                        "user.name=Tests",
                        "-c",
                        "user.email=tests@example.invalid",
                        "commit",
                        "-qm",
                        "base",
                    ]
                )
                subprocess.check_call(["git", "-C", str(sub_root), "branch", "base"])
                target = sub_root / rel_path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(snippet, encoding="utf-8")
                subprocess.check_call(["git", "-C", str(sub_root), "add", "-A"])
                subprocess.check_call(
                    [
                        "git",
                        "-C",
                        str(sub_root),
                        "-c",
                        "user.name=Tests",
                        "-c",
                        "user.email=tests@example.invalid",
                        "commit",
                        "-qm",
                        "head",
                    ]
                )
                output = io.StringIO()
                with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
                    code = colour_space.main(["--root", str(sub_root), "--base", "base"])
                self.assertEqual(code, 1, output.getvalue())
                self.assertIn(expected_label, output.getvalue())

    def test_exemptions_on_same_line_or_line_above_pass(self):
        head = {
            "crates/engine/src/sky.rs": (
                "fn ok() {\n"
                "    // colour-space: mudcrab-only because legacy look keeps sRGB\n"
                "    let a = Color::srgb(0.1, 0.2, 0.3);\n"
                "    let b = Color::srgb_u8(1, 2, 3); // colour-space: ui\n"
                "    // colour-space: SPEC-1 §2.4\n"
                "    let c = b.to_linear();\n"
                "}\n"
            ),
            "crates/engine/src/shaders/retail_hdr_tonemap.wgsl": (
                "fn out(c: vec3<f32>) -> vec3<f32> {\n"
                "    // colour-space: swapchain-cancel\n"
                "    return srgb_to_linear(c);\n"
                "}\n"
            ),
        }
        code, output = self.run_diff_check(head)
        self.assertEqual(code, 0, output)

    def test_empty_marker_string_literal_marker_and_two_lines_above_do_not_exempt(self):
        cases = [
            "// colour-space:\nlet a = Color::srgb(0.1, 0.2, 0.3);\n",
            'let s = "// colour-space: ui";\nlet a = Color::srgb(0.1, 0.2, 0.3);\n',
            "// colour-space: ui\nlet x = 1;\nlet a = Color::srgb(0.1, 0.2, 0.3);\n",
        ]
        for index, snippet in enumerate(cases):
            with self.subTest(case=index):
                sub = tempfile.TemporaryDirectory()
                self.addCleanup(sub.cleanup)
                self.root = Path(sub.name)
                subprocess.check_call(["git", "-C", str(self.root), "init", "-q"])
                code, output = self.run_diff_check({"crates/engine/src/sky.rs": snippet})
                self.assertEqual(code, 1, output)

    def test_skips_comments_strings_wgsl_definitions_tests_and_non_texture_srgb(self):
        head = {
            "crates/engine/src/sky.rs": (
                "/// Example: `Color::srgb(1.0, 0.0, 0.0)` and `.to_linear()`\n"
                "const DOC: &str = \"Color::srgb_u8(1, 2, 3)\";\n"
                "const RAW: &str = r#\"LinearRgba::from(x)\"#;\n"
                "enum Mode { SnapshotSrgb, RecordSrgb, DiffuseSrgb, ColorSrgb }\n"
                "const FMT: TextureFormat = TextureFormat::Rgba8Unorm;\n"
                "#[cfg(test)]\n"
                "mod tests {\n"
                "    #[test]\n"
                "    fn check() { let _ = Color::srgb(1.0, 0.0, 0.0).to_linear(); }\n"
                "}\n"
            ),
            "crates/engine/src/shaders/sky.wgsl": (
                "// srgb_to_linear(x)\n"
                "fn srgb_to_linear(colour: vec3<f32>) -> vec3<f32> { return colour; }\n"
                "fn sky_srgb_to_linear(colour: vec3<f32>) -> vec3<f32> { return colour; }\n"
            ),
            "crates/engine/src/color_pipeline.rs": (
                "fn ui_only() { let _ = Color::srgb(0.5, 0.5, 0.5).to_linear(); }\n"
            ),
        }
        code, output = self.run_diff_check(head)
        self.assertEqual(code, 0, output)

    def test_unchanged_and_moved_lines_pass_on_diff(self):
        line = "    let existing = Color::srgb(0.1, 0.2, 0.3);\n"
        base = {
            "crates/engine/src/sky.rs": "fn a() {\n" + line + "}\n",
            "crates/engine/src/lights.rs": "fn b() {}\n",
        }
        head = {
            "crates/engine/src/sky.rs": "fn a() {\n}\n",
            "crates/engine/src/lights.rs": "fn b() {\n    // reindented move:\n" + line.strip() + "\n}\n",
        }
        code, output = self.run_diff_check(head, base=base)
        self.assertEqual(code, 0, output)

    def test_ratchet_passes_at_or_below_baseline_and_fails_above(self):
        self.commit(
            {
                "tools/colour_space/scope.json": DEFAULT_SCOPE_JSON,
                "crates/engine/src/emittance.rs": (
                    "fn f() {\n"
                    "    let a = Color::srgb_u8(1, 2, 3);\n"
                    "    let b = a.to_linear();\n"
                    "}\n"
                ),
            }
        )
        code, output = self.run_main("--update-baseline")
        self.assertEqual(code, 0, output)
        baseline = json.loads((self.root / "tools/colour_space/baseline.json").read_text())
        self.assertEqual(baseline["unmarked_total"], 2)
        self.assertEqual(baseline["counts"], {"crates/engine/src/emittance.rs": 2})
        self.assertNotIn(b"\r", (self.root / "tools/colour_space/baseline.json").read_bytes())

        code, output = self.run_main("--ratchet")
        self.assertEqual(code, 0, output)

        # Adding an unmarked site fails the ratchet.
        self.write(
            {
                "crates/engine/src/underwater.rs": "fn g() { let _ = Color::srgb(0.2, 0.3, 0.4); }\n"
            }
        )
        code, output = self.run_main("--ratchet")
        self.assertEqual(code, 1, output)
        self.assertIn("exceeds baseline 2", output)

        # Lowering unmarked hits passes normally, fails under --strict.
        self.write(
            {
                "crates/engine/src/underwater.rs": "",
                "crates/engine/src/emittance.rs": (
                    "fn f() {\n"
                    "    // colour-space: mudcrab-only because legacy\n"
                    "    let a = Color::srgb_u8(1, 2, 3);\n"
                    "    let b = a.to_linear();\n"
                    "}\n"
                ),
            }
        )
        code, output = self.run_main("--ratchet")
        self.assertEqual(code, 0, output)
        self.assertIn("fell from 2 to 1", output)

        code, output = self.run_main("--ratchet", "--strict")
        self.assertEqual(code, 1, output)

    def test_ratchet_rejects_raised_baseline_against_base(self):
        self.commit(
            {
                "tools/colour_space/scope.json": DEFAULT_SCOPE_JSON,
                "tools/colour_space/baseline.json": json.dumps(
                    {"format_version": 1, "unmarked_total": 1, "counts": {"crates/engine/src/sky.rs": 1}}
                ),
                "crates/engine/src/sky.rs": "fn f() { let _ = Color::srgb(0.1, 0.2, 0.3); }\n",
            }
        )
        self.git("branch", "base")
        self.commit(
            {
                "tools/colour_space/baseline.json": json.dumps(
                    {"format_version": 1, "unmarked_total": 2, "counts": {"crates/engine/src/sky.rs": 2}}
                ),
            }
        )
        code, output = self.run_main("--base", "base", "--ratchet")
        self.assertEqual(code, 1, output)
        self.assertIn("rose from 1 to 2", output)

    def test_audit_and_unknown_base_and_custom_scope(self):
        self.commit(
            {
                "tools/colour_space/scope.json": json.dumps(
                    {"format_version": 1, "paths": ["crates/engine/src/custom.rs"]}
                ),
                "crates/engine/src/custom.rs": "fn f() { let _ = Color::srgb(0.1, 0.2, 0.3); }\n",
            }
        )
        code, output = self.run_main("--audit")
        self.assertEqual(code, 0, output)
        self.assertIn("crates/engine/src/custom.rs:1", output)
        self.assertIn("1 unmarked hit(s)", output)

        code, output = self.run_main("--base", "no-such-ref")
        self.assertEqual(code, 2, output)


if __name__ == "__main__":
    unittest.main()
