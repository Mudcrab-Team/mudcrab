import importlib.util
import json
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "room_culling_counts.py"
spec = importlib.util.spec_from_file_location("room_culling_counts", SCRIPT)
counts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(counts)


def receipt(kind, image, room_culling):
    frame = {"main_frame": 7}
    if room_culling is not None:
        frame["room_culling"] = room_culling
    return json.dumps({"capture": 0, "kind": kind, "image": image, "captured_frame": frame})


class ReadCountsTest(unittest.TestCase):
    def test_settled_receipts_give_one_row_per_pose(self):
        log = "\n".join(
            [
                "shot \"a\": settled after 12 frames",
                receipt("settled", "out/BleakFallsBarrow01-room0-c.png", {"listed": 3288, "drawn": 1188}),
                receipt("transition", "out/BleakFallsBarrow01-room0-c-transition.png", {"listed": 9, "drawn": 9}),
                receipt("settled", "out/Alftand01-room0-c.png", {"listed": 40, "drawn": 40}),
                json.dumps({"run_complete": True}),
            ]
        )
        self.assertEqual(
            counts.read_counts(log),
            [("BleakFallsBarrow01-room0-c", 3288, 1188), ("Alftand01-room0-c", 40, 40)],
        )

    def test_run_without_room_culling_has_no_rows(self):
        log = receipt("settled", "out/pose.png", None)
        self.assertEqual(counts.read_counts(log), [])


if __name__ == "__main__":
    unittest.main()
