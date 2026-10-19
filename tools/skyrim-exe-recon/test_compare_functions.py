"""Checks for the Ghidra-vs-.pdata boundary comparison, using synthetic tables."""
import unittest

import recon
from compare_functions import GhidraFunction, compare, parse_ghidra_tsv

TEXT = (0x1000, 0x9000)
# Primary 0x1000 owns a chained cold fragment at 0x8000; 0x2000 and 0x3000 are plain.
PDATA = {0x1000: [(0x1000, 0x1100), (0x8000, 0x8040)], 0x2000: [(0x2000, 0x2080)], 0x3000: [(0x3000, 0x3010)]}


class CompareTests(unittest.TestCase):
    def test_classifies_each_disagreement(self):
        ghidra = [
            GhidraFunction(0x1000, 0x1000, 0x1100),  # entry and end match, but cold fragment outside body
            GhidraFunction(0x2000, 0x2000, 0x2040),  # end differs
            GhidraFunction(0x2050, 0x2050, 0x2080),  # split inside a pdata function
            GhidraFunction(0x4000, 0x4000, 0x4008),  # no unwind entry: leaf candidate
            GhidraFunction(0xA000, 0xA000, 0xA010),  # outside .text: ignored
        ]
        report = compare(PDATA, ghidra, TEXT)
        self.assertEqual(report["entry_and_end_match"], 1)
        self.assertEqual(report["pdata_fragments"], 1)
        self.assertEqual(report["entry_match_end_differs"]["sample"], ["0x2000"])
        self.assertEqual(report["fragments_outside_ghidra_body"]["sample"], ["0x1000"])
        self.assertEqual(report["pdata_without_ghidra_function"]["sample"], ["0x3000"])
        self.assertEqual(report["ghidra_entry_inside_pdata_function"]["sample"], ["0x2050"])
        self.assertEqual(report["ghidra_entry_outside_pdata_in_text"]["sample"], ["0x4000"])

    def test_fragment_inside_body_is_covered(self):
        report = compare(PDATA, [GhidraFunction(0x1000, 0x1000, 0x8040)], TEXT)
        self.assertEqual(report["fragments_outside_ghidra_body"]["count"], 0)
        self.assertEqual(report["entry_match_end_differs"]["count"], 1)

    def test_tsv_parsing(self):
        self.assertEqual(parse_ghidra_tsv("1000\t1000\t1100\tFUN_1\n\n"), [GhidraFunction(0x1000, 0x1000, 0x1100)])
        with self.assertRaises(recon.ReconError):
            parse_ghidra_tsv("1000\tzz\t1100\n")
        with self.assertRaises(recon.ReconError):
            parse_ghidra_tsv("1000\n")


if __name__ == "__main__":
    unittest.main()
