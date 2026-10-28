#!/usr/bin/env python3
"""Write an original curved-grid GLB fixture; contains no game assets."""
import argparse
import json
import math
from pathlib import Path
import struct


def glb(document, binary):
    document = dict(document)
    document["buffers"] = [{"byteLength": len(binary)}]
    text = json.dumps(document, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    text += b" " * (-len(text) % 4)
    padded = binary + b"\0" * (-len(binary) % 4)
    return (struct.pack("<4sII", b"glTF", 2, 12 + 8 + len(text) + 8 + len(padded))
            + struct.pack("<II", len(text), 0x4E4F534A) + text
            + struct.pack("<II", len(padded), 0x004E4942) + padded)


def grid(size=24):
    vertices, indices = [], []
    for y in range(size + 1):
        for x in range(size + 1):
            u, v = x / size, y / size
            height = math.sin(u * math.pi * 2) * math.cos(v * math.pi * 2) * 0.1
            nx = -math.cos(u * math.pi * 2) * math.cos(v * math.pi * 2) * math.pi * 0.2
            ny = math.sin(u * math.pi * 2) * math.sin(v * math.pi * 2) * math.pi * 0.2
            length = math.sqrt(nx * nx + ny * ny + 1)
            vertices.extend((u, v, height, nx / length, ny / length, 1 / length, u, v))
    for y in range(size):
        for x in range(size):
            a = y * (size + 1) + x
            indices.extend((a, a + 1, a + size + 1, a + 1, a + size + 2, a + size + 1))
    vertex_bytes = struct.pack("<" + "f" * len(vertices), *vertices)
    index_bytes = struct.pack("<" + "I" * len(indices), *indices)
    count = (size + 1) ** 2
    document = {
        "asset": {"version": "2.0", "generator": "Mudcrab original synthetic LOD fixture"},
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": len(vertex_bytes), "byteStride": 32},
                        {"buffer": 0, "byteOffset": len(vertex_bytes), "byteLength": len(index_bytes)}],
        "accessors": [
            {"bufferView": 0, "byteOffset": 0, "componentType": 5126, "count": count, "type": "VEC3"},
            {"bufferView": 0, "byteOffset": 12, "componentType": 5126, "count": count, "type": "VEC3"},
            {"bufferView": 0, "byteOffset": 24, "componentType": 5126, "count": count, "type": "VEC2"},
            {"bufferView": 1, "componentType": 5125, "count": len(indices), "type": "SCALAR"}],
        "materials": [{"alphaMode": "OPAQUE", "pbrMetallicRoughness": {"baseColorFactor": [0.4, 0.6, 0.8, 1]}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "indices": 3, "material": 0}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0,
    }
    return document, vertex_bytes + index_bytes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--size", type=int, default=24)
    args = parser.parse_args()
    if not 2 <= args.size <= 256:
        parser.error("size must be in 2..256")
    document, binary = grid(args.size)
    # Exclusive creation avoids replacing another fixture or a source model.
    with args.output.open("xb") as stream:
        stream.write(glb(document, binary))


if __name__ == "__main__":
    main()
