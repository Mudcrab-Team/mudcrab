#!/usr/bin/env python3
"""Independently check v4 jump CALL ABI and raw-field consumers in the pinned PE."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

RUNTIME = '846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f'
CHECKS = [
    ('player_setter_call', 0x6752ba, {'length': 5, 'sha256': '1ded3bc87802bcddb4ac1b0729d9a60bbc81965bbdb7136c1d112387292b958a'}),
    ('player_setting_load', 0x675284, {'length': 8, 'sha256': '7afee74de8cca47abb9588a96555c574aa6b5872a3b74500631fc0bcc275aa41'}),
    ('player_scale_call', 0x67528f, {'length': 5, 'sha256': 'e1a65e02582a2dc1195f382e89c24e5ff239b0e0657ac47cc188d9e8aef559e2'}),
    ('player_product', 0x675297, {'length': 4, 'sha256': 'dcad82a963e26da298728203730c620921876e02ce3469bb2ca4765e75024de9'}),
    ('setter_call', 0x7cf269, {'length': 5, 'sha256': '0935b812621c725fa490d634f50ee4b8654fd2d2f4962d2fac9c401962aad45d'}),
    ('setter_rcx_xmm1_leaf', 0x10645e0, {'length': 27, 'sha256': '2a645d3496101be8ecf73b69a12625bedb045db671eaabf466db5c4a87308381'}),
    ('jump_setting_load', 0x7cf220, {'length': 8, 'sha256': '7eaad574c1695b0884a0bd3038e4b37df943948ee09e4db85f4174300c84676f'}),
    ('reference_scale_call', 0x7cf22d, {'length': 5, 'sha256': 'fea169ca3ce720f8d20da798512178a53d422ad1bced222bf1af63b7eb8cc70b'}),
    ('reference_unsigned_short', 0x2e6f34, {'length': 7, 'sha256': '1448e787ff803019c5ce61e29e61bdb9678a5fb13682e5706ad6d4a1f8297665'}),
    ('npc_type_guard', 0x2e6f58, {'length': 6, 'sha256': '6419b6a4c0db2f58e7ef6a33303776ffe56d91eeca9087289ba184bd6773280d'}),
    ('race_pointer', 0x3c65ca, {'length': 7, 'sha256': 'b6c5d742eae6fbaaf59038721fd8040cae65c2ed066446e6c1ef3736bb0b33db'}),
    ('sex_index', 0x3afeb0, {'length': 14, 'sha256': 'df5ff524aeeb67fbe6399b7b918fa643b5112858b17c119bc03aa744c7453f88'}),
    ('race_height_pair', 0x3c65ed, {'length': 9, 'sha256': '8712e0203c88057abc65c090c1488cc13c24c7650e6b27f5a3c900a02313377b'}),
    ('npc_height_product', 0x3c65f6, {'length': 8, 'sha256': '0def8ddf96deb43c6be678b95d826d6a1d542beee5da936d141d896cbab3f1db'}),
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
        if hashlib.sha256(at(rva, signature['length'])).hexdigest() != signature['sha256']:
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
