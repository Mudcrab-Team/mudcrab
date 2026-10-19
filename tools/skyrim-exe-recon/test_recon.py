"""Regression checks for the read-only PE32+ recon tool, using a synthetic image."""
from pathlib import Path
import hashlib
import io
import json
import struct
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

import recon

FILE_ALIGN = 0x200
TEXT, RDATA, PDATA, RSRC = 0x1000, 0x2000, 0x3000, 0x4000
HEADERS = 0x400
RDATA_RAW = HEADERS + FILE_ALIGN  # sections are laid out in file order after .text


def section_header(name: bytes, rva: int, raw_offset: int, flags: int) -> bytes:
    return struct.pack("<8sIIIIIIHHI", name, FILE_ALIGN, rva, FILE_ALIGN, raw_offset, 0, 0, 0, 0, flags)


def build_pe(text: bytes = b"\x90" * 16, bind: bool = False) -> bytes:
    """Return a minimal PE32+ with imports, CodeView, chained .pdata and a version resource."""
    rdata = bytearray(FILE_ALIGN)
    # Import descriptor -> KERNEL32.dll with two thunks (one by name, one by ordinal).
    struct.pack_into("<IIIII", rdata, 0x00, RDATA + 0x40, 0, 0, RDATA + 0x80, RDATA + 0x60)
    for table in (0x40, 0x60):
        struct.pack_into("<QQQ", rdata, table, RDATA + 0xA0, 0x8000000000000005, 0)
    rdata[0x80:0x8D] = b"KERNEL32.dll\0"
    rdata[0xA0:0xA8] = b"\0\0Sleep\0"
    pdb = b"C:\\build\\Fixture.pdb\0"
    struct.pack_into("<IIHHIIII", rdata, 0xC0, 0, 0, 0, 0, 2, 24 + len(pdb), RDATA + 0xE0, RDATA_RAW + 0xE0)
    rdata[0xE0:0xE4] = b"RSDS"
    struct.pack_into("<IHH8sI", rdata, 0xE4, 0x12345678, 0x9ABC, 0xDEF0, bytes(range(8)), 3)
    rdata[0xF8:0xF8 + len(pdb)] = pdb
    # Unwind info: primary at +0x180, chained (UNW_FLAG_CHAININFO) at +0x190 pointing to the primary.
    rdata[0x180:0x184] = bytes([1, 0, 0, 0])
    rdata[0x190:0x194] = bytes([1 | (recon.UNW_FLAG_CHAININFO << 3), 0, 0, 0])
    struct.pack_into("<III", rdata, 0x194, TEXT, TEXT + 0x80, RDATA + 0x180)

    pdata = bytearray(FILE_ALIGN)
    struct.pack_into("<IIIIII", pdata, 0, TEXT, TEXT + 0x80, RDATA + 0x180, TEXT + 0x80, TEXT + 0xC0, RDATA + 0x190)

    rsrc = bytearray(FILE_ALIGN)
    struct.pack_into("<IIIIII", rsrc, 0x40, 0xFEEF04BD, 0x10000, (1 << 16) | 7, 104 << 16, (1 << 16) | 7, 104 << 16)

    names = [(b".text", TEXT, 0x60000020), (b".rdata", RDATA, 0x40000040), (b".pdata", PDATA, 0x40000040), (b".rsrc", RSRC, 0x40000040)]
    bodies = [text.ljust(FILE_ALIGN, b"\0"), bytes(rdata), bytes(pdata), bytes(rsrc)]
    if bind:
        names.append((b".bind", 0x5000, 0xE0000020))
        bodies.append(bytes(FILE_ALIGN))

    dos = bytearray(0x80)
    dos[:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 0x80)
    coff = struct.pack("<HHIIIHH", 0x8664, len(names), 0x5F000000, 0, 0, 240, 0x22)
    optional = bytearray(240)
    struct.pack_into("<HBB", optional, 0, 0x20B, 14, 29)
    struct.pack_into("<I", optional, 16, TEXT)
    struct.pack_into("<Q", optional, 24, 0x140000000)
    struct.pack_into("<II", optional, 32, 0x1000, FILE_ALIGN)
    struct.pack_into("<III", optional, 56, 0x1000 * (len(names) + 1), HEADERS, 0)
    struct.pack_into("<HH", optional, 68, 2, 0x8160)
    struct.pack_into("<I", optional, 108, 16)
    for index, (rva, size) in {1: (RDATA, 40), 2: (RSRC, 0x100), 3: (PDATA, 24), 6: (RDATA + 0xC0, 28)}.items():
        struct.pack_into("<II", optional, 112 + index * 8, rva, size)
    headers = bytes(dos) + b"PE\0\0" + coff + bytes(optional)
    headers += b"".join(section_header(n, rva, HEADERS + i * FILE_ALIGN, f) for i, (n, rva, f) in enumerate(names))
    return headers.ljust(HEADERS, b"\0") + b"".join(bodies)


class ReconTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_headers_imports_codeview_and_version(self):
        report = recon.fingerprint(build_pe(), "SkyrimSE.exe", [])
        self.assertEqual(report["schema"], recon.SCHEMA)
        self.assertEqual(report["coff"]["machine"], "AMD64")
        self.assertEqual(report["optional_header"]["image_base"], "0x140000000")
        self.assertEqual(report["optional_header"]["entry_section"], ".text")
        self.assertEqual(report["optional_header"]["subsystem"], "WINDOWS_GUI")
        self.assertEqual([s["name"] for s in report["sections"]], [".text", ".rdata", ".pdata", ".rsrc"])
        self.assertEqual(report["sections"][0]["flags"], ["CODE", "EXECUTE", "READ"])
        self.assertEqual(report["imports"], [{"dll": "KERNEL32.dll", "functions": 2}])
        self.assertEqual(report["codeview"], {"guid": "12345678-9ABC-DEF0-0001-020304050607", "age": 3, "pdb_name": "Fixture.pdb"})
        self.assertEqual(report["version"], {"file_version": "1.7.104.0", "product_version": "1.7.104.0", "fixed_file_info_records": 1})
        self.assertEqual(report["runtime_functions"], 2)
        self.assertIsNone(report["known_build"])
        self.assertEqual(report["notes"], [])

    def test_rva_lookup_follows_chained_unwind_to_primary_function(self):
        queries = recon.fingerprint(build_pe(), "x.exe", [TEXT + 0x10, TEXT + 0xA0, TEXT + 0x1F0, RDATA])["rva_queries"]
        self.assertEqual(queries[0]["runtime_function"], {"begin": "0x1000", "end": "0x1080", "unwind": "0x2180", "primary_begin": "0x1000", "chain_hops": 0})
        self.assertEqual(queries[1]["runtime_function"]["begin"], "0x1080")
        self.assertEqual(queries[1]["runtime_function"]["primary_begin"], "0x1000")
        self.assertEqual(queries[1]["runtime_function"]["chain_hops"], 1)
        self.assertEqual(queries[2], {"rva": "0x11f0", "section": ".text", "runtime_function": None})
        self.assertEqual(queries[3]["section"], ".rdata")

    def test_packing_heuristics_are_reported_as_notes(self):
        noisy = bytes((i * 167 + 13) % 256 for i in range(FILE_ALIGN))
        notes = recon.fingerprint(build_pe(text=noisy, bind=True), "x.exe", [])["notes"]
        self.assertTrue(any("entropy" in note and ".text" in note for note in notes))
        self.assertTrue(any(".bind" in note for note in notes))

    def test_output_is_deterministic_and_omits_local_path(self):
        exe = self.root / "private-dir" / "SkyrimSE.exe"
        exe.parent.mkdir()
        exe.write_bytes(build_pe())
        out_a, out_b = self.root / "a.json", self.root / "b.json"
        with redirect_stdout(io.StringIO()):
            self.assertEqual(recon.main([str(exe), "--rva", "0x1010", "--out", str(out_a)]), 0)
            self.assertEqual(recon.main([str(exe), "--rva", "0x1010", "--out", str(out_b)]), 0)
        self.assertEqual(out_a.read_bytes(), out_b.read_bytes())
        self.assertNotIn("private-dir", out_a.read_text())
        self.assertEqual(json.loads(out_a.read_text())["file"]["name"], "SkyrimSE.exe")

    def test_expect_reports_changed_fields(self):
        exe = self.root / "SkyrimSE.exe"
        exe.write_bytes(build_pe())
        evidence = self.root / "evidence.json"
        with redirect_stdout(io.StringIO()):
            self.assertEqual(recon.main([str(exe), "--out", str(evidence)]), 0)
            self.assertEqual(recon.main([str(exe), "--expect", str(evidence)]), 0)
        exe.write_bytes(build_pe(text=b"\xcc" * 16))
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            self.assertEqual(recon.main([str(exe), "--expect", str(evidence)]), 1)
        self.assertIn("file", stderr.getvalue())
        self.assertIn("sections", stderr.getvalue())

    def test_known_build_is_matched_by_hash(self):
        data = build_pe()
        digest = hashlib.sha256(data).hexdigest()
        original = dict(recon.KNOWN_BUILDS)
        self.addCleanup(lambda: (recon.KNOWN_BUILDS.clear(), recon.KNOWN_BUILDS.update(original)))
        recon.KNOWN_BUILDS[digest] = {"file_version": "9.9.9.9", "recorded_in": "fixture"}
        report = recon.fingerprint(data, "x.exe", [])
        self.assertEqual(report["known_build"]["recorded_in"], "fixture")
        self.assertTrue(any("differs from recorded 9.9.9.9" in note for note in report["notes"]))

    def test_malformed_inputs_fail_with_recon_error(self):
        valid = build_pe()
        cases = {
            "not mz": b"ZZ" + valid[2:],
            "truncated": valid[:0x100],
            "pe32 not plus": valid[:0x98] + b"\x0b\x01" + valid[0x9A:],
        }
        for label, data in cases.items():
            with self.subTest(label), self.assertRaises(recon.ReconError):
                recon.fingerprint(data, "x.exe", [])

    def test_unreadable_input_returns_error_status(self):
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            self.assertEqual(recon.main([str(self.root / "missing.exe")]), 1)
        self.assertIn("Skyrim exe recon", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
