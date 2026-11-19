"""Export sealed native fixture observations for Mudcrab's ignored comparison test."""

import argparse
import hashlib
import json
import math
import struct
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def checked(root, relative, expected):
    data = (root / relative).read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"changed sealed input: {relative}")
    return json.loads(data.decode("utf-8"))


def export(native, model):
    manifest = (native / "final-results.json").read_bytes()
    results = json.loads(manifest.decode("utf-8"))
    blob = model.read_bytes()
    if struct.unpack_from("<III", blob) != (0x46546C67, 2, len(blob)):
        raise ValueError("invalid GLB header")
    size, kind = struct.unpack_from("<II", blob, 12)
    if kind != 0x4E4F534A:
        raise ValueError("missing GLB JSON")
    gltf = json.loads(blob[20:20 + size])
    if len(gltf["meshes"]) != 1 or len(gltf["meshes"][0]["primitives"]) != 1:
        raise ValueError("marker must have one drawable primitive")
    nodes = gltf["nodes"]
    roots = gltf["scenes"][gltf.get("scene", 0)]["nodes"]
    if len(roots) != 1:
        raise ValueError("marker must have one scene root")
    root = roots[0]
    if type(root) is not int or not 0 <= root < len(nodes):
        raise ValueError("invalid marker root")
    basis = nodes[root].get("rotation", [])
    expected = [-math.sqrt(0.5), 0, 0, math.sqrt(0.5)]
    if len(basis) != 4 or max(abs(a-b) for a,b in zip(basis, expected)) > 1e-6:
        raise ValueError("unsupported marker basis")
    visited = set()

    def walk(index):
        if type(index) is not int or not 0 <= index < len(nodes):
            raise ValueError("invalid marker child")
        if index in visited:
            raise ValueError("marker hierarchy is not a tree")
        visited.add(index)
        node = nodes[index]
        allowed = {"children", "name", "mesh", "extras"} | ({"rotation"} if index == root else set())
        if node.keys() - allowed or ("mesh" in node and node["mesh"] != 0):
            raise ValueError("unsupported marker node transform or mesh")
        for child in node.get("children", []):
            walk(child)

    walk(root)
    if len(visited) != len(nodes):
        raise ValueError("marker has detached nodes")
    if not any(nodes[index].get("mesh") == 0 for index in visited):
        raise ValueError("marker mesh has no reachable instance")
    primitive = gltf["meshes"][0]["primitives"][0]
    accessor = gltf["accessors"][primitive["attributes"]["POSITION"]]
    lo, hi = accessor["min"], accessor["max"]
    bounds = dict(min=[lo[0], lo[2], -hi[1]], max=[hi[0], hi[2], -lo[1]])
    cases = []
    for name, sealed in results["cases"].items():
        replay = checked(native, sealed["final_replay"], sealed["final_replay_sha256"])
        if replay["expression_oracle"] != "known" or len(replay["attempts"]) != 3:
            raise ValueError(f"unadmitted native case: {name}")
        checked(native, sealed["index"], sealed["index_sha256"])
        if digest(native / sealed["image"]) != sealed["image_sha256"]:
            raise ValueError(f"changed native image: {name}")
        for window, attempt in enumerate(replay["attempts"]):
            if not attempt["accepted"]:
                raise ValueError(f"rejected native window: {name}/{window}")
            obs = attempt["observation"]
            camera = obs["owner"]["camera"]
            if camera["target"]["exe_sha256"] != results["executable_sha256"]:
                raise ValueError("native executable identity changed")
            if abs(camera["yaw"]) > 1e-5 or abs(camera["pitch"]) > 1e-5 or camera["orthographic"]:
                raise ValueError("comparison requires the captured +Y perspective pose")
            if camera["rotation_matrix"] != [0, 0, 1, 1, 0, 0, 0, 1, 0]:
                raise ValueError("comparison requires the captured unrolled camera basis")
            fixture = obs["fixture"]
            node_roles = {v["node"]: k for k,v in fixture["rooms"].items()}
            rooms, portals, markers = [], [], []
            for role, room in fixture["rooms"].items():
                if room["rotation"] != [1,0,0,0,1,0,0,0,1] or room["joined"] or room["occluders"]:
                    raise ValueError("unsupported fixture room")
                rooms.append(dict(role=role, form=room["form"], center=room["center"], half=room["half"]))
            for role, portal in fixture["portals"].items():
                if portal["rotation"] != [1,0,0,0,1,0,0,0,1]:
                    raise ValueError("unsupported fixture portal")
                portals.append(dict(role=role, form=portal["form"], center=portal["center"], half=portal["size"],
                                    rooms=[fixture["rooms"][node_roles[n]]["form"] for n in portal["endpoints"]],
                                    corners=portal["derived_corners"]))
            for room, judgments in obs["judgments"]["controls"].items():
                for label in ("left", "gap", "right"):
                    marker = fixture["markers"][f"marker/{room}/{label}"]
                    if marker["base"] != 180151 or marker["rotation"] != [0,0,0]:
                        raise ValueError("marker does not use the verified soul gem")
                    judgment = judgments[label]["result"]
                    if type(judgment) is not bool:
                        raise ValueError("unknown native judgment")
                    markers.append(dict(room=room, label=label, position=marker["position"], sphere=marker["sphere"], native=judgment))
            cases.append(dict(name=name, window=window, camera=camera["position"], near_shifted=camera["near_shifted"],
                              frustum=camera["frustum"], rooms=rooms, portals=portals, markers=markers,
                              native_controls=sealed["controls"], replay_sha256=sealed["final_replay_sha256"]))
    return dict(schema=1, native_executable_sha256=results["executable_sha256"],
                sealed_results_sha256=hashlib.sha256(manifest).hexdigest(), marker_glb_sha256=hashlib.sha256(blob).hexdigest(),
                mesh_bounds_runtime=bounds, cases=cases)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = export(args.native, args.model)
    with args.output.open("x", encoding="utf-8") as out:
        json.dump(result, out, indent=2, sort_keys=True)
        out.write("\n")
