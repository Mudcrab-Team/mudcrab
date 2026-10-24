import copy
import math
import unittest
from prepare_skyrim_comparison import prepare


class PresetTests(unittest.TestCase):
    def setUp(self):
        self.shots = {"width": 1920, "height": 1080, "shots": [{"name": "terrace", "worldspace_id": 60, "position": [23400, -47100, 400], "pitch": 7, "yaw": 315, "hfov": 75}]}
        self.template = {"name": "native", "timestamp": "native", "position": [0, 0, 0], "rotation": [1, 1], "fov": 80, "viewRoll": 3, "pcPosition": [11, 22, 33], "pcRotation": [1, 2, 3], "pcCell": [1, 2, 3], "future_field": {"keep": True}}
        self.calibration = {"runtime_sha256": "a" * 64, "evidence": "synthetic test only", "pitch_sign": -1, "yaw_sign": -1, "yaw_offset_degrees": 90, "fov_axis": "vertical"}

    def test_mapping_and_preservation(self):
        original = copy.deepcopy(self.template)
        preset = prepare(self.shots, self.template, self.calibration)[0][1]
        self.assertEqual(self.template, original)
        self.assertEqual(preset["pcCell"], original["pcCell"])
        self.assertEqual(preset["pcPosition"], original["pcPosition"])
        self.assertEqual(preset["future_field"], original["future_field"])
        self.assertAlmostEqual(preset["rotation"][0], math.radians(-7))
        self.assertAlmostEqual(preset["rotation"][1], math.radians(135))
        self.assertAlmostEqual(preset["fov"], 46.69212571046592)
        self.assertEqual(preset["position"], self.shots["shots"][0]["position"])

    def test_zero_rotation_is_refused(self):
        self.shots["shots"][0].update(pitch=0, yaw=90)
        with self.assertRaisesRegex(ValueError, "skips"):
            prepare(self.shots, self.template, self.calibration)

    def test_near_zero_and_full_turn_rotations_are_refused(self):
        for pitch, yaw in [(0, 90.0000000001), (0, 89.9999999999),
                           (1e-10, 90), (0, 450), (0, -270)]:
            with self.subTest(pitch=pitch, yaw=yaw):
                self.shots["shots"][0].update(pitch=pitch, yaw=yaw)
                with self.assertRaisesRegex(ValueError, "skips"):
                    prepare(self.shots, self.template, self.calibration)

    def test_nonzero_rotations_are_preserved(self):
        for pitch, yaw in [(0.001, 90), (0, 90.001)]:
            with self.subTest(pitch=pitch, yaw=yaw):
                self.shots["shots"][0].update(pitch=pitch, yaw=yaw)
                self.assertEqual(len(prepare(self.shots, self.template, self.calibration)), 1)

    def test_missing_calibration_is_refused(self):
        del self.calibration["evidence"]
        with self.assertRaisesRegex(ValueError, "evidence"):
            prepare(self.shots, self.template, self.calibration)

    def test_path_names_and_nonfinite_values_are_refused(self):
        self.shots["shots"][0]["name"] = "../escape"
        with self.assertRaises(ValueError):
            prepare(self.shots, self.template, self.calibration)
        self.shots["shots"][0]["name"] = "terrace"
        self.shots["shots"][0]["position"][0] = float("nan")
        with self.assertRaises(ValueError):
            prepare(self.shots, self.template, self.calibration)


if __name__ == "__main__":
    unittest.main()
