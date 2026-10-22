#!/usr/bin/env python3
"""Collect bounded, read-only tone-map evidence from an existing Linux process.

This pinned-build collector uses only /proc reads and pread on /proc/PID/mem.
It never attaches a debugger, stops a process, changes permissions or settings,
or starts a game. Output belongs in a fresh directory. Two sequential samples
are evidence of observed CPU memory, never of a particular GPU frame or image.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import struct
import sys
import time


EXPECTED_EXE_SHA256 = '846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f'
PREFERRED_BASE = 0x140000000
HDR_FUNCTION_START = 0x14153D860
HDR_FUNCTION_END_INCLUSIVE = 0x14153DC2E
MANAGER_SINGLETON = 0x1433D40A0
HDR_VTABLE = 0x141B443C8
PARAMETER_VTABLE = 0x141B40A38
WINE_IMAGEBASE_WRITE_URL = (
    'https://github.com/wine-mirror/wine/blob/5b6b85421c2a2161728b2a55e9f09271770a3eb0/'
    'dlls/ntdll/unix/virtual.c#L2838-L2841')
SETTINGS = (
    ('bUseFilmicCurve:Display', 0x1420D7DC8, 'byte'),
    ('fReinhardWhiteScale:Display', 0x1420D7DE0, 'float32'),
    ('fFilmicWhiteScale:Display', 0x1420D7DF8, 'float32'),
    ('fGlobalEyeAdaptSpeedScale:Display', 0x1420D7D80, 'float32'),
    ('fGlobalEyeAdaptStrengthScale:Display', 0x1420D7D98, 'float32'),
    ('fConstHDRAdaptTimerForMenu:Display', 0x1420D7D68, 'float32'),
    ('fGamma:Display', 0x1420D0348, 'float32'),
)
BASE_FIELDS = (
    ('eye_adapt_speed', 0x00), ('bloom_blur_radius', 0x04),
    ('bloom_threshold', 0x08), ('bloom_scale', 0x0C),
    ('receive_bloom_threshold', 0x10), ('white', 0x14),
    ('sunlight_scale', 0x18), ('sky_scale', 0x1C),
    ('eye_adapt_strength', 0x20), ('saturation', 0x24),
    ('brightness', 0x28), ('contrast', 0x2C),
    ('tint_amount', 0x30), ('tint_red', 0x34),
    ('tint_green', 0x38), ('tint_blue', 0x3C),
)
LIMITATIONS = [
    'Reads are sequential and do not stop or synchronize the target process.',
    'Equal double reads do not establish an atomic sample or a single GPU frame.',
    'These samples do not establish settings of an earlier reference screenshot.',
    'CPU parameter contents do not prove a matching active draw or GPU upload.',
    'Image-space records, weather identity, GPU textures and adaptation history are not read.',
    'Only the first 0x40 bytes of base image-space data are decoded; depth of field is omitted.',
    'A pinned executable and unchanged HDRParameters do not exclude hooks elsewhere in the process.',
]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def json_float(value):
    """Keep JSON standards-compliant while retaining nonfinite float identity."""
    if math.isfinite(value):
        return value
    return 'NaN' if math.isnan(value) else ('Infinity' if value > 0 else '-Infinity')


def float32(data, offset=0):
    return json_float(struct.unpack_from('<f', data, offset)[0])


def parse_pe(data):
    require(len(data) >= 0x40 and data[:2] == b'MZ', 'Executable lacks DOS header')
    pe = struct.unpack_from('<I', data, 0x3C)[0]
    require(0x40 <= pe <= len(data) - 24 and data[pe:pe + 4] == b'PE\0\0',
            'Executable lacks bounded PE header')
    machine, count, timestamp = struct.unpack_from('<HHI', data, pe + 4)
    optional_size = struct.unpack_from('<H', data, pe + 20)[0]
    optional = pe + 24
    require(machine == 0x8664 and 0 < count <= 96, 'Expected AMD64 PE sections')
    require(optional_size >= 0x70 and optional + optional_size + 40 * count <= len(data),
            'Truncated PE optional header or sections')
    require(struct.unpack_from('<H', data, optional)[0] == 0x20B, 'Expected PE32+ image')
    preferred = struct.unpack_from('<Q', data, optional + 24)[0]
    size_image, size_headers = struct.unpack_from('<II', data, optional + 56)
    require(preferred == PREFERRED_BASE, 'Unexpected PE preferred image base')
    require(optional + optional_size + 40 * count <= size_headers <= min(len(data), 0x400000),
            'Invalid PE SizeOfHeaders')
    require(size_headers <= size_image < 0x80000000, 'Invalid PE SizeOfImage')
    sections = []
    for index in range(count):
        position = optional + optional_size + index * 40
        virtual_size, rva, raw_size, raw_offset = struct.unpack_from('<IIII', data, position + 8)
        characteristics = struct.unpack_from('<I', data, position + 36)[0]
        require(rva + max(virtual_size, raw_size) <= size_image, 'PE section exceeds image')
        require(raw_offset + raw_size <= len(data), 'PE section exceeds disk file')
        sections.append({'name': data[position:position + 8].rstrip(b'\0').decode('ascii', 'replace'),
                         'rva': rva, 'virtual_size': virtual_size, 'raw_offset': raw_offset,
                         'raw_size': raw_size, 'characteristics': characteristics})
    return {'preferred_base': preferred, 'timestamp': timestamp,
            'size_image': size_image, 'size_headers': size_headers,
            'pe_offset': pe, 'sections': sections}


def pe_file_range(pe, preferred_address, length):
    rva = preferred_address - pe['preferred_base']
    require(rva >= 0 and length > 0, 'Invalid PE requested range')
    if rva + length <= pe['size_headers']:
        return rva
    matches = [section for section in pe['sections']
               if section['rva'] <= rva and rva + length <= section['rva'] + section['raw_size']]
    require(len(matches) == 1, 'Requested PE range does not map uniquely to disk bytes')
    return matches[0]['raw_offset'] + rva - matches[0]['rva']


def identify_memory_headers(disk, observed, pe, actual_base):
    """Permit only Wine's exact ImageBase rewrite to this mapping's address.

    Wine's loader assigns OptionalHeader.ImageBase = image_info->map_addr.
    Every other header byte must still equal the pinned file, including the
    timestamp, image size, section table, and optional-header shape.
    """
    expected = disk[:pe['size_headers']]
    require(len(observed) == len(expected), 'Truncated in-memory PE headers')
    offset = pe['pe_offset'] + 24 + 24
    relocated = bytearray(expected)
    struct.pack_into('<Q', relocated, offset, actual_base)
    require(observed == expected or observed == relocated,
            'PE headers differ outside the exact Wine ImageBase rewrite')
    differences = [{'file_offset': hex(index), 'disk_byte': f'{left:02x}', 'memory_byte': f'{right:02x}'}
                   for index, (left, right) in enumerate(zip(expected, observed)) if left != right]
    return {'mode': 'wine-relocated-image-base' if differences else 'byte-exact',
            'disk_sha256': sha256(expected), 'memory_sha256': sha256(observed),
            'observed_byte_differences': differences,
            'allowed_loader_rewrite': {'field': 'PE32+ OptionalHeader.ImageBase',
                                       'file_offset': hex(offset), 'size_bytes': 8,
                                       'disk_value': hex(struct.unpack_from('<Q', expected, offset)[0]),
                                       'memory_value': hex(struct.unpack_from('<Q', observed, offset)[0]),
                                       'required_replacement_value': hex(actual_base),
                                       'source': WINE_IMAGEBASE_WRITE_URL}}


def parse_maps(text):
    mappings = []
    pattern = re.compile(r'^([0-9a-f]+)-([0-9a-f]+) ([rwxps-]{4}) '
                         r'([0-9a-f]+) ([0-9a-f]+):([0-9a-f]+) (\d+)(?: +(.*))?$')
    for line in text.splitlines():
        match = pattern.fullmatch(line)
        require(match is not None, 'Malformed /proc maps line')
        start, end, permissions, offset, major, minor, inode, path = match.groups()
        entry = {'start': int(start, 16), 'end': int(end, 16), 'permissions': permissions,
                 'offset': int(offset, 16), 'device_major': int(major, 16),
                 'device_minor': int(minor, 16), 'inode': int(inode),
                 'path': re.sub(r'\\([0-7]{3})', lambda m: chr(int(m[1], 8)), path or '')}
        require(entry['start'] < entry['end'], 'Invalid /proc mapping bounds')
        require(not mappings or mappings[-1]['end'] <= entry['start'], 'Overlapping /proc mappings')
        mappings.append(entry)
    require(mappings, 'Empty /proc maps')
    return mappings


def process_identity(path, pid):
    raw = path.read_bytes()
    require(len(raw) < 65536, 'Oversized /proc stat')
    closing = raw.rfind(b')')
    require(raw.startswith(str(pid).encode() + b' (') and closing > 0, 'Unexpected /proc stat PID')
    fields = raw[closing + 1:].split()
    require(len(fields) >= 20, 'Truncated /proc stat')
    require(fields[19].isdigit(), 'Invalid process starttime')
    return {'pid': pid, 'starttime_ticks': int(fields[19]),
            'comm': raw[raw.find(b'(') + 1:closing].decode(errors='replace')}


class MemoryReader:
    def __init__(self, fd, mappings):
        self.fd = fd
        self.mappings = mappings
        self.reads = []

    def require_mapped(self, address, length, executable=False):
        require(0 < address < 1 << 64 and 0 < length <= 0x400000,
                'Invalid or oversized memory read')
        end, current = address + length, address
        require(end <= 1 << 64, 'Memory read overflows address space')
        for entry in self.mappings:
            if entry['start'] <= current < entry['end']:
                require(entry['permissions'][0] == 'r', 'Memory range is not readable')
                require(not executable or entry['permissions'][2] == 'x',
                        'Code range is not executable')
                current = min(end, entry['end'])
                if current == end:
                    return
            elif entry['start'] > current:
                break
        raise ValueError('Memory range is outside readable process mappings')

    def read(self, label, address, length, executable=False):
        entry = {'label': label, 'address': hex(address), 'size_bytes': length,
                 'started_utc': utc_now(), 'started_monotonic_ns': time.monotonic_ns()}
        try:
            self.require_mapped(address, length, executable)
            data = os.pread(self.fd, length, address)
            entry.update(raw_hex=data.hex(), sha256=sha256(data), bytes_received=len(data))
            require(len(data) == length, 'Short process memory read: ' + label)
            entry['status'] = 'complete'
            return data
        except Exception as error:
            entry.update(status='failed', error={'type': type(error).__name__, 'message': str(error)})
            raise
        finally:
            entry.update(finished_utc=utc_now(), finished_monotonic_ns=time.monotonic_ns())
            self.reads.append(entry)


def bind_image(reader, mappings, executable, file_stat, disk, pe):
    candidates = [entry for entry in mappings
                  if entry['path'] == str(executable) and entry['offset'] == 0
                  and entry['inode'] == file_stat.st_ino
                  and entry['device_major'] == os.major(file_stat.st_dev)
                  and entry['device_minor'] == os.minor(file_stat.st_dev)]
    require(candidates, 'No exact executable file identity mapping at offset zero')
    matches = []
    for entry in candidates:
        base = entry['start']
        try:
            header = reader.read('identity.pe_headers.' + hex(base), base, pe['size_headers'])
            identity = identify_memory_headers(disk, header, pe, base)
            matches.append((base, identity))
        except (OSError, ValueError):
            continue
    require(len(matches) == 1, 'Memory PE headers do not identify one exact mapped executable')
    base, header_identity = matches[0]
    length = HDR_FUNCTION_END_INCLUSIVE - HDR_FUNCTION_START + 1
    offset = pe_file_range(pe, HDR_FUNCTION_START, length)
    code_rva = HDR_FUNCTION_START - pe['preferred_base']
    require(any(section['rva'] <= code_rva < section['rva'] + section['raw_size']
                and section['characteristics'] & 0x20000000 for section in pe['sections']),
            'HDRParameters is not in an executable PE section')
    address = base + HDR_FUNCTION_START - pe['preferred_base']
    code = reader.read('identity.HDRParameters', address, length, executable=True)
    require(code == disk[offset:offset + length], 'HDRParameters memory bytes differ from pinned executable')
    return base, {'actual_base': hex(base), 'preferred_base': hex(pe['preferred_base']),
                  'relocation_delta': base - pe['preferred_base'],
                  'timestamp': pe['timestamp'], 'size_image': pe['size_image'],
                  'headers_sha256': sha256(disk[:pe['size_headers']]),
                  'header_identity': header_identity,
                  'HDRParameters': {'preferred_start': hex(HDR_FUNCTION_START),
                                    'preferred_end_inclusive': hex(HDR_FUNCTION_END_INCLUSIVE),
                                    'memory_address': hex(address), 'file_offset': offset,
                                    'size_bytes': length, 'sha256': sha256(code),
                                    'disk_memory_equal': True}}


def decode_base(data):
    require(len(data) == 0x40, 'Base image-space data must be exactly 0x40 bytes')
    return {name: float32(data, offset) for name, offset in BASE_FIELDS}


def sample(reader, base, pe, number):
    prefix = 'sample-' + str(number) + '.'
    guards = []

    def read(label, address, length):
        return reader.read(prefix + label, address, length)

    def pointer(label, address, allow_null=True, alignment=8):
        raw = read(label, address, 8)
        value = struct.unpack('<Q', raw)[0]
        require(allow_null or value != 0, 'Required pointer is null: ' + label)
        require(value == 0 or value % alignment == 0, 'Unaligned pointer: ' + label)
        guards.append((label, address, raw))
        return value

    def relocate(address):
        require(PREFERRED_BASE <= address < PREFERRED_BASE + pe['size_image'],
                'Pinned address exceeds PE image')
        return base + address - PREFERRED_BASE

    def base_data(label, address):
        if address == 0:
            return None
        raw = read(label, address, 0x40)
        return {'address': hex(address), 'sha256': sha256(raw), 'fields': decode_base(raw)}

    started, before_ns = utc_now(), time.monotonic_ns()
    result = {'number': number, 'started_utc': started, 'started_monotonic_ns': before_ns,
              'settings': {}}
    for name, preferred, kind in SETTINGS:
        address = relocate(preferred)
        value = read('setting.' + name + '.value', address, 1 if kind == 'byte' else 4)
        name_pointer = pointer('setting.' + name + '.name_pointer', address + 8,
                               allow_null=False, alignment=1)
        expected_name = name.encode('ascii') + b'\0'
        require(base <= name_pointer and name_pointer + len(expected_name) <= base + pe['size_image'],
                'Setting name pointer escapes image')
        require(read('setting.' + name + '.name', name_pointer, len(expected_name)) == expected_name,
                'In-memory setting name mismatch: ' + name)
        result['settings'][name] = {'value': value[0] if kind == 'byte' else float32(value),
                                    'type': kind, 'address': hex(address), 'preferred_address': hex(preferred),
                                    'name_pointer': hex(name_pointer), 'raw_hex': value.hex()}

    manager = pointer('manager.singleton', relocate(MANAGER_SINGLETON), allow_null=False)
    current = pointer('manager.current_base', manager + 0xA8)
    override = pointer('manager.override_base', manager + 0xB0)
    fade_raw = read('manager.fade_predicate', manager + 0x118, 4)
    guards.append(('manager.fade_predicate', manager + 0x118, fade_raw))
    fade = float32(fade_raw)
    result['manager'] = {'address': hex(manager), 'current_base_pointer': hex(current),
                         'override_base_pointer': hex(override),
                         'current_base': base_data('manager.current_base.data', current),
                         'override_base': base_data('manager.override_base.data', override),
                         'resolved_base': base_data('manager.resolved_base.data', manager + 0xC8),
                         'fade_predicate_float': fade,
                         'selected_curve_parameter': 'fade' if isinstance(fade, (int, float)) and fade > 0
                         else ('ordinary' if isinstance(fade, (int, float)) else 'unresolved-nonfinite')}

    effects = pointer('manager.effects', manager + 0x28)
    result['parameters'] = {'effects_pointer': hex(effects), 'buffers': {}}
    if effects:
        hdr = pointer('effects.hdr_slot_30', effects + 30 * 8)
        result['parameters']['hdr_pointer'] = hex(hdr)
        if hdr:
            vtable = pointer('hdr.vtable', hdr, allow_null=False)
            require(vtable == relocate(HDR_VTABLE), 'Unexpected HDR object vtable')
            parameters = pointer('hdr.parameter_array', hdr + 0x30)
            result['parameters']['parameter_array_pointer'] = hex(parameters)
            if parameters:
                for name, slot, indices in (('ordinary', 2, (1, 2, 3)), ('fade', 3, (1, 2, 3)),
                                             ('adaptation', 8, (2,))):
                    parameter = pointer('parameters.' + name, parameters + slot * 8)
                    if not parameter:
                        result['parameters']['buffers'][name] = None
                        continue
                    vtable = pointer('parameters.' + name + '.vtable', parameter, allow_null=False)
                    require(vtable == relocate(PARAMETER_VTABLE), 'Unexpected parameter object vtable')
                    array = pointer('parameters.' + name + '.float4_array', parameter + 0x40,
                                    allow_null=False)
                    count_raw = read('parameters.' + name + '.float4_count', parameter + 0x48, 4)
                    guards.append(('parameters.' + name + '.float4_count', parameter + 0x48, count_raw))
                    count = struct.unpack('<I', count_raw)[0]
                    require(max(indices) < count <= 1024, 'Invalid parameter float4 count: ' + name)
                    tuples = {}
                    for index in indices:
                        raw = read('parameters.' + name + '.float4_' + str(index), array + 16 * index, 16)
                        tuples[str(index)] = [float32(raw, component * 4) for component in range(4)]
                    result['parameters']['buffers'][name] = {
                        'parameter_pointer': hex(parameter), 'float4_array_pointer': hex(array),
                        'float4_count': count, 'float4_values_by_cpu_index': tuples}

    changed = []
    for label, address, raw in guards:
        if read('guard.' + label, address, len(raw)) != raw:
            changed.append(label)
    result.update(pointer_and_selector_guards_consistent=not changed, changed_guards=changed,
                  finished_utc=utc_now(), finished_monotonic_ns=time.monotonic_ns())
    return result


def sample_payload(value):
    return {key: value[key] for key in ('settings', 'manager', 'parameters')}


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, indent=2, allow_nan=False) + '\n')
    os.replace(temporary, path)


def stat_identity(value):
    return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)


def collect(pid, executable, output, proc_root=Path('/proc')):
    require(isinstance(pid, int) and pid > 0, 'PID must be positive')
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    receipt = {'schema_version': 1, 'status': 'collecting', 'pid': pid,
               'started_utc': utc_now(), 'started_monotonic_ns': time.monotonic_ns(),
               'access': 'O_RDONLY plus pread; no attach, stop, write, or permission changes',
               'limitations': LIMITATIONS, 'samples': []}
    reader, executable_fd, memory_fd = None, None, None
    phase = 'source'
    try:
        script = Path(__file__).resolve()
        receipt['collector'] = {'path': str(script), 'sha256': sha256(script.read_bytes())}
        executable = Path(executable).resolve(strict=True)
        flags = os.O_RDONLY | getattr(os, 'O_CLOEXEC', 0) | getattr(os, 'O_NOFOLLOW', 0)
        executable_fd = os.open(executable, flags)
        before_file = os.fstat(executable_fd)
        require(stat.S_ISREG(before_file.st_mode), 'Executable is not a regular file')
        with os.fdopen(os.dup(executable_fd), 'rb') as source:
            disk = source.read()
        digest = sha256(disk)
        receipt['executable'] = {'path': str(executable), 'sha256': digest,
                                  'size_bytes': len(disk), 'device': before_file.st_dev,
                                  'inode': before_file.st_ino}
        require(digest == EXPECTED_EXE_SHA256, 'Executable hash differs from pinned retail build')
        pe = parse_pe(disk)
        phase = 'process-identity'
        directory = Path(proc_root) / str(pid)
        identity = process_identity(directory / 'stat', pid)
        receipt['process_identity_before'] = identity
        maps_before = (directory / 'maps').read_text()
        (output / 'maps-before.txt').write_text(maps_before)
        mappings = parse_maps(maps_before)
        phase = 'read-only-memory-open'
        memory_fd = os.open(directory / 'mem', flags)
        reader = MemoryReader(memory_fd, mappings)
        phase = 'image-identity'
        base, image = bind_image(reader, mappings, executable, before_file, disk, pe)
        receipt['image'] = image
        phase = 'bounded-samples'
        for number in (1, 2):
            require(process_identity(directory / 'stat', pid) == identity, 'Process identity changed before sample')
            receipt['samples'].append(sample(reader, base, pe, number))
        phase = 'final-identity'
        final_identity = process_identity(directory / 'stat', pid)
        receipt['process_identity_after'] = final_identity
        require(final_identity == identity, 'Process identity changed during collection')
        maps_after = (directory / 'maps').read_text()
        (output / 'maps-after.txt').write_text(maps_after)
        final_mappings = parse_maps(maps_after)
        # Unrelated heap/thread mappings may change. Every sampled byte must still
        # have the same mapping identity, permissions, and bounds afterward.
        for item in reader.reads:
            address, length = int(item['address'], 16), item['size_bytes']
            before_cover = [entry for entry in mappings
                            if entry['start'] < address + length and entry['end'] > address]
            after_cover = [entry for entry in final_mappings
                           if entry['start'] < address + length and entry['end'] > address]
            require(before_cover == after_cover, 'A sampled memory mapping changed during collection')
        require(stat_identity(os.fstat(executable_fd)) == stat_identity(before_file),
                'Executable file identity changed during collection')
        # Recheck the exact source path; replacement must not pass via the old FD.
        require(stat_identity(executable.stat()) == stat_identity(before_file),
                'Executable path identity changed during collection')
        base_after, image_after = bind_image(reader, final_mappings, executable, before_file, disk, pe)
        require(base_after == base and image_after == image, 'Executable image changed during collection')
        receipt['double_read'] = {
            'payloads_equal': sample_payload(receipt['samples'][0]) == sample_payload(receipt['samples'][1]),
            'all_pointer_and_selector_guards_consistent': all(
                item['pointer_and_selector_guards_consistent'] for item in receipt['samples']),
            'meaning': 'Two sequential CPU-memory samples; equality does not imply frame synchronization.'}
        receipt['maps'] = {'before_sha256': sha256(maps_before.encode()),
                           'after_sha256': sha256(maps_after.encode()),
                           'all_sampled_mappings_unchanged': True}
        receipt['status'] = 'complete'
        return receipt
    except Exception as error:
        receipt.update(status='failed', failure={'phase': phase, 'type': type(error).__name__, 'message': str(error)})
        raise
    finally:
        if memory_fd is not None:
            os.close(memory_fd)
        if executable_fd is not None:
            os.close(executable_fd)
        reads = reader.reads if reader is not None else []
        write_json(output / 'memory-reads.json', reads)
        receipt.update(finished_utc=utc_now(), finished_monotonic_ns=time.monotonic_ns(),
                       memory_reads={'file': 'memory-reads.json', 'count': len(reads),
                                     'sha256': sha256((output / 'memory-reads.json').read_bytes())})
        write_json(output / 'receipt.json', receipt)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pid', required=True, type=int, help='Existing SkyrimSE process PID')
    parser.add_argument('--exe', required=True, type=Path, help='Exact mapped SkyrimSE.exe')
    parser.add_argument('--output', required=True, type=Path, help='Fresh evidence directory')
    arguments = parser.parse_args()
    try:
        result = collect(arguments.pid, arguments.exe, arguments.output)
    except Exception as error:
        print(type(error).__name__ + ': ' + str(error), file=sys.stderr)
        return 1
    print(json.dumps({'status': result['status'], 'receipt': str(arguments.output / 'receipt.json'),
                      'double_read': result['double_read']}, allow_nan=False))
    return 0


if __name__ == '__main__':
    sys.exit(main())
