"""Copy a converted database and apply one retail load's portal order for a cell."""

import argparse
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import tempfile


def prepare(source, output, cell, portals):
    if not portals or len(set(portals)) != len(portals):
        raise ValueError("expected a nonempty sequence of distinct portal FormIDs")
    with tempfile.TemporaryDirectory(dir=output.parent) as staging:
        staged = Path(staging) / "ordered.db"
        with closing(sqlite3.connect(staged)) as db:
            with closing(sqlite3.connect(source.resolve().as_uri() + "?mode=ro", uri=True)) as src:
                src.backup(db)
            tables = {row[0] for row in db.execute("SELECT name FROM sqlite_schema WHERE type='table'")}
            endpoint_columns = {row[1] for row in db.execute("PRAGMA table_info(room_portals)")}
            if (not {"room_graph_cells", "room_graph_stats"} <= tables
                    or not {"unbounded_a", "unbounded_b"} <= endpoint_columns
                    or not db.execute("SELECT 1 FROM room_graph_cells WHERE cell_id=? AND complete=1",
                                      (cell,)).fetchone()
                    or not db.execute("SELECT 1 FROM room_graph_stats WHERE name='visibility_version' "
                                      "AND value=2").fetchone()
                    or not db.execute("SELECT 1 FROM room_graph_stats WHERE name='assignment_version' "
                                      "AND value=1").fetchone()):
                raise ValueError("captured order requires a complete visibility-v2 cell with authored assignment-v1 provenance")
            rows = sorted(row[0] for row in db.execute(
                "SELECT ref_id FROM room_portals WHERE cell_id=?", (cell,)
            ))
            if rows != sorted(portals):
                raise ValueError("captured portal IDs must match every physical portal in the converted cell")
            with db:
                if not db.execute(
                    "SELECT 1 FROM pragma_table_info('room_portals') WHERE name='retail_visit_index'"
                ).fetchone():
                    db.execute("ALTER TABLE room_portals ADD COLUMN retail_visit_index INTEGER")
                db.executemany(
                    "UPDATE room_portals SET retail_visit_index=? WHERE cell_id=? AND ref_id=?",
                    ((rank, cell, portal) for rank, portal in enumerate(portals)),
                )
        os.link(staged, output)  # Publish the closed copy only if the output name is still free.


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="converted world database")
    parser.add_argument("output", type=Path, help="new database path in a private comparison pack")
    parser.add_argument("cell", type=lambda value: int(value, 0), help="converted cell FormID")
    parser.add_argument("portals", nargs="+", type=lambda value: int(value, 0),
                        help="converted portal FormIDs in observed native registration order")
    args = parser.parse_args()
    prepare(args.source, args.output, args.cell, args.portals)
