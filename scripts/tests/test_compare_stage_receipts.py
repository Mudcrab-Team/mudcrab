import copy
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "compare_stage_receipts.py"
SPEC = importlib.util.spec_from_file_location("compare_stage_receipts", SCRIPT)
COMPARE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(COMPARE)


def receipt(hdr=(0.25, 0.2, 0.1, 1.0), output=(220.0, 198.0, 154.0)):
    return {
        "schema": 1,
        "side": "mudcrab",
        "pose": {"worldspace_id": 60, "interior_cell_id": None, "position": [21750, -44530, 140],
                 "yaw": 90, "pitch": -3, "hfov": 80},
        "probes": [{
            "name": "sunlit-thatch",
            "world_position": [23089.6, -44485.0, 301.2],
            "patch_px": 15,
            "stages": {"raw_albedo": None, "lit_pre_fog": None,
                       "hdr_pre_tonemap": list(hdr) if hdr is not None else None,
                       "output": list(output) if output is not None else None},
            "unavailable": {"raw_albedo": "lands with P1a #360", "lit_pre_fog": "lands with P1c #362"},
        }],
    }


def run(reference, candidate):
    return COMPARE.compare(reference, candidate, COMPARE.STAGE3_RELATIVE, COMPARE.STAGE4_BYTES)


class CompareStageReceiptsTests(unittest.TestCase):
    def test_identical_receipts_pass_both_stages(self):
        failures, skipped, passed = run(receipt(), receipt())
        self.assertEqual(failures, [])
        self.assertEqual(skipped, [])
        self.assertEqual(passed, ["sunlit-thatch hdr_pre_tonemap", "sunlit-thatch output"])

    def test_values_inside_and_outside_the_tolerance(self):
        inside = receipt(hdr=(0.2549, 0.2039, 0.1019, 1.0), output=(222.0, 196.0, 154.0))
        self.assertEqual(run(receipt(), inside)[0], [])
        outside = receipt(hdr=(0.256, 0.2, 0.1, 1.0), output=(220.0, 198.0, 157.0))
        failures = run(receipt(), outside)[0]
        self.assertEqual(len(failures), 2, failures)

    def test_rgb_and_rgba_stage_3_values_compare(self):
        self.assertEqual(run(receipt(), receipt(hdr=(0.25, 0.2, 0.1)))[0], [])

    def test_short_or_empty_channel_arrays_fail(self):
        for hdr, output in [((0.25,), (220.0, 198.0, 154.0)),
                            ((0.25, 0.2, 0.1, 1.0), ()),
                            ((), (220.0, 198.0)),
                            ((0.25, 0.2, 0.1, 1.0, 0.0), (220.0, 198.0, 154.0, 255.0))]:
            with self.subTest(hdr=hdr, output=output):
                failures, _, passed = run(receipt(), receipt(hdr=hdr, output=output))
                self.assertTrue(failures)
                self.assertTrue(all("channels" in failure for failure in failures), failures)
                self.assertEqual(len(failures) + len(passed), 2)
        failures = run(receipt(hdr=(0.25,)), receipt())[0]
        self.assertIn("the reference needs 3 or 4 channels", failures[0])

    def test_a_moved_or_resized_probe_is_a_different_measurement(self):
        for key, value in [("world_position", [23000.0, -44485.0, 301.2]), ("patch_px", 5)]:
            with self.subTest(key=key):
                candidate = receipt()
                candidate["probes"][0][key] = value
                failures, _, passed = run(receipt(), candidate)
                self.assertEqual(passed, [])
                self.assertEqual(len(failures), 1)
                self.assertIn(f"sunlit-thatch {key}", failures[0])

    def test_non_finite_channels_never_pass(self):
        for stage, bad in [("hdr_pre_tonemap", receipt(hdr=(float("inf"), 0.2, 0.1))),
                           ("output", receipt(output=(float("nan"), 198.0, 154.0)))]:
            with self.subTest(stage=stage):
                failures, _, passed = run(bad, receipt())
                self.assertNotIn(f"sunlit-thatch {stage}", passed)
                self.assertIn("the reference has a non-finite channel", failures[0])
        # A finite candidate must not satisfy an infinite reference channel.
        failures, _, passed = run(receipt(hdr=(float("inf"), 0.2, 0.1)), receipt(hdr=(1000.0, 0.2, 0.1)))
        self.assertNotIn("sunlit-thatch hdr_pre_tonemap", passed)
        self.assertTrue(failures[0].startswith(
            "sunlit-thatch hdr_pre_tonemap: the reference has a non-finite channel"), failures[0])
        candidate_failures, _, candidate_passed = run(receipt(), receipt(hdr=(float("inf"), 0.2, 0.1)))
        self.assertNotIn("sunlit-thatch hdr_pre_tonemap", candidate_passed)
        self.assertIn("the candidate has a non-finite channel", candidate_failures[0])

    def test_a_probe_missing_identity_fields_is_not_the_same_measurement(self):
        reference, candidate = receipt(), receipt()
        for document in (reference, candidate):
            del document["probes"][0]["world_position"]
        failures, _, passed = run(reference, candidate)
        self.assertEqual(passed, [])
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("world_position: not recorded by either receipt", failures[0])

        candidate = receipt()
        del candidate["probes"][0]["patch_px"]
        failures, _, passed = run(receipt(), candidate)
        self.assertEqual(passed, [])
        self.assertEqual([failure for failure in failures if "patch_px" in failure],
                         ["sunlit-thatch patch_px: missing from the candidate"])

    def test_a_pose_field_missing_from_both_receipts_fails(self):
        reference, candidate = receipt(), receipt()
        del reference["pose"]["worldspace_id"]
        del candidate["pose"]["worldspace_id"]
        failures = run(reference, candidate)[0]
        self.assertIn("pose worldspace_id: missing from the reference and the candidate", failures)

    def test_the_command_rejects_a_receipt_written_with_the_infinity_literal(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reference, candidate = root / "reference.json", root / "candidate.json"
            reference.write_text(json.dumps(receipt(hdr=(float("inf"), 0.2, 0.1))))
            self.assertIn("Infinity", reference.read_text())
            candidate.write_text(json.dumps(receipt()))
            result = subprocess.run([sys.executable, str(SCRIPT), str(reference), str(candidate)],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 1, result.stdout)
            report = json.loads(result.stdout)
            self.assertNotIn("sunlit-thatch hdr_pre_tonemap", report["passed"])
            self.assertIn("non-finite", report["failures"][0])

    def test_a_stage_only_one_side_measured_fails_and_neither_is_skipped(self):
        candidate = receipt(output=None)
        candidate["probes"][0]["unavailable"]["output"] = "off screen"
        failures = run(receipt(), candidate)[0]
        self.assertEqual(failures, ["sunlit-thatch output: not measured by the candidate (off screen)"])
        reference = copy.deepcopy(candidate)
        failures, skipped, _ = run(reference, candidate)
        self.assertEqual(failures, [])
        self.assertEqual(skipped, ["sunlit-thatch output: off screen / off screen"])

    def test_pose_and_probe_set_differences_fail(self):
        candidate = receipt()
        candidate["pose"]["yaw"] = 91
        candidate["probes"][0]["name"] = "renamed"
        failures = run(receipt(), candidate)[0]
        self.assertIn("pose yaw: 90 != 91", failures)
        self.assertIn("sunlit-thatch: missing from the candidate", failures)
        self.assertIn("renamed: not in the reference", failures)

    def test_the_command_exits_one_on_a_truncated_candidate(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reference, candidate = root / "reference.json", root / "candidate.json"
            reference.write_text(json.dumps(receipt()))
            candidate.write_text(json.dumps(receipt(hdr=(0.25,), output=())))
            result = subprocess.run([sys.executable, str(SCRIPT), str(reference), str(candidate)],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 1, result.stdout)
            report = json.loads(result.stdout)
            self.assertEqual(report["passed"], [])
            self.assertEqual(len(report["failures"]), 2)
            candidate.write_text(json.dumps(receipt()))
            result = subprocess.run([sys.executable, str(SCRIPT), str(reference), str(candidate)],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
