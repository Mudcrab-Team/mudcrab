"""Synthetic regressions for one-to-one, three-dimensional grass comparison."""

import csv
import json
import math
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import compare


def plant(ordinal, position, scale=1.0, packed=None, obstructed=False, form="00000001"):
    """Build deliberately explicit synthetic fields without production dump data."""
    values = tuple(position) + (scale, scale, scale) + (1., 0., 0., 0., 1., 0., 0., 0., 1.) + (0.75,)
    return compare.Plant(ordinal, form, values, tuple(packed) + values[3:] if packed else values, obstructed, "base")


def write_dump(directory, plants, ours=True, bad=False):
    """Serialize independent synthetic CSV input using the public column contract."""
    fields = ["cell_x", "cell_y", "ordinal", "form_id", *compare.FIELDS]
    if ours:
        fields += ["packed_" + field for field in compare.FIELDS] + ["stage", "obstructed"]
    directory.mkdir(parents=True)
    with (directory / "cell_0_0.csv").open("w", newline="", encoding="utf-8") as output:
        writer = csv.DictWriter(output, fields)
        writer.writeheader()
        for item in plants:
            row = dict(cell_x=0, cell_y=0, ordinal=item.ordinal, form_id=item.form)
            row.update(zip(compare.FIELDS, item.values))
            if ours:
                row.update(zip(("packed_" + field for field in compare.FIELDS), item.packed))
                row.update(stage=item.stage, obstructed=int(item.obstructed))
            writer.writerow(row)
        if bad:
            writer.writerow(dict(cell_x=0, cell_y=0, ordinal=99, form_id="invalid"))


class MatchingTests(unittest.TestCase):
    """Pin cases that reusable nearest-neighbour or XY-only checks get wrong."""

    def test_same_neighbour_cannot_be_reused(self):
        """Two nearby ours candidates cannot claim the same spud reference."""
        result = compare.compare_group([plant(0, (0.1, 0, 0)), plant(1, (0.2, 0, 0))], [plant(0, (0, 0, 0))])
        self.assertEqual(result["within_1_matches"], 1)
        self.assertEqual(result["within_1_recall"], 1)
        self.assertEqual(result["within_1_precision"], 0.5)

    def test_height_is_part_of_distance(self):
        """Identical XY with distinct Z cannot count as exact or within eight."""
        result = compare.compare_group([plant(0, (2, 3, 9))], [plant(0, (2, 3, 0))])
        self.assertEqual(result["within_8_matches"], 0)
        self.assertIsNone(result["within_8_scale_mean"])

    def test_exact_duplicates_remain_distinct(self):
        """Duplicate positions match by multiplicity and deterministic ordinal ties."""
        result = compare.compare_group([plant(0, (0, 0, 0), 2), plant(1, (0, 0, 0), 5), plant(2, (0, 0, 0), 8)],
                                       [plant(0, (0, 0, 0), 3), plant(1, (0, 0, 0), 7)])
        self.assertEqual(result["within_0_matches"], 2)
        self.assertEqual(result["within_0_scale_mean"], 1.5)
        self.assertEqual(result["within_0_scale_max"], 2)
        self.assertAlmostEqual(result["within_0_scale_rms"], math.sqrt(2.5))

    def test_closest_edge_wins_before_ordinal(self):
        """Global nearest candidates precede more distant lower ordinal candidates."""
        pairs = compare.match_positions([(0.9, 0, 0), (0.1, 0, 0)], [(0, 0, 0)])
        self.assertEqual(pairs, [(1, 0, 0.1)])

    def test_thresholds_and_negative_bucket_boundaries(self):
        """Inclusive 3D distances work across negative spatial hash boundaries."""
        result = compare.compare_group([plant(0, (-8, 0, 0)), plant(1, (20, 0, 2)), plant(2, (40, 0, 8))],
                                       [plant(0, (-7, 0, 0)), plant(1, (20, 0, 0)), plant(2, (40, 0, 0))])
        self.assertEqual([result[f"within_{t}_matches"] for t in (0, 1, 2, 8)], [0, 1, 2, 3])

    def test_source_and_packed_are_independent(self):
        """Packing loss cannot be reported as a source generation mismatch."""
        ours, spud = [plant(0, (0.1, 0, 0), packed=(0, 0, 0))], [plant(0, (0.1, 0, 0))]
        self.assertEqual(compare.compare_group(ours, spud)["within_0_matches"], 1)
        self.assertEqual(compare.compare_group(ours, spud, packed=True)["within_0_matches"], 0)

    def test_aggregate_weights_components(self):
        """Means and RMS are weighted by component count, not by cell count."""
        first = compare.compare_group([plant(0, (0, 0, 0), 2)], [plant(0, (0, 0, 0), 1)])
        second = compare.compare_group([plant(i, (i, 0, 0), 4) for i in range(3)], [plant(i, (i, 0, 0), 1) for i in range(3)])
        total = compare.aggregate([first, second])
        self.assertEqual(total["within_0_scale_mean"], 2.5)
        self.assertAlmostEqual(total["within_0_scale_rms"], math.sqrt(7))

    def test_pipeline_masks_forms_empty_cells_and_bad_record(self):
        """End-to-end output preserves grouping and reports recoverable bad rows."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_dump(root / "ours", [plant(0, (0, 0, 0)), plant(1, (1, 0, 0), obstructed=True), plant(2, (2, 0, 0), form="00000002")], bad=True)
            write_dump(root / "spud", [plant(0, (0, 0, 0)), plant(1, (1, 0, 0)), plant(2, (2, 0, 0))], ours=False)
            for side in ("ours", "spud"):
                (root / side / "cell_1_1.csv").write_text("cell_x,cell_y,ordinal,form_id\n", encoding="utf-8")
            result = compare.run(root / "ours", root / "spud", root / "out")
            self.assertFalse(result["complete"])
            self.assertEqual(len(result["cell_diagnostics"]), 2)
            self.assertEqual(result["cell_diagnostics"][0]["ours_bad_rows"], 1)
            totals = {(r["representation"], r["mask"], r["cap"]): r for r in result["totals"]}
            self.assertEqual(totals["source", "full", "uncapped"]["within_8_matches"], 2)
            self.assertEqual(totals["source", "unobstructed", "uncapped"]["within_8_matches"], 1)
            self.assertEqual(totals["source", "unobstructed", "uncapped"]["ours_count"], 2)
            self.assertEqual(json.loads((root / "out" / "summary.json").read_text(encoding="utf-8"))["cap"], 49152)
            self.assertTrue((root / "out" / "per-cell-form.csv").is_file())

    def test_cap_is_after_mask_and_uses_generation_order(self):
        """Interleaved forms share a cell cap, and blocked candidates free a slot."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            candidates = [plant(3, (30, 0, 0), form="00000002"), plant(0, (0, 0, 0)),
                          plant(2, (20, 0, 0)), plant(1, (10, 0, 0), obstructed=True)]
            write_dump(root / "ours", candidates)
            write_dump(root / "spud", candidates, ours=False)
            with patch.object(compare, "CAP", 2):
                result = compare.run(root / "ours", root / "spud", root / "out")
            totals = {(r["representation"], r["mask"], r["cap"]): r for r in result["totals"]}
            self.assertEqual(totals["source", "full", "capped"]["within_0_matches"], 2)
            self.assertEqual(totals["source", "unobstructed", "capped"]["within_0_matches"], 1)
            self.assertEqual(result["cell_diagnostics"][0]["ours_full_cap_removed"], 2)
            self.assertEqual(result["cell_diagnostics"][0]["ours_unobstructed_cap_removed"], 1)

    def test_manifest_detects_cell_missing_from_both_sides(self):
        """An agreed missing file cannot silently shrink the requested audit."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_dump(root / "ours", [])
            write_dump(root / "spud", [], ours=False)
            (root / "spud" / "requested-cells.csv").write_text("0,0\n-3,7\n", encoding="utf-8")
            result = compare.run(root / "ours", root / "spud", root / "out")
            self.assertFalse(result["complete"])
            self.assertEqual(len(result["warnings"]), 2)
            self.assertEqual(len(result["cell_diagnostics"]), 2)

    def test_distinct_roundtrip_decimals_match_as_f32(self):
        """Decimal formatting differences cannot manufacture source mismatches."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_dump(root / "ours", [plant(0, (0.1405029296875, 0, 0))])
            write_dump(root / "spud", [plant(0, (0.14050293, 0, 0))], ours=False)
            result = compare.run(root / "ours", root / "spud", root / "out")
            self.assertTrue(result["complete"])
            self.assertEqual(result["totals"][0]["within_0_matches"], 1)

    def test_f32_overflow_is_recoverable_bad_field(self):
        """An overflowing source field is rejected without leaking struct errors."""
        with self.assertRaisesRegex(ValueError, "outside finite f32"):
            compare.number({"brightness": "1e39"}, "brightness")


if __name__ == "__main__":
    unittest.main()
