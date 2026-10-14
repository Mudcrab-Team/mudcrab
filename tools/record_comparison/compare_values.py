"""Strict value and ordered-child comparison; reference mappings stay external.

This module compares already decoded DATA. It neither interprets plugin bytes
nor imports a reference implementation. Callers supply the independently
reviewed field mappings and any native-backed presentation adjudication.
"""

from collections import Counter
from dataclasses import dataclass
import hashlib
import math


ABSENT = object()


@dataclass(frozen=True)
class Result:
    """One exhaustive reference-cell classification with an explicit reason."""

    status: str
    reason: str


def compare(actual, expected, *, absent=ABSENT, link_type=(), adjudicate=None):
    """Require exact values, or a caller's independently witnessed explanation."""
    if adjudicate is not None:
        result = adjudicate(actual, expected)
        if result is not None:
            result = Result(*result)
            if result.status not in {"matched", "explained", "mismatched", "unclear"}:
                raise ValueError("invalid adjudication status")
            if not result.reason:
                raise ValueError("adjudication requires a reason")
            return result
    if actual is absent:
        return Result("unclear", "mapped value absent; native presence policy unproven")
    if isinstance(actual, link_type) and actual == 0 and expected is None:
        return Result("unclear", "zero link versus reference NULL needs a presence witness")
    if actual is None or expected is None:
        return Result("unclear", "NULL representation needs a native presence witness")
    if type(actual) is bool or type(expected) is bool:
        equal = type(actual) is type(expected) and actual == expected
    elif isinstance(actual, (int, float)) and isinstance(expected, (int, float)):
        if (isinstance(actual, float) and not math.isfinite(actual)) or (
            isinstance(expected, float) and not math.isfinite(expected)
        ):
            return Result("unclear", "nonfinite numeric representation")
        equal = actual == expected
    else:
        equal = type(actual) is type(expected) and actual == expected
    return Result("matched", "exact mapped value") if equal else Result(
        "mismatched", "different mapped values; no witnessed presentation rule"
    )


def compare_blob_digest(actual, expected):
    """Compare a typed byte digest with a reference BLOB without numeric coercion."""
    if not isinstance(actual, dict) or set(actual) != {"bytes", "sha256"}:
        return Result("unclear", "byte digest envelope unavailable")
    if type(actual["bytes"]) is not int or actual["bytes"] < 0:
        return Result("unclear", "invalid byte count")
    digest = actual["sha256"]
    if not isinstance(digest, str) or len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        return Result("unclear", "invalid SHA-256 digest")
    if not isinstance(expected, bytes):
        return Result("unclear", "reference is not a native byte BLOB")
    equal = actual["bytes"] == len(expected) and digest == hashlib.sha256(expected).hexdigest()
    return Result("matched", "exact BLOB length and SHA-256") if equal else Result(
        "mismatched", "different BLOB length or SHA-256"
    )


def navigate(value, path, *, absent=ABSENT):
    """Follow explicit member/ordinal paths, preserving missing versus empty."""
    if path in ("", "@"):
        return value
    for member in path.split("."):
        if isinstance(value, dict):
            value = value.get(member, absent)
        elif isinstance(value, list) and member.isdecimal():
            position = int(member)
            value = value[position] if position < len(value) else absent
        else:
            return absent
    return value


@dataclass(frozen=True)
class Child:
    """External relational identity plus an explicitly mapped ordered collection."""

    key: str
    owner: str
    parent: str | None
    position: int | None
    collection: str | None
    table: str


class Children:
    """Resolve descendants by exact parent and ordinal; never by a table prefix."""

    def __init__(self, rows, *, absent=ABSENT):
        self.absent = absent
        self.rows = {}
        self.positions = {}
        for row in rows:
            if row.key in self.rows:
                raise ValueError("duplicate relational child identity")
            self.rows[row.key] = row
            if row.collection is not None:
                slot = (row.owner, row.parent, row.table, row.position)
                if slot in self.positions:
                    raise ValueError("duplicate ordered child position")
                self.positions[slot] = row.key

    def resolve(self, key, root, owner, trail=()):
        """Return a child, classification reason, and its ordered ancestor trail."""
        if key in trail or len(trail) >= 128:
            return self.absent, "cyclic or excessive child ancestry", ()
        row = self.rows.get(key)
        if row is None:
            return self.absent, "missing relational parent", ()
        if row.owner != owner:
            return self.absent, "relational child crosses owner identity", ()
        if row.parent is None:
            parent, reason, ancestors = root, None, ()
        else:
            parent, reason, ancestors = self.resolve(row.parent, root, owner, trail + (key,))
        if reason is not None:
            return self.absent, reason, ancestors
        ancestors += ((row.table, row.position),)
        if row.collection is None:
            return self.absent, "collection has no reviewed mapping", ancestors
        if type(row.position) is not int or row.position < 0:
            return self.absent, "missing or invalid ordered child position", ancestors
        entries = navigate(parent, row.collection, absent=self.absent)
        if entries is self.absent:
            return self.absent, "mapped collection absent", ancestors
        if row.collection == "@":
            entries = [entries]
        elif not isinstance(entries, list):
            entries = [entries]
        if row.position >= len(entries):
            return self.absent, "reference child exceeds candidate collection", ancestors
        return entries[row.position], None, ancestors

    def extra_positions(self, owner, parent, table, candidate_entries):
        """Enumerate extra candidate children separately from reference-cell totals."""
        present = {
            row.position for row in self.rows.values()
            if row.owner == owner and row.parent == parent and row.table == table
        }
        return [position for position in range(len(candidate_entries)) if position not in present]


class Ledger:
    """Account every scoped reference cell exactly once, with per-type reasons."""

    def __init__(self, signatures):
        self.cells = {signature: Counter() for signature in signatures}
        self.reasons = {signature: Counter() for signature in signatures}
        self.expected = Counter()
        self.extra_children = Counter()

    def row(self, signature, columns, classifications):
        """Reject omissions/duplicates instead of silently shrinking the denominator."""
        columns = tuple(columns)
        if len(columns) != len(set(columns)):
            raise ValueError("duplicate scoped column")
        if set(classifications) != set(columns):
            raise ValueError("reference-cell partition does not close")
        if signature not in self.cells:
            raise ValueError("signature outside declared comparison scope")
        for column in columns:
            result = classifications[column]
            if result.status not in {"matched", "explained", "mismatched", "unclear", "excluded"}:
                raise ValueError("unknown reference-cell classification")
            if not result.reason:
                raise ValueError("reference-cell classification requires a reason")
            self.cells[signature][result.status] += 1
            self.reasons[signature][result.status + ": " + result.reason] += 1
        self.expected[signature] += len(columns)

    def report(self):
        """Include absent types explicitly without representing zero cells as matches."""
        output = {}
        for signature, counts in sorted(self.cells.items()):
            if sum(counts.values()) != self.expected[signature]:
                raise ValueError("per-type reference-cell partition does not close")
            output[signature] = {
                **{status: counts[status] for status in (
                    "matched", "explained", "mismatched", "unclear", "excluded"
                )},
                "reference_cells": self.expected[signature],
                "extra_candidate_children": self.extra_children[signature],
                "reasons": dict(sorted(self.reasons[signature].items())),
                "population": "present" if self.expected[signature] else "no reference cells observed",
            }
        return output
