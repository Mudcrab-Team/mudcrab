"""Asymmetric comparisons and adversarial descendant/accounting controls."""

import hashlib
import unittest

from compare_values import ABSENT, Child, Children, Ledger, Result, compare, compare_blob_digest


class Link(int):
    """Keep link absence distinct from an ordinary numeric zero."""


class ValueTests(unittest.TestCase):
    """Presentation differences need witnesses; ordinary unequal values stay unequal."""

    def test_exact_numeric_values_without_tolerance(self):
        """A one-bit float difference cannot become an approximate match."""
        self.assertEqual(compare(17.125, 17.125).status, "matched")
        self.assertEqual(compare(17.125, 17.1250001).status, "mismatched")
        self.assertEqual(compare(float("nan"), 0).status, "unclear")

    def test_missing_null_and_zero_links_are_not_matches(self):
        """Reference defaults do not invent source fields or resolve cleared links."""
        for actual, expected in ((ABSENT, None), (None, None), (Link(0), None)):
            self.assertEqual(compare(actual, expected, link_type=Link).status, "unclear")
        self.assertEqual(compare(0, 0, link_type=Link).status, "matched")

    def test_boolean_is_not_integer_without_a_witness(self):
        """Python's bool subclassing must not prove a native flag interpretation."""
        self.assertEqual(compare(True, 1).status, "mismatched")

    def test_witnessed_explanations_keep_their_separate_status(self):
        """Explained differing presentations do not inflate exact-value matches."""
        result = compare(-1, 255, adjudicate=lambda _a, _b: ("explained", "native i8 FF"))
        self.assertEqual(result, Result("explained", "native i8 FF"))
        with self.assertRaises(ValueError):
            compare(1, 2, adjudicate=lambda _a, _b: ("matched", ""))

    def test_byte_comparison_requires_both_length_and_hash(self):
        """A reference BLOB remains bytes, including asymmetric trailing padding."""
        blob = b"\x19\xA7\x00\x53"
        digest = {"bytes": 4, "sha256": hashlib.sha256(blob).hexdigest()}
        self.assertEqual(compare_blob_digest(digest, blob).status, "matched")
        self.assertEqual(compare_blob_digest(digest, blob[::-1]).status, "mismatched")
        self.assertEqual(compare_blob_digest(digest, 0x5300A719).status, "unclear")
        self.assertEqual(compare_blob_digest({**digest, "bytes": True}, blob).status, "unclear")


class ChildTests(unittest.TestCase):
    """Nested, repeated and empty collections require exact parent/ordinal joins."""

    def test_nested_lists_preserve_parent_and_order(self):
        """Two same-signature groups cannot borrow each other's child values."""
        rows = [Child("a", "owner", None, 1, "groups", "group"),
                Child("b", "owner", "a", 0, "values", "value")]
        children = Children(rows)
        root = {"groups": [{"values": [37]}, {"values": [-19, 93]}]}
        value, reason, ancestors = children.resolve("b", root, "owner")
        self.assertEqual(value, -19)
        self.assertIsNone(reason)
        self.assertEqual(ancestors, (("group", 1), ("value", 0)))

    def test_empty_and_missing_collections_have_distinct_reasons(self):
        """A missing mapped group is distinct from a reference extra child."""
        children = Children([Child("a", "owner", None, 0, "values", "value")])
        self.assertEqual(children.resolve("a", {}, "owner")[1], "mapped collection absent")
        self.assertEqual(children.resolve("a", {"values": []}, "owner")[1],
                         "reference child exceeds candidate collection")

    def test_extra_children_are_counted_bidirectionally(self):
        """Extra candidate ordinals remain visible when reference has no rows."""
        children = Children([Child("a", "owner", None, 1, "values", "value")])
        self.assertEqual(children.extra_positions("owner", None, "value", [37, -19, 93]), [0, 2])
        self.assertEqual(children.extra_positions("owner", None, "missing", [17, 23]), [0, 1])

    def test_cross_owner_parent_cycle_and_unknown_mapping_are_unclear(self):
        """Malformed relational metadata cannot be mistaken for source absence."""
        cases = [
            ([Child("a", "other", None, 0, "@", "value")], "crosses owner"),
            ([Child("a", "owner", "a", 0, "@", "value")], "cyclic"),
            ([Child("a", "owner", "missing", 0, "@", "value")], "missing relational"),
            ([Child("a", "owner", None, 0, None, "value")], "no reviewed mapping"),
        ]
        for rows, reason in cases:
            value, actual, _ = Children(rows).resolve("a", {"x": 37}, "owner")
            self.assertIs(value, ABSENT)
            self.assertIn(reason, actual)

    def test_duplicate_child_id_or_ordinal_is_rejected(self):
        """A keyed-map overwrite must not hide reference duplicates."""
        row = Child("a", "owner", None, 0, "@", "value")
        for duplicate in (row, Child("b", "owner", None, 0, "@", "value")):
            with self.assertRaises(ValueError):
                Children([row, duplicate])

    def test_singleton_mapping_does_not_repeat_at_nonzero_position(self):
        """Variant singleton position one cannot reuse position zero's payload."""
        children = Children([Child("a", "owner", None, 1, "@", "value")])
        self.assertIs(children.resolve("a", {"x": 37}, "owner")[0], ABSENT)


class LedgerTests(unittest.TestCase):
    """An all-type ledger closes its denominator and includes unobserved types."""

    def test_each_cell_is_accounted_and_explanations_are_not_matches(self):
        """Parallel classes sum exactly to the scoped reference-cell count."""
        ledger = Ledger(["PACK", "RFCT"])
        ledger.row("PACK", ["a", "b", "c"], {
            "a": Result("matched", "exact"), "b": Result("explained", "same native bits"),
            "c": Result("unclear", "no reviewed descendant mapping"),
        })
        report = ledger.report()
        self.assertEqual(report["PACK"]["reference_cells"], 3)
        self.assertEqual(report["PACK"]["matched"], 1)
        self.assertEqual(report["RFCT"]["population"], "no reference cells observed")

    def test_incomplete_partition_unknown_type_or_status_is_rejected(self):
        """Accounting failures must fail under both ordinary and optimized Python."""
        ledger = Ledger(["PACK"])
        for signature, columns, results in (
            ("PACK", ["a", "b"], {"a": Result("matched", "exact")}),
            ("PACK", ["a", "a"], {"a": Result("matched", "exact")}),
            ("RFCT", ["a"], {"a": Result("matched", "exact")}),
            ("PACK", ["a"], {"a": Result("guessed", "unsupported")}),
        ):
            with self.assertRaises(ValueError):
                ledger.row(signature, columns, results)


if __name__ == "__main__":
    unittest.main()
