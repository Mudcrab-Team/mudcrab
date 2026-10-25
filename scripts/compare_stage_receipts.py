#!/usr/bin/env python3
"""Compare two stage receipts (`<shot>.stages.json`) probe by probe under the V1 tolerance.

The tolerance and the probe set are frozen in docs/specs/engine/visual-profiles.md: a probe passes
when every channel is within 2 % of the reference at stage 3 and within 2 bytes at stage 4. A stage
is compared when both receipts have a value; one that only one side measured fails, and one
neither side measured is listed as skipped. Exits 1 when anything fails.
"""

import argparse
import json
from pathlib import Path
import sys


STAGE3_RELATIVE = 0.02
STAGE4_BYTES = 2.0
POSE_KEYS = ("worldspace_id", "interior_cell_id", "position", "yaw", "pitch")


def stage3_ok(reference, candidate, relative):
    return all(abs(c - r) <= relative * abs(r) for r, c in zip(reference[:3], candidate[:3]))


def stage4_ok(reference, candidate, tolerance):
    return all(abs(c - r) <= tolerance for r, c in zip(reference, candidate))


def compare(reference, candidate, relative, tolerance):
    failures, skipped, passed = [], [], []
    for key in POSE_KEYS:
        if reference["pose"].get(key) != candidate["pose"].get(key):
            failures.append(f"pose {key}: {reference['pose'].get(key)} != {candidate['pose'].get(key)}")
    by_name = {probe["name"]: probe for probe in candidate["probes"]}
    for probe in reference["probes"]:
        other = by_name.pop(probe["name"], None)
        if other is None:
            failures.append(f"{probe['name']}: missing from the candidate")
            continue
        for stage, check, limit in (
            ("hdr_pre_tonemap", stage3_ok, relative),
            ("output", stage4_ok, tolerance),
        ):
            mine, theirs = probe["stages"].get(stage), other["stages"].get(stage)
            label = f"{probe['name']} {stage}"
            if mine is None and theirs is None:
                skipped.append(f"{label}: {probe['unavailable'].get(stage)} / {other['unavailable'].get(stage)}")
            elif mine is None or theirs is None:
                missing = "reference" if mine is None else "candidate"
                reason = (probe if mine is None else other)["unavailable"].get(stage)
                failures.append(f"{label}: not measured by the {missing} ({reason})")
            elif check(mine, theirs, limit):
                passed.append(label)
            else:
                failures.append(f"{label}: {mine} -> {theirs}")
    failures.extend(f"{name}: not in the reference" for name in by_name)
    return failures, skipped, passed


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("reference", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--stage3-relative", type=float, default=STAGE3_RELATIVE)
    parser.add_argument("--stage4-bytes", type=float, default=STAGE4_BYTES)
    args = parser.parse_args()
    reference = json.loads(args.reference.read_text())
    candidate = json.loads(args.candidate.read_text())
    failures, skipped, passed = compare(reference, candidate, args.stage3_relative, args.stage4_bytes)
    print(json.dumps({
        "reference": {"side": reference.get("side"), "visual_profile": reference.get("visual_profile"),
                      "build": reference.get("build")},
        "candidate": {"side": candidate.get("side"), "visual_profile": candidate.get("visual_profile"),
                      "build": candidate.get("build")},
        "tolerance": {"stage3_relative": args.stage3_relative, "stage4_bytes": args.stage4_bytes},
        "passed": passed, "skipped": skipped, "failures": failures,
    }, indent=2))
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
