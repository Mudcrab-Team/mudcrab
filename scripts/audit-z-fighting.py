#!/usr/bin/env python3
"""Find depth-state gaps, overlapping triangles and duplicate world placements.

This is a static risk audit, not a rendered z-fighting oracle. Exit 0 means no
risks in the selected scope and no coverage gaps; 1 means risks; 2 means gaps
or invalid input. Reports always distinguish inventory from geometry coverage.
Only Python's standard library is needed. All input files are read-only.
"""

import argparse
from collections import Counter, defaultdict
from dataclasses import dataclass
from concurrent.futures import ProcessPoolExecutor
import fnmatch
import hashlib
import json
import math
from pathlib import Path
import sqlite3
import struct
import sys
import time


FORMAT = "mudcrab-z-fighting-audit-v1"
DECAL = 1 << 26
DYNAMIC_DECAL = 1 << 27
DEPTH_TEST = 1 << 31
DEPTH_WRITE = 1
IDENTITY = (1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.)


class AuditError(ValueError):
    """An input cannot be audited without guessing."""


class BudgetError(AuditError):
    """Retain observed overlaps when the remaining search exceeds its budget."""

    def __init__(self, message, partial):
        super().__init__(message)
        self.partial = partial


def load_glb(path, geometry=True):
    """Validate the container and read its JSON and optional embedded buffer."""
    with path.open("rb") as source:
        header = source.read(12)
        if len(header) != 12:
            raise AuditError("truncated GLB header")
        magic, version, total = struct.unpack("<4sII", header)
        if magic != b"glTF" or version != 2 or total != path.stat().st_size:
            raise AuditError("invalid GLB magic, version or length")
        document = None
        binary = None
        offset = 12
        while offset < total:
            chunk = source.read(8)
            if len(chunk) != 8:
                raise AuditError("truncated GLB chunk header")
            size, kind = struct.unpack("<I4s", chunk)
            if size % 4 or offset + 8 + size > total:
                raise AuditError("invalid GLB chunk length")
            if offset == 12 and kind != b"JSON":
                raise AuditError("first GLB chunk must be JSON")
            if kind == b"JSON":
                if document is not None or size > 64 * 1024 * 1024:
                    raise AuditError("duplicate or oversized GLB JSON")
                document = json.loads(source.read(size))
            elif kind == b"BIN\0" and geometry:
                if binary is not None:
                    raise AuditError("duplicate GLB buffer")
                binary = source.read(size)
            else:
                source.seek(size, 1)
            offset += 8 + size
    asset = document.get("asset") if isinstance(document, dict) else None
    if not isinstance(asset, dict) or asset.get("version") != "2.0":
        raise AuditError("missing glTF 2.0 document")
    return document, binary


def hash_file(path):
    """Hash a file without requiring Python 3.11's file_digest."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def skyrim_tag(material):
    """Return optional Skyrim metadata only when it is a JSON object."""
    extras = material.get("extras") if isinstance(material, dict) else None
    tag = extras.get("openSkyrim") if isinstance(extras, dict) else None
    return tag if isinstance(tag, dict) else {}


def material_risks(material):
    """Return authored depth requirements not implemented by the native hook."""
    tag = skyrim_tag(material)
    first, second = tag.get("shaderFlags1"), tag.get("shaderFlags2")
    if any(type(value) is not int or not 0 <= value <= 0xffffffff
           for value in (first, second)):
        return None
    risks = []
    if first & DECAL:
        risks.append("authored_decal")
    if first & DYNAMIC_DECAL:
        risks.append("authored_dynamic_decal")
    if not first & DEPTH_TEST:
        risks.append("depth_test_disabled")
    if not second & DEPTH_WRITE and material.get("alphaMode", "OPAQUE") != "BLEND":
        risks.append("opaque_or_mask_depth_write_disabled")
    return risks


def accessor(document, binary, index, kind):
    """Decode supported dense position/index accessors, respecting stride."""
    item = indexed(document["accessors"], index, "accessor")
    if "sparse" in item or item.get("normalized", False):
        raise AuditError("sparse/normalized geometry accessor is unsupported")
    component = item.get("componentType")
    formats = {5121: "B", 5123: "H", 5125: "I", 5126: "f"}
    width = 3 if kind == "VEC3" else 1
    if item.get("type") != kind or component not in formats:
        raise AuditError("unsupported geometry accessor type")
    if (kind == "VEC3" and component != 5126) or (kind == "SCALAR" and component == 5126):
        raise AuditError("positions must be float32; indices must be unsigned integers")
    view = indexed(document["bufferViews"], item["bufferView"], "buffer view")
    buffers = document.get("buffers", [])
    if view.get("buffer", 0) != 0 or not buffers or "uri" in buffers[0] or binary is None:
        raise AuditError("external or absent geometry buffer is unsupported")
    fmt = "<" + formats[component] * width
    packed = struct.calcsize(fmt)
    stride = view.get("byteStride", packed)
    count = item["count"]
    relative = item.get("byteOffset", 0)
    start = view.get("byteOffset", 0) + relative
    end = relative + (count - 1) * stride + packed if count else relative
    if (type(count) is not int or count < 0 or stride < packed or relative < 0
            or end > view["byteLength"] or start < 0
            or view.get("byteOffset", 0) + view["byteLength"] > min(len(binary), buffers[0]["byteLength"])):
        raise AuditError("geometry accessor exceeds its buffer view")
    values = [struct.unpack_from(fmt, binary, start + i * stride) for i in range(count)]
    if kind == "VEC3":
        if any(not all(map(math.isfinite, value)) for value in values):
            raise AuditError("nonfinite vertex position")
        return values
    return [value[0] for value in values]


def indexed(values, index, name):
    """Reject negative indices instead of applying Python's array semantics."""
    if type(index) is not int or index < 0 or index >= len(values):
        raise AuditError(f"invalid {name} index {index}")
    return values[index]


def multiply(a, b):
    """Multiply column-major glTF matrices."""
    return tuple(sum(a[k * 4 + row] * b[col * 4 + k] for k in range(4))
                 for col in range(4) for row in range(4))


def node_matrix(node):
    """Construct the glTF local matrix, rejecting malformed transforms."""
    if "matrix" in node:
        if any(key in node for key in ("translation", "rotation", "scale")):
            raise AuditError("node mixes matrix and TRS")
        result = tuple(node["matrix"])
    else:
        tx, ty, tz = node.get("translation", (0., 0., 0.))
        sx, sy, sz = node.get("scale", (1., 1., 1.))
        x, y, z, w = node.get("rotation", (0., 0., 0., 1.))
        if abs(x*x + y*y + z*z + w*w - 1.) > 1e-4:
            raise AuditError("non-unit node quaternion")
        result = ((1-2*y*y-2*z*z)*sx, (2*x*y+2*z*w)*sx, (2*x*z-2*y*w)*sx, 0.,
                  (2*x*y-2*z*w)*sy, (1-2*x*x-2*z*z)*sy, (2*y*z+2*x*w)*sy, 0.,
                  (2*x*z+2*y*w)*sz, (2*y*z-2*x*w)*sz, (1-2*x*x-2*y*y)*sz, 0.,
                  tx, ty, tz, 1.)
    if len(result) != 16 or not all(map(math.isfinite, result)):
        raise AuditError("invalid node transform")
    if any(abs(result[i]) > 1e-8 for i in (3, 7, 11)) or abs(result[15] - 1.) > 1e-8:
        raise AuditError("non-affine node transform")
    return result


def transform(matrix, point):
    """Transform a local position to model scene space."""
    x, y, z = point
    return (matrix[0]*x + matrix[4]*y + matrix[8]*z + matrix[12],
            matrix[1]*x + matrix[5]*y + matrix[9]*z + matrix[13],
            matrix[2]*x + matrix[6]*y + matrix[10]*z + matrix[14])


def subtract(a, b):
    return tuple(x-y for x, y in zip(a, b))


def dot(a, b):
    return sum(x*y for x, y in zip(a, b))


def cross(a, b):
    return (a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0])


@dataclass
class Triangle:
    points: tuple
    identity: dict
    double_sided: bool
    normal: tuple
    bounds: tuple


def triangles(document, binary, max_triangles):
    """Flatten the default scene, keeping node and primitive identities."""
    if document.get("animations"):
        raise AuditError("animated geometry requires a runtime pose")
    scenes = document.get("scenes", [])
    if not scenes:
        raise AuditError("missing glTF scene")
    nodes = document.get("nodes", [])
    scene = indexed(scenes, document.get("scene", 0), "scene")
    stack = [(index, IDENTITY, frozenset()) for index in reversed(scene["nodes"])]
    result = []
    degenerates = 0
    visited = set()
    while stack:
        index, parent, ancestors = stack.pop()
        if index in ancestors or index in visited:
            raise AuditError("cyclic or multiply-parented glTF scene")
        visited.add(index)
        node = indexed(nodes, index, "node")
        if "skin" in node:
            raise AuditError("skinned geometry requires a runtime pose")
        world = multiply(parent, node_matrix(node))
        determinant = dot((world[0], world[1], world[2]),
                          cross((world[4], world[5], world[6]), (world[8], world[9], world[10])))
        if abs(determinant) <= 1e-14:
            raise AuditError("singular node transform")
        stack.extend((child, world, ancestors | {index}) for child in reversed(node.get("children", [])))
        if "mesh" not in node:
            continue
        mesh_index = node["mesh"]
        mesh = indexed(document["meshes"], mesh_index, "mesh")
        for primitive_index, primitive in enumerate(mesh["primitives"]):
            if primitive.get("mode", 4) != 4 or primitive.get("targets") or primitive.get("extensions"):
                raise AuditError("non-triangle, morph or extended geometry is unsupported")
            positions = [transform(world, point) for point in
                         accessor(document, binary, primitive["attributes"]["POSITION"], "VEC3")]
            if any(not all(map(math.isfinite, point)) for point in positions):
                raise AuditError("node transform overflow")
            indices = (accessor(document, binary, primitive["indices"], "SCALAR")
                       if "indices" in primitive else list(range(len(positions))))
            if len(indices) % 3 or any(i >= len(positions) for i in indices):
                raise AuditError("invalid triangle indices")
            if len(result) + len(indices)//3 > max_triangles:
                raise AuditError(f"triangle budget exceeded ({max_triangles})")
            material_index = primitive.get("material")
            material = indexed(document.get("materials", []), material_index, "material") if material_index is not None else {}
            for offset in range(0, len(indices), 3):
                points = tuple(positions[i] for i in indices[offset:offset+3])
                normal = cross(subtract(points[1], points[0]), subtract(points[2], points[0]))
                length = math.sqrt(dot(normal, normal))
                if not math.isfinite(length):
                    raise AuditError("triangle normal overflow")
                if length <= 1e-14:
                    degenerates += 1
                    continue
                # Bevy reverses culling for mirrored transforms. Keep the effective
                # front face orientation instead of inferring it from world winding.
                normal = tuple(v/length * (-1 if determinant < 0 else 1) for v in normal)
                bounds = (tuple(min(p[k] for p in points) for k in range(3)),
                          tuple(max(p[k] for p in points) for k in range(3)))
                tag = skyrim_tag(material)
                identity = {"node": index, "mesh": mesh_index, "primitive": primitive_index,
                            "triangle": offset//3, "material": material_index,
                            "shape_block": tag.get("shapeBlock"), "shader_block": tag.get("shaderBlock")}
                result.append(Triangle(points, identity, material.get("doubleSided", False), normal, bounds))
    return result, degenerates


def overlap_area(a, b, normal):
    """Clip projected triangles; shared edges and point contacts have zero area."""
    axis = max(range(3), key=lambda k: abs(normal[k]))
    polygon = [tuple(v for k, v in enumerate(p) if k != axis) for p in a]
    clip = [tuple(v for k, v in enumerate(p) if k != axis) for p in b]

    def side(p, q, r):
        return (q[0]-p[0])*(r[1]-p[1]) - (q[1]-p[1])*(r[0]-p[0])

    sign = 1 if side(*clip) > 0 else -1
    for edge in range(3):
        p, q = clip[edge], clip[(edge+1) % 3]
        before = polygon
        polygon = []
        if not before:
            break
        previous = before[-1]
        old = sign * side(p, q, previous)
        for current in before:
            new = sign * side(p, q, current)
            if (old >= 0) != (new >= 0):
                fraction = old / (old-new)
                polygon.append(tuple(previous[k] + fraction*(current[k]-previous[k]) for k in range(2)))
            if new >= 0:
                polygon.append(current)
            previous, old = current, new
    projected = abs(sum(polygon[i][0]*polygon[(i+1) % len(polygon)][1]
                        - polygon[i][1]*polygon[(i+1) % len(polygon)][0] for i in range(len(polygon))))/2
    return projected / abs(normal[axis])


def find_overlaps(items, tolerance, minimum_area, max_comparisons, max_examples):
    """Traverse a bounding volume tree, then check plane distance and overlap."""
    if not items:
        return {"pairs": 0, "comparisons": 0, "examples": []}

    def build(group):
        bounds = (tuple(min(t.bounds[0][k] for t in group) for k in range(3)),
                  tuple(max(t.bounds[1][k] for t in group) for k in range(3)))
        if len(group) <= 8:
            return bounds, group, None
        axis = max(range(3), key=lambda k: bounds[1][k]-bounds[0][k])
        group = sorted(group, key=lambda t: t.bounds[0][axis]+t.bounds[1][axis])
        middle = len(group)//2
        return bounds, None, (build(group[:middle]), build(group[middle:]))

    def touching(a, b):
        return all(a[1][k]+tolerance >= b[0][k] and b[1][k]+tolerance >= a[0][k] for k in range(3))

    tree = build(items)
    stack = [(tree, tree)]
    comparisons = pairs = 0
    examples = []

    def spend():
        nonlocal comparisons
        comparisons += 1
        if comparisons > max_comparisons:
            raise BudgetError(f"comparison budget exceeded ({max_comparisons}); no clean result",
                              {"pairs": pairs, "comparisons": comparisons-1, "examples": examples})

    while stack:
        a, b = stack.pop()
        spend()
        if not touching(a[0], b[0]):
            continue
        if a[2] is not None or b[2] is not None:
            if a is b:
                left, right = a[2]
                stack.extend(((left,left), (right,right), (left,right)))
            elif a[2] is not None:
                stack.extend((child, b) for child in a[2])
            else:
                stack.extend((a, child) for child in b[2])
            continue
        candidates = ((old, current) for i, old in enumerate(a[1])
                      for current in (b[1][i+1:] if a is b else b[1]))
        for old, current in candidates:
            spend()
            if not touching(old.bounds, current.bounds):
                continue
            alignment = dot(old.normal, current.normal)
            # Opposite single-sided faces cannot be visible from the same side.
            if alignment < 0 and not (old.double_sided or current.double_sided):
                continue
            if abs(alignment) < 1-1e-6:
                continue
            separation = max(abs(dot(old.normal, subtract(p, old.points[0]))) for p in current.points)
            separation = max(separation, *(abs(dot(current.normal, subtract(p, current.points[0]))) for p in old.points))
            if separation > tolerance:
                continue
            area = overlap_area(old.points, current.points, old.normal)
            if area <= minimum_area:
                continue
            pairs += 1
            if len(examples) < max_examples:
                examples.append({"a": old.identity, "b": current.identity,
                                 "maximum_plane_separation": separation, "overlap_area": area})
    return {"pairs": pairs, "comparisons": comparisons, "examples": examples}


def duplicate_placements(database, max_examples):
    """Find exact same-model placements; conditional references remain unknown."""
    with sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True) as connection:
        rows = connection.execute('''SELECT f.id, f.cell_id, f.worldspace_id, f.is_exterior,
            f.pos_x, f.pos_y, f.pos_z, f.rot_x, f.rot_y, f.rot_z, f.scale,
            f.header_flags, f.enable_parent_id, s.model_path
            FROM "references" f JOIN statics s ON s.id=f.base_form_id
            WHERE s.model_path IS NOT NULL AND s.model_path != '' ORDER BY f.id''')
        groups = defaultdict(list)
        counts = Counter()
        for row in rows:
            counts["model_references"] += 1
            if row[11] is None:
                counts["null_placement_fields"] += 1
                continue
            if row[11] & (0x20 | 0x800):
                counts["deleted_or_initially_disabled"] += 1
                continue
            if row[12] is not None:
                counts["conditional_enable_state_unknown"] += 1
                continue
            if any(value is None for value in row[4:11]):
                counts["null_placement_fields"] += 1
                continue
            if not all(math.isfinite(v) for v in row[4:11]):
                counts["nonfinite_transforms"] += 1
                continue
            scope = ("worldspace", row[2]) if row[3] else ("interior", row[1])
            key = (scope, row[13].replace("\\", "/").lower(), *row[4:11])
            groups[key].append(row[0])
        duplicates = [{"scope": key[0], "model": key[1], "transform": key[2:],
                       "reference_ids": ids} for key, ids in groups.items() if len(ids) > 1]
    return {"coverage": dict(counts), "duplicate_groups": len(duplicates),
            "extra_placements": sum(len(row["reference_ids"])-1 for row in duplicates),
            "examples": duplicates[:max_examples]}


def audit_file(task):
    """Inspect one model in a worker; aggregate coverage in the parent process."""
    path, args, mesh_root = task
    counts, materials = Counter(), Counter()
    errors = []
    included = False
    relative = path.relative_to(mesh_root).as_posix()
    entry = {"path": relative, "material_risks": []}
    if path.is_symlink() or not path.resolve().is_relative_to(mesh_root.resolve()):
        errors.append({"path": relative, "error": "symlink or escaped mesh path"})
        return counts, materials, None, errors
    try:
        document, binary = load_glb(path, not args.materials_only)
        counts["files_read"] += 1
        for index, material in enumerate(document.get("materials", [])):
            materials["total"] += 1
            risks = material_risks(material)
            if risks is None:
                materials["unannotated"] += 1
                continue
            materials["annotated"] += 1
            first = material["extras"]["openSkyrim"]["shaderFlags1"]
            second = material["extras"]["openSkyrim"]["shaderFlags2"]
            materials["depth_write_disabled_all_alpha_modes"] += not second & DEPTH_WRITE
            if risks:
                materials["risk_materials"] += 1
                materials.update(risks)
                tag = material["extras"]["openSkyrim"]
                entry["material_risks"].append({"material": index, "name": material.get("name"),
                    "shape_block": tag.get("shapeBlock"), "shader_block": tag.get("shaderBlock"),
                    "shader_flags_1": first, "shader_flags_2": second,
                    "alpha_mode": material.get("alphaMode", "OPAQUE"), "requirements": risks})
        if not args.materials_only:
            try:
                geometry, degenerates = triangles(document, binary, args.max_triangles)
                overlap = find_overlaps(geometry, args.plane_tolerance, args.min_overlap_area,
                                        args.max_comparisons, args.max_examples)
                counts["geometry_complete_files"] += 1
                counts["triangles_checked"] += len(geometry)
                counts["degenerate_triangles"] += degenerates
                counts["overlapping_triangle_pairs"] += overlap["pairs"]
                entry["geometry"] = {"complete": True, "triangles": len(geometry),
                                     "degenerate_triangles": degenerates, **overlap}
            except (AuditError, KeyError, IndexError, TypeError, struct.error) as error:
                counts["geometry_incomplete_files"] += 1
                entry["geometry"] = {"complete": False, "reason": str(error)}
                if isinstance(error, BudgetError):
                    entry["geometry"].update(error.partial)
                    counts["overlapping_triangle_pairs"] += error.partial["pairs"]
        if entry["material_risks"] or entry.get("geometry", {}).get("pairs") or entry.get("geometry", {}).get("complete") is False:
            entry["sha256"] = hash_file(path)
            included = True
    except (OSError, ValueError, KeyError, IndexError, TypeError, AttributeError, struct.error) as error:
        errors.append({"path": relative, "error": str(error)})
    return counts, materials, entry if included else None, errors


def audit(args):
    """Audit every selected file; a skipped/failed scan cannot produce a pass."""
    started = time.monotonic()
    mesh_root = args.assets / "meshes"
    if not mesh_root.is_dir():
        raise AuditError("assets directory must contain meshes/")
    paths = sorted(p for p in mesh_root.rglob("*") if p.suffix.lower() == ".glb")
    selected = [p for p in paths if not args.include or any(fnmatch.fnmatchcase(p.relative_to(mesh_root).as_posix(), pattern) for pattern in args.include)]
    counts = Counter()
    materials = Counter()
    files = []
    errors = []
    pool = ProcessPoolExecutor(max_workers=args.jobs) if args.jobs > 1 else None
    try:
        tasks = ((path, args, mesh_root) for path in selected)
        results = pool.map(audit_file, tasks, chunksize=8) if pool else map(audit_file, tasks)
        for number, (file_counts, file_materials, entry, file_errors) in enumerate(results, 1):
            counts.update(file_counts)
            materials.update(file_materials)
            errors.extend(file_errors)
            if entry is not None:
                files.append(entry)
            if args.progress and number % 250 == 0:
                print(f"Audited {number}/{len(selected)} GLBs", file=sys.stderr, flush=True)
    finally:
        if pool:
            pool.shutdown()
    counts["discovered_files"] = len(paths)
    counts["selected_files"] = len(selected)
    gaps = []
    if not selected:
        gaps.append("no GLBs selected")
    if args.include:
        gaps.append("mesh inventory restricted by include patterns")
    if args.materials_only:
        gaps.append("geometry not requested")
    if materials["unannotated"]:
        gaps.append("materials lack Skyrim shader flags")
    if counts["geometry_incomplete_files"]:
        gaps.append("one or more geometry scans incomplete")
    if errors:
        gaps.append("one or more input files unreadable or invalid")
    placement = None
    if not args.no_placements:
        try:
            placement = duplicate_placements(args.assets / "skyrim_world.db", args.max_examples)
            if placement["coverage"].get("conditional_enable_state_unknown"):
                gaps.append("conditional placement enable states not evaluated")
            if placement["coverage"].get("nonfinite_transforms"):
                gaps.append("nonfinite placement transforms")
            if placement["coverage"].get("null_placement_fields"):
                gaps.append("NULL placement flags or transforms")
        except (OSError, sqlite3.Error, ValueError, TypeError) as error:
            errors.append({"path": "skyrim_world.db", "error": str(error)})
            gaps.append("placement audit failed")
    else:
        gaps.append("placement audit not requested")
    risks_found = bool(materials["risk_materials"] or counts["overlapping_triangle_pairs"]
                       or (placement and placement["duplicate_groups"]))
    return {"format": FORMAT, "elapsed_seconds": round(time.monotonic()-started, 3),
            "scope": {"assets": str(args.assets.resolve()), "include": args.include,
                      "plane_tolerance_scene_units": args.plane_tolerance,
                      "minimum_overlap_area_scene_units_squared": args.min_overlap_area,
                      "max_triangles_per_file": args.max_triangles,
                      "max_comparisons_per_file": args.max_comparisons},
            "coverage": dict(counts), "material_requirements": dict(materials),
            "files": files, "placements": placement, "errors": errors,
            "coverage_gaps": gaps, "risks_found": risks_found,
            "static_scope_passed": not risks_found and not gaps,
            "rendered_z_fighting_verified": False,
            "limitations": ["Candidates require rendered confirmation; overlap can be intentional.",
                "Geometry is evaluated within each GLB's default static scene, before world placement.",
                "Different models, terrain, water and LOD intersections need a runtime surface audit.",
                "Plane tolerance is a geometric threshold, not a depth precision bound.",
                "Alpha texture coverage, render ordering, pass overrides and camera distance are not evaluated.",
                "Material requirements describe authored flags; they are not a shader parity proof.",
                "Placement scan tests exact transforms only; conditional and moved runtime objects remain unknown."]}


def main(argv=None):
    """Write a report and return the audit's machine-readable gate status."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("assets", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--include", action="append", default=[], help="mesh-relative glob, repeatable")
    parser.add_argument("--materials-only", action="store_true")
    parser.add_argument("--no-placements", action="store_true")
    parser.add_argument("--plane-tolerance", type=float, default=1e-4)
    parser.add_argument("--min-overlap-area", type=float, default=1e-8)
    parser.add_argument("--max-triangles", type=int, default=100000)
    parser.add_argument("--max-comparisons", type=int, default=2000000)
    parser.add_argument("--max-examples", type=int, default=20)
    parser.add_argument("--progress", action="store_true")
    parser.add_argument("--jobs", type=int, default=1, help="parallel file workers")
    args = parser.parse_args(argv)
    if (not math.isfinite(args.plane_tolerance) or args.plane_tolerance < 0
            or not math.isfinite(args.min_overlap_area) or args.min_overlap_area < 0
            or min(args.max_triangles, args.max_comparisons, args.max_examples, args.jobs) < 1):
        parser.error("thresholds must be finite and nonnegative; budgets must be positive")
    try:
        result = audit(args)
    except Exception as error:
        # Limit this fallback to the scan: output failures still surface, and
        # KeyboardInterrupt/SystemExit retain their normal control flow.
        result = {"format": FORMAT, "static_scope_passed": False,
                  "rendered_z_fighting_verified": False,
                  "coverage_gaps": [f"audit failed ({type(error).__name__}): {error}"]}
    result["audit_script_sha256"] = hash_file(Path(__file__))
    encoded = json.dumps(result, indent=2, allow_nan=False) + "\n"
    if args.out:
        args.out.write_text(encoded)
    else:
        print(encoded, end="")
    return 2 if result["coverage_gaps"] else 1 if result["risks_found"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
