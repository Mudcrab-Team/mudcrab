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

constants = {'havok_to_game': (0x17f423c, {'length': 4, 'sha256': '1c2665f1d14ba95cc9e76c55515d3bf755c024c6e2eac29525e9b75113fd7251'}),
             'game_to_havok': (0x17fde5c, {'length': 4, 'sha256': 'b5db0bd9d29d70e88a2fea7f30e0f0d7e61c5fc45ea035a3a16d5f6fb9de6816'}),
             'fall_multiplier': (0x20d2558, {'length': 4, 'sha256': 'a20789a7f63f03f8e14941bd6d57167bb5e9243bfd9536babfb0062869a58fe6'}),
             'ordinary_multiplier': (0x1b5be28, {'length': 4, 'sha256': 'e00e5eb9444182f352323374ef4e08ebcb784725fdd4fd612d7730540b3e0c8c'}),
             'launch_factor': (0x1b5be44, {'length': 4, 'sha256': 'd88c86f15bbea365d658ad95a81d45367c465f7af6f7264fb077f01747ddc77d'})}
result = {'executable_sha256': sha256, 'constants': {}, 'callbacks': {}, 'checks': []}
for name, (rva, expected) in constants.items():
    actual = at(rva, 4)
    if hashlib.sha256(actual).hexdigest() != expected['sha256']:
        raise ValueError(f'{name} changed')
    result['constants'][name] = {'rva': hex(rva), 'float32': struct.unpack('<f', actual)[0]}
for name, slot, target in [('air', 0x1a92608, 0x10c7cc0), ('launch', 0x1a92680, 0x10c8030)]:
    if struct.unpack('<Q', at(slot, 8))[0] != image_base + target:
        raise ValueError(f'{name} callback changed')
    result['callbacks'][name] = {'slot_rva': hex(slot), 'function_rva': hex(target)}
for name, rva, expected in [
    ('height comparison', 0x10c7ee3, {'length': 9, 'sha256': '4d04b53ad68a23c90ab519e1ed849d9a453d905e5df478f02e7138789843b52e'}),
    ('gravity scale read', 0x10c7f1d, {'length': 8, 'sha256': '0ca04ef5f5152f567cf2ffea09aa2fc7138ea360b442df964e1fcfc30839ecda'}),
    ('controller timestep read', 0x10c7f25, {'length': 8, 'sha256': 'cebeaeba07b2131e448571dd03c8e63f241df1f8568d0493bccfd604bd3728d0'}),
    ('launch height read', 0x10c80bb, {'length': 8, 'sha256': '458ed3bf3f977a84af6dd1db33564244d6b2ba45537d0d88e0093cd962cf9d86'}),
    ('launch square root', 0x10c815e, {'length': 4, 'sha256': '821555acccc2b7a821afd93342a60fbeb172b59fe46e2008bd431121d776fc60'}),
    ('actor process read', 0x66ffa7, {'length': 7, 'sha256': '143456023a8a99c1bb4edf4d7d841f5a655bfbd9973a08ec35a5e895eb89863c'}),
    ('jump setting load', 0x7cf220, {'length': 8, 'sha256': '7eaad574c1695b0884a0bd3038e4b37df943948ee09e4db85f4174300c84676f'}),
    ('jump setting times reference scale', 0x7cf23c, {'length': 4, 'sha256': 'dcad82a963e26da298728203730c620921876e02ce3469bb2ca4765e75024de9'}),
    ('jump height conversion', 0x10645e0, {'length': 8, 'sha256': '0d58c7296249f69d00b1a3010ca7e99233aeb2e687b39e6f15195b4c50c5a35b'}),
    ('jump height store', 0x10645f2, {'length': 8, 'sha256': 'daa3b96d090852a1ef6cfce25d298af44019094238d96efdf54f81d4ae6305ef'}),
    ('reference scale read', 0x2e6f34, {'length': 7, 'sha256': '1448e787ff803019c5ce61e29e61bdb9678a5fb13682e5706ad6d4a1f8297665'}),
    ('NPC base height times race height', 0x3c65f6, {'length': 8, 'sha256': '0def8ddf96deb43c6be678b95d826d6a1d542beee5da936d141d896cbab3f1db'})]:
    if hashlib.sha256(at(rva, expected['length'])).hexdigest() != expected['sha256']:
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
