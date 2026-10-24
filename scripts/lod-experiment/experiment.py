#!/usr/bin/env python3
"""Compare conventional index LODs with a cluster DAG without modifying GLBs."""
import argparse
import copy
from collections import Counter, defaultdict
import hashlib
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import time
from urllib.parse import unquote, urlsplit

PIN = "9e1f07b159d3cb777f1c67ed31fc11fd117986f4"
COMPONENTS = {5120: ("b", 1), 5121: ("B", 1), 5122: ("h", 2), 5123: ("H", 2), 5125: ("I", 4), 5126: ("f", 4)}
WIDTHS = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}
MATERIAL_EXTENSIONS = {"KHR_materials_emissive_strength", "KHR_materials_specular"}
REQUIRED_EXTENSIONS = {"KHR_texture_basisu", "KHR_texture_transform"} | MATERIAL_EXTENSIONS
DRIVER_BYTES = Path(__file__).read_bytes()


class Omission(ValueError):
    """Supported GLB data outside this experiment's narrow geometry route."""


class InputChanged(RuntimeError):
    """An immutable input boundary changed; do not publish mixed candidates."""


BUILDER_DIAGNOSTIC_LIMIT = 2048


def builder_failure(error, staging):
    """Failed-row reason for a builder that exited nonzero or timed out.

    Only the staging directory is normalized, so one input and outcome give the
    same manifest in every run; the builder's own stderr is kept, bounded.
    """
    if isinstance(error, subprocess.TimeoutExpired):
        status = f"timed out after {error.timeout:g} s"
    else:
        status = f"exited with status {error.returncode}"
    stderr = error.stderr or b""
    if isinstance(stderr, str):
        stderr = stderr.encode()
    text = stderr.decode("utf-8", errors="replace").replace(staging, "<staging>").strip()
    if len(text) > BUILDER_DIAGNOSTIC_LIMIT:
        text = text[:BUILDER_DIAGNOSTIC_LIMIT] + " ...[truncated]"
    return f"builder {status}" + (f": {text}" if text else "")


def watch(watched, path, checksum):
    if path in watched and watched[path] != checksum:
        raise InputChanged("source input changed during experiment: " + str(path))
    watched[path] = checksum


def without_symlinks(path, root):
    path = Path(os.path.abspath(path))
    if not path.is_relative_to(root):
        raise Omission("path escapes asset root")
    current = path
    while current != root:
        if current.is_symlink():
            raise Omission("symlinked asset paths are unsupported")
        current = current.parent
    return path


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_digest(path):
    hasher = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


def strict_json(data):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError(f"duplicate JSON key: {key}")
            result[key] = value
        return result

    def constant(value):
        raise ValueError(f"non-finite JSON constant: {value}")

    def number(value):
        result = float(value)
        if not math.isfinite(result):
            raise ValueError("non-finite JSON number")
        return result

    return json.loads(data, object_pairs_hook=pairs, parse_constant=constant, parse_float=number)


def integer(value, name, maximum=None):
    if type(value) is not int or value < 0 or (maximum is not None and value > maximum):
        raise ValueError(f"invalid {name}")
    return value


def item(document, table, index):
    rows = document.get(table, [])
    index = integer(index, table + " index")
    if not isinstance(rows, list) or index >= len(rows) or not isinstance(rows[index], dict):
        raise ValueError(f"invalid {table} index {index}")
    return rows[index]


def parse_glb(data):
    if len(data) < 20:
        raise ValueError("truncated GLB")
    magic, version, length = struct.unpack_from("<4sII", data)
    if magic != b"glTF" or version != 2 or length != len(data):
        raise ValueError("invalid GLB header/length")
    chunks, offset = [], 12
    while offset < len(data):
        if offset + 8 > len(data):
            raise ValueError("truncated GLB chunk header")
        size, kind = struct.unpack_from("<II", data, offset)
        offset += 8
        if size % 4 or offset + size > len(data):
            raise ValueError("invalid GLB chunk size")
        chunks.append((kind, data[offset:offset + size]))
        offset += size
    if not chunks or chunks[0][0] != 0x4E4F534A:
        raise ValueError("GLB JSON chunk must come first")
    if len(chunks) > 2 or (len(chunks) == 2 and chunks[1][0] != 0x004E4942):
        raise Omission("unsupported GLB chunk layout")
    document = strict_json(chunks[0][1].decode("utf-8"))
    if not isinstance(document, dict) or not isinstance(document.get("asset"), dict) or document["asset"].get("version") != "2.0":
        raise ValueError("invalid glTF document/version")
    for table in ("buffers", "bufferViews", "accessors", "nodes", "meshes", "materials", "textures", "images", "animations"):
        rows = document.get(table, [])
        if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
            raise ValueError("invalid glTF table: " + table)
        if any("extensions" in row and not isinstance(row["extensions"], dict) for row in rows):
            raise ValueError("invalid extensions object: " + table)
    for mesh in document.get("meshes", []):
        primitives = mesh.get("primitives")
        if not isinstance(primitives, list) or not primitives or any(not isinstance(row, dict) for row in primitives):
            raise ValueError("mesh has no valid primitive array")
        for primitive in primitives:
            if not isinstance(primitive.get("attributes"), dict):
                raise ValueError("primitive has no valid attribute map")
            if "extensions" in primitive and not isinstance(primitive["extensions"], dict):
                raise ValueError("invalid primitive extensions object")
    if any("uri" in image and not isinstance(image["uri"], str) for image in document.get("images", [])):
        raise ValueError("invalid image URI")
    unsupported = set(document.get("extensionsRequired", [])) - REQUIRED_EXTENSIONS
    if unsupported:
        raise Omission("unsupported required extensions: " + ", ".join(sorted(unsupported)))
    buffers = document.get("buffers", [])
    if len(buffers) != 1 or buffers[0].get("uri") is not None:
        raise Omission("requires one embedded GLB buffer")
    binary = chunks[1][1] if len(chunks) == 2 else b""
    declared = integer(buffers[0].get("byteLength"), "buffer length")
    if declared > len(binary) or len(binary) - declared > 3:
        raise ValueError("GLB buffer length differs from BIN chunk")
    return document, binary[:declared]


def view_bytes(document, binary, index):
    view = item(document, "bufferViews", index)
    if view.get("extensions"):
        raise Omission("compressed/extended buffer views are unsupported")
    if view.get("buffer") != 0:
        raise ValueError("buffer view does not reference embedded buffer")
    offset = integer(view.get("byteOffset", 0), "view offset")
    size = integer(view.get("byteLength"), "view length")
    if offset + size > len(binary):
        raise ValueError("buffer view exceeds embedded buffer")
    return view, binary[offset:offset + size]


def read_accessor(document, binary, index):
    accessor = item(document, "accessors", index)
    if "sparse" in accessor or accessor.get("extensions"):
        raise Omission("sparse/extended accessors are unsupported")
    component_type = accessor.get("componentType")
    width = WIDTHS.get(accessor.get("type"))
    if component_type not in COMPONENTS or width is None:
        raise Omission("unsupported accessor component/type")
    code, component_size = COMPONENTS[component_type]
    count = integer(accessor.get("count"), "accessor count", 1000000)
    if count == 0:
        raise ValueError("empty accessor")
    view, data = view_bytes(document, binary, accessor.get("bufferView"))
    offset = integer(accessor.get("byteOffset", 0), "accessor offset")
    packed_size = width * component_size
    stride = integer(view.get("byteStride", packed_size), "accessor stride")
    if stride < packed_size or stride % component_size or offset % component_size:
        raise ValueError("invalid accessor alignment/stride")
    if offset + (count - 1) * stride + packed_size > len(data):
        raise ValueError("accessor exceeds its buffer view")
    normalized = accessor.get("normalized", False)
    if type(normalized) is not bool or (normalized and component_type not in (5120, 5121, 5122, 5123)):
        raise ValueError("invalid normalized component type")
    result = []
    for row in range(count):
        values = struct.unpack_from("<" + code * width, data, offset + row * stride)
        if normalized:
            maximum = {5120: 127, 5121: 255, 5122: 32767, 5123: 65535}[component_type]
            values = tuple(max(-1.0, value / maximum) for value in values)
        if not all(math.isfinite(value) for value in values):
            raise ValueError("non-finite accessor value")
        result.append(values)
    return accessor, result


def boundary_locks(positions, indices):
    """Lock source open boundaries, including coincident attribute wedges."""
    canonical_positions = {}
    remap = [canonical_positions.setdefault(tuple(position), len(canonical_positions)) for position in positions]
    edges = Counter()
    for offset in range(0, len(indices), 3):
        triangle = indices[offset:offset + 3]
        points = [positions[index] for index in triangle]
        ab = [points[1][axis] - points[0][axis] for axis in range(3)]
        ac = [points[2][axis] - points[0][axis] for axis in range(3)]
        cross = [ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0]]
        if len(set(triangle)) != 3 or not any(cross):
            raise Omission("degenerate triangles are outside the first experiment")
        vertices = [remap[index] for index in triangle]
        for a, b in zip(vertices, vertices[1:] + vertices[:1]):
            edges[tuple(sorted((a, b)))] += 1
    if any(count > 2 for count in edges.values()):
        raise Omission("non-manifold position edges are outside the first experiment")
    locked = {vertex for edge, count in edges.items() if count == 1 for vertex in edge}
    return bytes(int(vertex in locked) for vertex in remap)


def extract_primitive(document, binary, mesh_index, primitive_index, minimum_triangles=128):
    mesh = item(document, "meshes", mesh_index)
    primitive = mesh["primitives"][primitive_index]
    nodes = [(i, node) for i, node in enumerate(document.get("nodes", [])) if node.get("mesh") == mesh_index]
    if not nodes:
        raise Omission("mesh has no node owner")
    if document.get("animations") or any("skin" in node for _, node in nodes):
        raise Omission("animated/skinned GLBs are unsupported")
    if mesh.get("weights") or primitive.get("targets") or any(node.get("weights") for _, node in nodes):
        raise Omission("morph geometry is unsupported")
    if primitive.get("mode", 4) != 4:
        raise Omission("only triangle-list primitives are supported")
    if primitive.get("extensions"):
        raise Omission("extended/compressed primitives are unsupported")
    material_id = primitive.get("material")
    material = item(document, "materials", material_id) if material_id is not None else {}
    alpha = material.get("alphaMode", "OPAQUE")
    if alpha not in {"OPAQUE", "MASK", "BLEND"}:
        raise ValueError("invalid alpha mode")
    if alpha != "OPAQUE":
        raise Omission("alpha-tested/blended materials are unsupported")
    unsupported = set(material.get("extensions", {})) - MATERIAL_EXTENSIONS
    if unsupported:
        raise Omission("unsupported material extensions: " + ", ".join(sorted(unsupported)))
    attributes = primitive.get("attributes", {})
    if "POSITION" not in attributes:
        raise ValueError("primitive has no POSITION")
    position_accessor, positions = read_accessor(document, binary, attributes["POSITION"])
    if position_accessor.get("type") != "VEC3" or position_accessor.get("componentType") != 5126:
        raise Omission("POSITION must be float32 VEC3")
    if any(abs(value) > 1e12 for row in positions for value in row):
        raise Omission("positions exceed numeric experiment range")
    if "indices" in primitive:
        accessor, index_values = read_accessor(document, binary, primitive["indices"])
        if accessor.get("type") != "SCALAR" or accessor.get("componentType") not in (5121, 5123, 5125) or accessor.get("normalized", False):
            raise ValueError("indices must be unsigned, unnormalized scalars")
        indices = [row[0] for row in index_values]
    else:
        indices = list(range(len(positions)))
    if len(indices) % 3 or any(index >= len(positions) for index in indices):
        raise ValueError("invalid primitive indices")
    if len(indices) // 3 < minimum_triangles:
        raise Omission("small primitive retains ordinary mesh")
    decoded_attributes, layout = [], []
    for semantic, accessor_index in sorted(attributes.items()):
        if semantic == "POSITION":
            continue
        expected = {"NORMAL": {"VEC3"}, "TANGENT": {"VEC4"}, "COLOR_0": {"VEC3", "VEC4"}}.get(semantic)
        if semantic.startswith("TEXCOORD_") and semantic[9:].isdigit():
            expected = {"VEC2"}
        if expected is None:
            raise Omission("unsupported vertex attribute: " + semantic)
        accessor, values = read_accessor(document, binary, accessor_index)
        if accessor.get("type") not in expected or len(values) != len(positions):
            raise ValueError("attribute layout/count mismatch: " + semantic)
        if accessor.get("componentType") != 5126 and not (semantic.startswith(("TEXCOORD_", "COLOR_")) and accessor.get("normalized", False) and accessor.get("componentType") in (5121, 5123)):
            raise Omission("unsupported vertex encoding: " + semantic)
        decoded_attributes.append(values)
        layout.append({"semantic": semantic, "accessor": accessor_index, "width": len(values[0]), "weight_per_component": 1.0})
    attribute_count = sum(row["width"] for row in layout)
    if attribute_count > 32:
        raise Omission("more than 32 simplification attribute components")
    interleaved = [value for vertex in range(len(positions)) for attribute in decoded_attributes for value in attribute[vertex]]
    locks = boundary_locks(positions, indices)
    return {"positions": positions, "indices": indices, "attributes": interleaved,
            "attribute_count": attribute_count, "locks": locks, "layout": layout,
            "source_nodes": [index for index, _ in nodes], "source_accessors": attributes,
            "material": material_id, "material_document": material}


def input_bytes(primitive):
    positions = [value for row in primitive["positions"] for value in row]
    attributes = primitive["attributes"]
    weights = [1.0] * primitive["attribute_count"]
    indices = primitive["indices"]
    return (b"MCLODX01" + struct.pack("<III", len(primitive["positions"]), len(indices), primitive["attribute_count"])
            + struct.pack("<" + "f" * len(positions), *positions)
            + struct.pack("<" + "f" * len(attributes), *attributes)
            + struct.pack("<" + "f" * len(weights), *weights) + primitive["locks"]
            + struct.pack("<" + "I" * len(indices), *indices))


def oriented_triangle(triangle):
    a, b, c = triangle
    return min((a, b, c), (b, c, a), (c, a, b))


def checked_bounds(bounds, terminal=False):
    if not isinstance(bounds, dict):
        raise ValueError("invalid bounds object")
    center, radius, error = bounds.get("center"), bounds.get("radius"), bounds.get("error_estimate")
    if not isinstance(center, list) or len(center) != 3 or any(type(value) not in (int, float) or not math.isfinite(value) for value in center):
        raise ValueError("invalid bounds center")
    if type(radius) not in (int, float) or not math.isfinite(radius) or radius < 0:
        raise ValueError("invalid bounds radius")
    if (terminal and error is not None) or (not terminal and (type(error) not in (int, float) or not math.isfinite(error) or error < 0)):
        raise ValueError("invalid bounds error estimate")


def finalize_candidate(raw, primitive, max_triangles=128):
    if raw.get("meshoptimizer_commit") != PIN:
        raise ValueError("builder returned wrong upstream identity")
    groups = raw.get("groups", [])
    if not groups:
        raise ValueError("empty cluster hierarchy")
    source_triangles = defaultdict(list)
    for offset in range(0, len(primitive["indices"]), 3):
        source_triangles[oriented_triangle(primitive["indices"][offset:offset + 3])].append(offset // 3)
    for triangle_ids in source_triangles.values():
        triangle_ids.reverse()
    clusters, replacements = [], defaultdict(list)
    for group_id, group in enumerate(groups):
        if group.get("id") != group_id:
            raise ValueError("noncanonical group ID")
        depth = integer(group.get("depth"), "group depth")
        if type(group.get("terminal")) is not bool:
            raise ValueError("invalid terminal marker")
        bounds = group.get("simplified_bounds", {})
        checked_bounds(bounds, group["terminal"])
        group_clusters = []
        for cluster in group.get("clusters", []):
            indices = cluster.get("indices", [])
            if not indices or len(indices) % 3 or any(type(index) is not int or not 0 <= index < len(primitive["positions"]) for index in indices):
                raise ValueError("invalid cluster indices")
            if len(indices) // 3 > max_triangles:
                raise ValueError("cluster exceeds requested triangle limit")
            checked_bounds(cluster.get("bounds"))
            center, radius = cluster["bounds"]["center"], cluster["bounds"]["radius"]
            tolerance = max(1e-6, radius * 1e-5)
            if any(math.dist(primitive["positions"][index], center) > radius + tolerance for index in indices):
                raise ValueError("cluster sphere does not contain its emitted vertices")
            refines = cluster.get("refines_group")
            if type(refines) is not int or refines < -1 or refines >= group_id:
                raise ValueError("invalid/cyclic refinement link")
            if (refines == -1 and depth != 0) or (refines >= 0 and depth != groups[refines]["depth"] + 1):
                raise ValueError("invalid refinement depth")
            cluster_id = len(clusters)
            cluster["id"] = cluster_id
            cluster["group"] = group_id
            group_clusters.append(cluster_id)
            if refines == -1:
                triangle_ids = []
                for offset in range(0, len(indices), 3):
                    available = source_triangles.get(oriented_triangle(indices[offset:offset + 3]))
                    if not available:
                        raise ValueError("source cut duplicates or changes source triangles")
                    triangle_ids.append(available.pop())
                cluster["source_triangle_ids"] = triangle_ids
            else:
                replacements[refines].append(cluster_id)
            clusters.append(cluster)
        if not group_clusters:
            raise ValueError("empty cluster group")
        group["cluster_ids"] = group_clusters
        del group["clusters"]
    if any(source_triangles.values()):
        raise ValueError("source cut has missing triangles")
    for group in groups:
        if group["terminal"] == bool(replacements[group["id"]]):
            raise ValueError("terminal/replacement mismatch")

    # Independently compare uniform cuts from group rewriting with the upstream
    # depth/terminal rule. Replacement consumes the whole group, not one child.
    active = {cluster["id"] for cluster in clusters if cluster["refines_group"] == -1}
    cuts = []
    for depth in range(max(group["depth"] for group in groups) + 1):
        if depth:
            for group in groups:
                if group["depth"] == depth - 1 and not group["terminal"]:
                    if not set(group["cluster_ids"]).issubset(active):
                        raise ValueError("partial replacement in uniform cut")
                    active.difference_update(group["cluster_ids"])
                    active.update(replacements[group["id"]])
        expected = {cluster_id for group in groups if group["depth"] == depth or (group["terminal"] and group["depth"] < depth) for cluster_id in group["cluster_ids"]}
        if active != expected or not active:
            raise ValueError("incomplete/overlapping uniform group cut")
        selected = sorted(active)
        cuts.append({"depth": depth, "cluster_ids": selected, "triangle_count": sum(len(clusters[index]["indices"]) // 3 for index in selected)})
    for candidate in raw.get("conventional", []):
        indices = candidate.get("indices", [])
        error = candidate.get("error_estimate")
        if not indices or len(indices) % 3 or any(type(index) is not int or not 0 <= index < len(primitive["positions"]) for index in indices):
            raise ValueError("invalid conventional indices")
        if not isinstance(error, (int, float)) or not math.isfinite(error) or error < 0:
            raise ValueError("invalid conventional error estimate")
    raw["clusters"], raw["uniform_cuts"] = clusters, cuts
    raw["validation"] = {"source_cut_exact": True, "complete_uniform_group_cuts": True,
                         "shape_material_quality_certified": False, "error_bound_certified_conservative": False}
    return raw


def material_dependencies(document, binary, material, source_path, asset_root):
    texture_ids = set()

    def visit(value):
        if isinstance(value, dict):
            for key, nested in value.items():
                if key.endswith("Texture") and isinstance(nested, dict) and "index" in nested:
                    texture_ids.add(integer(nested["index"], "material texture index"))
                visit(nested)
        elif isinstance(value, list):
            for nested in value:
                visit(nested)

    visit(material)
    dependencies, watched = [], {}
    for texture_id in sorted(texture_ids):
        texture = item(document, "textures", texture_id)
        image_ids = {texture[key] for key in ("source",) if key in texture}
        basis = texture.get("extensions", {}).get("KHR_texture_basisu", {})
        if "source" in basis:
            image_ids.add(basis["source"])
        if not image_ids:
            raise ValueError("texture has no image source")
        for image_id in sorted(image_ids):
            image = item(document, "images", image_id)
            uri = image.get("uri")
            dependency = {"texture": texture_id, "image": image_id}
            if uri is None:
                _, data = view_bytes(document, binary, image.get("bufferView"))
                dependency.update(kind="embedded", sha256=digest(data))
            elif uri.startswith("data:"):
                dependency.update(kind="data_uri", sha256=digest(uri.encode()))
            else:
                parsed = urlsplit(uri)
                if parsed.scheme or parsed.netloc or parsed.query or parsed.fragment:
                    raise Omission("non-local image URI is unsupported")
                try:
                    path = without_symlinks(source_path.parent / unquote(parsed.path), asset_root)
                except Omission as error:
                    raise Omission("image URI " + str(error)) from error
                if not path.is_file():
                    raise Omission("material image is missing")
                checksum = file_digest(path)
                watch(watched, path, checksum)
                dependency.update(kind="external", path=path.relative_to(asset_root).as_posix(), sha256=checksum)
            dependencies.append(dependency)
    return dependencies, watched


def encode_glb(document, binary):
    document = copy.deepcopy(document)
    document.setdefault("buffers", [{}])[0]["byteLength"] = len(binary)
    text = canonical(document)
    text += b" " * (-len(text) % 4)
    padded = binary + b"\0" * (-len(binary) % 4)
    return (struct.pack("<4sII", b"glTF", 2, 28 + len(text) + len(padded))
            + struct.pack("<II", len(text), 0x4E4F534A) + text
            + struct.pack("<II", len(padded), 0x004E4942) + padded)


def embed_control_images(document, binary, source, asset_root, watched):
    """Make the whole-model control portable, including retained primitives."""
    document = copy.deepcopy(document)
    binary = bytearray(binary)
    for image in document.get("images", []):
        uri = image.get("uri")
        if uri is None or uri.startswith("data:"):
            continue
        parsed = urlsplit(uri)
        if parsed.scheme or parsed.netloc or parsed.query or parsed.fragment:
            raise Omission("control image URI is not local")
        path = without_symlinks(source.parent / unquote(parsed.path), asset_root)
        if not path.is_file():
            raise Omission("control image is missing")
        if path.stat().st_size > 128 * 1024 * 1024:
            raise Omission("control image exceeds 128 MiB experiment limit")
        data = path.read_bytes()
        watch(watched, path, digest(data))
        mime = image.get("mimeType") or {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".ktx2": "image/ktx2"}.get(path.suffix.lower())
        if mime not in {"image/png", "image/jpeg", "image/ktx2"}:
            raise Omission("control image has unsupported MIME type")
        binary.extend(b"\0" * (-len(binary) % 4))
        image["bufferView"] = len(document.setdefault("bufferViews", []))
        document["bufferViews"].append({"buffer": 0, "byteOffset": len(binary), "byteLength": len(data)})
        image["mimeType"] = mime
        del image["uri"]
        binary.extend(data)
    return document, bytes(binary)


def verify_control_glb(data, source_document, source_binary, portable_document, portable_binary, replacements):
    """Decode new indices independently and prove all source payloads survive."""
    document, binary = parse_glb(data)
    if binary[:len(source_binary)] != source_binary:
        raise ValueError("control changed original vertex/image/omitted payload bytes")
    if binary[:len(portable_binary)] != portable_binary:
        raise ValueError("control changed embedded dependency bytes")
    for key in set(source_document) | set(document):
        if key not in {"buffers", "bufferViews", "accessors", "meshes", "images"} and document.get(key) != source_document.get(key):
            raise ValueError("control changed scene/node/material metadata: " + key)
    for key in ("bufferViews", "accessors"):
        if document.get(key, [])[:len(source_document.get(key, []))] != source_document.get(key, []):
            raise ValueError("control changed source " + key)
    if document.get("images", []) != portable_document.get("images", []):
        raise ValueError("control changed image mapping")
    if len(document.get("meshes", [])) != len(source_document.get("meshes", [])):
        raise ValueError("control changed mesh coverage")
    for mesh_index, source_mesh in enumerate(source_document.get("meshes", [])):
        mesh = document["meshes"][mesh_index]
        if {k: v for k, v in mesh.items() if k != "primitives"} != {k: v for k, v in source_mesh.items() if k != "primitives"}:
            raise ValueError("control changed mesh metadata")
        if len(mesh["primitives"]) != len(source_mesh["primitives"]):
            raise ValueError("control lost primitive coverage")
        for primitive_index, original in enumerate(source_mesh["primitives"]):
            emitted = mesh["primitives"][primitive_index]
            indices = replacements.get((mesh_index, primitive_index))
            if indices is None:
                if emitted != original:
                    raise ValueError("control changed retained primitive")
                continue
            if {k: v for k, v in emitted.items() if k != "indices"} != {k: v for k, v in original.items() if k != "indices"}:
                raise ValueError("control changed attributes/material/primitive metadata")
            accessor, decoded = read_accessor(document, binary, emitted["indices"])
            if accessor.get("componentType") != 5125 or accessor.get("type") != "SCALAR" or [row[0] for row in decoded] != indices:
                raise ValueError("control index stream differs from complete selected cut")
            for index in original["attributes"].values():
                if read_accessor(document, binary, index) != read_accessor(source_document, source_binary, index):
                    raise ValueError("control changed decoded vertex attributes")
    return document, binary


def export_model_controls(stage, name, document, binary, source, asset_root, watched, candidates):
    portable, portable_binary = embed_control_images(document, binary, source, asset_root, watched)
    variants = [("source", {}, {})]
    for level in range(3):
        variants.append((f"conventional-{level}", {key: candidate["conventional"][level]["indices"] for key, candidate in candidates.items()}, {}))
    max_depth = max(len(candidate["uniform_cuts"]) for candidate in candidates.values())
    for depth in range(max_depth):
        replacements, selected_depths = {}, {}
        for key, candidate in candidates.items():
            selected = candidate["uniform_cuts"][min(depth, len(candidate["uniform_cuts"]) - 1)]
            replacements[key] = [index for cluster_id in selected["cluster_ids"] for index in candidate["clusters"][cluster_id]["indices"]]
            selected_depths[f"{key[0]}:{key[1]}"] = selected["depth"]
        variants.append((f"cluster-depth-{depth}", replacements, selected_depths))
    folder = stage / "controls" / name
    folder.mkdir(parents=True)
    artifacts = []
    for label, replacements, selected_depths in variants:
        emitted = copy.deepcopy(portable)
        payload = bytearray(portable_binary)
        for (mesh_index, primitive_index), indices in sorted(replacements.items()):
            payload.extend(b"\0" * (-len(payload) % 4))
            view = len(emitted.setdefault("bufferViews", []))
            emitted["bufferViews"].append({"buffer": 0, "byteOffset": len(payload), "byteLength": len(indices) * 4, "target": 34963})
            payload.extend(struct.pack("<" + "I" * len(indices), *indices))
            accessor = len(emitted.setdefault("accessors", []))
            emitted["accessors"].append({"bufferView": view, "componentType": 5125, "count": len(indices), "type": "SCALAR", "min": [min(indices)], "max": [max(indices)]})
            emitted["meshes"][mesh_index]["primitives"][primitive_index]["indices"] = accessor
        data = encode_glb(emitted, bytes(payload))
        verify_control_glb(data, document, binary, portable, portable_binary, replacements)
        path = folder / (label + ".glb")
        path.write_bytes(data)
        artifacts.append({"kind": label, "path": path.relative_to(stage).as_posix(), "sha256": digest(data), "bytes": len(data),
                          "replaced_primitive_triangles": {f"{key[0]}:{key[1]}": len(indices) // 3 for key, indices in replacements.items()},
                          "selected_cluster_depths": selected_depths})
    return {"status": "exported", "coverage": "whole model; omitted/failed primitives retain original payload", "portable_images": True,
            "certified_visual_quality": False, "adaptive_view_cut": False, "artifacts": artifacts}


def run_experiment(input_path, output_path, builder_path, asset_root=None, max_triangles=128, limit=32, export_controls=False):
    if not 4 <= max_triangles <= 256 or limit < 0:
        raise ValueError("max-cluster-triangles must be in 4..256 and limit must be nonnegative")
    if input_path.is_symlink():
        raise ValueError("symlinked GLB inputs are unsupported")
    source = input_path.resolve(strict=True)
    asset_root = (asset_root or (source if source.is_dir() else source.parent)).resolve(strict=True)
    output = output_path.resolve()
    if not asset_root.is_dir() or not source.is_relative_to(asset_root):
        raise ValueError("input must be inside asset root")
    if output.is_relative_to(asset_root) or asset_root.is_relative_to(output):
        raise ValueError("output must be disjoint from asset root")
    if output.exists():
        raise ValueError("output must be a new directory")
    builder = builder_path.resolve(strict=True)
    receipt_path = Path(str(builder) + ".build.json")
    receipt_bytes = receipt_path.read_bytes()
    receipt = strict_json(receipt_bytes)
    builder_bytes = builder.read_bytes()
    if receipt.get("meshoptimizer_commit") != PIN or receipt.get("builder_sha256") != digest(builder_bytes):
        raise ValueError("builder pin/hash receipt mismatch")
    files = sorted(path for path in source.rglob("*") if path.is_file() and path.suffix.lower() == ".glb") if source.is_dir() else [source]
    discovered = len(files)
    files = files[:limit] if limit else files
    if not files:
        raise ValueError("no GLB inputs")
    settings = {
        "max_cluster_triangles": max_triangles, "minimum_primitive_triangles": max_triangles,
        "conventional_ratios": [0.5, 0.25, 0.125], "conventional_error_limit": "unbounded",
        "attribute_weights": "1.0 for every supported component", "source_open_borders_locked": True,
        "cluster_recipe": "clodDefaultConfig; permissive/fallback/sloppy/clamped/dilate disabled; preserve_folds enabled",
        "meshoptimizer_commit": PIN, "builder_sha256": receipt["builder_sha256"],
        "builder_source_sha256": receipt["builder_source_sha256"], "driver_sha256": digest(DRIVER_BYTES),
        "limit": limit, "export_controls": export_controls,
    }
    settings_hash = digest(canonical(settings))
    manifest = {"format": "offline-lod-experiment-v1", "runtime_payload": False,
                "settings": settings, "settings_sha256": settings_hash,
                "discovered_models": discovered, "selected_models": len(files), "truncated_inventory": len(files) < discovered,
                "files": []}
    measurements, watched = [], {}
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="lod-experiment-", dir=output.parent) as temporary:
        stage = Path(temporary) / "output"
        stage.mkdir()
        (stage / "candidates").mkdir()
        snapshot_builder = Path(temporary) / "builder"
        snapshot_builder.write_bytes(builder_bytes)
        snapshot_builder.chmod(0o700)
        for path in files:
            relative = path.relative_to(asset_root).as_posix()
            file_row = {"path": relative, "primitives": []}
            manifest["files"].append(file_row)
            try:
                without_symlinks(path, asset_root)
                if path.stat().st_size > 128 * 1024 * 1024:
                    raise Omission("GLB exceeds 128 MiB experiment limit")
                data = path.read_bytes()
                checksum = digest(data)
                watch(watched, path, checksum)
                file_row.update(sha256=checksum, source_bytes=len(data))
                document, binary = parse_glb(data)
                meshes = document.get("meshes", [])
                if not meshes:
                    raise Omission("GLB has no meshes")
                model_candidates = {}
                for mesh_index, mesh in enumerate(meshes):
                    for primitive_index, _ in enumerate(mesh.get("primitives", [])):
                        row = {"mesh": mesh_index, "primitive": primitive_index}
                        file_row["primitives"].append(row)
                        try:
                            primitive = extract_primitive(document, binary, mesh_index, primitive_index, max_triangles)
                            dependencies, dependency_hashes = material_dependencies(document, binary, primitive["material_document"], path, asset_root)
                            for dependency, dependency_hash in dependency_hashes.items():
                                watch(watched, dependency, dependency_hash)
                            request = input_bytes(primitive)
                            request_path = Path(temporary) / "primitive.bin"
                            request_path.write_bytes(request)
                            started = time.perf_counter()
                            result = subprocess.run([str(snapshot_builder), str(request_path), str(max_triangles)], capture_output=True, timeout=120, check=True)
                            process_seconds = time.perf_counter() - started
                            candidate = finalize_candidate(strict_json(result.stdout), primitive, max_triangles)
                            candidate.update(format="offline-index-candidates-v1", runtime_payload=False,
                                source={"path": relative, "sha256": checksum, "mesh": mesh_index, "primitive": primitive_index,
                                        "nodes": primitive["source_nodes"], "accessors": primitive["source_accessors"],
                                        "material": primitive["material"], "material_sha256": digest(canonical(primitive["material_document"])),
                                        "material_dependencies": dependencies, "attribute_layout": primitive["layout"],
                                        "vertex_count": len(primitive["positions"]), "triangle_count": len(primitive["indices"]) // 3},
                                settings_sha256=settings_hash, simplification_input_sha256=digest(request))
                            name = f"{checksum[:16]}-{digest(relative.encode())[:12]}-m{mesh_index}-p{primitive_index}.json"
                            candidate_data = canonical(candidate)
                            # Validate the builder's timing before any candidate is registered or written.
                            timing = strict_json(result.stderr)
                            if not isinstance(timing, dict):
                                raise ValueError("builder timing must be a JSON object")
                            timing.update(path=relative, mesh=mesh_index, primitive=primitive_index,
                                          process_seconds=process_seconds, candidate_json_bytes=len(candidate_data),
                                          conventional_index_u32_bytes=sum(len(level["indices"]) * 4 for level in candidate["conventional"]),
                                          cluster_index_u32_bytes=sum(len(cluster["indices"]) * 4 for cluster in candidate["clusters"]))
                            (stage / "candidates" / name).write_bytes(candidate_data)
                            model_candidates[(mesh_index, primitive_index)] = candidate
                            row.update(status="candidate", candidate="candidates/" + name, sha256=digest(candidate_data),
                                       source_triangles=len(primitive["indices"]) // 3,
                                       conventional_triangles=[len(level["indices"]) // 3 for level in candidate["conventional"]],
                                       cluster_cut_triangles=[cut["triangle_count"] for cut in candidate["uniform_cuts"]])
                            measurements.append(timing)
                        except Omission as error:
                            row.update(status="omitted", reason=str(error))
                        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
                            row.update(status="failed", reason=builder_failure(error, temporary))
                        except (ValueError, KeyError, TypeError, OverflowError, subprocess.SubprocessError) as error:
                            row.update(status="failed", reason=str(error))
                if export_controls and model_candidates:
                    file_row["controls"] = export_model_controls(stage, checksum[:16] + "-" + digest(relative.encode())[:12],
                        document, binary, path, asset_root, watched, model_candidates)
            except Omission as error:
                file_row.update(status="omitted", reason=str(error))
            except (ValueError, KeyError, TypeError, OSError) as error:
                file_row.update(status="failed", reason=str(error))
        for path, checksum in watched.items():
            if not path.is_file() or file_digest(path) != checksum:
                raise ValueError("source input changed during experiment: " + path.relative_to(asset_root).as_posix())
        if file_digest(builder) != receipt["builder_sha256"] or receipt_path.read_bytes() != receipt_bytes or Path(__file__).read_bytes() != DRIVER_BYTES:
            raise InputChanged("builder, receipt or driver changed during experiment")
        rows = [row for file_row in manifest["files"] for row in file_row["primitives"]] + [file_row for file_row in manifest["files"] if "status" in file_row]
        manifest["counts"] = {status: sum(row.get("status") == status for row in rows) for status in ("candidate", "omitted", "failed")}
        (stage / "manifest.json").write_bytes(canonical(manifest))
        (stage / "measurements.json").write_bytes(canonical({"scope": "single-process offline stage timings; not performance acceptance", "primitives": measurements}))
        if output.exists():
            raise ValueError("output appeared during experiment; refusing replacement")
        stage.rename(output)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True, help="one GLB or a directory inventoried in lexical order")
    parser.add_argument("--asset-root", type=Path, help="boundary for local material image dependencies")
    parser.add_argument("--output", type=Path, required=True, help="new directory disjoint from input assets")
    parser.add_argument("--builder", type=Path, required=True)
    parser.add_argument("--max-cluster-triangles", type=int, default=128)
    parser.add_argument("--limit", type=int, default=32, help="maximum models; 0 selects all")
    parser.add_argument("--export-controls", action="store_true", help="export portable whole-model source, conventional and complete cluster GLBs")
    args = parser.parse_args()
    if not 4 <= args.max_cluster_triangles <= 256 or args.limit < 0:
        parser.error("max-cluster-triangles must be in 4..256 and limit must be nonnegative")
    try:
        manifest = run_experiment(args.input, args.output, args.builder, args.asset_root, args.max_cluster_triangles, args.limit, args.export_controls)
    except (OSError, ValueError, InputChanged) as error:
        parser.exit(2, str(error) + "\n")
    print(json.dumps(manifest["counts"], sort_keys=True))
    return 1 if manifest["counts"]["failed"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
