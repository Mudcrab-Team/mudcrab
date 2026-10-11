"""Small Skyrim SE P0 plugins, authored directly as wire bytes.

These fixtures never pass through Mutagen's writer.  The full-plugin fixture's
expected signatures, FormIDs, strings, floats, and reference target are listed
separately in HAND_ENCODED_EXPECTED and its bytes are pinned by a golden SHA.
"""

from __future__ import annotations

import hashlib
import struct


RECORD_HEADER = struct.Struct("<4sIIIIHH")
GROUP_HEADER = struct.Struct("<4sI4sIHHI")
SUBRECORD_HEADER = struct.Struct("<4sH")
GROUP_HEADER_SIZE = GROUP_HEADER.size
RECORD_HEADER_SIZE = RECORD_HEADER.size

HAND_ENCODED_GOLDEN_SHA256 = "2b6a56869dfb17fb41e1fc5faa58f529afd98818515b1c632b072492fb0f50c8"
UNKNOWN_REPEATED_GOLDEN_SHA256 = "59d69e472640987a5823edb3097efbcaa7ac4a9647dbaf35a1fa7c91dc1f5d52"

HAND_ENCODED_EXPECTED = {
    "plugin": "p0-hand-full.esp",
    "records": [
        {"signature": "GLOB", "form_id": 0x800, "editor_id": "P0HandGlobal", "raw_flags": 0, "typed_data": "4.25"},
        {"signature": "GMST", "form_id": 0x801, "editor_id": "fP0HandValue", "raw_flags": 0, "typed_data": "12.5"},
        {"signature": "STAT", "form_id": 0x802, "editor_id": "P0HandStatic", "raw_flags": 0, "typed_data": "meshes\\p0_hand_stat.nif"},
        {"signature": "CELL", "form_id": 0x803, "editor_id": "P0HandCell", "raw_flags": 0, "typed_data": None},
        {
            "signature": "REFR",
            "form_id": 0x804,
            "editor_id": "P0HandPlacedRef",
            "raw_flags": 0xC00,
            "typed_data": "12.5, -25, 100",
            "typed_link": "000802:p0-hand-full.esp",
        },
    ],
}


def _subrecord(signature: str, payload: bytes) -> bytes:
    encoded_signature = signature.encode("ascii")
    if len(encoded_signature) != 4:
        raise ValueError(f"subrecord signature must be four ASCII bytes: {signature!r}")
    if len(payload) > 0xFFFF:
        raise ValueError("fixture subrecord exceeds the short-length encoding")
    return SUBRECORD_HEADER.pack(encoded_signature, len(payload)) + payload


def _record(
    signature: str,
    form_id: int,
    payload: bytes,
    *,
    flags: int = 0,
    form_version: int = 44,
) -> bytes:
    return RECORD_HEADER.pack(
        signature.encode("ascii"), len(payload), flags, form_id, 0, form_version, 0
    ) + payload


def _group(label: bytes | int, group_type: int, contents: bytes) -> bytes:
    if isinstance(label, int):
        encoded_label = struct.pack("<I", label)
    else:
        encoded_label = label
    if len(encoded_label) != 4:
        raise ValueError("group label must occupy four bytes")
    return GROUP_HEADER.pack(
        b"GRUP", GROUP_HEADER_SIZE + len(contents), encoded_label, group_type, 0, 0, 0
    ) + contents


def _interior_cell_tree(cell_id: int, cell: bytes, persistent_records: bytes) -> bytes:
    persistent = _group(cell_id, 8, persistent_records)
    children = _group(cell_id, 6, persistent)
    subblock = _group(0, 3, cell + children)
    block = _group(0, 2, subblock)
    return _group(b"CELL", 0, block)


def _tes4(
    *,
    record_count: int,
    next_object_id: int,
    flags: int = 0,
    masters: tuple[str, ...] = (),
) -> bytes:
    fields = [_subrecord("HEDR", struct.pack("<fII", 1.7, record_count, next_object_id))]
    fields.append(_subrecord("CNAM", b"Mudcrab P0 hand fixture\0"))
    for master in masters:
        fields.append(_subrecord("MAST", master.encode("ascii") + b"\0"))
        fields.append(_subrecord("DATA", struct.pack("<Q", 0)))
    return _record("TES4", 0, b"".join(fields), flags=flags)


def hand_encoded_full() -> bytes:
    """Return a standalone full plugin with known typed fields and a nested REFR."""
    glob = _record(
        "GLOB",
        0x800,
        _subrecord("EDID", b"P0HandGlobal\0")
        + _subrecord("FNAM", b"f")
        + _subrecord("FLTV", struct.pack("<f", 4.25)),
    )
    gmst = _record(
        "GMST",
        0x801,
        _subrecord("EDID", b"fP0HandValue\0")
        + _subrecord("DATA", struct.pack("<f", 12.5)),
    )
    static = _record(
        "STAT",
        0x802,
        _subrecord("EDID", b"P0HandStatic\0")
        + _subrecord("MODL", b"meshes\\p0_hand_stat.nif\0"),
    )
    cell_id = 0x803
    cell = _record(
        "CELL",
        cell_id,
        _subrecord("EDID", b"P0HandCell\0") + _subrecord("DATA", struct.pack("<H", 1)),
    )
    placed = _record(
        "REFR",
        0x804,
        _subrecord("EDID", b"P0HandPlacedRef\0")
        + _subrecord("NAME", struct.pack("<I", 0x802))
        + _subrecord("DATA", struct.pack("<ffffff", 12.5, -25.0, 100.0, 0.0, 1.5707964, 0.0))
        + _subrecord("XSCL", struct.pack("<f", 1.0)),
        flags=0xC00,
    )
    cell_tree = _interior_cell_tree(cell_id, cell, placed)
    return b"".join(
        (
            _tes4(record_count=13, next_object_id=0x805),
            _group(b"GLOB", 0, glob),
            _group(b"GMST", 0, gmst),
            _group(b"STAT", 0, static),
            cell_tree,
        )
    )


def hand_encoded_light() -> bytes:
    glob = _record(
        "GLOB",
        0x800,
        _subrecord("EDID", b"P0LightGlobal\0")
        + _subrecord("FNAM", b"f")
        + _subrecord("FLTV", struct.pack("<f", 2.5)),
    )
    return _tes4(record_count=2, next_object_id=0x801, flags=0x200) + _group(
        b"GLOB", 0, glob
    )


def hand_encoded_base_master() -> bytes:
    """Base for override/deletion and placed-lo tests."""
    glob = _record(
        "GLOB",
        0x800,
        _subrecord("EDID", b"P0OverrideGlobal\0")
        + _subrecord("FNAM", b"f")
        + _subrecord("FLTV", struct.pack("<f", 1.0)),
    )
    static = _record(
        "STAT",
        0x801,
        _subrecord("EDID", b"P0OverrideStatic\0")
        + _subrecord("MODL", b"meshes\\p0_override.nif\0"),
    )
    cell_id = 0x802
    cell = _record(
        "CELL",
        cell_id,
        _subrecord("EDID", b"P0OverrideCell\0") + _subrecord("DATA", struct.pack("<H", 1)),
    )
    placed = _record(
        "REFR",
        0x803,
        _subrecord("NAME", struct.pack("<I", 0x801))
        + _subrecord("DATA", struct.pack("<ffffff", 1.0, 2.0, 3.0, 0.0, 0.0, 0.0)),
        flags=0x400,
    )
    return b"".join(
        (
            _tes4(record_count=11, next_object_id=0x804, flags=0x1),
            _group(b"GLOB", 0, glob),
            _group(b"STAT", 0, static),
            _interior_cell_tree(cell_id, cell, placed),
        )
    )


def hand_encoded_override() -> bytes:
    """Override two master records and tombstone one without loading a corpus."""
    glob = _record(
        "GLOB",
        0x800,
        _subrecord("EDID", b"P0OverrideGlobal\0")
        + _subrecord("FNAM", b"f")
        + _subrecord("FLTV", struct.pack("<f", 9.5)),
    )
    deleted_static = _record("STAT", 0x801, b"", flags=0x20)
    cell_id = 0x802
    cell = _record(
        "CELL",
        cell_id,
        _subrecord("EDID", b"P0OverrideCell\0") + _subrecord("DATA", struct.pack("<H", 1)),
    )
    placed_override = _record(
        "REFR",
        0x803,
        _subrecord("NAME", struct.pack("<I", 0x801))
        + _subrecord("DATA", struct.pack("<ffffff", 50.0, 60.0, 70.0, 0.0, 0.0, 0.0)),
        flags=0xC00,
    )
    return b"".join(
        (
            _tes4(record_count=11, next_object_id=0x800, masters=("p0-base.esm",)),
            _group(b"GLOB", 0, glob),
            _group(b"STAT", 0, deleted_static),
            _interior_cell_tree(cell_id, cell, placed_override),
        )
    )


def hand_encoded_localized() -> bytes:
    localized_armor = _record(
        "ARMO",
        0x800,
        _subrecord("EDID", b"P0LocalizedArmor\0")
        + _subrecord("FULL", struct.pack("<I", 0x12345678)),
    )
    return _tes4(record_count=2, next_object_id=0x801, flags=0x80) + _group(
        b"ARMO", 0, localized_armor
    )


def inject_repeated_unknown_subrecords(plugin: bytes) -> bytes:
    """Raw-inject two same-signature framed opaque subrecords into the first GLOB."""
    tes4_size = struct.unpack_from("<I", plugin, 4)[0]
    group_offset = RECORD_HEADER_SIZE + tes4_size
    group_signature, group_size = struct.unpack_from("<4sI", plugin, group_offset)
    if group_signature != b"GRUP":
        raise ValueError("expected the first GLOB group immediately after TES4")
    group_label = plugin[group_offset + 8 : group_offset + 12]
    if group_label != b"GLOB":
        raise ValueError("expected the first group label to be GLOB")
    record_offset = group_offset + GROUP_HEADER_SIZE
    signature, data_size = struct.unpack_from("<4sI", plugin, record_offset)
    if signature != b"GLOB":
        raise ValueError("expected a GLOB record inside the GLOB group")
    record_end = record_offset + RECORD_HEADER_SIZE + data_size
    injected = _subrecord("ZZZZ", b"opaque\x00one") + _subrecord("ZZZZ", b"\x00\xfftwo")
    out = bytearray(plugin[:record_end] + injected + plugin[record_end:])
    struct.pack_into("<I", out, record_offset + 4, data_size + len(injected))
    struct.pack_into("<I", out, group_offset + 4, group_size + len(injected))
    return bytes(out)


def corrupt_first_glob_subrecord_length(plugin: bytes) -> bytes:
    """Overstate the first GLOB subrecord length without changing its enclosing sizes."""
    tes4_size = struct.unpack_from("<I", plugin, 4)[0]
    group_offset = RECORD_HEADER_SIZE + tes4_size
    record_offset = group_offset + GROUP_HEADER_SIZE
    data_offset = record_offset + RECORD_HEADER_SIZE
    if plugin[record_offset : record_offset + 4] != b"GLOB":
        raise ValueError("expected GLOB record inside the first group")
    if plugin[data_offset : data_offset + 4] != b"EDID":
        raise ValueError("expected EDID as the first GLOB subrecord")
    out = bytearray(plugin)
    struct.pack_into("<H", out, data_offset + 4, 0xFFFF)
    return bytes(out)


def corrupt_tes4_record_length(plugin: bytes) -> bytes:
    """Make TES4 declare more bytes than the complete source file contains."""
    out = bytearray(plugin)
    struct.pack_into("<I", out, 4, len(plugin))
    return bytes(out)


def hand_encoded_cases() -> dict[str, tuple[bytes, str]]:
    """Return filename to (bytes, provenance) cases for task-local qualification."""
    full = hand_encoded_full()
    base = hand_encoded_base_master()
    cases = {
        "p0-hand-full.esp": (full, "hand-encoded-independent-expected-wire-values"),
        "p0-hand-light.esl": (hand_encoded_light(), "hand-encoded-independent-expected-wire-values"),
        "p0-base.esm": (base, "hand-encoded-independent-expected-wire-values"),
        "p0-override.esp": (hand_encoded_override(), "hand-encoded-independent-expected-wire-values"),
        "p0-localized.esp": (hand_encoded_localized(), "hand-encoded-localized-id-no-string-table"),
        "p0-unknown-repeated.esp": (
            inject_repeated_unknown_subrecords(full),
            "raw-byte-injection-after-hand-encoding-two-unknown-ZZZZ-subrecords",
        ),
        "p0-truncated-tail.esp": (
            inject_repeated_unknown_subrecords(full)[:-1],
            "raw-byte-truncation-after-hand-encoding-and-unknown-injection",
        ),
        "p0-truncated-header.esp": (
            full[:12],
            "raw-byte-truncation-inside-TES4-record-header",
        ),
        "p0-bad-subrecord-length.esp": (
            corrupt_first_glob_subrecord_length(full),
            "raw-byte-length-corruption-after-hand-encoding",
        ),
        "p0-bad-tes4-length.esp": (
            corrupt_tes4_record_length(full),
            "raw-byte-TES4-length-corruption-after-hand-encoding",
        ),
    }
    return cases


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()
