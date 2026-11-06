#!/usr/bin/env python3
"""Check facts against an operator-owned Skyrim executable. No executable is bundled."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('executable', type=Path)
args = parser.parse_args()
data = args.executable.read_bytes()
expected_sha256 = '846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f'
sha256 = hashlib.sha256(data).hexdigest()
if sha256 != expected_sha256:
    parser.error('executable SHA256 does not match the inspected runtime')
pe = struct.unpack_from('<I', data, 0x3c)[0]
section_count = struct.unpack_from('<H', data, pe + 6)[0]
optional_size = struct.unpack_from('<H', data, pe + 20)[0]
image_base = struct.unpack_from('<Q', data, pe + 24 + 24)[0]
sections = [struct.unpack_from('<8sIIII', data, pe + 24 + optional_size + 40 * i)
            for i in range(section_count)]

def at(rva, size):
    for _, _, address, raw_size, raw in sections:
        if address <= rva and rva + size <= address + raw_size:
            return data[raw + rva - address:raw + rva - address + size]
    raise ValueError(f'RVA 0x{rva:x} is not file-backed')

constants = {'havok_to_game': (0x17f423c, '85fb8b42'),
             'game_to_havok': (0x17fde5c, '1e166a3c'),
             'fall_multiplier': (0x20d2558, 'cdccac3f'),
             'ordinary_multiplier': (0x1b5be28, '0000803f'),
             'launch_factor': (0x1b5be44, '00000040')}
result = {'executable_sha256': sha256, 'constants': {}, 'callbacks': {}, 'checks': []}
for name, (rva, expected) in constants.items():
    actual = at(rva, 4)
    if actual.hex() != expected:
        raise ValueError(f'{name} changed')
    result['constants'][name] = {'rva': hex(rva), 'float32': struct.unpack('<f', actual)[0]}
for name, slot, target in [('air', 0x1a92608, 0x10c7cc0), ('launch', 0x1a92680, 0x10c8030)]:
    if struct.unpack('<Q', at(slot, 8))[0] != image_base + target:
        raise ValueError(f'{name} callback changed')
    result['callbacks'][name] = {'slot_rva': hex(slot), 'function_rva': hex(target)}
for name, rva, expected in [
    ('height comparison', 0x10c7ee3, '0f2f9740020000760a'),
    ('gravity scale read', 0x10c7f1d, 'f30f59b748020000'),
    ('controller timestep read', 0x10c7f25, 'f30f108788000000'),
    ('launch height read', 0x10c80bb, 'f30f10b73c020000'),
    ('launch square root', 0x10c815e, 'f30f51c6'),
    ('actor process read', 0x66ffa7, '488b81f8000000'),
    ('jump setting load', 0x7cf220, 'f30f103538978d01'),
    ('jump setting times reference scale', 0x7cf23c, 'f30f59fe'),
    ('jump height conversion', 0x10645e0, 'f30f590d74987900'),
    ('jump height store', 0x10645f2, 'f30f11893c020000'),
    ('reference scale read', 0x2e6f34, '0fb78198000000'),
    ('NPC base height times race height', 0x3c65f6, 'f30f5983f8010000')]:
    if at(rva, len(bytes.fromhex(expected))).hex() != expected:
        raise ValueError(f'{name} changed')
    result['checks'].append({'fact': name, 'rva': hex(rva), 'verified': True})
result['scope'] = 'Static identity and field-consumer evidence. Runtime trajectories and scheduling are not verified.'
name_rva = struct.unpack('<Q', at(0x20a8968, 8))[0] - image_base
if at(name_rva, 15) != b'fJumpHeightMin\0':
    raise ValueError('jump setting name changed')
result['jump_height_setting'] = {'name': 'fJumpHeightMin', 'value_rva': '0x20a8960',
    'file_initializer_only': struct.unpack('<f', at(0x20a8960, 4))[0],
    'path': 'setting times reference scale, with NPC/race height and conditional swimming factor, then game-to-Havok conversion',
    'limitation': 'The runtime setting value and individual scale inputs were not captured.'}
print(json.dumps(result, indent=2))
