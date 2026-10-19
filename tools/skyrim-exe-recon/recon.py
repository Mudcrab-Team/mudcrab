#!/usr/bin/env python3
"""Read-only PE32+ reconnaissance for a locally installed SkyrimSE.exe.

Produces a deterministic JSON fingerprint: file hash, COFF/optional header
fields, per-section hashes and entropy, import DLL counts, CodeView (PDB)
identity, VS_FIXEDFILEINFO version, and the size of the x64 runtime-function
(.pdata) table. ``--rva`` reports the section and runtime-function bounds that
contain an address, following chained unwind entries to the primary function.

The output holds metadata only: no code bytes, strings tables, or decompiled
text, and the input is recorded by file name, not by its local path.
"""
from __future__ import annotations

import argparse
from bisect import bisect_right
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import struct
import sys

SCHEMA = "mudcrab.skyrim-exe-recon/1"
# Builds whose hash was recorded by an earlier, independently written evidence note.
KNOWN_BUILDS = {
    "846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f": {
        "file_version": "1.7.104.0",
        "recorded_in": "docs/specs/engine/landscape-texture-scale.md",
    },
}
MACHINES = {0x14C: "i386", 0x8664: "AMD64", 0xAA64: "ARM64"}
SUBSYSTEMS = {2: "WINDOWS_GUI", 3: "WINDOWS_CUI"}
SECTION_FLAGS = (
    (0x00000020, "CODE"), (0x00000040, "INITIALIZED_DATA"), (0x00000080, "UNINITIALIZED_DATA"),
    (0x20000000, "EXECUTE"), (0x40000000, "READ"), (0x80000000, "WRITE"),
)
DIRECTORIES = {0: "export", 1: "import", 2: "resource", 3: "exception", 5: "base_relocation", 6: "debug", 9: "tls", 10: "load_config", 12: "iat"}
UNW_FLAG_CHAININFO = 0x4
VS_FIXEDFILEINFO_SIGNATURE = struct.pack("<I", 0xFEEF04BD)
HIGH_ENTROPY = 7.2
MAX_IMPORT_DLLS = 4096
MAX_THUNKS = 1 << 16
MAX_CHAIN = 32


class ReconError(ValueError):
    """The input is not a PE32+ image this tool can describe."""


def u8(data: bytes, offset: int) -> int:
    return unpack("<B", data, offset)


def u16(data: bytes, offset: int) -> int:
    return unpack("<H", data, offset)


def u32(data: bytes, offset: int) -> int:
    return unpack("<I", data, offset)


def u64(data: bytes, offset: int) -> int:
    return unpack("<Q", data, offset)


def unpack(fmt: str, data: bytes, offset: int) -> int:
    if offset < 0 or offset + struct.calcsize(fmt) > len(data):
        raise ReconError(f"truncated image: read of {fmt} at file offset {offset:#x}")
    return struct.unpack_from(fmt, data, offset)[0]


def hexaddr(value: int) -> str:
    return f"{value:#x}"


def flags_of(characteristics: int) -> list[str]:
    return [label for bit, label in SECTION_FLAGS if characteristics & bit]


def entropy(data: bytes) -> float:
    if not data:
        return 0.0
    total = len(data)
    return round(-sum(n / total * math.log2(n / total) for n in Counter(data).values()), 3)


def c_string(data: bytes, offset: int, limit: int = 256) -> str:
    if offset < 0 or offset >= len(data):
        raise ReconError(f"string offset {offset:#x} is outside the file")
    end = data.find(b"\0", offset, offset + limit)
    if end < 0:
        raise ReconError(f"unterminated string at file offset {offset:#x}")
    return data[offset:end].decode("ascii", "replace")


class Image:
    """Parsed PE32+ headers plus RVA translation over the raw file bytes."""

    def __init__(self, data: bytes):
        self.data = data
        if data[:2] != b"MZ":
            raise ReconError("missing MZ header")
        pe = u32(data, 0x3C)
        if data[pe:pe + 4] != b"PE\0\0":
            raise ReconError(f"missing PE signature at {pe:#x}")
        coff = pe + 4
        self.machine = u16(data, coff)
        section_count = u16(data, coff + 2)
        self.timestamp = u32(data, coff + 4)
        optional_size = u16(data, coff + 16)
        self.characteristics = u16(data, coff + 18)
        opt = coff + 20
        if u16(data, opt) != 0x20B:
            raise ReconError(f"optional header magic {u16(data, opt):#x} is not PE32+ (0x20b)")
        self.optional = opt
        self.headers_size = u32(data, opt + 60)
        directory_count = min(u32(data, opt + 108), 16)
        self.directories = {i: (u32(data, opt + 112 + i * 8), u32(data, opt + 116 + i * 8)) for i in range(directory_count)}
        self.sections = []
        table = opt + optional_size
        for i in range(section_count):
            at = table + i * 40
            if at + 40 > len(data):
                raise ReconError("truncated section table")
            self.sections.append({
                "name": data[at:at + 8].rstrip(b"\0").decode("ascii", "replace"),
                "virtual_size": u32(data, at + 8), "rva": u32(data, at + 12),
                "raw_size": u32(data, at + 16), "raw_offset": u32(data, at + 20),
                "characteristics": u32(data, at + 36),
            })

    def directory(self, index: int) -> tuple[int, int]:
        return self.directories.get(index, (0, 0))

    def section_for(self, rva: int) -> dict | None:
        for section in self.sections:
            if section["rva"] <= rva < section["rva"] + max(section["virtual_size"], section["raw_size"]):
                return section
        return None

    def offset(self, rva: int) -> int:
        if rva < self.headers_size:
            return rva
        section = self.section_for(rva)
        if section is None or rva - section["rva"] >= section["raw_size"]:
            raise ReconError(f"RVA {rva:#x} has no file-backed bytes")
        return section["raw_offset"] + rva - section["rva"]

    def raw(self, section: dict) -> bytes:
        start, size = section["raw_offset"], section["raw_size"]
        if start + size > len(self.data):
            raise ReconError(f"section {section['name']} extends past end of file")
        return self.data[start:start + size]


def runtime_functions(image: Image) -> list[tuple[int, int, int]]:
    rva, size = image.directory(3)
    if not rva:
        return []
    base = image.offset(rva)
    entries = [(u32(image.data, base + i), u32(image.data, base + i + 4), u32(image.data, base + i + 8)) for i in range(0, size - size % 12, 12)]
    if any(a[0] > b[0] for a, b in zip(entries, entries[1:])):
        raise ReconError("runtime-function table is not sorted by begin address")
    return entries


def primary_function(image: Image, entry: tuple[int, int, int]) -> tuple[tuple[int, int, int], int]:
    """Follow UNW_FLAG_CHAININFO links; return the primary entry and hop count."""
    for hops in range(MAX_CHAIN + 1):
        info = image.offset(entry[2])
        if (u8(image.data, info) >> 3) & UNW_FLAG_CHAININFO == 0:
            return entry, hops
        codes = u8(image.data, info + 2)
        chained = info + 4 + 2 * ((codes + 1) & ~1)
        entry = (u32(image.data, chained), u32(image.data, chained + 4), u32(image.data, chained + 8))
    raise ReconError(f"unwind chain longer than {MAX_CHAIN} entries")


def imports(image: Image) -> list[dict]:
    rva, _ = image.directory(1)
    if not rva:
        return []
    result = []
    descriptor = image.offset(rva)
    for _ in range(MAX_IMPORT_DLLS):
        lookup, name, first = u32(image.data, descriptor), u32(image.data, descriptor + 12), u32(image.data, descriptor + 16)
        if not (lookup or name or first):
            return result
        thunk = image.offset(lookup or first)
        count = 0
        while u64(image.data, thunk + count * 8):
            count += 1
            if count > MAX_THUNKS:
                raise ReconError("import thunk table is unterminated")
        result.append({"dll": c_string(image.data, image.offset(name)), "functions": count})
        descriptor += 20
    raise ReconError("import descriptor table is unterminated")


def codeview(image: Image) -> dict | None:
    rva, size = image.directory(6)
    if not rva:
        return None
    base = image.offset(rva)
    for at in range(base, base + size - size % 28, 28):
        if u32(image.data, at + 12) != 2:
            continue
        record = u32(image.data, at + 24)
        if image.data[record:record + 4] != b"RSDS":
            continue
        d1, d2, d3 = u32(image.data, record + 4), u16(image.data, record + 8), u16(image.data, record + 10)
        d4 = image.data[record + 12:record + 20]
        pdb = c_string(image.data, record + 24, 1024)
        return {
            "guid": f"{d1:08X}-{d2:04X}-{d3:04X}-{d4[:2].hex().upper()}-{d4[2:].hex().upper()}",
            "age": u32(image.data, record + 20),
            # Basename only: the full path can name a developer machine.
            "pdb_name": pdb.replace("\\", "/").rsplit("/", 1)[-1],
        }
    return None


def version(image: Image) -> dict | None:
    rva, _ = image.directory(2)
    section = image.section_for(rva) if rva else None
    if section is None:
        return None
    raw = image.raw(section)
    hits = []
    at = raw.find(VS_FIXEDFILEINFO_SIGNATURE)
    while at >= 0:
        hits.append(at)
        at = raw.find(VS_FIXEDFILEINFO_SIGNATURE, at + 4)
    if not hits:
        return None

    def dotted(ms: int, ls: int) -> str:
        return f"{ms >> 16}.{ms & 0xFFFF}.{ls >> 16}.{ls & 0xFFFF}"

    first = hits[0]
    return {
        "file_version": dotted(u32(raw, first + 8), u32(raw, first + 12)),
        "product_version": dotted(u32(raw, first + 16), u32(raw, first + 20)),
        "fixed_file_info_records": len(hits),
    }


def fingerprint(data: bytes, name: str, rvas: list[int]) -> dict:
    image = Image(data)
    opt = image.optional
    entry = u32(data, opt + 16)
    entry_section = image.section_for(entry)
    sections = []
    notes = []
    for section in image.sections:
        raw = image.raw(section)
        flags = flags_of(section["characteristics"])
        bits = entropy(raw)
        sections.append({
            "name": section["name"], "rva": hexaddr(section["rva"]), "virtual_size": hexaddr(section["virtual_size"]),
            "raw_offset": hexaddr(section["raw_offset"]), "raw_size": hexaddr(section["raw_size"]),
            "flags": flags, "entropy": bits, "sha256": hashlib.sha256(raw).hexdigest(),
        })
        if "EXECUTE" in flags and bits > HIGH_ENTROPY:
            notes.append(f"heuristic: executable section {section['name']} has entropy {bits}; code may be encrypted or packed on disk, so static disassembly is unreliable until that is ruled out")
        if section["name"] == ".bind":
            notes.append("heuristic: a .bind section is present, consistent with a Steam DRM wrapper; confirm before trusting static analysis")
    if entry_section is not None and "EXECUTE" not in flags_of(entry_section["characteristics"]):
        notes.append(f"entry point lies in non-executable section {entry_section['name']}")

    functions = runtime_functions(image)
    begins = [f[0] for f in functions]
    queries = []
    for rva in rvas:
        section = image.section_for(rva)
        found = None
        index = bisect_right(begins, rva) - 1
        if index >= 0 and functions[index][0] <= rva < functions[index][1]:
            hit = functions[index]
            primary, hops = primary_function(image, hit)
            found = {
                "begin": hexaddr(hit[0]), "end": hexaddr(hit[1]), "unwind": hexaddr(hit[2]),
                "primary_begin": hexaddr(primary[0]), "chain_hops": hops,
            }
        queries.append({"rva": hexaddr(rva), "section": section["name"] if section else None, "runtime_function": found})

    digest = hashlib.sha256(data).hexdigest()
    ver = version(image)
    known = KNOWN_BUILDS.get(digest)
    if known and ver and known["file_version"] != ver["file_version"]:
        notes.append(f"known-build hash matched but parsed version {ver['file_version']} differs from recorded {known['file_version']}")
    return {
        "schema": SCHEMA,
        "file": {"name": name, "size": len(data), "sha256": digest},
        "known_build": known,
        "coff": {
            "machine": MACHINES.get(image.machine, hexaddr(image.machine)),
            "timestamp": image.timestamp, "characteristics": hexaddr(image.characteristics),
            "sections": len(image.sections),
        },
        "optional_header": {
            "linker_version": f"{data[opt + 2]}.{data[opt + 3]}",
            "image_base": hexaddr(u64(data, opt + 24)),
            "entry_rva": hexaddr(entry),
            "entry_section": entry_section["name"] if entry_section else None,
            "size_of_image": hexaddr(u32(data, opt + 56)),
            "checksum": hexaddr(u32(data, opt + 64)),
            "subsystem": SUBSYSTEMS.get(u16(data, opt + 68), str(u16(data, opt + 68))),
            "dll_characteristics": hexaddr(u16(data, opt + 70)),
        },
        "data_directories": {label: {"rva": hexaddr(r), "size": hexaddr(s)} for i, label in DIRECTORIES.items() for r, s in [image.directory(i)] if r},
        "sections": sections,
        "imports": imports(image),
        "codeview": codeview(image),
        "version": ver,
        "runtime_functions": len(functions),
        "rva_queries": queries,
        "notes": notes,
    }


def differences(expected: dict, actual: dict) -> list[str]:
    return sorted(key for key in expected.keys() | actual.keys() if expected.get(key) != actual.get(key))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("exe", type=Path, help="path to SkyrimSE.exe (read only)")
    parser.add_argument("--rva", action="append", default=[], type=lambda v: int(v, 0), help="report section and runtime-function bounds for this RVA; repeatable")
    parser.add_argument("--out", type=Path, help="write the JSON report here instead of stdout")
    parser.add_argument("--expect", type=Path, help="compare against a previously written report; exit 1 on any difference")
    args = parser.parse_args(argv)
    try:
        report = fingerprint(args.exe.read_bytes(), args.exe.name, args.rva)
        text = json.dumps(report, indent=2, sort_keys=True) + "\n"
        if args.expect:
            changed = differences(json.loads(args.expect.read_text()), report)
            if changed:
                print(f"Skyrim exe recon: differs from {args.expect} in: {', '.join(changed)}", file=sys.stderr)
                return 1
            print(f"Matches {args.expect}: sha256 {report['file']['sha256']}")
        elif args.out:
            args.out.write_text(text)
            print(f"Wrote {args.out}: sha256 {report['file']['sha256']}, {report['runtime_functions']} runtime functions")
        else:
            sys.stdout.write(text)
        return 0
    except (OSError, ReconError, json.JSONDecodeError) as error:
        print(f"Skyrim exe recon: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
