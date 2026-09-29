#!/usr/bin/env python3
"""Read-only storage audit for mudcrab output (Python 3.10+, standard library).

Reads file metadata and short DDS/KTX2 headers; never hashes or decodes assets.
Does not follow symbolic links. Run against a completed, idle conversion.

Usage:
  python3 audit_asset_sizes.py /path/to/modern_assets
  python3 audit_asset_sizes.py /path/to/modern_assets --original /path/to/Skyrim/Data
  python3 audit_asset_sizes.py /path/to/modern_assets --json > asset-size-report.json

KTX2 layout: https://registry.khronos.org/KTX/specs/2.0/ktxspec.v2.html
DDS layout: https://learn.microsoft.com/en-us/windows/win32/direct3ddds/dds-header
Allocated-byte figures deduplicate hard links but cannot resolve shared reflink
extents, filesystem compression, directory overhead, or snapshots. On platforms
without st_blocks, allocated-byte figures are unavailable. Root-level symlinks
explicitly supplied as arguments are resolved; nested symlinks are skipped.
"""
from __future__ import annotations

import argparse
import json
import os
import stat
import struct
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

MAGIC_KTX2 = b"\xabKTX 20\xbb\r\n\x1a\n"
SCHEMES = {0: "None", 1: "BasisLZ", 2: "Zstandard", 3: "ZLIB"}
DXGI = {71: "BC1", 72: "BC1 sRGB", 74: "BC2", 75: "BC2 sRGB",
        77: "BC3", 78: "BC3 sRGB", 80: "BC4", 81: "BC4 signed",
        83: "BC5", 84: "BC5 signed", 95: "BC6H unsigned", 96: "BC6H signed",
        98: "BC7", 99: "BC7 sRGB"}
FOURCC = {b"DXT1": "BC1", b"DXT2": "BC2 premultiplied", b"DXT3": "BC2",
          b"DXT4": "BC3 premultiplied", b"DXT5": "BC3", b"ATI1": "BC4",
          b"BC4U": "BC4", b"BC4S": "BC4 signed", b"ATI2": "BC5",
          b"BC5U": "BC5", b"BC5S": "BC5 signed"}
SOURCE_EXT = {".ktx2": ".dds", ".glb": ".nif", ".luau": ".pex"}


def texture_kind(path: Path, ext: str) -> str:
    with path.open("rb") as stream:
        header = stream.read(148 if ext == ".dds" else 80)
        if ext == ".dds":
            if len(header) < 128 or header[:4] != b"DDS ":
                raise ValueError("invalid/truncated DDS header")
            fourcc = header[84:88]
            if fourcc == b"DX10":
                if len(header) < 148:
                    raise ValueError("truncated DDS DX10 header")
                fmt = struct.unpack_from("<I", header, 128)[0]
                return DXGI.get(fmt, f"DXGI {fmt}")
            if fourcc in FOURCC:
                return FOURCC[fourcc]
            if any(fourcc):
                return f"FourCC {fourcc!r}"
            bits = struct.unpack_from("<I", header, 88)[0]
            return f"Uncompressed/legacy {bits}-bit"
        if len(header) < 80 or header[:12] != MAGIC_KTX2:
            raise ValueError("invalid/truncated KTX2 header")
        vk_format = struct.unpack_from("<I", header, 12)[0]
        scheme, dfd_offset, dfd_length = struct.unpack_from("<III", header, 44)
        codec = f"VkFormat {vk_format}"
        if vk_format == 0 and dfd_offset >= 80 and dfd_length >= 13:
            # First descriptor: total size, vendor/type, version, block size, color model.
            stream.seek(dfd_offset + 12)
            color_model = stream.read(1)
            codec = {b"\xa6": "UASTC", b"\xa3": "ETC1S"}.get(
                color_model, f"DFD color model {int.from_bytes(color_model, 'little')}"
            )
        return f"{codec}; supercompression={SCHEMES.get(scheme, str(scheme))}"


def scan(root: Path, inspect_headers: bool) -> dict[str, Any]:
    groups: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    extensions: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    textures: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    seen: set[tuple[int, int]] = set()
    sizes: dict[str, int] = {}
    total = unique = allocated = files = hardlinks = symlinks = errors = 0
    error_examples: list[str] = []
    has_blocks = True

    def error(message: str) -> None:
        nonlocal errors
        errors += 1
        if len(error_examples) < 10:
            error_examples.append(message)

    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    path = Path(entry.path)
                    try:
                        if entry.is_symlink():
                            symlinks += 1
                            continue
                        if entry.is_dir(follow_symlinks=False):
                            pending.append(path)
                            continue
                        info = entry.stat(follow_symlinks=False)
                        if not stat.S_ISREG(info.st_mode):
                            continue
                        rel = path.relative_to(root)
                        ext = path.suffix.lower()
                        group = rel.parts[0] if len(rel.parts) > 1 else "[root files]"
                        files += 1
                        total += info.st_size
                        for table, key in ((groups, group), (extensions, ext or "[no extension]")):
                            table[key][0] += 1
                            table[key][1] += info.st_size
                        if inspect_headers and (group == "vfs" or ext in SOURCE_EXT):
                            sizes[rel.as_posix()] = info.st_size
                        # Zero inodes are not reliable identifiers on every platform.
                        inode = (info.st_dev, info.st_ino)
                        if info.st_ino and inode in seen:
                            hardlinks += 1
                            continue
                        if info.st_ino:
                            seen.add(inode)
                        unique += info.st_size
                        if hasattr(info, "st_blocks"):
                            allocated += info.st_blocks * 512
                        else:
                            has_blocks = False
                        if inspect_headers and ext in (".dds", ".ktx2"):
                            try:
                                kind = f"{ext}: {texture_kind(path, ext)}"
                                textures[kind][0] += 1
                                textures[kind][1] += info.st_size
                            except (OSError, ValueError, struct.error) as exc:
                                error(f"{path}: {exc}")
                    except OSError as exc:
                        error(f"{path}: {exc}")
        except OSError as exc:
            error(f"{directory}: {exc}")

    pairs: dict[str, dict[str, Any]] = defaultdict(
        lambda: {"count": 0, "source_bytes": 0, "output_bytes": 0, "largest_growth": []}
    )
    lowered = {name.lower(): size for name, size in sizes.items()}
    for name, output_size in sizes.items():
        rel = Path(name)
        source_ext = SOURCE_EXT.get(rel.suffix.lower())
        if not source_ext or rel.parts[0] in ("vfs", ".ingestion-cache"):
            continue
        source_name = "vfs/" + rel.with_suffix(source_ext).as_posix()
        source_size = lowered.get(source_name.lower())
        if source_size is None:
            continue
        label = f"{source_ext} -> {rel.suffix.lower()}"
        item = pairs[label]
        item["count"] += 1
        item["source_bytes"] += source_size
        item["output_bytes"] += output_size
        item["largest_growth"].append({"path": name, "source_bytes": source_size,
                                      "output_bytes": output_size,
                                      "growth_bytes": output_size - source_size,
                                      "ratio": output_size / source_size if source_size else None})
    for item in pairs.values():
        source_size = item["source_bytes"]
        item["ratio"] = item["output_bytes"] / source_size if source_size else None
        item["largest_growth"] = sorted(item["largest_growth"],
                                       key=lambda row: row["growth_bytes"], reverse=True)[:10]

    def rows(table: dict[str, list[int]]) -> list[dict[str, Any]]:
        return [{"name": k, "files": v[0], "bytes": v[1]}
                for k, v in sorted(table.items(), key=lambda kv: kv[1][1], reverse=True)]

    return {"root": str(root), "files": files, "path_bytes": total,
            "unique_inode_bytes": unique, "allocated_file_bytes": allocated if has_blocks else None,
            "duplicate_hardlink_paths": hardlinks, "skipped_symlinks": symlinks,
            "errors": errors, "error_examples": error_examples,
            "top_level_path_bytes": rows(groups), "extension_path_bytes": rows(extensions),
            "texture_unique_inode_bytes": rows(textures), "matched_pairs": dict(pairs)}


def human(value: int | None) -> str:
    if value is None:
        return "unavailable"
    return f"{value / (1024**3):.3f} GiB"


def print_scan(data: dict[str, Any]) -> None:
    print(f"\n{data['root']}")
    print(f"  Files: {data['files']:,}; summed file sizes: {human(data['path_bytes'])}")
    print(f"  Hard-link-deduplicated file sizes: {human(data['unique_inode_bytes'])}")
    print(f"  Allocated file blocks: {human(data['allocated_file_bytes'])}")
    print(f"  Duplicate hard-link paths: {data['duplicate_hardlink_paths']:,}; "
          f"skipped symlinks: {data['skipped_symlinks']:,}; errors: {data['errors']:,}")
    for key, title in (("top_level_path_bytes", "Top-level contents (summed path sizes)"),
                       ("extension_path_bytes", "Extensions (summed path sizes)"),
                       ("texture_unique_inode_bytes", "Texture headers (hard links counted once)")):
        if data[key]:
            print(f"\n{title}:")
            for row in data[key]:
                print(f"  {human(row['bytes']):>14}  {row['files']:>8,}  {row['name']}")
    for label, item in data["matched_pairs"].items():
        ratio = f"{item['ratio']:.3f}x" if item['ratio'] is not None else "n/a"
        print(f"\nMatched {label}: {item['count']:,} pairs; "
              f"{human(item['source_bytes'])} -> {human(item['output_bytes'])} ({ratio})")
        print("  Largest absolute growth (not necessarily largest ratios):")
        for row in item["largest_growth"]:
            ratio = f"{row['ratio']:.2f}x" if row['ratio'] is not None else "n/a"
            print(f"    {row['growth_bytes'] / (1024**2):+10.2f} MiB  {ratio:>9}  {row['path']}")
    for message in data["error_examples"]:
        print(f"  WARNING: {message}", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("output", type=Path)
    parser.add_argument("--original", type=Path)
    parser.add_argument("--json", action="store_true", help="Print JSON instead of the text report")
    args = parser.parse_args()
    for path in (args.output, args.original):
        if path is not None and not path.is_dir():
            parser.error(f"Not an accessible directory: {path}")
    output = scan(args.output.resolve(), inspect_headers=True)
    original = scan(args.original.resolve(), inspect_headers=False) if args.original else None
    report = {"output": output, "original": original,
              "notes": ["No assets are written, decoded, or content-hashed.",
                        "Run while conversion is idle; this is not an atomic filesystem snapshot.",
                        "Allocated blocks do not identify shared reflink extents or snapshots.",
                        "Directory allocation is excluded. Header parsing is not full format validation.",
                        "Source/output pairs use matching relative paths under vfs; missing pairs and "
                        "sRGB aliases are not fabricated."]}
    if args.json:
        json.dump(report, sys.stdout, indent=2)
        print()
    else:
        print_scan(output)
        if original:
            print_scan(original)
            denom = original["unique_inode_bytes"]
            if denom:
                print(f"\nOutput/original logical size, hard links counted once: "
                      f"{output['unique_inode_bytes'] / denom:.3f}x")
        print("\nNote: allocated blocks exclude directory overhead and do not resolve shared "
              "reflink extents or snapshots. Texture headers are not full validation.")
    return 1 if output["errors"] or (original and original["errors"]) else 0


if __name__ == "__main__":
    raise SystemExit(main())
