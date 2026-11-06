#!/usr/bin/env python3
"""Independently check v4 jump CALL ABI and raw-field consumers in the pinned PE."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

RUNTIME = '846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f'
CHECKS = [
    ('player_setter_call', 0x6752ba, 'e821f39e00'),
    ('player_setting_load', 0x675284, 'f30f1035d436a301'),
    ('player_scale_call', 0x67528f, 'e89c1cc7ff'),
    ('player_product', 0x675297, 'f30f59fe'),
    ('setter_call', 0x7cf269, 'e872538900'),
    ('setter_rcx_xmm1_leaf', 0x10645e0, 'f30f590d74987900c7811c02000001000000f30f11893c020000c3'),
    ('jump_setting_load', 0x7cf220, 'f30f103538978d01'),
    ('reference_scale_call', 0x7cf22d, 'e8fe7cb1ff'),
    ('reference_unsigned_short', 0x2e6f34, '0fb78198000000'),
    ('npc_type_guard', 0x2e6f58, '80791a2b7509'),
    ('race_pointer', 0x3c65ca, '488bb958010000'),
    ('sex_index', 0x3afeb0, '80791a2b75080fb6413883e001c3'),
    ('race_height_pair', 0x3c65ed, 'f30f108487f8000000'),
    ('npc_height_product', 0x3c65f6, 'f30f5983f8010000'),
]


def inspect(executable):
    data = executable.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != RUNTIME:
        raise ValueError('Unsupported executable identity')
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    count = struct.unpack_from('<H', data, pe + 6)[0]
    optional = struct.unpack_from('<H', data, pe + 20)[0]
    sections = [struct.unpack_from('<8sIIII', data, pe + 24 + optional + i * 40)
                for i in range(count)]

    def at(rva, length):
        for _, _, address, size, raw in sections:
            if address <= rva and rva + length <= address + size:
                return data[raw + rva - address:raw + rva - address + length]
        raise ValueError('Unmapped native consumer')

    checks = []
    for name, rva, signature in CHECKS:
        if at(rva, len(bytes.fromhex(signature))).hex() != signature:
            raise ValueError(f'{name} consumer differs')
        checks.append({'fact': name, 'rva': hex(rva), 'verified': True})
    target = 0x7cf269 + 5 + struct.unpack('<i', at(0x7cf26a, 4))[0]
    if target != 0x10645e0:
        raise ValueError('Setter target differs')
    constants = {name: struct.unpack('<f', at(rva, 4))[0] for name, rva in
                 [('reference_unit', 0x1b5bdd0), ('game_to_havok', 0x17fde5c)]}
    return {'executable_sha256': digest, 'checks': checks, 'setter_target_rva': hex(target),
            'abi': 'Windows x64 void(controller* RCX, float XMM1)', 'constants': constants,
            'scope': 'Static consumers and ABI only. Live values require separate v4 captures.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('executable', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    output = json.dumps(inspect(args.executable), indent=2) + '\n'
    if args.output:
        args.output.write_text(output)
    print(output, end='')


if __name__ == '__main__':
    main()
