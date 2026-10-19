#!/usr/bin/env python3
"""Compare Ghidra function boundaries with the x64 runtime-function (.pdata) table.

``.pdata`` is compiler-emitted unwind data: every non-leaf function has a
primary entry, and split cold or funclet fragments point back to it through
``UNW_FLAG_CHAININFO``. Ghidra's boundaries come from its own analysis. This
reports where they disagree, so a finding can say which source it relied on.

The Ghidra side is a TSV written by ``ExportFunctionBounds.java``:
``entry_rva  min_rva  max_rva_exclusive  name`` (hex RVAs, one function per line).
Output is aggregate counts plus a bounded sample of RVAs; no code bytes.
"""
from __future__ import annotations

import argparse
from bisect import bisect_right
from dataclasses import dataclass
import json
from pathlib import Path
import sys

import recon

SAMPLE = 20


@dataclass(frozen=True)
class GhidraFunction:
    entry: int
    lo: int
    hi: int


def parse_ghidra_tsv(text: str) -> list[GhidraFunction]:
    functions = []
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        fields = line.split("\t")
        if len(fields) < 3:
            raise recon.ReconError(f"Ghidra TSV line {number}: expected at least 3 tab-separated fields")
        try:
            entry, lo, hi = (int(field, 16) for field in fields[:3])
        except ValueError as error:
            raise recon.ReconError(f"Ghidra TSV line {number}: {error}") from error
        functions.append(GhidraFunction(entry, lo, hi))
    return functions


def pdata_functions(image: recon.Image) -> dict[int, list[tuple[int, int]]]:
    """Map each primary function begin RVA to all [begin, end) ranges it owns, primary first."""
    owned: dict[int, list[tuple[int, int]]] = {}
    for entry in recon.runtime_functions(image):
        primary, hops = recon.primary_function(image, entry)
        ranges = owned.setdefault(primary[0], [])
        if hops == 0:
            ranges.insert(0, (entry[0], entry[1]))
        else:
            ranges.append((entry[0], entry[1]))
    return owned


def compare(pdata: dict[int, list[tuple[int, int]]], ghidra: list[GhidraFunction], text_range: tuple[int, int]) -> dict:
    by_entry = {f.entry: f for f in ghidra}
    begins = sorted(pdata)
    # Every [begin, end) owned by some primary, sorted, to locate an RVA inside a pdata function.
    spans = sorted((lo, hi, primary) for primary, ranges in pdata.items() for lo, hi in ranges)
    span_starts = [lo for lo, _, _ in spans]

    def owner(rva: int) -> int | None:
        i = bisect_right(span_starts, rva) - 1
        if i >= 0 and spans[i][0] <= rva < spans[i][1]:
            return spans[i][2]
        return None

    missing = [b for b in begins if b not in by_entry]
    exact, end_differs, partial_cover = 0, [], []
    for begin in begins:
        function = by_entry.get(begin)
        if function is None:
            continue
        _, hi = pdata[begin][0]
        if function.hi == hi:
            exact += 1
        else:
            end_differs.append(begin)
        if any(not (function.lo <= flo and fhi <= function.hi) for flo, fhi in pdata[begin][1:]):
            partial_cover.append(begin)
    inside, leaf_candidates = [], []
    text_lo, text_hi = text_range
    for function in ghidra:
        if function.entry in pdata or not text_lo <= function.entry < text_hi:
            continue
        (inside if owner(function.entry) is not None else leaf_candidates).append(function.entry)
    inside.sort()
    leaf_candidates.sort()

    def sample(values: list[int]) -> list[str]:
        return [recon.hexaddr(v) for v in values[:SAMPLE]]

    return {
        "pdata_primary_functions": len(begins),
        "pdata_fragments": sum(len(r) - 1 for r in pdata.values()),
        "ghidra_functions": len(ghidra),
        "entry_and_end_match": exact,
        "entry_match_end_differs": {"count": len(end_differs), "sample": sample(end_differs)},
        "fragments_outside_ghidra_body": {"count": len(partial_cover), "sample": sample(partial_cover)},
        "pdata_without_ghidra_function": {"count": len(missing), "sample": sample(missing)},
        "ghidra_entry_inside_pdata_function": {"count": len(inside), "sample": sample(inside)},
        "ghidra_entry_outside_pdata_in_text": {"count": len(leaf_candidates), "sample": sample(leaf_candidates)},
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("exe", type=Path, help="path to SkyrimSE.exe (read only)")
    parser.add_argument("ghidra_tsv", type=Path, help="output of ExportFunctionBounds.java")
    args = parser.parse_args(argv)
    try:
        image = recon.Image(args.exe.read_bytes())
        text = next((s for s in image.sections if s["name"] == ".text"), None)
        if text is None:
            raise recon.ReconError("no .text section")
        report = compare(pdata_functions(image), parse_ghidra_tsv(args.ghidra_tsv.read_text()),
                         (text["rva"], text["rva"] + text["virtual_size"]))
        sys.stdout.write(json.dumps(report, indent=2, sort_keys=True) + "\n")
        return 0
    except (OSError, recon.ReconError) as error:
        print(f"compare_functions: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
