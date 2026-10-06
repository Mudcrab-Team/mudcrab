#!/usr/bin/env python3
"""Read-only verification of the corrected PR #167 Fiji cold/warm evidence."""

import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import sqlite3


HEAD = "c9894eded2e7c3dbe1732965b176f1a624b959ae"
TREE = "24690d6e15004bf91359269273958feec922f0ac"
BINARY = "daf24867aa9aa4782b20712a6bcdafec45fcad6f4ee0d1fb2e1957af5a052f8a"
FILES = (
    "measurement-v4.json", "provenance-v4.json", "cold-lod-v4.json",
    "warm-lod-v4.json", "cold-lod-v4.log", "warm-lod-v4.log",
    "check-full-v4.log", "setup-primary-proof-v4.json", "measure_v4_fiji.py",
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def json_sha(value):
    return hashlib.sha256(json.dumps(value, separators=(",", ":")).encode()).hexdigest()


def load(path):
    return json.loads(path.read_text())


def ordinary_proof(manifest):
    canonical = "\n".join(
        f"{key}:{entry['output']}:{entry['output_size']}:{entry['output_hash']}"
        for key, entry in sorted(manifest["entries"].items())
    )
    return {
        "count": len(manifest["entries"]),
        "digest": hashlib.sha256(canonical.encode()).hexdigest(),
        "schema": manifest["schema_version"],
        "configuration": manifest["configuration_hash"],
    }


def check_evidence(root):
    measurement = load(root / "measurement-v4.json")
    provenance = load(root / "provenance-v4.json")
    for document in (measurement, provenance):
        require(document["head"] == HEAD and document["tree"] == TREE, "Source identity differs")
        require(document["converter_sha256"] == BINARY, "Binary identity differs")
    require(measurement["status"] == "finished", "Measurement did not finish")
    require("active_run" not in measurement, "Measurement still has an active run")
    require(measurement["full_check_exit_code"] == 0, "Ordinary full check failed")
    require(provenance["compiler_version"] == 4, "Wrong terrain producer")
    seed = provenance["seed_ordinary_proof"]
    require(seed == measurement["seed_ordinary_proof"] and seed["count"] == 76213, "Seed differs")
    require(provenance["input_inventory"] == measurement["input_inventory"], "Inventory differs")
    primary = load(root / "setup-primary-proof-v4.json")
    require(primary["before"] == primary["after"], "Setup changed the primary checkout")
    runs = measurement["runs"]
    require(len(runs) == 2, "Expected exactly one cold/warm pair")
    summaries = []
    for run, name, hits in zip(runs, ("cold-lod-v4", "warm-lod-v4"), (0, 4673)):
        require(run["name"] == name and run["exit_code"] == 0, "Run failed or differs")
        require(not run["disk_guard_interrupted"], "Disk guard interrupted the run")
        # The raw command pins its native root even when metadata is copied locally.
        native_report = Path(run["command"][-1])
        native_root = native_report.parent
        expected_command = [
            provenance["loader"], "--library-path", provenance["libraries"],
            str(native_root / "bin/converter-v4"), provenance["data"], str(native_root / "assets"),
            "--cpu-jobs", "6", "--io-jobs", "4", "--report-json", str(native_root / f"{name}.json"),
        ]
        require(run["command"] == expected_command, "Run command differs from pinned producer/options")
        require(native_root == Path(runs[0]["command"][-1]).parent, "Cold/warm package paths differ")
        report_path = root / f"{name}.json"
        require(sha(report_path) == run["report_sha256"], "Report bytes differ")
        report = load(report_path)
        require(all(report[key] == value for key, value in run["report"].items()), "Report summary differs")
        require(report["complete"] and report["converted"] == report["skipped"] == 0, "Run incomplete")
        require(report["lod_chunks"] == 4673 and report["lod_cache_hits"] == hits, "Chunk reuse differs")
        require(report["integration"]["passed"], "Integration failed")
        require(run["ordinary_proof"] == seed, "Ordinary proofs changed")
        rows = run["index_rows"]
        require(rows == sorted(rows) and len(rows) == 4673, "Index coverage/order differs")
        paths = {row[0] for row in rows}
        require(len(paths) == 4673 and paths == set(run["chunk_inputs"]), "Chunk input coverage differs")
        require(json_sha(rows) == run["lod_index_digest"], "Chunk index digest differs")
        integration = {key: value for key, value in report["integration"].items() if key != "issues"}
        summaries.append({
            **{key: run[key] for key in ("name", "exit_code", "wall_seconds", "disk_guard_interrupted", "report_sha256", "report")},
            "integration": integration,
            "lod_warnings": len(report["lod_warnings"]),
            "pruned_texture_references": report["pruned_texture_references"],
        })
    for key in ("index_rows", "chunk_inputs", "lod_index_digest", "lod_build_identity"):
        require(runs[0][key] == runs[1][key], f"Cold/warm {key} differs")
    require("All good: 76213 files" in (root / "check-full-v4.log").read_text(), "Full check completion missing")
    require("Reused 4673/4673 terrain LOD chunks" in (root / "warm-lod-v4.log").read_text(), "Warm completion missing")
    return {
        "implementation_head": HEAD, "implementation_tree": TREE, "converter_sha256": BINARY,
        "terrain_compiler_version": 4,
        "started_utc": measurement["started_utc"], "finished_utc": measurement["finished_utc"],
        "cpu_jobs": measurement["cpu_jobs"], "io_jobs": measurement["io_jobs"],
        "minimum_free_disk_bytes": measurement["minimum_free_disk_bytes"],
        "ordinary_proof": seed, "runs": summaries,
        "lod_index_digest": runs[0]["lod_index_digest"],
        "lod_build_identity": runs[0]["lod_build_identity"],
        "chunk_inputs_digest": json_sha(sorted(runs[0]["chunk_inputs"].items())),
        "full_check_exit_code": measurement["full_check_exit_code"],
        "source_sha256": {name: sha(root / name) for name in FILES},
    }, measurement, provenance


def check_published(root, assets, measurement, provenance):
    # Share the converter's package lock so publication cannot race this read.
    with assets.with_suffix(".lock").open("rb") as lock:
        fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
        ordinary_path = assets / "conversion-manifest.json"
        lod_path = assets / "lod-manifest.json"
        before = (sha(ordinary_path), sha(lod_path))
        ordinary, lod = load(ordinary_path), load(lod_path)
        require(ordinary["complete"] and not ordinary["failures"], "Published package incomplete")
        require(ordinary_proof(ordinary) == measurement["seed_ordinary_proof"], "Published ordinary proof differs")
        require((lod["compiler_version"], lod["converter_schema"], lod["world_database_schema"]) == (4, 24, 7), "Published producer differs")
        with sqlite3.connect(f"{(assets / 'skyrim_world.db').as_uri()}?mode=ro", uri=True) as database:
            rows = list(database.execute("SELECT payload_path, content_hash, source_cells FROM lod_chunks ORDER BY payload_path"))
            build = database.execute("SELECT build_identity FROM lod_build WHERE id=1").fetchone()[0]
        warm = measurement["runs"][1]
        require(Path(warm["command"][-1]).parent == root and assets == root / "assets", "Hashed package differs from recorded runs")
        require(rows == [tuple(row) for row in warm["index_rows"]], "Published index differs")
        require(build == lod["build_identity"] == warm["lod_build_identity"], "Published build differs")
        require(lod["chunks"] == len(rows) == 4673 and lod["chunk_inputs"] == warm["chunk_inputs"], "Published inputs differ")
        byte_counts = {"ordinary": 0, "lod": 0}
        for kind, entries in (
            ("ordinary", ((e["output"], e["output_hash"], e["output_size"]) for e in ordinary["entries"].values())),
            ("lod", ((row[0], row[1], None) for row in rows)),
        ):
            for relative, expected, size in entries:
                path = (assets / relative).resolve(strict=True)
                require(path.is_relative_to(assets), f"Output escapes package: {relative}")
                actual_size = path.stat().st_size
                require(size is None or actual_size == size, f"Output size differs: {relative}")
                require(sha(path) == expected, f"Output hash differs: {relative}")
                byte_counts[kind] += actual_size
        require(before == (sha(ordinary_path), sha(lod_path)), "Published manifests changed during hashing")
        require(sha(root / "bin/converter-v4") == BINARY, "Native binary differs")
        require(all(sha(Path(path)) == expected for path, expected in provenance["runtime_files"].items()), "Native runtime differs")
        require(sha(Path(provenance["seed_source"]) / "conversion-manifest.json") == provenance["seed_manifest_sha256"], "Original seed manifest changed")
        return {
            "ordinary_files_verified": 76213, "lod_files_verified": 4673,
            "bytes_verified": byte_counts, "ordinary_manifest_sha256": before[0],
            "lod_manifest_sha256": before[1], "original_seed_manifest_unchanged": True,
            "native_binary_and_runtime_hashes_verified": True,
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--assets", type=Path, help="Also hash every published ordinary and LOD output on Fiji")
    args = parser.parse_args()
    root = args.evidence.resolve(strict=True)
    result, measurement, provenance = check_evidence(root)
    if args.assets:
        result["published_hash_check"] = check_published(root, args.assets.resolve(strict=True), measurement, provenance)
    expected = load(Path(__file__).with_name("summary.json"))
    if not args.assets:
        del expected["published_hash_check"]
    require(result == expected, "Evidence differs from committed summary.json")
    print(json.dumps(result, indent=2) + "\n", end="")


if __name__ == "__main__":
    main()
