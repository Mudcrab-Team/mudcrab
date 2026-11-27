"""Unit tests for the Skyrim record field inventory and disposition ratchet guard."""

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


def make_fixture(
    directory,
    *,
    disposition="respected",
    issue="",
    spec="",
    reason="Implemented in engine.",
    extra_field=False,
    grouped_sni=False,
):
    root = Path(directory)
    here = root / "tools/field_inventory"
    here.mkdir(parents=True)
    src = root / "crates/engine/src/example.rs"
    src.parent.mkdir(parents=True)
    src.write_text("pub const EXAMPLE_FIELD: u32 = 1;\n", encoding="utf-8")

    catalog = [
        {
            "id": "EXAM.DATA.field_one",
            "signature_or_block": "EXAM",
            "subrecord": "DATA",
            "field": "field_one",
            "offset": 0,
            "type": "uint32",
            "meaning": "Example field",
            "source": "xEdit 053da98 wbDefinitionsTES5.pas",
            "kind": "esm",
            "occurrences": 10,
            "size_bytes": 4,
            "conditional": None,
        }
    ]
    entry = {
        "disposition": disposition,
        "owner": "Mudcrab maintainers",
        "reason": reason,
        "evidence": [{"path": "crates/engine/src/example.rs", "anchor": "EXAMPLE_FIELD"}],
    }
    if issue:
        entry["issue"] = issue
    if spec:
        entry["spec"] = spec
    dispositions = {"EXAM.DATA.field_one": entry}

    if extra_field:
        catalog.append(
            {
                "id": "EXAM.DATA.field_two",
                "signature_or_block": "EXAM",
                "subrecord": "DATA",
                "field": "field_two",
                "offset": 4,
                "type": "float",
                "meaning": "Second example field",
                "source": "xEdit 053da98 wbDefinitionsTES5.pas",
                "kind": "esm",
                "occurrences": 10,
                "size_bytes": 4,
            }
        )
        dispositions["EXAM.DATA.field_two"] = {
            "disposition": "dropped",
            "owner": "Mudcrab maintainers",
            "reason": "Ignored by converter.",
            "evidence": [{"path": "crates/engine/src/example.rs", "anchor": "EXAMPLE_FIELD"}],
            "issue": "#1062",
        }

    sni_groups = {}
    if grouped_sni:
        catalog.append(
            {
                "id": "UNIM.DATA.field_three",
                "signature_or_block": "UNIM",
                "subrecord": "DATA",
                "field": "field_three",
                "offset": 0,
                "type": "uint32",
                "meaning": "Unimplemented system field",
                "source": "xEdit 053da98 wbDefinitionsTES5.pas",
                "kind": "esm",
                "occurrences": 5,
            }
        )
        sni_groups["UNIM"] = {
            "disposition": "system-not-implemented",
            "missing_system": "Missing UNIM gameplay system",
            "owner": "Mudcrab maintainers",
            "reason": "UNIM system is not implemented in crates/engine.",
            "evidence": [{"path": "crates/engine/src/example.rs", "anchor": "EXAMPLE_FIELD"}],
            "field_ids": ["UNIM.DATA.field_three"],
        }

    baseline_count = sum(
        1 for item in dispositions.values() if item["disposition"] in inv.RATCHET_DISPOSITIONS
    )
    (here / "catalog.json").write_text(json.dumps(catalog, indent=2) + "\n", encoding="utf-8")
    payload = {"format_version": 1, "dispositions": dispositions}
    if sni_groups:
        payload["system_not_implemented_by_signature"] = sni_groups
    (here / "dispositions.json").write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    (here / "baseline.json").write_text(
        json.dumps({"format_version": 1, "dropped_plus_partial": baseline_count}, indent=2) + "\n",
        encoding="utf-8",
    )
    return root, here, src


def run_check(root, here, *argv):
    with contextlib.redirect_stdout(io.StringIO()):
        return inv.run(list(argv) if argv else ["check"], root=root, here=here)


class FieldInventoryTests(unittest.TestCase):
    def test_valid_inventory_and_grouped_sni_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(
                directory, disposition="partial", spec="SPEC-1 2.1-b", reason="", grouped_sni=True
            )
            result = run_check(root, here, "check")
            self.assertEqual(result["total"], 2)
            self.assertEqual(result["dropped_plus_partial"], 1)
            self.assertEqual(result["counts"]["system-not-implemented"], 1)
            alias_result = run_check(root, here, "--check")
            self.assertEqual(alias_result["baseline"], 1)

    def test_covered_by_flag_audit_disposition_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(
                directory, disposition="covered-by-flag-audit", reason="Guarded by flag_inventory."
            )
            result = run_check(root, here, "check")
            self.assertEqual(result["counts"]["covered-by-flag-audit"], 1)

    def test_missing_catalog_id_in_dispositions_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, extra_field=True)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"].pop("EXAM.DATA.field_two")
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"catalog field\(s\) without disposition.*EXAM\.DATA\.field_two"):
                run_check(root, here)

    def test_unknown_disposition_id_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.ghost"] = doc["dispositions"]["EXAM.DATA.field_one"]
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"disposition id\(s\) absent from catalog.*EXAM\.DATA\.ghost"):
                run_check(root, here)

    def test_signature_group_rejects_overlap_and_wrong_signature(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, grouped_sni=True)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["system_not_implemented_by_signature"]["UNIM"]["field_ids"].append("EXAM.DATA.field_one")
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"also in 'dispositions'"):
                run_check(root, here)

            doc["system_not_implemented_by_signature"]["EXAM"] = doc["system_not_implemented_by_signature"].pop("UNIM")
            doc["system_not_implemented_by_signature"]["EXAM"]["field_ids"] = ["UNIM.DATA.field_three"]
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"belongs to signature_or_block 'UNIM'"):
                run_check(root, here)

    def test_broken_evidence_anchor_or_missing_file_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, src = make_fixture(directory)
            src.write_text("pub const RENAMED_FIELD: u32 = 1;\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"broken evidence anchor in crates/engine/src/example\.rs"):
                run_check(root, here)

            src.unlink()
            with self.assertRaisesRegex(ValueError, r"missing file: crates/engine/src/example\.rs"):
                run_check(root, here)

    def test_non_relative_evidence_path_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.field_one"]["evidence"] = [
                {"path": "../escape.rs", "anchor": "EXAMPLE_FIELD"}
            ]
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"evidence path must be repo-relative"):
                run_check(root, here)

    def test_invalid_disposition_value_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.field_one"]["disposition"] = "uncertain"
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"invalid disposition 'uncertain'"):
                run_check(root, here)

    def test_dropped_and_partial_require_issue_spec_or_reason(self):
        for disp in ("dropped", "partial"):
            with self.subTest(disposition=disp):
                with tempfile.TemporaryDirectory() as directory:
                    root, here, _ = make_fixture(directory, disposition=disp, issue="", spec="", reason="   ")
                    with self.assertRaisesRegex(
                        ValueError, rf"{disp} entry must have an issue number, spec reference, or a written reason"
                    ):
                        run_check(root, here)

                with tempfile.TemporaryDirectory() as directory:
                    root, here, _ = make_fixture(directory, disposition=disp, spec="SPEC-1 2.1-b", reason="")
                    result = run_check(root, here)
                    self.assertEqual(result["dropped_plus_partial"], 1)

                with tempfile.TemporaryDirectory() as directory:
                    root, here, _ = make_fixture(directory, disposition=disp, issue="#1062", reason="")
                    result = run_check(root, here)
                    self.assertEqual(result["dropped_plus_partial"], 1)

    def test_dropped_plus_partial_ratchet_blocks_increases_and_allows_decreases(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="respected", extra_field=True)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["dispositions"]["EXAM.DATA.field_one"]["disposition"] = "partial"
            doc["dispositions"]["EXAM.DATA.field_one"]["issue"] = "#1062"
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"dropped\+partial count rose from baseline 1 to 2"):
                run_check(root, here)

            doc["dispositions"]["EXAM.DATA.field_one"]["disposition"] = "respected"
            doc["dispositions"]["EXAM.DATA.field_one"].pop("issue", None)
            doc["dispositions"]["EXAM.DATA.field_two"]["disposition"] = "respected"
            doc["dispositions"]["EXAM.DATA.field_two"].pop("issue", None)
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            result = run_check(root, here)
            self.assertEqual(result["dropped_plus_partial"], 0)
            self.assertEqual(result["baseline"], 1)

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
                "xEdit 053da98 /tmp/wbDefinitionsTES5.pas",
            ):
                catalog[0]["source"] = bad_source
                (here / "catalog.json").write_text(json.dumps(catalog), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, r"source must not contain local paths"):
                    run_check(root, here)

    def test_negative_occurrences_and_size_bytes_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            catalog = json.loads((here / "catalog.json").read_text(encoding="utf-8"))
            catalog[0]["occurrences"] = 0
            catalog[0]["size_bytes"] = 0
            (here / "catalog.json").write_text(json.dumps(catalog), encoding="utf-8")
            self.assertEqual(run_check(root, here)["total"], 1)

            catalog[0]["occurrences"] = -1
            (here / "catalog.json").write_text(json.dumps(catalog), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"invalid occurrences"):
                run_check(root, here)

            catalog[0]["occurrences"] = None
            catalog[0]["size_bytes"] = -4
            (here / "catalog.json").write_text(json.dumps(catalog), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"invalid size_bytes"):
                run_check(root, here)

    def test_boolean_format_version_rejected_in_dispositions_and_baseline(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory)
            disp_doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            disp_doc["format_version"] = True
            (here / "dispositions.json").write_text(json.dumps(disp_doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"unsupported dispositions format_version: True"):
                run_check(root, here)

            disp_doc["format_version"] = 1
            (here / "dispositions.json").write_text(json.dumps(disp_doc), encoding="utf-8")
            base_doc = json.loads((here / "baseline.json").read_text(encoding="utf-8"))
            base_doc["format_version"] = True
            (here / "baseline.json").write_text(json.dumps(base_doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"unsupported baseline format_version: True"):
                run_check(root, here)

    def test_signature_group_requires_non_empty_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, grouped_sni=True)
            doc = json.loads((here / "dispositions.json").read_text(encoding="utf-8"))
            doc["system_not_implemented_by_signature"]["UNIM"]["evidence"] = []
            (here / "dispositions.json").write_text(json.dumps(doc), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"must cite at least one evidence anchor"):
                run_check(root, here)

    def test_update_baseline_never_raises_ceiling(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="dropped", issue="#1062", reason="", extra_field=True)
            # Lower stored ceiling to 1 while dispositions currently have 2 dropped fields.
            (here / "baseline.json").write_text(
                json.dumps({"format_version": 1, "dropped_plus_partial": 1}) + "\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, r"refusing to raise baseline from 1 to 2"):
                run_check(root, here, "update-baseline")
            self.assertEqual(
                json.loads((here / "baseline.json").read_text(encoding="utf-8"))["dropped_plus_partial"], 1
            )

    def test_base_ceiling_check_requires_issue_linked_raise_reason(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="dropped", issue="#1062", reason="")
            base_file = root / "base_baseline.json"
            base_file.write_text(json.dumps({"format_version": 1, "dropped_plus_partial": 0}), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, r"without a non-empty raise_reason naming an issue"):
                run_check(root, here, "check", "--base-baseline", str(base_file))

            (here / "baseline.json").write_text(
                json.dumps({"format_version": 1, "dropped_plus_partial": 1, "raise_reason": "no issue"}),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, r"without a non-empty raise_reason naming an issue"):
                run_check(root, here, "check", "--base-baseline", str(base_file))

            (here / "baseline.json").write_text(
                json.dumps(
                    {
                        "format_version": 1,
                        "dropped_plus_partial": 1,
                        "raise_reason": "Reclassified by enum audit (#1061).",
                    }
                ),
                encoding="utf-8",
            )
            result = run_check(root, here, "check", "--base-baseline", str(base_file))
            self.assertEqual(result["base_baseline"], 0)
            self.assertIn("#1061", result["base_note"])

    def test_base_ref_with_git_show_handles_initial_and_raised_baselines(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="dropped", issue="#1062", reason="")
            subprocess.run(["git", "-C", str(root), "init"], check=True, capture_output=True)
            subprocess.run(["git", "-C", str(root), "config", "user.email", "test@example.invalid"], check=True)
            subprocess.run(["git", "-C", str(root), "config", "user.name", "Test"], check=True)
            subprocess.run(["git", "-C", str(root), "add", "crates"], check=True)
            subprocess.run(["git", "-C", str(root), "commit", "-m", "initial without baseline"], check=True, capture_output=True)

            # Initial baseline absent at HEAD passes with notice.
            initial_result = run_check(root, here, "check", "--base-ref", "HEAD")
            self.assertIsNone(initial_result["base_baseline"])
            self.assertIn("initial baseline", initial_result["base_note"])

            # Commit baseline=0, then raise working tree to 1 without raise_reason -> fails.
            (here / "baseline.json").write_text(json.dumps({"format_version": 1, "dropped_plus_partial": 0}), encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "tools"], check=True)
            subprocess.run(["git", "-C", str(root), "commit", "-m", "add baseline"], check=True, capture_output=True)
            (here / "baseline.json").write_text(json.dumps({"format_version": 1, "dropped_plus_partial": 1}), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"rose from base 0 to 1"):
                run_check(root, here, "check", "--base-ref", "HEAD")

    def test_explicit_root_overrides_caller_injected_default(self):
        with tempfile.TemporaryDirectory() as temp_dir, tempfile.TemporaryDirectory() as real_dir:
            temp_root, here, temp_src = make_fixture(temp_dir)
            real_root = Path(real_dir)
            real_src = real_root / "crates/engine/src/example.rs"
            real_src.parent.mkdir(parents=True)
            real_src.write_text("pub const EXAMPLE_FIELD: u32 = 1;\n", encoding="utf-8")
            # Break the anchor in temp_root so passing --root real_root only passes if --root is honored.
            temp_src.write_text("pub const BROKEN: u32 = 0;\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, r"broken evidence anchor"):
                run_check(temp_root, here, "check")
            result = run_check(temp_root, here, "check", "--root", str(real_root))
            self.assertEqual(result["total"], 1)

    def test_cli_returns_nonzero_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root, here, _ = make_fixture(directory, disposition="dropped", issue="", spec="", reason="")
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
            self.assertIn("must have an issue number, spec reference, or a written reason", proc.stderr)


if __name__ == "__main__":
    unittest.main()


class UpdateBaselineKeepsRaiseReasonTests(unittest.TestCase):
    def test_lowering_a_raised_ceiling_keeps_the_raise_reason(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        import inventory_common

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "baseline.json"
            path.write_text(json.dumps({
                "format_version": 1,
                "dropped_plus_partial": 309,
                "raise_reason": "Reclassified by the enum audit (#1061).",
            }))
            counts = {"dropped": 200, "partial": 108}
            inventory_common.update_baseline_file(path, counts)
            doc = json.loads(path.read_text())
            self.assertEqual(doc["dropped_plus_partial"], 308)
            self.assertEqual(doc["raise_reason"], "Reclassified by the enum audit (#1061).")
