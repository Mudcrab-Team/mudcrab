"""Recreate the 989-model room-culling GLB comparison."""

import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import struct
import subprocess
import tempfile

BASELINE_DIGEST = "a4e08f52273b700779c5e0cd40d2b039ac9be6e3f1443ee45980674f69126526"
SOURCE_DIGEST = "cf8ac2dcb0c409e2b917b495467f948297c48a9954c896d70d52f67486ab4115"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def read_manifest(path, expected):
    raw = path.read_bytes()
    if sha256(raw) != expected:
        raise ValueError(f"Unexpected manifest digest for {path}")
    return json.loads(raw)


def checked_inputs(sources, snapshot, baseline_assets, baseline_manifest, source_inventory):
    hashes = read_manifest(baseline_manifest, BASELINE_DIGEST)
    inventory = read_manifest(source_inventory, SOURCE_DIGEST)
    extracted = {path.relative_to(sources).as_posix() for path in sources.rglob("*.nif")}
    if len(hashes) != 989 or set(hashes) != set(inventory) or set(hashes) != extracted:
        raise ValueError("Captured hashes, audited sources and extracted sources disagree")
    for model, expected in inventory.items():
        relative = Path(model)
        if relative.parts[0] != "meshes" or ".." in relative.parts:
            raise ValueError(f"Unexpected model path {model}")
        source = sources / relative
        baseline = (baseline_assets / relative).with_suffix(".glb")
        data = source.read_bytes()
        if sha256(data) != expected["sha256"]:
            raise ValueError(f"Source digest mismatch for {model}")
        destination = snapshot / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        if sha256(baseline.read_bytes()) != hashes[model]:
            raise ValueError(f"Captured GLB digest mismatch for {model}")
    return hashes


def make_selection(assets, models):
    with sqlite3.connect(assets / "skyrim_world.db") as db:
        db.execute("CREATE TABLE statics(id INTEGER PRIMARY KEY, model_path TEXT)")
        db.execute('CREATE TABLE "references"(base_form_id INTEGER)')
        db.executemany("INSERT INTO statics VALUES (?, ?)", enumerate(models, 1))
        db.execute('INSERT INTO "references" SELECT id FROM statics')


def glb_chunks(raw):
    magic, version, size = struct.unpack_from("<4sII", raw)
    if magic != b"glTF" or version != 2 or size != len(raw):
        raise ValueError("Invalid GLB header")
    size, kind = struct.unpack_from("<I4s", raw, 12)
    if kind != b"JSON" or 20 + size > len(raw):
        raise ValueError("Invalid GLB JSON chunk")
    return json.loads(raw[20:20 + size]), raw[20 + size:]


def scene_metrics(document):
    nodes = document.get("nodes", [])
    meshes = document.get("meshes", [])
    accessors = document.get("accessors", [])
    scenes = document.get("scenes", [])
    roots = scenes[document.get("scene", 0)].get("nodes", []) if scenes else []
    pending = [(index, None, ()) for index in roots]
    counts = dict(mesh_instances=0, primitives=0, triangles=0)
    drawable_transforms = []
    parents = {}
    local_transforms = {}
    referenced_skins = set()
    while pending:
        index, parent, path = pending.pop()
        if index in parents:
            raise ValueError("A node is reached twice in the GLB scene")
        parents[index] = parent
        node = nodes[index]
        transform = {key: node[key] for key in ("matrix", "translation", "rotation", "scale")
                     if key in node}
        local_transforms[index] = transform
        if transform:
            path += (transform,)
        pending.extend((child, index, path) for child in node.get("children", []))
        if "mesh" not in node:
            continue
        if "skin" in node:
            referenced_skins.add(node["skin"])
        counts["mesh_instances"] += 1
        drawable_transforms.append((node["mesh"], node.get("skin"), path))
        for primitive in meshes[node["mesh"]]["primitives"]:
            counts["primitives"] += 1
            if primitive.get("mode", 4) == 4:
                index = primitive.get("indices", primitive["attributes"]["POSITION"])
                counts["triangles"] += accessors[index]["count"] // 3
    joint_paths = {}
    for skin_index in referenced_skins:
        for joint in document["skins"][skin_index]["joints"]:
            if joint not in parents:
                raise ValueError(f"Joint {joint} of skin {skin_index} is outside the drawable scene")
            if joint in joint_paths:
                continue
            chain = []
            index = joint
            while index is not None:
                chain.append((index, local_transforms[index]))
                index = parents[index]
            joint_paths[joint] = chain
    return counts, drawable_transforms, joint_paths


def primitive_geometry(document):
    primitives = [
        [
            {key: primitive[key] for key in ("attributes", "indices", "mode", "targets")
             if key in primitive}
            for primitive in mesh["primitives"]
        ]
        for mesh in document.get("meshes", [])
    ]
    referenced = set()
    for mesh in primitives:
        for primitive in mesh:
            referenced.update(primitive["attributes"].values())
            if "indices" in primitive:
                referenced.add(primitive["indices"])
            for target in primitive.get("targets", []):
                referenced.update(target.values())
    skins = document.get("skins", [])
    referenced.update(skin["inverseBindMatrices"] for skin in skins if "inverseBindMatrices" in skin)
    accessors = {i: document["accessors"][i] for i in referenced}
    views = set()
    for accessor in accessors.values():
        if "bufferView" in accessor:
            views.add(accessor["bufferView"])
        if "sparse" in accessor:
            views.update(accessor["sparse"][part]["bufferView"] for part in ("indices", "values"))
    mesh_weights = {i: mesh["weights"] for i, mesh in enumerate(document.get("meshes", []))
                    if "weights" in mesh}
    node_weights = {i: node["weights"] for i, node in enumerate(document.get("nodes", []))
                    if "mesh" in node and "weights" in node}
    return primitives, skins, accessors, {i: document["bufferViews"][i] for i in views}, mesh_weights, node_weights


def compare(model, expected, baseline_assets, output):
    relative = Path(model).with_suffix(".glb")
    final = (output / relative).read_bytes()
    result = {"model": model, "baseline_sha256": expected, "final_sha256": sha256(final)}
    if result["final_sha256"] == expected:
        return result
    previous = (baseline_assets / relative).read_bytes()
    if sha256(previous) != expected:
        raise ValueError(f"Captured GLB digest mismatch for {model}")
    before, before_binary = glb_chunks(previous)
    after, after_binary = glb_chunks(final)
    before_counts, before_transforms, before_joints = scene_metrics(before)
    after_counts, after_transforms, after_joints = scene_metrics(after)
    result.update(
        changed_json_fields=sorted(key for key in before.keys() | after.keys()
                                   if before.get(key) != after.get(key)),
        same_binary_chunks=before_binary == after_binary,
        same_primitive_geometry=primitive_geometry(before) == primitive_geometry(after),
        same_drawable_transform_paths=before_transforms == after_transforms,
        same_joint_transform_paths=before_joints == after_joints,
        before_counts=before_counts,
        after_counts=after_counts,
        before_room_sphere=before.get("extras", {}).get("mudcrabRoomSphere"),
        after_room_sphere=after.get("extras", {}).get("mudcrabRoomSphere"),
    )
    return result


def run(worktree, output, sources, baseline_assets, hashes):
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=worktree, text=True).strip()

    if output.is_relative_to(Path(git("rev-parse", "--show-toplevel"))):
        raise ValueError("Choose an output directory outside the worktree")
    head = git("rev-parse", "HEAD")
    if git("status", "--porcelain"):
        raise ValueError(f"Expected a clean committed HEAD in {worktree}")
    models = sorted(hashes)
    output.mkdir()
    assets = output / "converted"
    assets.mkdir()
    make_selection(assets, models)
    receipt_path = output / "conversion.json"
    command = [
        "cargo", "run", "--locked", "--profile", "test", "-p", "converter",
        "--bin", "asset-closure-convert", "--", str(sources / "meshes"),
        str(assets), str(receipt_path),
    ]
    print(f"Converting {len(models)} models at {head} into {output}", flush=True)
    subprocess.run(command, cwd=worktree, check=True)
    receipt = json.loads(receipt_path.read_text())
    entries = receipt["results"]
    if (receipt["converted"] != len(models) or len(entries) != len(models)
            or {entry["model_path"] for entry in entries} != set(models)
            or any(entry["status"] != "converted" for entry in entries)):
        raise ValueError(f"Expected a fresh conversion of every model in {receipt_path}")
    if git("rev-parse", "HEAD") != head or git("status", "--porcelain"):
        raise ValueError(f"Worktree changed during the conversion into {output}")
    comparison = [compare(model, hashes[model], baseline_assets, assets) for model in models]
    changes = [row for row in comparison if row["baseline_sha256"] != row["final_sha256"]]
    (output / "mesh-hash-check.json").write_text(json.dumps({
        "head": head,
        "worktree": str(worktree),
        "scope": "full",
        "command": command,
        "source_inventory_sha256": SOURCE_DIGEST,
        "baseline_manifest_sha256": BASELINE_DIGEST,
        "models_verified": len(models),
        "mismatch_counts": {
            "binary_chunks": sum(not row["same_binary_chunks"] for row in changes),
            "primitive_geometry": sum(not row["same_primitive_geometry"] for row in changes),
            "drawable_transform_paths": sum(not row["same_drawable_transform_paths"] for row in changes),
            "joint_transform_paths": sum(not row["same_joint_transform_paths"] for row in changes),
            "draw_counts": sum(row["before_counts"] != row["after_counts"] for row in changes),
        },
        "changed": changes,
        "models": comparison,
    }, indent=2) + "\n")
    print(f"{len(changes)}/{len(models)} changed. Report at {output / 'mesh-hash-check.json'}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--worktree", type=Path, default=Path.cwd())
    parser.add_argument("--sources", type=Path, required=True)
    parser.add_argument("--captured-assets", type=Path, required=True)
    parser.add_argument("--captured-hashes", type=Path, required=True)
    parser.add_argument("--source-inventory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    sources = args.sources.resolve()
    baseline_assets = args.captured_assets.resolve()
    with tempfile.TemporaryDirectory(prefix="room-corpus-") as directory:
        snapshot = Path(directory)
        hashes = checked_inputs(sources, snapshot, baseline_assets, args.captured_hashes, args.source_inventory)
        run(args.worktree.resolve(), args.output.resolve(), snapshot, baseline_assets, hashes)


if __name__ == "__main__":
    main()
