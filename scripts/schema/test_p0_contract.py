import hashlib
import json
import os
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import p0_fixtures
import run_mutagen_p0


class P0WireFixtureTests(unittest.TestCase):
    def test_hand_fixture_matches_reviewed_golden_and_expected_values(self):
        raw = p0_fixtures.hand_encoded_full()
        self.assertEqual(len(raw), 595)
        self.assertEqual(p0_fixtures.sha256(raw), p0_fixtures.HAND_ENCODED_GOLDEN_SHA256)
        self.assertEqual(
            p0_fixtures.HAND_ENCODED_EXPECTED,
            {
                "plugin": "p0-hand-full.esp",
                "records": [
                    {"signature": "GLOB", "form_id": 0x800, "editor_id": "P0HandGlobal", "raw_flags": 0, "typed_data": "4.25"},
                    {"signature": "GMST", "form_id": 0x801, "editor_id": "fP0HandValue", "raw_flags": 0, "typed_data": "12.5"},
                    {"signature": "STAT", "form_id": 0x802, "editor_id": "P0HandStatic", "raw_flags": 0, "typed_data": "meshes\\p0_hand_stat.nif"},
                    {"signature": "CELL", "form_id": 0x803, "editor_id": "P0HandCell", "raw_flags": 0, "typed_data": None},
                    {
                        "signature": "REFR",
                        "form_id": 0x804,
                        "editor_id": "P0HandPlacedRef",
                        "raw_flags": 0xC00,
                        "typed_data": "12.5, -25, 100",
                        "typed_link": "000802:p0-hand-full.esp",
                    },
                ],
            },
        )

    def test_hand_fixture_has_interior_persistent_cell_hierarchy(self):
        raw = p0_fixtures.hand_encoded_full()

        def group_at(offset):
            signature, size, label, group_type, *_ = p0_fixtures.GROUP_HEADER.unpack_from(raw, offset)
            self.assertEqual(signature, b"GRUP")
            self.assertGreaterEqual(size, p0_fixtures.GROUP_HEADER_SIZE)
            return label, group_type, offset + size, offset + p0_fixtures.GROUP_HEADER_SIZE

        def record_at(offset):
            signature, data_size, flags, form_id, *_ = p0_fixtures.RECORD_HEADER.unpack_from(raw, offset)
            return signature, flags, form_id, offset + p0_fixtures.RECORD_HEADER_SIZE + data_size

        header_size = struct.unpack_from("<I", raw, 4)[0]
        offset = p0_fixtures.RECORD_HEADER_SIZE + header_size
        group_types = []
        for signature in (b"GLOB", b"GMST", b"STAT"):
            label, group_type, end, child_offset = group_at(offset)
            self.assertEqual((label, group_type), (signature, 0))
            record_signature, _, _, record_end = record_at(child_offset)
            self.assertEqual(record_signature, signature)
            self.assertEqual(record_end, end)
            group_types.append(group_type)
            offset = end

        label, group_type, cell_group_end, block_offset = group_at(offset)
        self.assertEqual((label, group_type), (b"CELL", 0))
        group_types.append(group_type)
        block_label, block_type, block_end, subblock_offset = group_at(block_offset)
        self.assertEqual((block_label, block_type), (b"\0\0\0\0", 2))
        group_types.append(block_type)
        subblock_label, subblock_type, subblock_end, cell_offset = group_at(subblock_offset)
        self.assertEqual((subblock_label, subblock_type), (b"\0\0\0\0", 3))
        group_types.append(subblock_type)
        cell_signature, _, cell_id, cell_end = record_at(cell_offset)
        self.assertEqual((cell_signature, cell_id), (b"CELL", 0x803))
        child_label, child_type, child_end, persistent_offset = group_at(cell_end)
        self.assertEqual((child_label, child_type), (struct.pack("<I", 0x803), 6))
        group_types.append(child_type)
        persistent_label, persistent_type, persistent_end, placed_offset = group_at(persistent_offset)
        self.assertEqual((persistent_label, persistent_type), (struct.pack("<I", 0x803), 8))
        group_types.append(persistent_type)
        placed_signature, placed_flags, placed_id, placed_end = record_at(placed_offset)
        self.assertEqual((placed_signature, placed_id, placed_flags), (b"REFR", 0x804, 0xC00))
        self.assertEqual(placed_end, persistent_end)
        self.assertEqual(persistent_end, child_end)
        self.assertEqual(child_end, subblock_end)
        self.assertEqual(subblock_end, block_end)
        self.assertEqual(block_end, cell_group_end)
        self.assertEqual(group_types, [0, 0, 0, 0, 2, 3, 6, 8])

    def test_unknown_repeat_and_negative_probes_are_raw_byte_mutations(self):
        cases = p0_fixtures.hand_encoded_cases()
        full = cases["p0-hand-full.esp"][0]
        unknown = cases["p0-unknown-repeated.esp"][0]
        self.assertEqual(
            cases["p0-unknown-repeated.esp"][1],
            "raw-byte-injection-after-hand-encoding-two-unknown-ZZZZ-subrecords",
        )
        self.assertEqual(
            p0_fixtures.sha256(unknown), p0_fixtures.UNKNOWN_REPEATED_GOLDEN_SHA256
        )
        self.assertEqual(full.count(b"ZZZZ"), 0)
        self.assertEqual(unknown.count(b"ZZZZ"), 2)
        self.assertEqual(
            len(unknown) - len(full),
            (p0_fixtures.SUBRECORD_HEADER.size + 10) + (p0_fixtures.SUBRECORD_HEADER.size + 5),
        )
        self.assertEqual(len(cases["p0-truncated-header.esp"][0]), 12)
        self.assertEqual(len(cases["p0-truncated-tail.esp"][0]), len(unknown) - 1)
        malformed_subrecord = cases["p0-bad-subrecord-length.esp"][0]
        tes4_size = struct.unpack_from("<I", malformed_subrecord, 4)[0]
        first_glob = p0_fixtures.RECORD_HEADER_SIZE + tes4_size + p0_fixtures.GROUP_HEADER_SIZE
        first_edid_length = first_glob + p0_fixtures.RECORD_HEADER_SIZE + 4
        self.assertEqual(struct.unpack_from("<H", malformed_subrecord, first_edid_length)[0], 0xFFFF)
        malformed_tes4 = cases["p0-bad-tes4-length.esp"][0]
        self.assertGreater(
            p0_fixtures.RECORD_HEADER_SIZE + struct.unpack_from("<I", malformed_tes4, 4)[0],
            len(malformed_tes4),
        )

    def test_hedr_record_count_matches_physical_major_and_group_headers_for_positive_fixtures(self):
        cases = p0_fixtures.hand_encoded_cases()
        expected_counts = {
            "p0-hand-full.esp": (5, 8, 13),
            "p0-hand-light.esl": (1, 1, 2),
            "p0-base.esm": (4, 7, 11),
            "p0-override.esp": (4, 7, 11),
            "p0-localized.esp": (1, 1, 2),
            "p0-unknown-repeated.esp": (5, 8, 13),
        }

        def count_physical_headers(raw):
            tes4_size = struct.unpack_from("<I", raw, 4)[0]
            hedr_offset = p0_fixtures.RECORD_HEADER_SIZE
            hedr_signature, hedr_size = p0_fixtures.SUBRECORD_HEADER.unpack_from(raw, hedr_offset)
            self.assertEqual(hedr_signature, b"HEDR")
            self.assertEqual(hedr_size, 12)
            _, declared_count, _ = struct.unpack_from(
                "<fII", raw, hedr_offset + p0_fixtures.SUBRECORD_HEADER.size
            )

            def count_region(start, end):
                major_records = 0
                groups = 0
                offset = start
                while offset < end:
                    signature = raw[offset : offset + 4]
                    if signature == b"GRUP":
                        _, group_size, _, _, *_ = p0_fixtures.GROUP_HEADER.unpack_from(raw, offset)
                        group_end = offset + group_size
                        self.assertGreaterEqual(group_size, p0_fixtures.GROUP_HEADER_SIZE)
                        self.assertLessEqual(group_end, end)
                        nested_records, nested_groups = count_region(
                            offset + p0_fixtures.GROUP_HEADER_SIZE, group_end
                        )
                        major_records += nested_records
                        groups += nested_groups + 1
                        offset = group_end
                    else:
                        record_signature, data_size, *_ = p0_fixtures.RECORD_HEADER.unpack_from(raw, offset)
                        self.assertNotEqual(record_signature, b"TES4")
                        record_end = offset + p0_fixtures.RECORD_HEADER_SIZE + data_size
                        self.assertLessEqual(record_end, end)
                        major_records += 1
                        offset = record_end
                self.assertEqual(offset, end)
                return major_records, groups

            physical_records, group_headers = count_region(
                p0_fixtures.RECORD_HEADER_SIZE + tes4_size, len(raw)
            )
            return declared_count, physical_records, group_headers

        self.assertEqual(set(expected_counts), {name for name in expected_counts if name in cases})
        for filename, expected in expected_counts.items():
            declared, records, groups = count_physical_headers(cases[filename][0])
            self.assertEqual((records, groups, declared), expected, filename)
            self.assertEqual(declared, records + groups, filename)

    def test_localized_fixture_uses_typed_armor_name_id_without_string_table(self):
        raw, provenance = p0_fixtures.hand_encoded_cases()["p0-localized.esp"]
        self.assertEqual(provenance, "hand-encoded-localized-id-no-string-table")
        self.assertEqual(raw.count(b"ARMO"), 2)
        self.assertIn(b"FULL\x04\x00\x78\x56\x34\x12", raw)
        self.assertEqual(struct.unpack("<I", b"\x78\x56\x34\x12")[0], 0x12345678)


class P0RunnerContractTests(unittest.TestCase):
    def test_dispatch_patch_anchors_before_commands_and_preserves_legacy_body(self):
        original = (
            "using System.Text.Json;\n"
            "using Mutagen.Bethesda.Skyrim;\n"
            'if (args.Length >= 3 && args[0] == "placed-lo")\n'
            "    return PlacedLoadOrder.Run(args[1], args.Skip(2).ToArray());\n"
            'if (args.Length < 2 || args[0] != "placed")\n'
            "    return 2;\n"
            'using var mod = SkyrimMod.CreateFromBinaryOverlay(args[1], SkyrimRelease.SkyrimSE);\n'
        )
        extension = b"static class MutagenP0Inspect {}\n"
        patched = run_mutagen_p0.apply_inspect_extension(original.encode(), extension).decode()
        self.assertLess(patched.index("MutagenP0Inspect.Run(args[1])"), patched.index(run_mutagen_p0.ORACLE_ENTRY))
        self.assertLess(patched.index(run_mutagen_p0.ORACLE_ENTRY), patched.index("using var mod"))
        extension_tail = "\n\nstatic class MutagenP0Inspect {}\n"
        self.assertTrue(patched.endswith(extension_tail))
        original_body = patched[: -len(extension_tail)].replace(
            run_mutagen_p0.INSPECT_ENTRY.decode(), "", 1
        )
        self.assertEqual(original_body.rstrip() + "\n", original)
        self.assertEqual(patched.count(run_mutagen_p0.ORACLE_ENTRY), 1)

    def test_v148_managed_negative_rejection_requires_nonzero_error_and_no_stdout(self):
        accepted = run_mutagen_p0.classify_negative_run(
            "bad.esp", b"bad", 2, "", "Mutagen inspect failed: malformed plugin\n"
        )
        self.assertEqual(accepted["status"], "rejected_as_expected")
        self.assertFalse(accepted["completed_verdict_saved"])
        zero_exit = run_mutagen_p0.classify_negative_run(
            "bad.esp", b"bad", 0, '{"execution_status":"completed"}\n', ""
        )
        self.assertEqual(zero_exit["status"], "failed")
        partial = run_mutagen_p0.classify_negative_run(
            "bad.esp", b"bad", 2, "warning\n", "Mutagen inspect failed: malformed plugin\n"
        )
        self.assertEqual(partial["status"], "failed")
        unhandled = run_mutagen_p0.classify_negative_run("bad.esp", b"bad", 2, "", "Unhandled exception\n")
        self.assertEqual(unhandled["status"], "failed")
        self.assertFalse(unhandled["completed_verdict_saved"])

    def test_v144_unavailable_physical_observations_do_not_collapse_to_empty(self):
        record = {
            "signature": {"value": "STAT"},
            "form_key": "000800:test.esp",
            "editor_id": {"state": "present", "value": "TestStatic"},
            "major_record_flags_raw": 0,
            "is_deleted": False,
            "typed_data_print": {"state": "available", "text": ""},
            "typed_links": {"state": "available", "items": []},
            "physical_offset": {"state": "unavailable"},
            "physical_subrecord_order": {"state": "unavailable"},
            "unknown_payloads": {"state": "unavailable"},
        }
        records = {("STAT", "000800:test.esp"): record}
        run_mutagen_p0._expect_record(
            records,
            signature="STAT",
            form_key="000800:test.esp",
            editor_id="TestStatic",
            raw_flags=0,
        )
        record["unknown_payloads"] = {"state": "available", "items": []}
        with self.assertRaisesRegex(run_mutagen_p0.QualificationError, "unknown_payloads"):
            run_mutagen_p0._expect_record(
                records,
                signature="STAT",
                form_key="000800:test.esp",
                editor_id="TestStatic",
                raw_flags=0,
            )

    def test_v145_typed_model_path_matches_wire_value_after_separator_normalization(self):
        record = {
            "signature": {"value": "STAT"},
            "form_key": "000800:test.esp",
            "editor_id": {"state": "present", "value": "TestStatic"},
            "major_record_flags_raw": 0,
            "is_deleted": False,
            "typed_data_print": {
                "state": "available",
                "text": "Model (Model) =>\n[\n    File => meshes/p0_stat.nif\n]",
            },
            "typed_links": {"state": "available", "items": []},
            "physical_offset": {"state": "unavailable"},
            "physical_subrecord_order": {"state": "unavailable"},
            "unknown_payloads": {"state": "unavailable"},
        }
        run_mutagen_p0._expect_record(
            {("STAT", "000800:test.esp"): record},
            signature="STAT",
            form_key="000800:test.esp",
            editor_id="TestStatic",
            raw_flags=0,
            printed_contains="meshes\\p0_stat.nif",
        )

    def test_v145_localized_id_and_translated_text_remain_separate_observations(self):
        filename = "p0-localized.esp"
        input_bytes = b"localized fixture input"
        record = {
            "signature": {"state": "available", "value": "ARMO"},
            "form_key": "000800:p0-localized.esp",
            "editor_id": {"state": "present", "value": "P0LocalizedArmor"},
            "major_record_flags_raw": 0,
            "is_deleted": False,
            "typed_data_print": {"state": "available", "text": "Name => \n"},
            "typed_links": {"state": "available", "items": []},
            "physical_offset": {"state": "unavailable"},
            "physical_subrecord_order": {"state": "unavailable"},
            "unknown_payloads": {"state": "unavailable"},
        }
        report = {
            "execution_status": "completed",
            "acceptance_verdict": {"state": "unavailable"},
            "adapter": {"package_version": run_mutagen_p0.PINNED_MUTAGEN},
            "diagnostics": {"captured_stdout": ""},
            "source_plugin": {
                "file_name": filename,
                "mod_key": filename,
                "sha256": hashlib.sha256(input_bytes).hexdigest(),
                "size_bytes": len(input_bytes),
                "using_localization": True,
            },
            "source_occurrence_coverage": {"state": "unavailable"},
            "typed_major_record_count": 1,
            "records": [record],
        }
        summary = run_mutagen_p0.validate_report(report, filename, input_bytes)
        observation = summary["localized_name_observation"]
        self.assertEqual(observation["fixture_wire_identifier"]["value"], "0x12345678")
        self.assertEqual(observation["fixture_wire_identifier"]["source"], "hand-encoded fixture provenance")
        self.assertEqual(observation["printable_identifier"]["state"], "unavailable")
        self.assertEqual(observation["mutagen_strings_key"]["state"], "unavailable")
        self.assertEqual(observation["translated_text"]["state"], "unavailable")
        record["localized_name_strings_key"] = {
            "state": "available",
            "value": "305419896",
            "source": "Mutagen IOptionalStringsKeyGetter",
        }
        keyed_summary = run_mutagen_p0.validate_report(report, filename, input_bytes)
        self.assertEqual(
            keyed_summary["localized_name_observation"]["mutagen_strings_key"]["value"],
            "305419896",
        )
        record["localized_name_strings_key"]["value"] = "305419897"
        with self.assertRaisesRegex(run_mutagen_p0.QualificationError, "did not match"):
            run_mutagen_p0.validate_report(report, filename, input_bytes)

    def test_v147_default_artifact_under_protected_tmp_is_rejected_before_write(self):
        args = SimpleNamespace(artifact_dir=None, oracle_source="/tmp/not-the-oracle")
        candidate = run_mutagen_p0.REPO_ROOT / "mudcrab-p0-mutagen-test-no-write"
        with mock.patch.dict(
            os.environ,
            {"TMPDIR": str(run_mutagen_p0.REPO_ROOT), "TMP": str(run_mutagen_p0.REPO_ROOT), "TEMP": str(run_mutagen_p0.REPO_ROOT)},
        ), mock.patch.object(run_mutagen_p0.uuid, "uuid4", return_value=SimpleNamespace(hex="test-no-write")):
            with self.assertRaisesRegex(run_mutagen_p0.QualificationError, "protected path"):
                run_mutagen_p0.run_suite(args)
        self.assertFalse(candidate.exists())

    def test_v147_explicit_artifact_ancestor_of_oracle_is_rejected_without_write(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            oracle = root / "oracle"
            candidate = root
            with self.assertRaisesRegex(run_mutagen_p0.QualificationError, "protected path"):
                run_mutagen_p0.validate_artifact_destination(candidate, oracle)
            self.assertEqual(list(root.iterdir()), [])

    def test_v148_case_timeout_preserves_incomplete_evidence_without_verdict(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            observations = root / "observations"
            fixtures.mkdir()
            observations.mkdir()
            timeout = run_mutagen_p0.ProcessTimeout("inspect-bad", 45, "partial stdout", "partial stderr")
            with mock.patch.object(run_mutagen_p0, "_run_supervised", side_effect=timeout):
                summary, report = run_mutagen_p0._case_run(
                    dotnet=Path("/fake/dotnet"),
                    assembly=Path("/fake/records.dll"),
                    filename="p0-timeout.esp",
                    input_bytes=b"fixture",
                    fixtures_dir=fixtures,
                    observations_dir=observations,
                    env={},
                )
            self.assertEqual(summary["status"], "failed")
            self.assertFalse(summary["completed_verdict_saved"])
            self.assertIsNone(report)
            self.assertEqual((observations / "p0-timeout.raw.stdout.txt").read_text(), "partial stdout")
            self.assertEqual((observations / "p0-timeout.stderr.txt").read_text(), "partial stderr")
            timeout_evidence = json.loads((observations / "p0-timeout.timeout.json").read_text())
            self.assertEqual(timeout_evidence["status"], "incomplete")
            self.assertFalse(timeout_evidence["completed_verdict_saved"])
            self.assertFalse((observations / "p0-timeout.json").exists())

    def test_v148_legacy_timeout_preserves_incomplete_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            observations = root / "observations"
            fixtures.mkdir()
            observations.mkdir()
            timeout = run_mutagen_p0.ProcessTimeout("legacy-placed", 45, "partial legacy", "slow parser")
            with mock.patch.object(run_mutagen_p0, "_run_supervised", side_effect=timeout):
                with self.assertRaisesRegex(run_mutagen_p0.QualificationError, "incomplete evidence"):
                    run_mutagen_p0._run_legacy_smokes(
                        Path("/fake/dotnet"), Path("/fake/records.dll"), fixtures, {}, observations
                    )
            evidence = json.loads((observations / "legacy-placed.timeout.json").read_text())
            self.assertEqual(evidence["status"], "incomplete")
            self.assertFalse(evidence["completed_verdict_saved"])
            self.assertEqual(
                (observations / "legacy-placed.timeout.stdout.txt").read_text(), "partial legacy"
            )
            self.assertEqual(
                (observations / "legacy-placed.timeout.stderr.txt").read_text(), "slow parser"
            )

    def test_v148_wrong_shaped_json_is_raw_evidence_not_completed_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            observations = root / "observations"
            fixtures.mkdir()
            observations.mkdir()
            filename = "p0-wrong-shape.esp"
            input_bytes = b"fixture"
            wrong_shape = {
                "execution_status": "completed",
                "acceptance_verdict": {"state": "unavailable"},
                "adapter": {"package_version": run_mutagen_p0.PINNED_MUTAGEN},
                "diagnostics": {"captured_stdout": ""},
                "source_plugin": {
                    "file_name": filename,
                    "mod_key": filename,
                    "sha256": hashlib.sha256(input_bytes).hexdigest(),
                    "size_bytes": len(input_bytes),
                },
                "source_occurrence_coverage": None,
            }
            raw_stdout = json.dumps(wrong_shape)
            completed = subprocess.CompletedProcess([], 0, raw_stdout, "")
            with mock.patch.object(run_mutagen_p0, "_run_supervised", return_value=completed):
                summary, report = run_mutagen_p0._case_run(
                    dotnet=Path("/fake/dotnet"),
                    assembly=Path("/fake/records.dll"),
                    filename=filename,
                    input_bytes=input_bytes,
                    fixtures_dir=fixtures,
                    observations_dir=observations,
                    env={},
                )
            self.assertEqual(summary["status"], "failed")
            self.assertIn("source-occurrence coverage", summary["failure"])
            self.assertFalse(summary["completed_verdict_saved"])
            self.assertIsNone(report)
            self.assertEqual((observations / "p0-wrong-shape.raw.stdout.txt").read_text(), raw_stdout)
            self.assertFalse((observations / "p0-wrong-shape.json").exists())


if __name__ == "__main__":
    unittest.main()
