"""Unit tests for the Skyrim flag inventory and disposition ratchet guard."""

import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import inventory as inv


def make_fixture(directory, *, disposition="respected", issue="", reason="Implemented in engine.", extra_flag=False):
    root = Path(directory)
    here = root / "tools/flag_inventory"
    here.mkdir(parents=True)
    src = root / "crates/engine/src/example.rs"
    src.parent.mkdir(parents=True)
    src.write_text("pub const EXAMPLE_FLAG: u32 = 0x1;\n", encoding="utf-8")

    catalog = [
        {
            "id": "EXAM.DATA.flag_one",
            "container": "EXAM.DATA",
            "bit": 0,
            "name": "FlagOne",
            "meaning": "Example flag",
            "source": "xEdit 053da98 wbDefinitionsTES5.pas",
            "group": "other",
        }
    ]
    dispositions = {
        "EXAM.DATA.flag_one": {
            "disposition": disposition,
            "owner": "Mudcrab maintainers",
            "reason": reason,
            "evidence": [{"path": "crates/engine/src/example.rs", "anchor": "EXAMPLE_FLAG"}],
        }
    }
    if disposition in inv.RATCHET_DISPOSITIONS or issue:
        dispositions["EXAM.DATA.flag_one"]["issue"] = issue

    if extra_flag:
        catalog.append(
            {
                "id": "EXAM.DATA.flag_two",
                "container": "EXAM.DATA",
                "bit": 1,
                "name": "FlagTwo",
                "meaning": "Second example flag",
                "source": "xEdit 053da98 wbDefinitionsTES5.pas",
                "group": "other",
            }
        )
        dispositions["EXAM.DATA.flag_two"] = {
            "disposition": "dropped",
            "owner": "Mudcrab maintainers",
            "reason": "Ignored by converter.",
            "evidence": [{"path": "crates/engine/src/example.rs", "anchor": "EXAMPLE_FLAG"}],
            "issue": "",
        }

    baseline_count = sum(
        1 for entry in dispositions.values() if entry["disposition"] in inv.RATCHET_DISPOSITIONS
    )
    (here / "catalog.json").write_text(json.dumps(catalog, indent=2) + "\n", encoding="utf-8")
    (here / "dispositions.json").write_text(
        json.dumps({"format_version": 1, "dispositions": dispositions}, indent=2) + "\n",
        encoding="utf-8",
    )
    (here / "baseline.json").write_text(
        json.dumps({"format_version": 1, "dropped_plus_partial": baseline_count}, indent=2) + "\n",
        encoding="utf-8",
    )
    return root, here, src


def run_check(root, here, *argv):
    with contextlib.redirect_stdout(io.StringIO()):
        return inv.run(list(argv) if argv else ["check"], root=root, here=here)


class FlagInventoryTests(unittest.TestCase):
    def test_valid_inventory_passes_and_supports_check_alias(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="dropped", issue="#42", reason="")
            result = run_check(root, here, "check")
            self.assertEqual(result["total"], 1)
            self.assertEqual(result["dropped_plus_partial"], 1)
            self.assertEqual(result["baseline"], 1)
            alias_result = run_check(root, here, "--check")
            self.assertEqual(alias_result["dropped_plus_partial"], 1)

    def test_missing_catalog_id_in_dispositions_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, extra_flag=True)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"].pop("EXAM.DATA.flag_two")
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"catalog flag\(s\) without disposition.*EXAM\.DATA\.flag_two"):
                run_check(root, here)

    def test_unknown_disposition_id_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.ghost"] = doc["dispositions"]["EXAM.DATA.flag_one"]
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"disposition id\(s\) absent from catalog.*EXAM\.DATA\.ghost"):
                run_check(root, here)

    def test_broken_evidence_anchor_or_missing_file_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, src = make_fixture(directory)
            src.write_text("pub const RENAMED_FLAG: u32 = 0x1;\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"broken evidence anchor in crates/engine/src/example\.rs"):
                run_check(root, here)

            src.unlink()
            with self.assertRaisesRegex(ValueError, r"missing file: crates/engine/src/example\.rs"):
                run_check(root, here)

    def test_non_relative_or_escaping_evidence_path_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.flag_one"]["evidence"] = [
                {"path": "../escape.rs", "anchor": "EXAMPLE_FLAG"}
            ]
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"evidence path must be repo-relative"):
                run_check(root, here)

    def test_invalid_disposition_value_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.flag_one"]["disposition"] = "uncertain"
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"invalid disposition 'uncertain'"):
                run_check(root, here)

    def test_dropped_and_partial_require_issue_or_reason(self):
        for disp in ("dropped", "partial"):
            with self.subTest(disposition=disp):
                with tempfile.TemporaryDirectory() as directory:
                    root, here, _ = make_fixture(directory, disposition=disp, issue="", reason="   ")
                    with self.assertRaisesRegex(
                        ValueError, rf"{disp} entry must have an issue number or a written reason"
                    ):
                        run_check(root, here)

                with tempfile.TemporaryDirectory() as directory:
                    root, here, _ = make_fixture(directory, disposition=disp, issue="", reason="Tracked by audit.")
                    result = run_check(root, here)
                    self.assertEqual(result["dropped_plus_partial"], 1)

                with tempfile.TemporaryDirectory() as directory:
                    root, here, _ = make_fixture(directory, disposition=disp, issue="#1234", reason="")
                    result = run_check(root, here)
                    self.assertEqual(result["dropped_plus_partial"], 1)

    def test_dropped_plus_partial_ratchet_blocks_increases_and_allows_decreases(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="respected", extra_flag=True)
            # Baseline starts at 1 (flag_two is dropped). Changing flag_one to partial raises count to 2.
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.flag_one"]["disposition"] = "partial"
            doc["dispositions"]["EXAM.DATA.flag_one"]["issue"] = ""
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"dropped\+partial count rose from baseline 1 to 2"):
                run_check(root, here)

            # Fixing flag_two to respected lowers count to 0 (< baseline 1), which is allowed.
            doc["dispositions"]["EXAM.DATA.flag_one"]["disposition"] = "respected"
            doc["dispositions"]["EXAM.DATA.flag_one"].pop("issue", None)
            doc["dispositions"]["EXAM.DATA.flag_two"]["disposition"] = "respected"
            doc["dispositions"]["EXAM.DATA.flag_two"].pop("issue", None)
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            result = run_check(root, here)
            self.assertEqual(result["dropped_plus_partial"], 0)
            self.assertEqual(result["baseline"], 1)

            # update-baseline tightens baseline.json to 0.
            run_check(root, here, "update-baseline")
            baseline_doc = json.loads((here / "baseline.json").read_text(encoding="utf-8"))
            self.assertEqual(baseline_doc["dropped_plus_partial"], 0)

    def test_catalog_source_rejects_local_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            catalog = json.loads((here / "catalog.json").read_text(encoding="utf-8"))
            for bad_source in (
                "/Users/taylor/xedit/wbDefinitionsTES5.pas:10",
                "xEdit 053da98 C:/work/wbDefinitionsTES5.pas",
            ):
                catalog[0]["source"] = bad_source
                (here / "catalog.json").write_text(json.dumps(catalog), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, r"source must not contain local paths"):
                    run_check(root, here)

    def test_boolean_format_version_rejected_and_update_baseline_cannot_raise(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="dropped", issue="#42", reason="", extra_flag=True)
            disp_doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            disp_doc["format_version"] = True
            (here / "dispositions.json").write_text(json.dumps(disp_doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"unsupported dispositions format_version: True"):
                run_check(root, here)

            disp_doc["format_version"] = 1
            (here / "dispositions.json").write_text(json.dumps(disp_doc), encoding="utf-8")
            (here / "baseline.json").write_text(
                json.dumps({"format_version": 1, "dropped_plus_partial": 1}) + "\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, r"refusing to raise baseline from 1 to 2"):
                run_check(root, here, "update-baseline")

    def test_base_ceiling_and_explicit_root_honored(self):
        with tempfile.TemporaryDirectory() as temp_dir, tempfile.TemporaryDirectory() as real_dir:
            temp_root, here, temp_src = make_fixture(temp_dir, disposition="dropped", issue="#1061", reason="")
            base_file = temp_root / "base_baseline.json"
            base_file.write_text(json.dumps({"format_version": 1, "dropped_plus_partial": 0}), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, r"without a non-empty raise_reason naming an issue"):
                run_check(temp_root, here, "check", "--base-baseline", str(base_file))

            (here / "baseline.json").write_text(
                json.dumps(
                    {
                        "format_version": 1,
                        "dropped_plus_partial": 1,
                        "raise_reason": "Enum audit reclassified mode 6 (#1061).",
                    }
                ),
                encoding="utf-8",
            )
            real_root = Path(real_dir)
            real_src = real_root / "crates/engine/src/example.rs"
            real_src.parent.mkdir(parents=True)
            real_src.write_text("pub const EXAMPLE_FLAG: u32 = 0x1;\n", encoding="utf-8")
            temp_src.write_text("pub const BROKEN: u32 = 0;\n", encoding="utf-8")
            result = run_check(
                temp_root, here, "check", "--root", str(real_root), "--base-baseline", str(base_file)
            )
            self.assertEqual(result["base_baseline"], 0)
            self.assertIn("#1061", result["base_note"])

    def test_cli_returns_nonzero_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="dropped", issue="", reason="")
            proc = subprocess.run(
                [
                    sys.executable,
                    "-B",
                    str(Path(__file__).resolve().parent / "inventory.py"),
                    "check",
                    "--root",
                    str(root),
                    "--catalog",
                    str(here / "catalog.json"),
                    "--dispositions",
                    str(here / "dispositions.json"),
                    "--baseline",
                    str(here / "baseline.json"),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("must have an issue number or a written reason", proc.stderr)


if __name__ == "__main__":
    unittest.main()
