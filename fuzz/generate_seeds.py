#!/usr/bin/env python3
"""Write the fuzz seed corpus from the converter's synthetic test fixtures.

Every byte here is built by a unit test in the converter crate or by this script.
No Skyrim file, or any other game file, is read or copied.

libFuzzer writes new inputs next to the seeds, so fuzz a copy of a seed directory,
not the committed tree:

    python3 fuzz/generate_seeds.py
    cp -R fuzz/seeds/esm_subrecords /tmp/corpus/
    cargo +nightly fuzz run --fuzz-dir fuzz esm_subrecords /tmp/corpus/esm_subrecords

Run this script only when the fixtures change.
"""

from __future__ import annotations

import zlib
from pathlib import Path

SEEDS = Path(__file__).resolve().parent / "seeds"


def write(target: str, name: str, data: bytes) -> None:
    directory = SEEDS / target
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"{name}.seed").write_bytes(data)


def subrecord(tag: bytes, payload: bytes) -> bytes:
    """Frame one subrecord as `extractors::extract_subrecords` expects it."""
    assert len(tag) == 4 and len(payload) <= 0xFFFF
    return tag + len(payload).to_bytes(2, "little") + payload


def frame(tag: bytes, form_id: int, flags: int, payload: bytes) -> bytes:
    """Frame one record as `records::scanner` expects it."""
    assert len(tag) == 4
    return (
        tag
        + len(payload).to_bytes(4, "little")
        + flags.to_bytes(4, "little")
        + form_id.to_bytes(4, "little")
        + bytes(8)
        + payload
    )


def zlib_record(payload: bytes, declared_size: int | None = None) -> bytes:
    """A compressed record payload: declared size, then one zlib stream."""
    if declared_size is None:
        declared_size = len(payload)
    return declared_size.to_bytes(4, "little") + zlib.compress(payload, 9)


def main() -> None:
    # crates/converter/src/esm/extractors.rs, subrecord_tests
    write("esm_subrecords", "short", subrecord(b"EDID", b"x\0"))
    # The XXXX form carries a 32-bit length for payloads above the 16-bit field.
    extended = bytearray(b"XXXX")
    extended += (4).to_bytes(2, "little") + (300).to_bytes(4, "little")
    extended += subrecord(b"DATA", b"\x07" * 300)
    write("esm_subrecords", "extended-length", bytes(extended))
    # A complete subrecord followed by a truncated header and payload.
    write(
        "esm_subrecords",
        "truncated",
        subrecord(b"EDID", b"x\0") + bytes(b"DATA\x04\x00xy"),
    )

    # crates/converter/src/records/scanner.rs, compression_requires_complete_single_stream
    write("record_inflate", "valid", zlib_record(b"data"))
    write("record_inflate", "truncated-stream", zlib_record(b"data")[:-1])
    write("record_inflate", "oversized-declared", zlib_record(b"data", 0xFFFF_FFFF))

    # crates/converter/src/records/scanner.rs, corrupt_record_keeps_neighbors.
    # The middle record claims compression but holds a 1-byte invalid stream.
    write(
        "record_scan",
        "corrupt-neighbor",
        frame(b"TES4", 0, 0, b"")
        + frame(b"STAT", 1, 0, subrecord(b"EDID", b"a\0"))
        + frame(b"STAT", 2, 0x0004_0000, bytes([0, 0, 0, 0, 99]))
        + frame(b"STAT", 3, 0, subrecord(b"EDID", b"b\0")),
    )
    # A valid compressed middle record plus an incomplete trailing frame.
    write(
        "record_scan",
        "compressed",
        frame(b"TES4", 0, 0, b"")
        + frame(b"STAT", 1, 0x0004_0000, zlib_record(subrecord(b"EDID", b"a\0")))
        + frame(b"STAT", 2, 0, b"EDID")[0:10],
    )

    for path in sorted(SEEDS.rglob("*.seed")):
        print(f"{path.relative_to(SEEDS.parent)}: {path.stat().st_size} bytes")


if __name__ == "__main__":
    main()
