#!/usr/bin/env python3
"""Audit authored fixed collision for a worldspace cell window.

Reads only the packaged database and GLB scene extras. A failed gate means a
placed fixed model is missing, legacy, or has collision the extractor skipped.
"""

import argparse
from collections import Counter
import json
from pathlib import Path
import sqlite3
import struct


FIXED_RECORD_TYPES = {"STAT", "TREE", "FURN"}


def collision_contract(path: Path):
    with path.open("rb") as source:
        header = source.read(20)
        if len(header) != 20 or header[:4] != b"glTF":
            raise ValueError("invalid GLB header")
        length = struct.unpack_from("<I", header, 12)[0]
        if header[16:20] != b"JSON" or length > 64 * 1024 * 1024:
            raise ValueError("missing or oversized GLB JSON")
        document = json.loads(source.read(length))
    scenes = document.get("scenes", [])
    if not scenes:
        return None
    extras = scenes[0].get("extras", {})
    # Assets converted before the rename carry the old key.
    return extras.get("mudcrabCollision", extras.get("openSkyrimCollision"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("assets", type=Path, help="package assets directory with skyrim_world.db")
    parser.add_argument("--meshes", type=Path, help="alternate annotated meshes directory")
    parser.add_argument("--worldspace", type=int, default=60)
    parser.add_argument("--min-x", type=int, default=3)
    parser.add_argument("--max-x", type=int, default=7)
    parser.add_argument("--min-y", type=int, default=-14)
    parser.add_argument("--max-y", type=int, default=-10)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--expect-solid", action="append", default=[], help="NIF model path expected to have a collider")
    parser.add_argument("--expect-passable", action="append", default=[], help="NIF model path expected to have no collider")
    args = parser.parse_args()
    mesh_root = args.meshes or args.assets / "meshes"
    with sqlite3.connect(args.assets / "skyrim_world.db") as database:
        rows = database.execute(
            '''SELECT lower(s.model_path), r.record_type, count(*)
               FROM exterior_spatial x
               JOIN "references" f ON f.id=x.id
               JOIN statics s ON s.id=f.base_form_id
               JOIN records r ON r.form_id=f.base_form_id
               WHERE x.worldspace_id=? AND x.minX>=? AND x.minX<?
                 AND x.minY>=? AND x.minY<?
               GROUP BY lower(s.model_path), r.record_type''',
            (args.worldspace, args.min_x * 4096, (args.max_x + 1) * 4096,
             args.min_y * 4096, (args.max_y + 1) * 4096),
        ).fetchall()

    placements = Counter()
    models = Counter()
    families = Counter()
    skipped = []
    states = {}
    cache = {}
    for model, record_type, count in rows:
        if record_type not in FIXED_RECORD_TYPES:
            placements[(record_type, "excluded_record_type")] += count
            continue
        if not model:
            state, reason, shapes = "no_model", "", []
        else:
            normalized = model.replace("\\", "/").removeprefix("meshes/")
            path = (mesh_root / normalized).with_suffix(".glb")
            if path not in cache:
                try:
                    contract = collision_contract(path)
                    if contract is None:
                        cache[path] = ("legacy", "GLB lacks authored collision data", [])
                    elif contract.get("version") != 1 or contract.get("authored") is not True:
                        cache[path] = ("unsupported", "unknown collision contract", [])
                    elif contract.get("shapes"):
                        cache[path] = ("authored", "; ".join(contract.get("skipped", [])), contract["shapes"])
                    elif contract.get("skipped"):
                        cache[path] = ("unsupported", "; ".join(contract["skipped"]), [])
                    else:
                        cache[path] = ("absent", "", [])
                except (OSError, ValueError, KeyError, IndexError) as error:
                    cache[path] = ("missing", str(error), [])
            state, reason, shapes = cache[path]
        placements[(record_type, state)] += count
        models[(record_type, state)] += 1
        for shape in shapes:
            families[(record_type, shape.get("kind", "unknown"))] += count
        if state in {"missing", "legacy", "unsupported"} or reason:
            skipped.append({"record_type": record_type, "model": model, "placements": count, "status": state, "reason": reason})
        if model:
            states[model.replace("\\", "/").lower()] = state

    for model in args.expect_solid:
        state = states.get(model.replace("\\", "/").lower(), "missing")
        if state != "authored":
            skipped.append({"model": model, "status": state, "reason": "expected solid"})
    for model in args.expect_passable:
        state = states.get(model.replace("\\", "/").lower(), "missing")
        if state != "absent":
            skipped.append({"model": model, "status": state, "reason": "expected passable"})

    report = {
        "worldspace": args.worldspace,
        "grid": [args.min_x, args.max_x, args.min_y, args.max_y],
        "placements": {f"{kind}:{state}": count for (kind, state), count in sorted(placements.items())},
        "models": {f"{kind}:{state}": count for (kind, state), count in sorted(models.items())},
        "shape_families": {f"{kind}:{family}": count for (kind, family), count in sorted(families.items())},
        "skipped": sorted(skipped, key=lambda row: (-row.get("placements", 0), row["model"])),
        "passed": not skipped,
    }
    encoded = json.dumps(report, indent=2) + "\n"
    if args.out:
        args.out.write_text(encoded)
    else:
        print(encoded, end="")
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
