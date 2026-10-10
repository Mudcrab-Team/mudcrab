#!/usr/bin/env python3
"""Audit published LOD without changing it; invoke explicitly after conversions finish."""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import struct
import time


def sha(data):
    return hashlib.sha256(data).hexdigest()


def sha_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def canonical_digest(value):
    return sha(json.dumps(value, sort_keys=True, separators=(",", ":")).encode())


def coverage_snapshot(connection):
    worlds = connection.execute("SELECT id,editor_id,lod_origin_x,lod_origin_y FROM worldspaces ORDER BY id").fetchall()
    require(all((x is None) == (y is None) for _, _, x, y in worlds), "partially indexed LOD origin")
    land = connection.execute("SELECT c.worldspace_id,c.grid_x,c.grid_y,c.id FROM cells c JOIN land l ON l.cell_id=c.id WHERE c.grid_x IS NOT NULL AND c.grid_y IS NOT NULL ORDER BY c.worldspace_id,c.grid_x,c.grid_y,c.id").fetchall()
    plugins = connection.execute("SELECT id,name,priority,hex(checksum) FROM plugins ORDER BY id").fetchall()
    return {"worldspaces": worlds, "land_cells": land, "plugins": plugins}


def reference_coverage(connection, reference):
    current = coverage_snapshot(connection)
    if reference is None:
        return current, None
    reference = reference.resolve()
    with sqlite3.connect("file:" + str(reference / "skyrim_world.db") + "?mode=ro", uri=True) as previous:
        expected = coverage_snapshot(previous)
    for kind in current:
        require(current[kind] == expected[kind], f"{kind} differs from the explicit coverage reference")
    manifest_path = reference / "conversion-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    require(manifest["complete"] and not manifest.get("failures"), "coverage reference conversion is incomplete")
    return current, {"pack": str(reference), "manifest_hash": sha_file(manifest_path), "coverage_hash": canonical_digest(expected), "archives": manifest["archives"]}


def audit_lod_warnings(root, report, coverage, reference, data_root):
    warnings = report.get("lod_warnings", [])
    require(isinstance(warnings, list) and all(isinstance(w, str) for w in warnings), "invalid LOD warning inventory")
    require(len(warnings) == len(set(warnings)), "duplicate LOD warning")
    missing = [w for w in coverage["worldspaces"] if w[2] is None]
    expected_origins = {
        f"worldspace {name} ({world:08X}) has no LOD origin: no lod_origins entry and no lodsettings/{name}.lod; its terrain LOD is skipped"
        for world, name, _, _ in missing
    }
    if reference is None:
        require(not warnings and not missing, "LOD omissions require an explicit verified coverage reference")
        return {"missing_origin_worldspaces": [], "omitted_archives": []}
    # Archive omissions belong only to the retained-asset metadata route. Full
    # extraction processes the original archive inventory, including this archive.
    plugin_stems = [Path(row[1]).stem.lower() for row in coverage["plugins"]]
    archives = reference["archives"]
    unmatched = {}
    if (root / "metadata-rebuild.json").is_file():
        for name, archive in archives.items():
            stem = Path(name).stem.lower()
            matched = any(stem == plugin or (stem.startswith(plugin) and stem[len(plugin):len(plugin) + 1] in (" ", "-", "_")) for plugin in plugin_stems)
            if not matched:
                unmatched[Path(name).name.lower()] = archive
    remaining = set(warnings) - expected_origins
    require(expected_origins <= set(warnings), "missing-origin warning inventory differs from reference coverage")
    omitted = {}
    for warning in remaining:
        match = re.fullmatch(r"LOD extraction omitted archive (.+): no matching source-package plugin", warning)
        require(match is not None, f"unexpected LOD warning: {warning}")
        require(data_root is not None, "archive omissions require --data-root")
        path = Path(match[1]).resolve()
        name = path.name.lower()
        require(path.is_relative_to(data_root.resolve()) and path.is_file(), "omitted archive escapes source data or is missing")
        require(name in unmatched and name not in omitted, f"unexpected omitted archive: {name}")
        digest = sha_file(path)
        require(digest == unmatched[name]["source_hash"], f"omitted archive changed from reference: {name}")
        omitted[name] = {"path": str(path), "source_hash": digest, "reason": "no matching reference source-package plugin"}
    require(set(omitted) == set(unmatched), "omitted-archive warning inventory differs from reference packages")
    counts = {}
    for world, _, _, _ in coverage["land_cells"]:
        counts[world] = counts.get(world, 0) + 1
    return {"missing_origin_worldspaces": [{"id": world, "editor_id": name, "land_cells": counts.get(world, 0)} for world, name, _, _ in missing], "omitted_archives": list(omitted.values()), "warnings_checked": len(warnings)}


def ktx_contract(data):
    require(data[:12] == b"\xabKTX 20\xbb\r\n\x1a\n", "invalid KTX2 identifier")
    require(len(data) >= 80, "truncated KTX2 header")
    fields = struct.unpack_from("<13I", data, 12)
    vk, typesize, width, height, depth, layers, faces, levels, compression, dfd_off, dfd_len, kvd_off, kvd_len = fields
    require(vk == 0 and typesize == 1, "atlas must use a DFD-described byte format")
    require(0 < width <= 1024 and height == width and depth <= 1 and layers <= 1 and faces == 1, "invalid atlas dimensions")
    require(levels == 3, "terrain atlas must preserve exactly three mips")
    require(compression in (0, 2), "unexpected atlas supercompression")
    require(dfd_len >= 28 and dfd_off + dfd_len <= len(data), "invalid DFD range")
    require(data[dfd_off + 12] == 166, "atlas DFD must identify UASTC")
    require(data[dfd_off + 14] == 2, "atlas DFD must identify sRGB")
    require(kvd_off + kvd_len <= len(data), "invalid KVD range")
    payloads = []
    for mip in range(levels):
        offset, length, expanded = struct.unpack_from("<3Q", data, 80 + mip * 24)
        expected = ((max(width >> mip, 1) + 3) // 4) ** 2 * 16
        require(offset >= 80 + levels * 24 and offset + length <= len(data), "invalid mip range")
        require(expanded == expected, "invalid expanded UASTC block length")
        require(compression != 0 or length == expected, "invalid raw UASTC block length")
        payloads.append({"mip": mip, "offset": offset, "bytes": length, "expanded_bytes": expanded})
    return {"width": width, "height": height, "levels": levels, "faces": faces, "layers": layers, "depth": depth, "srgb": True, "uastc": True, "supercompression": compression, "mips": payloads}


def audit_chunk(root, row, cells, proof):
    world, tier, ax, ay, rel, recorded_hash, xmin, ymin, zmin, xmax, ymax, zmax, source_cells = row
    expected_path = f"lod/{world:08x}/{tier}/cell_{ax}_{ay}.glb"
    require(rel == expected_path, "noncanonical payload path")
    path = (root / rel).resolve()
    require(path.is_relative_to(root) and path.is_file(), "missing or escaping payload")
    data = path.read_bytes()
    digest = sha(data)
    require(digest == recorded_hash, "payload hash differs from database")
    require(len(data) >= 28 and data[:4] == b"glTF", "invalid GLB header")
    version, total, jlen, jtype = struct.unpack_from("<4I", data, 4)
    require(version == 2 and total == len(data) and jtype == 0x4E4F534A, "invalid GLB header fields")
    doc = json.loads(data[20:20 + jlen])
    boff = 20 + jlen
    blen, btype = struct.unpack_from("<2I", data, boff)
    require(btype == 0x004E4942 and boff + 8 + blen == len(data), "invalid GLB BIN range")
    binary = data[boff + 8:]
    require(len(doc["buffers"]) == 1 and doc["buffers"][0]["byteLength"] <= len(binary), "invalid embedded buffer")
    views = doc["bufferViews"]
    for view in views:
        require(view["buffer"] == 0 and view.get("byteOffset", 0) + view["byteLength"] <= len(binary), "bufferView escapes BIN")
    require(len(doc["images"]) == 1 and doc["images"][0]["mimeType"] == "image/ktx2", "invalid embedded atlas image")
    image_view_id = doc["images"][0]["bufferView"]
    image_view = views[image_view_id]
    image_start = image_view.get("byteOffset", 0)
    atlas_bytes = binary[image_start:image_start + image_view["byteLength"]]
    atlas = ktx_contract(atlas_bytes)
    nodes = doc["nodes"]
    roots = doc["scenes"][doc["scene"]]["nodes"]
    require(roots == [0], "terrain scene must use its sole chunk root")
    seen = set()
    stack = list(roots)
    while stack:
        index = stack.pop()
        require(0 <= index < len(nodes) and index not in seen, "missing node, graph cycle, or shared child")
        seen.add(index)
        node = nodes[index]
        if "mesh" in node:
            require(0 <= node["mesh"] < len(doc["meshes"]), "invalid mesh node")
        stack.extend(node.get("children", []))
    require(len(seen) == len(nodes), "unreachable terrain nodes")
    expected_cells = [tuple(map(int, p.split(","))) for p in source_cells.split(";") if p]
    require(expected_cells and len(set(expected_cells)) == len(expected_cells), "empty or duplicate indexed cells")
    actual_cells = []
    for index in nodes[0].get("children", []):
        node = nodes[index]
        require(node["name"].startswith("cell_"), "unexpected chunk-root child")
        xy = tuple(map(int, node["name"][5:].split("_")))
        require(cells.get((world, *xy)) == node["extras"]["cell_id"], "source-cell handoff identity differs from current DB")
        actual_cells.append(xy)
        require(len(node["children"]) == 1, "source cell must have one terrain group")
        group = nodes[node["children"][0]]
        require(group["name"] == "terrain" and len(group["children"]) == 4, "missing terrain quadrants")
        require([nodes[n]["name"] for n in group["children"]] == ["terrain_quadrant_sw", "terrain_quadrant_se", "terrain_quadrant_nw", "terrain_quadrant_ne"], "quadrant identity/order differs")
    require(len(actual_cells) == len(expected_cells) and set(actual_cells) == set(expected_cells), "GLB source cells differ from indexed coverage")
    # Hash geometry and scene metadata independently of codec/container differences.
    geometry_doc = json.loads(json.dumps(doc))
    geometry_doc["buffers"][0]["byteLength"] = image_start
    geometry_doc["bufferViews"][image_view_id]["byteLength"] = 0
    geometry = sha(json.dumps(geometry_doc, sort_keys=True, separators=(",", ":")).encode() + binary[:image_start])
    return rel, {"content_hash": digest, "bytes": len(data), "geometry_hash": geometry, "atlas_hash": sha(atlas_bytes), "atlas_contract": atlas, "key": [world, tier, ax, ay], "source_cells": [list(xy) for xy in sorted(expected_cells)], "bounds": [xmin, ymin, zmin, xmax, ymax, zmax], "input_proof": proof.get(rel)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pack", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expected-chunks", type=int, default=4673)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--allow-unproven", action="store_true", help="Only for an intentionally induced GPU fallback run")
    parser.add_argument("--compare", type=Path)
    parser.add_argument("--comparison", choices=["exact", "encoder-transition"], default="exact")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--expected-hits", type=int)
    parser.add_argument("--encoder", choices=["cpu", "gpu"])
    parser.add_argument("--coverage-reference-pack", type=Path, help="Verify unchanged worldspace origins, LAND identities and plugin proofs before accepting documented source omissions")
    parser.add_argument("--data-root", type=Path, help="Source Data directory; hash-check any explicitly omitted archive against the coverage reference")
    args = parser.parse_args()
    started = time.monotonic()
    root = args.pack.resolve()
    conversion = json.loads((root / "conversion-manifest.json").read_text())
    lod = json.loads((root / "lod-manifest.json").read_text())
    integration = json.loads((root / "integration-report.json").read_text())
    require(conversion["complete"] and not conversion.get("failures"), "ordinary conversion is incomplete")
    require(integration["passed"], "integration validation failed")
    require(lod.get("compiler_version", 0) > 0, "LOD manifest lacks explicit producer proof")
    require(len(lod["build_identity"]) == 64 and all(c in "0123456789abcdef" for c in lod["build_identity"]), "noncanonical build identity")
    connection = sqlite3.connect("file:" + str(root / "skyrim_world.db") + "?mode=ro", uri=True)
    require(connection.execute("PRAGMA integrity_check").fetchone() == ("ok",), "database integrity failure")
    require(connection.execute("SELECT build_identity FROM lod_build WHERE id=1").fetchone() == (lod["build_identity"],), "LOD build identity mismatch")
    rows = connection.execute("SELECT worldspace_id,tier,anchor_x,anchor_y,payload_path,content_hash,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,source_cells FROM lod_chunks ORDER BY worldspace_id,tier,anchor_x,anchor_y").fetchall()
    require(len(rows) == lod["chunks"] == args.expected_chunks, "LOD chunk count differs from expected coverage")
    coverage, reference = reference_coverage(connection, args.coverage_reference_pack)
    omissions = None
    if args.report:
        report = json.loads(args.report.read_text())
        require(report["complete"], "conversion report is incomplete")
        omissions = audit_lod_warnings(root, report, coverage, reference, args.data_root)
        require(report["lod_chunks"] == len(rows), "report and indexed chunk counts differ")
        if args.expected_hits is not None:
            require(report["lod_cache_hits"] == args.expected_hits, "LOD reuse differs from the expected count")
        if not args.allow_unproven:
            require(report.get("lod_cpu_fallback_chunks", 0) == 0, "healthy run fell back to CPU")
            if args.encoder == "gpu":
                require(report.get("lod_gpu_chunks") == len(rows) - report["lod_cache_hits"], "new GPU chunks were not all GPU encoded")
            elif args.encoder == "cpu":
                require(report.get("lod_gpu_chunks", 0) == 0, "CPU atlas run unexpectedly reports GPU chunks")
    proof = lod.get("chunk_inputs", {})
    paths = {row[4] for row in rows}
    require(set(proof) <= paths, "input proofs reference nonexistent chunks")
    require(args.allow_unproven or set(proof) == paths, "healthy encoder run lacks reusable input proof")
    require(all(len(h) == 64 and all(c in "0123456789abcdef" for c in h) for h in proof.values()), "noncanonical input proof")
    cells = {(w, x, y): cell_id for w, x, y, cell_id in connection.execute("SELECT c.worldspace_id,c.grid_x,c.grid_y,c.id FROM cells c JOIN land l ON l.cell_id=c.id WHERE c.grid_x IS NOT NULL AND c.grid_y IS NOT NULL")}
    origins = {w: (x, y) for w, x, y in connection.execute("SELECT id,lod_origin_x,lod_origin_y FROM worldspaces WHERE lod_origin_x IS NOT NULL AND lod_origin_y IS NOT NULL")}
    compiled_worlds = {row[0] for row in rows}
    require(compiled_worlds == {w for w, _, _ in cells if w in origins}, "compiled worldspaces differ from all LAND worlds with indexed origins")
    expected_members = {}
    for world, x, y in cells:
        if world not in compiled_worlds:
            continue
        require(world in origins, "compiled world lacks an indexed LOD origin")
        ox, oy = origins[world]
        for tier in (4, 8, 16):
            key = (world, tier, (x - ox) // tier, (y - oy) // tier)
            expected_members.setdefault(key, set()).add((x, y))
    require(set(expected_members) == {tuple(row[:4]) for row in rows}, "chunk keys differ from current LAND coverage")
    for row in rows:
        indexed = [tuple(map(int, p.split(","))) for p in row[-1].split(";") if p]
        require(len(indexed) == len(expected_members[tuple(row[:4])]) and set(indexed) == expected_members[tuple(row[:4])], "chunk membership differs from current LAND coverage")
    spatial = connection.execute("SELECT worldspace_id,tier,anchor_x,anchor_y,minX,maxX,minY,maxY FROM lod_chunks_spatial").fetchall()
    spatial_by_key = {}
    for w, t, x, y, *bounds in spatial:
        require((w, t, x, y) not in spatial_by_key, "duplicate spatial chunk key")
        spatial_by_key[w, t, x, y] = bounds
    require(set(spatial_by_key) == {tuple(row[:4]) for row in rows}, "spatial keys differ from chunk keys")
    for row in rows:
        sminx, smaxx, sminy, smaxy = spatial_by_key[tuple(row[:4])]
        xmin, ymin, _, xmax, ymax, _, _ = row[6:]
        require(sminx <= xmin and smaxx >= xmax and sminy <= ymin and smaxy >= ymax, "spatial bounds do not enclose chunk bounds")
    actual_paths = {p.relative_to(root).as_posix() for p in (root / "lod").rglob("*.glb")}
    require(actual_paths == paths, "missing or orphan LOD files")
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        chunks = dict(pool.map(lambda row: audit_chunk(root, row, cells, proof), rows))
    result = {"passed": True, "pack": str(root), "chunks": chunks, "lod_manifest": lod, "conversion_schema": conversion["schema_version"], "world_database_hash": sha_file(root / "skyrim_world.db"), "cell_cache_hash": sha_file(root / "cell_cache.rkyv"), "ordinary_manifest_hash": sha((root / "conversion-manifest.json").read_bytes()), "chunks_with_proof": len(proof), "method": "Independent hashes, DB integrity, LAND/file/row/spatial coverage, GLB scene handoff graph, cached terrain identity, and KTX2 three-mip/UASTC/sRGB contract. Does not decode UASTC or replace converter check --full for ordinary assets."}
    result["coverage_hash"] = canonical_digest(coverage)
    result["coverage_reference"] = {k: v for k, v in reference.items() if k != "archives"} if reference else None
    result["documented_omissions"] = omissions
    if args.compare:
        previous = json.loads(args.compare.read_text())
        require(previous.get("coverage_hash") == result["coverage_hash"], "compared worldspace origins, LAND identities or plugins changed")
        require(set(previous["chunks"]) == set(chunks), "compared chunk coverage differs")
        require(previous["lod_manifest"]["terrain_sources"] == lod["terrain_sources"], "compared DDS input identities differ")
        require(previous["cell_cache_hash"] == result["cell_cache_hash"], "compared cached terrain changed")
        for rel, chunk in chunks.items():
            old = previous["chunks"][rel]
            require(old["geometry_hash"] == chunk["geometry_hash"], f"geometry changed: {rel}")
            require(old["source_cells"] == chunk["source_cells"] and old["bounds"] == chunk["bounds"], f"indexed coverage changed: {rel}")
            require({k: old["atlas_contract"][k] for k in ("width", "height", "levels", "faces", "srgb", "uastc")} == {k: chunk["atlas_contract"][k] for k in ("width", "height", "levels", "faces", "srgb", "uastc")}, f"atlas contract changed: {rel}")
            if args.comparison == "exact":
                require(old["content_hash"] == chunk["content_hash"] and old["input_proof"] == chunk["input_proof"], f"warm output or proof changed: {rel}")
            else:
                require(old["input_proof"] != chunk["input_proof"], f"encoder transition reused the old recipe: {rel}")
        if args.comparison == "exact":
            require(previous["lod_manifest"] == lod, "warm LOD manifest changed")
        result["comparison"] = {"passed": True, "mode": args.comparison, "previous": str(args.compare)}
    result["elapsed_seconds"] = time.monotonic() - started
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ("chunks", "lod_manifest")}))


if __name__ == "__main__":
    main()
