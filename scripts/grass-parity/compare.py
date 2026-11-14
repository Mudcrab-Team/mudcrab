#!/usr/bin/env python3
"""Compare CPU grass dumps without third-party dependencies or game fixtures.

Matching is deterministic greedy nearest-neighbour in 3D, within each cell/form:
exact positions first (ordinal breaks duplicate ties), then candidate edges in
ascending (distance, ours ordinal, spud ordinal) order. Each row is used once.
This is a greedy assignment, not a maximum-cardinality bipartite assignment.
Source values and production-packed values are reported independently. Packed
ours is compared to spud's exported source values, the reference being tested.
"""

import argparse
import csv
import json
import math
import re
import struct
import sys
from collections import defaultdict, deque
from dataclasses import dataclass
from pathlib import Path

CAP = 49152
THRESHOLDS = (0, 1, 2, 8)
POSITION = ("x_su", "y_su", "z_su")
ATTRIBUTES = {
    "scale": tuple("scale_" + axis for axis in "xyz"),
    "rotation": tuple(f"r{i}{j}" for i in range(3) for j in range(3)),
    "brightness": ("brightness",),
}
FIELDS = POSITION + tuple(name for fields in ATTRIBUTES.values() for name in fields)
CELL_FILE = re.compile(r"cell_(-?\d+)_(-?\d+)\.csv$")


@dataclass(frozen=True)
class Plant:
    """One candidate with independent source and packed representations."""

    ordinal: int
    form: str
    values: tuple
    packed: tuple
    obstructed: bool = False
    stage: str = "unknown"


def number(row, field, prefix=""):
    """Recover the exported IEEE f32, accepting world-position header aliases."""
    key = prefix + field
    if key not in row and field in POSITION:
        key = prefix + "world_" + field[0]
    value = float(row[key])
    if not math.isfinite(value):
        raise ValueError(f"nonfinite {key}")
    # Writers may print nine significant digits or shortest-roundtrip decimals.
    # Comparing their parsed f64 decimals would manufacture exact mismatches.
    try:
        return struct.unpack("<f", struct.pack("<f", value))[0]
    except (OverflowError, struct.error) as error:
        raise ValueError(f"{key} outside finite f32 range") from error


def read_cell(path, cell, ours, warnings):
    """Skip malformed records with bounded diagnostics and keep valid neighbours."""
    plants = []
    ordinals = set()
    skipped = 0
    with path.open(newline="", encoding="utf-8-sig") as source:
        for line, row in enumerate(csv.DictReader(source), 2):
            try:
                if (int(row["cell_x"]), int(row["cell_y"])) != cell:
                    raise ValueError("cell coordinates disagree with filename")
                ordinal = int(row["ordinal"])
                if ordinal < 0 or ordinal in ordinals:
                    raise ValueError("negative or duplicate generation ordinal")
                form = f"{int(row['form_id'], 16):08x}"
                values = tuple(number(row, name) for name in FIELDS)
                packed = tuple(number(row, name, "packed_") for name in FIELDS) if ours else values
                mask = row.get("obstructed", "0")
                if mask not in ("0", "1"):
                    raise ValueError("obstructed must be 0 or 1")
                plants.append(Plant(ordinal, form, values, packed, mask == "1", row.get("stage", "unknown")))
                ordinals.add(ordinal)
            except (KeyError, ValueError, TypeError) as error:
                skipped += 1
                if skipped <= 10:
                    warnings.append(f"{'ours' if ours else 'spud'}/{path.name}:{line}: skipped: {error}")
    if skipped:
        warnings.append(f"{'ours' if ours else 'spud'}/{path.name}: skipped {skipped} malformed records in total")
    return sorted(plants, key=lambda plant: plant.ordinal), skipped


def discover(directory):
    """Find only instance dumps, including empty cells represented by headers."""
    cells = {}
    for path in sorted(directory.glob("cell_*.csv")):
        match = CELL_FILE.fullmatch(path.name)
        if match:
            cells[tuple(map(int, match.groups()))] = path
    if not cells:
        raise ValueError(f"no cell_<x>_<y>.csv dumps in {directory}")
    return cells


def requested_cells(directory):
    """Honor the optional headerless cell manifest so jointly absent cells surface."""
    manifest = directory / "requested-cells.csv"
    if not manifest.exists():
        return set()
    with manifest.open(newline="", encoding="utf-8-sig") as source:
        return {(int(row[0]), int(row[1])) for row in csv.reader(source) if row}


def match_positions(ours, spud):
    """Return disjoint (ours index, reference index, distance) pairs up to 8 SU."""
    exact = defaultdict(deque)
    for j, position in enumerate(spud):
        exact[position].append(j)
    used_ours, used_spud, pairs = set(), set(), []
    for i, position in enumerate(ours):
        if exact[position]:
            j = exact[position].popleft()
            used_ours.add(i)
            used_spud.add(j)
            pairs.append((i, j, 0.0))
    buckets = defaultdict(list)
    for j, position in enumerate(spud):
        if j not in used_spud:
            buckets[tuple(math.floor(axis / 8) for axis in position)].append(j)
    edges = []
    for i, position in enumerate(ours):
        if i in used_ours:
            continue
        bx, by, bz = (math.floor(axis / 8) for axis in position)
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for dz in (-1, 0, 1):
                    for j in buckets.get((bx + dx, by + dy, bz + dz), ()):
                        squared = sum((a - b) ** 2 for a, b in zip(position, spud[j]))
                        if squared <= 64:
                            edges.append((squared, i, j))
    edges.sort()
    for squared, i, j in edges:
        if i not in used_ours and j not in used_spud:
            used_ours.add(i)
            used_spud.add(j)
            pairs.append((i, j, math.sqrt(squared)))
    return pairs


def statistics(values):
    """Compute absolute component errors; None means no matched components."""
    return {"n": len(values), "mean": sum(values) / len(values) if values else None,
            "max": max(values) if values else None,
            "rms": math.sqrt(sum(value * value for value in values) / len(values)) if values else None}


def compare_group(ours, spud, packed=False):
    """Measure counts, position precision/recall, and matched attribute errors."""
    av = [plant.packed if packed else plant.values for plant in ours]
    bv = [plant.values for plant in spud]
    pairs = match_positions([value[:3] for value in av], [value[:3] for value in bv])
    result = {"ours_count": len(ours), "spud_count": len(spud), "count_delta": len(ours) - len(spud)}
    for tolerance in THRESHOLDS:
        selected = [(i, j, distance) for i, j, distance in pairs if distance <= tolerance]
        count = len(selected)
        stem = f"within_{tolerance}"
        result[stem + "_matches"] = count
        result[stem + "_recall"] = count / len(spud) if spud else 1.0
        result[stem + "_precision"] = count / len(ours) if ours else 1.0
        errors = {"position": [distance for _, _, distance in selected]}
        offset = 3
        for attribute, names in ATTRIBUTES.items():
            errors[attribute] = [abs(av[i][k] - bv[j][k]) for i, j, _ in selected
                                 for k in range(offset, offset + len(names))]
            offset += len(names)
        for attribute, values in errors.items():
            for statistic, value in statistics(values).items():
                result[f"{stem}_{attribute}_{statistic}"] = value
    # Ordinals help trace source consumption without requiring identical row order.
    result["sample_pairs"] = [{"ours_ordinal": ours[i].ordinal, "spud_ordinal": spud[j].ordinal,
                               "distance": distance, "ours_stage": ours[i].stage}
                              for i, j, distance in sorted(pairs, key=lambda pair: (-pair[2], pair[0]))[:5]]
    used_ours = {i for i, _, _ in pairs}
    used_spud = {j for _, j, _ in pairs}
    result["sample_unmatched_ours"] = [{"ordinal": plant.ordinal, "stage": plant.stage, "position": av[i][:3]}
                                       for i, plant in enumerate(ours) if i not in used_ours][:5]
    result["sample_unmatched_spud"] = [{"ordinal": plant.ordinal, "position": bv[j][:3]}
                                       for j, plant in enumerate(spud) if j not in used_spud][:5]
    return result


def aggregate(rows):
    """Combine component statistics using sample counts, preserving proper RMS."""
    result = {name: sum(row[name] for row in rows) for name in ("ours_count", "spud_count", "count_delta")}
    for tolerance in THRESHOLDS:
        stem = f"within_{tolerance}"
        count = sum(row[stem + "_matches"] for row in rows)
        result[stem + "_matches"] = count
        for metric, denominator in (("recall", "spud_count"), ("precision", "ours_count")):
            result[stem + "_" + metric] = count / result[denominator] if result[denominator] else 1.0
        for attribute in ("position", *ATTRIBUTES):
            prefix = f"{stem}_{attribute}_"
            n = sum(row[prefix + "n"] for row in rows)
            result[prefix + "n"] = n
            result[prefix + "mean"] = sum((row[prefix + "mean"] or 0) * row[prefix + "n"] for row in rows) / n if n else None
            result[prefix + "rms"] = math.sqrt(sum((row[prefix + "rms"] or 0) ** 2 * row[prefix + "n"] for row in rows) / n) if n else None
            maxima = [row[prefix + "max"] for row in rows if row[prefix + "n"]]
            result[prefix + "max"] = max(maxima) if maxima else None
    return result


def write_csv(path, rows):
    """Write portable tabular metrics, leaving detailed pairs in JSON."""
    if not rows:
        return
    fields = [field for field in rows[0] if not field.startswith("sample_")]
    with path.open("w", newline="", encoding="utf-8") as output:
        writer = csv.DictWriter(output, fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)


def run(ours_directory, spud_directory, output):
    """Compare every cell with explicit missing-cell and malformed-row evidence."""
    ours_files, spud_files = discover(ours_directory), discover(spud_directory)
    warnings, forms, cells, diagnostics = [], [], [], []
    expected = ours_files.keys() | spud_files.keys() | requested_cells(ours_directory) | requested_cells(spud_directory)
    for cell in sorted(expected):
        inputs, skipped = [], []
        for files, ours in ((ours_files, True), (spud_files, False)):
            if cell in files:
                plants, bad = read_cell(files[cell], cell, ours, warnings)
            else:
                plants, bad = [], 0
                warnings.append(f"missing {'ours' if ours else 'spud'} cell {cell}")
            inputs.append(plants)
            skipped.append(bad)
        a, b = inputs
        diagnostics.append({"cell_x": cell[0], "cell_y": cell[1], "ours_candidates": len(a),
                            "spud_candidates": len(b), "obstructed": sum(p.obstructed for p in a),
                            "ours_bad_rows": skipped[0], "spud_bad_rows": skipped[1],
                            "ours_full_cap_removed": max(0, len(a) - CAP),
                            "ours_unobstructed_cap_removed": max(0, sum(not p.obstructed for p in a) - CAP),
                            "spud_cap_removed": max(0, len(b) - CAP)})
        for representation in ("source", "packed"):
            for mask in ("full", "unobstructed"):
                masked = [p for p in a if mask == "full" or not p.obstructed]
                previous = None
                for cap in ("uncapped", "capped"):
                    aa, bb = (masked, b) if cap == "uncapped" else (masked[:CAP], b[:CAP])
                    identity = {"representation": representation, "mask": mask, "cap": cap,
                                "cell_x": cell[0], "cell_y": cell[1]}
                    if cap == "capped" and len(masked) <= CAP and len(b) <= CAP:
                        group_rows = [dict(row, cap=cap) for row in previous]
                    else:
                        ag, bg = defaultdict(list), defaultdict(list)
                        for plant in aa:
                            ag[plant.form].append(plant)
                        for plant in bb:
                            bg[plant.form].append(plant)
                        group_rows = [dict(identity, form_id=form, **compare_group(ag[form], bg[form], representation == "packed"))
                                      for form in sorted(ag.keys() | bg.keys())]
                    forms.extend(group_rows)
                    cells.append(dict(identity, **aggregate(group_rows)))
                    previous = group_rows
    totals, form_totals = [], []
    for representation in ("source", "packed"):
        for mask in ("full", "unobstructed"):
            for cap in ("uncapped", "capped"):
                selected = [row for row in cells if (row["representation"], row["mask"], row["cap"]) == (representation, mask, cap)]
                totals.append(dict(representation=representation, mask=mask, cap=cap, **aggregate(selected)))
                by_form = defaultdict(list)
                for row in forms:
                    if (row["representation"], row["mask"], row["cap"]) == (representation, mask, cap):
                        by_form[row["form_id"]].append(row)
                form_totals.extend(dict(representation=representation, mask=mask, cap=cap, form_id=form, **aggregate(group))
                                   for form, group in sorted(by_form.items()))
    ranked = sorted((row for row in cells if (row["representation"], row["mask"], row["cap"]) == ("source", "full", "uncapped")),
                    key=lambda row: (-(row["ours_count"] + row["spud_count"] - 2 * row["within_0_matches"]), row["cell_x"], row["cell_y"]))
    summary = {"method": __doc__, "cap": CAP, "cap_order": "mask, then first 49152 valid rows in generation ordinal order; spud independently capped",
               "attribute_errors": "absolute component errors, rotation matrix entries (not angular degrees)",
               "complete": not warnings, "warnings": warnings, "totals": totals, "cell_diagnostics": diagnostics,
               "ranked_cells": ranked,
               "sample_pairs": [{key: row[key] for key in ("cell_x", "cell_y", "form_id", "sample_pairs", "sample_unmatched_ours", "sample_unmatched_spud")}
                                for row in forms if row["representation"] == "source" and row["mask"] == "full" and row["cap"] == "uncapped"]}
    output.mkdir(parents=True, exist_ok=True)
    (output / "summary.json").write_text(json.dumps(summary, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    write_csv(output / "per-cell.csv", cells)
    write_csv(output / "per-cell-form.csv", forms)
    write_csv(output / "per-form.csv", form_totals)
    write_csv(output / "cell-diagnostics.csv", diagnostics)
    lines = ["# Grass plant comparison", "", "Deterministic one-to-one greedy nearest neighbour in 3D; source and production-packed representations are independent.",
             "Exact duplicate ties use generation ordinal. Recall = matched/spud; precision = matched/ours. Empty denominators are 1.",
             "Scale/rotation/brightness are absolute component errors; rotation uses matrix entries. Statistics are provided at each position tolerance.",
             "Cap is applied in ordinal order after the selected obstruction mask. Spud is independently capped. See cell-diagnostics.csv for removed records.",
             "", f"Complete: {not warnings}; warnings: {len(warnings)}.", "", "| Values | Mask | Cap | Ours | Spud | Exact recall | Exact precision | <=1 recall | <=2 recall | <=8 recall |", "|---|---|---|---:|---:|---:|---:|---:|---:|---:|"]
    for row in totals:
        lines.append(f"| {row['representation']} | {row['mask']} | {row['cap']} | {row['ours_count']} | {row['spud_count']} | " + " | ".join(f"{row[key]:.6%}" for key in ("within_0_recall", "within_0_precision", "within_1_recall", "within_2_recall", "within_8_recall")) + " |")
    lines += ["", "## Cells ranked by exact unmatched rows (source, full, uncapped)", "", "| Cell | Ours | Spud | Exact pairs | Unmatched rows | <=8 pairs |", "|---|---:|---:|---:|---:|---:|"]
    for row in ranked:
        lines.append(f"| ({row['cell_x']}, {row['cell_y']}) | {row['ours_count']} | {row['spud_count']} | {row['within_0_matches']} | {row['ours_count'] + row['spud_count'] - 2 * row['within_0_matches']} | {row['within_8_matches']} |")
    if warnings:
        lines += ["", "## Input warnings", ""] + ["- " + warning for warning in warnings]
    (output / "COMPARE.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return summary


def main():
    """Run the directory comparison and report incomplete input via exit status."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ours", required=True, type=Path)
    parser.add_argument("--spud", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    try:
        summary = run(args.ours, args.spud, args.out)
    except (OSError, ValueError, csv.Error) as error:
        parser.exit(2, f"comparison failed: {error}\n")
    print(f"Compared {len(summary['cell_diagnostics'])} cells; {len(summary['warnings'])} warnings; outputs: {args.out}")
    return 0 if summary["complete"] else 1


if __name__ == "__main__":
    sys.exit(main())
