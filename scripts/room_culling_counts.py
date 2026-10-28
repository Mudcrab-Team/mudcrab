#!/usr/bin/env python3
"""Print the main view's mesh list before and after room culling for every pose of a shots run.

Usage: python3 scripts/room_culling_counts.py <shots-out dir or shots.log>

Run the engine with `--room-culling --shots <file> --shots-out <dir>` first, for example with
`crates/engine/tests/fixtures/room-culling-shots.json`. Each settled capture receipt in
`shots.log` carries `room_culling: {listed, drawn}`: the streaming camera's `Mesh3d` list after
Bevy's frustum check and after room culling, in the frame the image was taken from. A run without
`--room-culling` has no counts, and the script fails rather than printing an empty table.
"""

import json
import sys
from pathlib import Path


def read_counts(log_text):
    """Return (pose, listed, drawn) for every settled capture with room-culling counts."""
    rows = []
    for line in log_text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if entry.get("kind") != "settled":
            continue
        counts = (entry.get("captured_frame") or {}).get("room_culling")
        if counts is None:
            continue
        pose = Path(str(entry.get("image", ""))).stem
        rows.append((pose, int(counts["listed"]), int(counts["drawn"])))
    return rows


def main(argv):
    if len(argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    path = Path(argv[1])
    if path.is_dir():
        path = path / "shots.log"
    rows = read_counts(path.read_text(encoding="utf-8"))
    if not rows:
        print(f"{path}: no capture carries room-culling counts (was --room-culling given?)", file=sys.stderr)
        return 1
    width = max(len(pose) for pose, _, _ in rows)
    print(f"{'pose':<{width}}  listed   drawn  hidden")
    for pose, listed, drawn in rows:
        print(f"{pose:<{width}}  {listed:>6}  {drawn:>6}  {listed - drawn:>6}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
