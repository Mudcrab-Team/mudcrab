#!/usr/bin/env python3
"""Compare two stage receipts (`<shot>.stages.json`) probe by probe under the V1 tolerance.

The tolerance and the probe set are frozen in docs/specs/engine/visual-profiles.md: a probe passes
when every channel is within 2 % of the reference at stage 3 and within 2 bytes at stage 4. A stage
is compared when both receipts have a value; one that only one side measured fails, and one
neither side measured is listed as skipped. Probes are matched by name, and a probe whose
`world_position` or `patch_px` differs between the receipts is a different measurement and fails,
and so is one that does not record both fields: two absent values are not evidence of agreement.
A stage 3 value must hold RGB or RGBA and a stage 4 value RGB; anything shorter fails, and so does
a non-finite channel, which would otherwise let any candidate satisfy an infinite reference. Exits
1 when anything fails.
"""

import argparse
import json
import math
from pathlib import Path
import sys


STAGE3_RELATIVE = 0.02
STAGE4_BYTES = 2.0
POSE_KEYS = ("worldspace_id", "interior_cell_id", "position", "yaw", "pitch")
PROBE_KEYS = ("world_position", "patch_px")
# Channel counts each stage may carry: stage 3 is RGB or RGBA (alpha is not compared), stage 4 RGB.
STAGE_CHANNELS = {"hdr_pre_tonemap": (3, 4), "output": (3,)}


def stage3_ok(reference, candidate, relative):
    return all(abs(c - r) <= relative * abs(r) for r, c in zip(reference[:3], candidate[:3]))


def stage4_ok(reference, candidate, tolerance):
    return all(abs(c - r) <= tolerance for r, c in zip(reference[:3], candidate[:3]))


def malformed(stage, value):
    """Why a stage value cannot be compared, or None when it holds the channels the stage needs."""
    if not isinstance(value, list) or len(value) not in STAGE_CHANNELS[stage]:
        counts = " or ".join(str(count) for count in STAGE_CHANNELS[stage])
        return f"needs {counts} channels, has {value!r}"
    if not all(isinstance(channel, (int, float)) and not isinstance(channel, bool) for channel in value):
        return f"has a non-numeric channel: {value!r}"
    # An infinite reference channel accepts every candidate, because `inf <= relative * inf` holds.
    # `json.loads` parses the `Infinity` and `NaN` literals by default, so a receipt can carry one.
    if not all(math.isfinite(channel) for channel in value):
        return f"has a non-finite channel: {value!r}"
    return None


def compare(reference, candidate, relative, tolerance):
    failures, skipped, passed = [], [], []
    for key in POSE_KEYS:
        absent = [side for side, posed in (("the reference", reference["pose"]),
                                           ("the candidate", candidate["pose"])) if key not in posed]
        if absent:
            failures.append(f"pose {key}: missing from {' and '.join(absent)}")
        elif reference["pose"][key] != candidate["pose"][key]:
            failures.append(f"pose {key}: {reference['pose'][key]} != {candidate['pose'][key]}")
    by_name = {probe["name"]: probe for probe in candidate["probes"]}
    for probe in reference["probes"]:
        other = by_name.pop(probe["name"], None)
        if other is None:
            failures.append(f"{probe['name']}: missing from the candidate")
            continue
        identity = []
        for key in PROBE_KEYS:
            if key not in probe and key not in other:
                identity.append(f"{probe['name']} {key}: not recorded by either receipt, so the two "
                                f"probes are not shown to be the same measurement")
            elif key not in probe:
                identity.append(f"{probe['name']} {key}: missing from the reference")
            elif key not in other:
                identity.append(f"{probe['name']} {key}: missing from the candidate")
            elif probe[key] != other[key]:
                identity.append(f"{probe['name']} {key}: {probe[key]} != {other[key]}, "
                                f"a different measurement")
        if identity:
            failures.extend(identity)
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
            elif problem := malformed(stage, mine) or malformed(stage, theirs):
                side = "reference" if malformed(stage, mine) else "candidate"
                failures.append(f"{label}: the {side} {problem}")
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
