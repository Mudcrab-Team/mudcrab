"""Synthetic PE/proc fixtures verify the collector without accessing a process."""

import importlib.util
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('inspect_skyrim_tonemap_memory.py')
SPEC = importlib.util.spec_from_file_location('inspect_skyrim_tonemap_memory', SCRIPT)
MEMORY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MEMORY)

CODE_RVA = 0x153D000
# The fixture mirrors Linux /proc and needs os.major, os.minor, os.pread and
# os.pwrite; Windows provides none of them.
POSIX_PROC_FIXTURE = os.name == 'posix' and all(
    hasattr(os, name) for name in ('major', 'minor', 'pread', 'pwrite'))


def synthetic_pe():
    """A bounded AMD64 image; no retail executable bytes are embedded."""
    disk = bytearray(0x2000)
    disk[:2] = b'MZ'
    struct.pack_into('<I', disk, 0x3C, 0x80)
    disk[0x80:0x84] = b'PE\0\0'
    struct.pack_into('<HHI', disk, 0x84, 0x8664, 1, 0x12345678)
    struct.pack_into('<H', disk, 0x94, 0xF0)
    optional = 0x98
    struct.pack_into('<H', disk, optional, 0x20B)
    struct.pack_into('<Q', disk, optional + 24, MEMORY.PREFERRED_BASE)
    struct.pack_into('<II', disk, optional + 56, 0x3700000, 0x400)
    section = optional + 0xF0
    disk[section:section + 8] = b'.text\0\0\0'
    struct.pack_into('<IIII', disk, section + 8, 0x1000, CODE_RVA, 0x1000, 0x1000)
    struct.pack_into('<I', disk, section + 36, 0x60000020)
    # All code bytes differ from zero; one flipped byte must be detected.
    disk[0x1000:] = bytes((index * 17 + 3) % 256 for index in range(0x1000))
    return bytes(disk)


class Fixture:
    def __init__(self, root, base=0x5F00000000):
        self.root, self.base, self.pid = root, base, 12345
        self.executable = root / 'game directory/SkyrimSE.exe'
        self.executable.parent.mkdir()
        self.code_rva = CODE_RVA
        self.disk = synthetic_pe()
        self.executable.write_bytes(self.disk)
        self.executable = self.executable.resolve()
        self.proc = root / 'proc'
        self.directory = self.proc / str(self.pid)
        self.directory.mkdir(parents=True)
        self.stat_path = self.directory / 'stat'
        self.write_stat(987654321)
        file_stat = self.executable.stat()
        escaped = str(self.executable).replace(' ', r'\040')
        device = f'{os.major(file_stat.st_dev):02x}:{os.minor(file_stat.st_dev):02x}'
        self.maps_text = (
            '0000000070000000-0000000070100000 rw-p 00000000 00:00 0\n'
            f'{base:016x}-{base + 0x1000:016x} r--p 00000000 {device} {file_stat.st_ino} {escaped}\n'
            f'{base + 0x1000:016x}-{base + 0x2000000:016x} r-xp 00000000 00:00 0\n'
            f'{base + 0x2000000:016x}-{base + 0x3700000:016x} rw-p 00000000 00:00 0\n')
        self.maps_path = self.directory / 'maps'
        self.maps_path.write_text(self.maps_text)
        self.mem_path = self.directory / 'mem'
        self.mem_fd = os.open(self.mem_path, os.O_CREAT | os.O_RDWR, 0o600)
        self.write(base, self.disk[:0x400])
        self.write(base + self.code_rva, self.disk[0x1000:])
        for index, (name, address, kind) in enumerate(MEMORY.SETTINGS):
            name_address = base + 0x1F00001 + index * 0x100
            value = b'\0' if kind == 'byte' else struct.pack('<f', [0, 1.1, 1.4, 2, 0.4, 0.3, 1][index])
            self.write(self.relocate(address), value)
            self.u64(self.relocate(address) + 8, name_address)
            self.write(name_address, name.encode() + b'\0')
        self.manager = 0x70001000
        self.current, self.override = 0x70002000, 0x70002100
        self.u64(self.relocate(MEMORY.MANAGER_SINGLETON), self.manager)
        self.u64(self.manager + 0xA8, self.current)
        self.u64(self.manager + 0xB0, self.override)
        data = struct.pack('<16f', *range(1, 17))
        for address in (self.current, self.override, self.manager + 0xC8):
            self.write(address, data)
        self.write(self.manager + 0x118, struct.pack('<f', 0))
        self.effects, self.hdr, self.parameters = 0x70003000, 0x70004000, 0x70005000
        self.u64(self.manager + 0x28, self.effects)
        self.u64(self.effects + 30 * 8, self.hdr)
        self.u64(self.hdr, self.relocate(MEMORY.HDR_VTABLE))
        self.u64(self.hdr + 0x30, self.parameters)
        self.parameter_addresses, self.array_addresses = {}, {}
        for name, slot, offset in (('ordinary', 2, 0), ('fade', 3, 0x100), ('adaptation', 8, 0x200)):
            parameter, array = 0x70006000 + offset, 0x70007000 + offset
            self.parameter_addresses[name], self.array_addresses[name] = parameter, array
            self.u64(self.parameters + slot * 8, parameter)
            self.u64(parameter, self.relocate(MEMORY.PARAMETER_VTABLE))
            self.u64(parameter + 0x40, array)
            self.write(parameter + 0x48, struct.pack('<I', 5 if name == 'fade' else 4))
            for index in (1, 2, 3):
                self.write(array + index * 16, struct.pack('<4f', *[index + item / 10 for item in range(4)]))

    def close(self):
        os.close(self.mem_fd)

    def relocate(self, address):
        return self.base + address - MEMORY.PREFERRED_BASE

    def write(self, address, data):
        os.pwrite(self.mem_fd, data, address)

    def u64(self, address, value):
        self.write(address, struct.pack('<Q', value))

    def write_stat(self, starttime):
        # Include spaces and parentheses in comm; field 22 remains starttime.
        fields = [b'S'] + [b'0'] * 18 + [str(starttime).encode()] + [b'0'] * 10
        self.stat_path.write_bytes(str(self.pid).encode() + b' (Skyrim (Main)) ' + b' '.join(fields) + b'\n')

    def collect(self, output):
        with patch.object(MEMORY, 'EXPECTED_EXE_SHA256', MEMORY.sha256(self.disk)):
            return MEMORY.collect(self.pid, self.executable, output, proc_root=self.proc)


@unittest.skipUnless(POSIX_PROC_FIXTURE,
                     'collector fixture needs POSIX /proc with os.pread, os.pwrite and os.major')
class ReadOnlyCollectorTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.fixture = Fixture(self.root)
        self.addCleanup(self.fixture.close)
        self.output = self.root / 'evidence'

    def test_aslr_identity_names_offsets_and_parameter_tuples(self):
        result = self.fixture.collect(self.output)
        self.assertEqual(result['status'], 'complete')
        self.assertEqual(result['image']['actual_base'], hex(self.fixture.base))
        self.assertEqual(result['image']['HDRParameters']['size_bytes'], 0x3CF)
        self.assertTrue(result['image']['HDRParameters']['disk_memory_equal'])
        self.assertTrue(result['double_read']['payloads_equal'])
        self.assertTrue(result['double_read']['all_pointer_and_selector_guards_consistent'])
        first = result['samples'][0]
        self.assertEqual(first['settings']['bUseFilmicCurve:Display']['value'], 0)
        self.assertAlmostEqual(first['settings']['fReinhardWhiteScale:Display']['value'], 1.1)
        self.assertEqual(first['manager']['resolved_base']['fields']['white'], 6)
        self.assertEqual(first['manager']['resolved_base']['fields']['tint_amount'], 13)
        self.assertEqual(first['manager']['resolved_base']['fields']['tint_blue'], 16)
        self.assertEqual(first['manager']['selected_curve_parameter'], 'ordinary')
        self.assertEqual(first['parameters']['buffers']['ordinary']['float4_count'], 4)
        self.assertEqual(set(first['parameters']['buffers']['adaptation']['float4_values_by_cpu_index']), {'2'})
        persisted = json.loads((self.output / 'receipt.json').read_text())
        reads = json.loads((self.output / 'memory-reads.json').read_text())
        self.assertEqual(persisted, result)
        self.assertEqual(result['memory_reads']['count'], len(reads))
        for entry in reads:
            raw = bytes.fromhex(entry['raw_hex'])
            self.assertEqual(len(raw), entry['size_bytes'])
            self.assertEqual(MEMORY.sha256(raw), entry['sha256'])

    def test_all_collector_file_descriptors_are_read_only(self):
        original_open = os.open
        opens = []

        def checked(path, flags, *args, **kwargs):
            opens.append((str(path), flags))
            self.assertEqual(flags & os.O_ACCMODE, os.O_RDONLY)
            self.assertEqual(flags & (os.O_CREAT | os.O_TRUNC | os.O_APPEND), 0)
            return original_open(path, flags, *args, **kwargs)

        with patch.object(MEMORY.os, 'open', side_effect=checked):
            self.fixture.collect(self.output)
        self.assertEqual({Path(path).name for path, _ in opens}, {'SkyrimSE.exe', 'mem'})

    def test_disk_hash_mismatch_fails_before_memory_open_and_retains_receipt(self):
        with self.assertRaisesRegex(ValueError, 'hash differs'):
            MEMORY.collect(self.fixture.pid, self.fixture.executable, self.output, self.fixture.proc)
        failure = json.loads((self.output / 'receipt.json').read_text())
        self.assertEqual(failure['status'], 'failed')
        self.assertEqual(failure['failure']['phase'], 'source')
        self.assertEqual(failure['memory_reads']['count'], 0)

    def test_memory_header_and_function_mismatch_fail_closed(self):
        for address, error in ((self.fixture.base + 0x88, 'PE headers'),
                               (self.fixture.relocate(MEMORY.HDR_FUNCTION_START) + 23, 'HDRParameters')):
            with self.subTest(address=address):
                original = os.pread(self.fixture.mem_fd, 1, address)
                self.fixture.write(address, bytes([original[0] ^ 0x80]))
                with self.assertRaisesRegex(ValueError, error):
                    self.fixture.collect(self.root / ('failed-' + str(address)))
                self.fixture.write(address, original)

    def test_wine_imagebase_rewrite_accepts_only_the_actual_mapping_base(self):
        field_offset = 0x98 + 24
        self.fixture.u64(self.fixture.base + field_offset, self.fixture.base)
        result = self.fixture.collect(self.output)
        identity = result['image']['header_identity']
        self.assertEqual(identity['mode'], 'wine-relocated-image-base')
        self.assertEqual(identity['allowed_loader_rewrite']['memory_value'], hex(self.fixture.base))
        self.assertEqual(identity['allowed_loader_rewrite']['file_offset'], hex(field_offset))
        self.assertTrue(identity['observed_byte_differences'])
        self.assertTrue(all(field_offset <= int(item['file_offset'], 16) < field_offset + 8
                            for item in identity['observed_byte_differences']))
        self.fixture.u64(self.fixture.base + field_offset, self.fixture.base + 0x1000)
        with self.assertRaisesRegex(ValueError, 'PE headers'):
            self.fixture.collect(self.root / 'incorrect-image-base')
        self.fixture.u64(self.fixture.base + field_offset, self.fixture.base)
        self.fixture.write(self.fixture.base + 0x88, b'\0')
        with self.assertRaisesRegex(ValueError, 'PE headers'):
            self.fixture.collect(self.root / 'rewritten-base-plus-unrelated-mutation')

    def test_pinned_header_shape_and_observed_wine_base_regression(self):
        # The retained live failure changed only ImageBase at0x1b0 from
        # 0x140000000 to0x6ffff54f0000. No retail bytes are embedded here.
        pe = MEMORY.parse_pe(self.fixture.disk)
        expected = bytearray(self.fixture.disk[:pe['size_headers']])
        pe['pe_offset'] = 0x180
        field_offset = 0x1B0
        struct.pack_into('<Q', expected, field_offset, MEMORY.PREFERRED_BASE)
        observed = bytearray(expected)
        struct.pack_into('<Q', observed, field_offset, 0x6FFFF54F0000)
        identity = MEMORY.identify_memory_headers(bytes(expected), bytes(observed), pe, 0x6FFFF54F0000)
        self.assertEqual([item['file_offset'] for item in identity['observed_byte_differences']],
                         ['0x1b2', '0x1b3', '0x1b4', '0x1b5'])

    def test_wrong_mapping_inode_or_path_cannot_identify_executable(self):
        for changed in (self.fixture.maps_text.replace(str(self.fixture.executable.stat().st_ino), '999999999'),
                        self.fixture.maps_text.replace('SkyrimSE.exe', 'Other.exe')):
            self.fixture.maps_path.write_text(changed)
            with self.assertRaisesRegex(ValueError, 'exact executable file identity'):
                self.fixture.collect(self.root / ('map-' + str(len(list(self.root.iterdir())))))

    def test_setting_name_identity_and_escaped_pointer_are_rejected(self):
        address = self.fixture.relocate(MEMORY.SETTINGS[0][1]) + 8
        original = os.pread(self.fixture.mem_fd, 8, address)
        self.fixture.u64(address, self.fixture.base + 0x1F00101)
        with self.assertRaisesRegex(ValueError, 'setting name mismatch'):
            self.fixture.collect(self.root / 'wrong-name')
        self.fixture.u64(address, 0x70008000)
        with self.assertRaisesRegex(ValueError, 'name pointer escapes image'):
            self.fixture.collect(self.root / 'escaped-name')
        self.fixture.write(address, original)

    def test_pointer_mapping_bounds_and_count_and_vtable_guards(self):
        cases = (
            (self.fixture.manager + 0xA8, struct.pack('<Q', 0x70110000), 'outside readable'),
            (self.fixture.hdr, struct.pack('<Q', self.fixture.base + 0x2000000), 'HDR object vtable'),
            (self.fixture.parameter_addresses['ordinary'], struct.pack('<Q', self.fixture.base + 0x2000000),
             'parameter object vtable'),
            (self.fixture.parameter_addresses['ordinary'] + 0x48, struct.pack('<I', 3), 'float4 count'),
            (self.fixture.parameter_addresses['ordinary'] + 0x48, struct.pack('<I', 0xFFFFFFFF), 'float4 count'),
            (self.fixture.parameter_addresses['ordinary'] + 0x40, struct.pack('<Q', 0x700FFFF0), 'outside readable'),
        )
        for index, (address, replacement, error) in enumerate(cases):
            with self.subTest(index=index):
                original = os.pread(self.fixture.mem_fd, len(replacement), address)
                self.fixture.write(address, replacement)
                with self.assertRaisesRegex(ValueError, error):
                    self.fixture.collect(self.root / ('bounds-' + str(index)))
                self.fixture.write(address, original)

    def test_nullable_base_and_uninitialized_hdr_are_recorded(self):
        self.fixture.u64(self.fixture.manager + 0xA8, 0)
        self.fixture.u64(self.fixture.manager + 0xB0, 0)
        self.fixture.u64(self.fixture.effects + 30 * 8, 0)
        result = self.fixture.collect(self.output)
        self.assertIsNone(result['samples'][0]['manager']['current_base'])
        self.assertEqual(result['samples'][0]['parameters']['hdr_pointer'], '0x0')
        self.assertEqual(result['samples'][0]['parameters']['buffers'], {})

    def test_pid_reuse_is_detected_and_partial_samples_retained(self):
        original = MEMORY.sample

        def replace_pid(*args, **kwargs):
            result = original(*args, **kwargs)
            self.fixture.write_stat(987654322)
            return result

        with patch.object(MEMORY, 'sample', side_effect=replace_pid), self.assertRaisesRegex(
                ValueError, 'Process identity changed'):
            self.fixture.collect(self.output)
        receipt = json.loads((self.output / 'receipt.json').read_text())
        self.assertEqual(receipt['status'], 'failed')
        self.assertEqual(len(receipt['samples']), 1)

    def test_live_value_changes_are_reported_as_inconsistent_not_atomic(self):
        original = MEMORY.sample

        def change_value(*args, **kwargs):
            result = original(*args, **kwargs)
            self.fixture.write(self.fixture.manager + 0xDC, struct.pack('<f', 42))
            return result

        with patch.object(MEMORY, 'sample', side_effect=change_value):
            result = self.fixture.collect(self.output)
        self.assertFalse(result['double_read']['payloads_equal'])
        self.assertTrue(result['double_read']['all_pointer_and_selector_guards_consistent'])

    def test_pointer_changes_during_a_sample_are_explicit(self):
        original = MEMORY.MemoryReader.read
        changed = False

        def swap_pointer(reader, label, address, length, *args, **kwargs):
            nonlocal changed
            value = original(reader, label, address, length, *args, **kwargs)
            if label == 'sample-1.manager.current_base.data' and not changed:
                self.fixture.u64(self.fixture.manager + 0xA8, self.fixture.override)
                changed = True
            return value

        with patch.object(MEMORY.MemoryReader, 'read', new=swap_pointer):
            result = self.fixture.collect(self.output)
        self.assertFalse(result['double_read']['all_pointer_and_selector_guards_consistent'])
        self.assertIn('manager.current_base', result['samples'][0]['changed_guards'])

    def test_sample_mapping_changes_are_rejected(self):
        original = MEMORY.sample

        def change_map(*args, **kwargs):
            result = original(*args, **kwargs)
            self.fixture.maps_path.write_text(self.fixture.maps_text.replace('rw-p', 'r--p'))
            return result

        with patch.object(MEMORY, 'sample', side_effect=change_map), self.assertRaisesRegex(
                ValueError, 'sampled memory mapping changed'):
            self.fixture.collect(self.output)

    def test_unrelated_new_mapping_does_not_invalidate_bounded_sample(self):
        original = MEMORY.sample

        def add_map(*args, **kwargs):
            result = original(*args, **kwargs)
            self.fixture.maps_path.write_text(self.fixture.maps_text +
                                             '0000007f00000000-0000007f00001000 rw-p 00000000 00:00 0\n')
            return result

        with patch.object(MEMORY, 'sample', side_effect=add_map):
            result = self.fixture.collect(self.output)
        self.assertTrue(result['maps']['all_sampled_mappings_unchanged'])
        self.assertNotEqual(result['maps']['before_sha256'], result['maps']['after_sha256'])

    def test_code_mutation_after_samples_fails_final_identity_check(self):
        original = MEMORY.sample

        def patch_code(*args, **kwargs):
            result = original(*args, **kwargs)
            self.fixture.write(self.fixture.relocate(MEMORY.HDR_FUNCTION_START), b'\xCC')
            return result

        with patch.object(MEMORY, 'sample', side_effect=patch_code), self.assertRaisesRegex(
                ValueError, 'HDRParameters memory bytes differ'):
            self.fixture.collect(self.output)
        receipt = json.loads((self.output / 'receipt.json').read_text())
        self.assertEqual(receipt['failure']['phase'], 'final-identity')
        self.assertEqual(len(receipt['samples']), 2)

    def test_partial_memory_read_keeps_received_bytes_in_failure_evidence(self):
        original = os.pread

        def short_read(fd, length, address):
            raw = original(fd, length, address)
            if address == self.fixture.manager + 0xC8:
                return raw[:8]
            return raw

        with patch.object(MEMORY.os, 'pread', side_effect=short_read), self.assertRaisesRegex(
                ValueError, 'Short process memory read'):
            self.fixture.collect(self.output)
        reads = json.loads((self.output / 'memory-reads.json').read_text())
        self.assertEqual(reads[-1]['status'], 'failed')
        self.assertEqual(reads[-1]['bytes_received'], 8)
        self.assertEqual(len(bytes.fromhex(reads[-1]['raw_hex'])), 8)

    def test_permission_failure_retains_receipt_without_escalation(self):
        original_open = os.open

        def refuse_memory(path, *args, **kwargs):
            if Path(path) == self.fixture.mem_path:
                raise PermissionError('fixture denies read-only memory access')
            return original_open(path, *args, **kwargs)

        with patch.object(MEMORY.os, 'open', side_effect=refuse_memory), self.assertRaises(PermissionError):
            self.fixture.collect(self.output)
        receipt = json.loads((self.output / 'receipt.json').read_text())
        self.assertEqual(receipt['failure']['phase'], 'read-only-memory-open')
        self.assertEqual(receipt['memory_reads']['count'], 0)

    def test_existing_output_is_never_overwritten(self):
        self.output.mkdir()
        marker = self.output / 'keep.txt'
        marker.write_text('original')
        with self.assertRaises(FileExistsError):
            self.fixture.collect(self.output)
        self.assertEqual(list(self.output.iterdir()), [marker])
        self.assertEqual(marker.read_text(), 'original')

    def test_nonfinite_binary32_values_keep_raw_bits_and_valid_json(self):
        self.fixture.write(self.fixture.manager + 0xDC, bytes.fromhex('0100c07f'))
        result = self.fixture.collect(self.output)
        self.assertEqual(result['samples'][0]['manager']['resolved_base']['fields']['white'], 'NaN')
        self.assertNotIn(': NaN', (self.output / 'receipt.json').read_text())

class ParserTests(unittest.TestCase):
    """PE, maps and range parsers; these run on every platform."""

    def setUp(self):
        self.disk = synthetic_pe()
        self.pe = MEMORY.parse_pe(self.disk)

    def test_parse_pe_bounds_the_amd64_image(self):
        self.assertEqual(self.pe['preferred_base'], MEMORY.PREFERRED_BASE)
        self.assertEqual(self.pe['size_image'], 0x3700000)
        self.assertEqual(self.pe['size_headers'], 0x400)
        self.assertEqual([section['name'] for section in self.pe['sections']], ['.text'])
        with self.assertRaisesRegex(ValueError, 'DOS header'):
            MEMORY.parse_pe(b'')
        broken = bytearray(self.disk)
        struct.pack_into('<H', broken, 0x84, 0x14C)
        with self.assertRaisesRegex(ValueError, 'AMD64'):
            MEMORY.parse_pe(bytes(broken))

    def test_pe_file_range_maps_headers_and_section_bytes_once(self):
        self.assertEqual(MEMORY.pe_file_range(self.pe, MEMORY.PREFERRED_BASE, 0x10), 0)
        self.assertEqual(MEMORY.pe_file_range(
            self.pe, MEMORY.PREFERRED_BASE + CODE_RVA, 0x1000), 0x1000)
        with self.assertRaisesRegex(ValueError, 'map uniquely'):
            MEMORY.pe_file_range(self.pe, MEMORY.PREFERRED_BASE + 0x2000000, 0x10)
        with self.assertRaisesRegex(ValueError, 'Invalid PE requested range'):
            MEMORY.pe_file_range(self.pe, MEMORY.PREFERRED_BASE, 0)

    def test_parse_maps_decodes_escaped_paths_and_rejects_bad_bounds(self):
        mapping = MEMORY.parse_maps(
            '0000000000001000-0000000000002000 r-xp 00000000 00:01 7 /tmp/a\\040b\n')[0]
        self.assertEqual(mapping['path'], '/tmp/a b')
        self.assertEqual((mapping['start'], mapping['end']), (0x1000, 0x2000))
        self.assertEqual((mapping['device_major'], mapping['device_minor']), (0, 1))
        with self.assertRaisesRegex(ValueError, 'Malformed'):
            MEMORY.parse_maps('not a maps line\n')
        with self.assertRaisesRegex(ValueError, 'Overlapping'):
            MEMORY.parse_maps(
                '0000000000001000-0000000000002000 r-xp 00000000 00:01 7 /a\n'
                '0000000000001f00-0000000000003000 r-xp 00000000 00:01 7 /b\n')
        with self.assertRaisesRegex(ValueError, 'Empty'):
            MEMORY.parse_maps('')

    def test_identify_memory_headers_accepts_only_the_wine_imagebase_rewrite(self):
        field_offset = self.pe['pe_offset'] + 24 + 24
        expected = self.disk[:self.pe['size_headers']]
        identity = MEMORY.identify_memory_headers(expected, expected, self.pe, MEMORY.PREFERRED_BASE)
        self.assertEqual(identity['mode'], 'byte-exact')
        allowed = 0x6FFFF54F0000
        observed = bytearray(expected)
        struct.pack_into('<Q', observed, field_offset, allowed)
        identity = MEMORY.identify_memory_headers(expected, bytes(observed), self.pe, allowed)
        self.assertEqual(identity['mode'], 'wine-relocated-image-base')
        self.assertEqual(identity['allowed_loader_rewrite']['memory_value'], hex(allowed))
        self.assertEqual(identity['observed_byte_differences'][0]['file_offset'],
                         hex(field_offset + 2))
        observed[0] ^= 0x80
        with self.assertRaisesRegex(ValueError, 'PE headers'):
            MEMORY.identify_memory_headers(expected, bytes(observed), self.pe, allowed)
        with self.assertRaisesRegex(ValueError, 'Truncated in-memory PE headers'):
            MEMORY.identify_memory_headers(expected, expected[:-1], self.pe, allowed)

    def test_decode_base_reads_all_sixteen_fields_and_names_nonfinites(self):
        fields = MEMORY.decode_base(struct.pack('<16f', *range(1, 17)))
        self.assertEqual(fields['eye_adapt_speed'], 1)
        self.assertEqual(fields['white'], 6)
        self.assertEqual(fields['tint_amount'], 13)
        self.assertEqual(fields['tint_blue'], 16)
        with self.assertRaisesRegex(ValueError, 'exactly 0x40'):
            MEMORY.decode_base(bytes(0x3C))
        self.assertEqual(MEMORY.float32(bytes.fromhex('0100c07f')), 'NaN')

    def test_process_identity_reads_starttime_after_the_comm_parentheses(self):
        with tempfile.TemporaryDirectory() as temporary:
            stat_path = Path(temporary) / 'stat'
            fields = [b'S'] + [b'0'] * 18 + [b'987654321'] + [b'0'] * 10
            stat_path.write_bytes(b'123 (Skyrim (Main)) ' + b' '.join(fields) + b'\n')
            identity = MEMORY.process_identity(stat_path, 123)
        self.assertEqual(identity['starttime_ticks'], 987654321)
        self.assertEqual(identity['comm'], 'Skyrim (Main)')

    def test_memory_read_may_cross_only_adjacent_readable_mappings(self):
        reader = MEMORY.MemoryReader(-1, [
            {'start': 0x1000, 'end': 0x2000, 'permissions': 'r--p'},
            {'start': 0x2000, 'end': 0x3000, 'permissions': 'rw-p'}])
        reader.require_mapped(0x1FF0, 32)
        reader.mappings[1]['start'] += 1
        with self.assertRaisesRegex(ValueError, 'outside readable'):
            reader.require_mapped(0x1FF0, 32)


if __name__ == '__main__':
    unittest.main()
