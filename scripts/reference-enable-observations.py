#!/usr/bin/env python3
"""Prepare or verify database samples for a pending clean-save comparison.

Uses read-only SQLite. Predictions describe the static bootstrap contract;
neither generation nor verification observes Skyrim or Mudcrab running.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
import sqlite3
import time


DISABLED = 0x800
PLAYER = 0x14
MAX_CHAIN = 256
MAX_CANDIDATES = 64
CASES = {
    "own_enabled": "COALESCE(r.enable_parent_id,0)=0 AND r.header_flags&2048=0",
    "own_disabled": "COALESCE(r.enable_parent_id,0)=0 AND r.header_flags&2048!=0",
    "parent_enables_disabled_child": "r.enable_parent_id!=0 AND r.header_flags&2048!=0 AND r.enable_parent_flags&1=0",
    "parent_disables_enabled_child": "r.enable_parent_id!=0 AND r.header_flags&2048=0 AND r.enable_parent_flags&1=0",
    "invert_enabled_parent": "r.enable_parent_id!=0 AND r.enable_parent_flags&1=1",
    "invert_disabled_parent": "r.enable_parent_id!=0 AND r.enable_parent_flags&1=1",
    "player_ancestry": "r.enable_parent_id=20 OR p.enable_parent_id=20",
    "player_noninverted": "r.enable_parent_id=20 AND r.enable_parent_flags&1=0",
    "differing_child_flags": "r.header_flags&2048=0 AND r.enable_parent_id IN (SELECT enable_parent_id FROM \"references\" WHERE enable_parent_id!=0 GROUP BY enable_parent_id,enable_parent_flags HAVING MIN(header_flags&2048)!=MAX(header_flags&2048))",
    "pop_in": "r.enable_parent_id!=0 AND r.enable_parent_flags&2=2",
    "reserved_bytes": "r.enable_parent_id!=0 AND r.enable_parent_flags&4294967040!=0",
}


def placement(database, form_id):
    row = database.execute(
        '''SELECT r.id,r.cell_id,r.base_form_id,r.header_flags,r.enable_parent_id,
                  r.enable_parent_flags,r.is_exterior,r.worldspace_id,
                  r.pos_x,r.pos_y,r.pos_z,c.interior_name,c.grid_x,c.grid_y,
                  s.model_path,f.plugin_name,f.internal_id,b.record_type AS base_type
           FROM "references" r LEFT JOIN cells c ON c.id=r.cell_id
           LEFT JOIN statics s ON s.id=r.base_form_id
           LEFT JOIN formid_map f ON f.form_id=r.id
           LEFT JOIN records b ON b.form_id=r.base_form_id WHERE r.id=?''',
        (form_id,),
    ).fetchone()
    if row is None:
        return None
    result = dict(row)
    result["form_id_hex"] = f"{form_id:08X}"
    if row["is_exterior"]:
        # Persistent rows can belong to a holding cell. The worker queries XY,
        # so use the actual placement rather than that cell's stored grid.
        result["load_key"] = {
            "worldspace_id": row["worldspace_id"],
            "grid_x": math.floor(row["pos_x"] / 4096),
            "grid_y": math.floor(row["pos_y"] / 4096),
        }
    else:
        result["load_key"] = {"interior_cell_id": row["cell_id"]}
    return result


def prediction(database, form_id):
    chain = []
    seen = set()
    invert = False
    while form_id != PLAYER:
        if form_id in seen or len(chain) >= MAX_CHAIN:
            return None
        seen.add(form_id)
        row = placement(database, form_id)
        if row is None:
            return None
        chain.append(row)
        parent = row["enable_parent_id"]
        if not parent:
            return {"enabled": (row["header_flags"] & DISABLED == 0) ^ invert,
                    "uses_player_assumption": False, "chain": chain}
        flags = row["enable_parent_flags"]
        if flags is None:
            return None
        invert ^= bool(flags & 1)
        form_id = parent
    return {"enabled": True ^ invert, "uses_player_assumption": True, "chain": chain}


def matches(kind, predicted):
    enabled = predicted["enabled"]
    if kind == "parent_enables_disabled_child":
        return enabled
    if kind == "parent_disables_enabled_child":
        return not enabled
    if kind == "invert_enabled_parent":
        return not enabled
    if kind == "invert_disabled_parent":
        return enabled
    if kind.startswith("player_"):
        return predicted["uses_player_assumption"]
    return True


def build(database, path):
    plugins = [dict(row) for row in database.execute(
        "SELECT name,priority,hex(checksum) AS checksum FROM plugins ORDER BY priority,name")]
    source = {
        "database_name": path.name,
        "world_schema": database.execute("SELECT version FROM schema_info").fetchone()[0],
        "reference_count": database.execute('SELECT count(*) FROM "references"').fetchone()[0],
        "plugins": plugins,
    }
    cases = []
    for kind, condition in CASES.items():
        for exterior in (False, True):
            case = {"case": kind, "space": "exterior" if exterior else "interior"}
            candidates = database.execute(
                f'''SELECT r.id FROM "references" r
                    LEFT JOIN "references" p ON p.id=r.enable_parent_id
                    LEFT JOIN statics s ON s.id=r.base_form_id
                    WHERE r.is_exterior=? AND ({condition})
                    ORDER BY (s.model_path IS NULL),r.id LIMIT ?''',
                (int(exterior), MAX_CANDIDATES),
            )
            for candidate in candidates:
                predicted = prediction(database, candidate[0])
                if predicted is None or not matches(kind, predicted):
                    continue
                case["prediction"] = predicted
                if kind == "differing_child_flags":
                    child = predicted["chain"][0]
                    sibling = database.execute(
                        '''SELECT id FROM "references" WHERE enable_parent_id=?
                           AND enable_parent_flags=? AND header_flags&2048!=0
                           ORDER BY id LIMIT 1''',
                        (child["enable_parent_id"], child["enable_parent_flags"]),
                    ).fetchone()
                    if sibling is None:
                        del case["prediction"]
                        continue
                    case["paired_prediction"] = prediction(database, sibling[0])
                    if case["paired_prediction"] is None:
                        del case["prediction"]
                        continue
                break
            case["sample_status"] = "selected" if "prediction" in case else "no_sample_within_bound"
            case["retail_observation"] = {"status": "pending", "enabled": None, "evidence": None}
            case["mudcrab_observation"] = {"status": "pending", "enabled": None, "evidence": None}
            cases.append(case)
    proof = {"source": source, "cases": cases}
    digest = hashlib.sha256(json.dumps(proof, sort_keys=True).encode()).hexdigest()
    return {"format_version": 1, "status": "retail_gate_pending",
            "evidence_kind": "database_samples_and_static_predictions",
            "selection_bounds": {"candidates_per_case": MAX_CANDIDATES, "parent_chain": MAX_CHAIN},
            "snapshot_sha256": digest, **proof}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=Path)
    destination = parser.add_mutually_exclusive_group(required=True)
    destination.add_argument("--output", type=Path)
    destination.add_argument("--verify", type=Path, help="verify the unobserved source manifest")
    args = parser.parse_args()
    try:
        with sqlite3.connect(args.database.resolve().as_uri() + "?mode=ro", uri=True) as database:
            database.row_factory = sqlite3.Row
            deadline = time.monotonic() + 120
            database.set_progress_handler(lambda: time.monotonic() > deadline, 10000)
            database.execute("PRAGMA query_only=ON")
            database.execute("BEGIN")
            manifest = build(database, args.database)
        if args.verify:
            if json.loads(args.verify.read_text()) != manifest:
                raise ValueError("manifest differs from the bounded database snapshot")
        else:
            args.output.write_text(json.dumps(manifest, indent=2) + "\n")
        selected = sum(case["sample_status"] == "selected" for case in manifest["cases"])
        print(f"Verified database samples: {selected}/{len(manifest['cases'])}; retail gate pending")
    except (OSError, ValueError, sqlite3.Error) as error:
        parser.exit(1, f"reference observation manifest: {error}\n")


if __name__ == "__main__":
    main()
