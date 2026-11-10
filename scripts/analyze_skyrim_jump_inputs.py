#!/usr/bin/env python3
"""Check v4 native jump inputs against the independently observed setter and launch."""
import argparse
import gzip
import hashlib
import json
import math
from pathlib import Path
import struct

from record_skyrim_motion import RECORD, verify_trace

HEADER = 'mudcrab-jump-inputs-256-v4'
SETTER_REQUIRED = sum((1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536))
LAUNCH_REQUIRED = SETTER_REQUIRED & ~(128 | 512)


def f32(value):
    return struct.unpack('<f', struct.pack('<f', value))[0]


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def decode(path):
    raw = gzip.decompress(path.read_bytes()) if path.suffix == '.gz' else path.read_bytes()
    require(raw and len(raw) % RECORD.size == 0, 'Truncated v4 trace')
    rows = []
    for v in RECORD.iter_unpack(raw):
        rows.append(dict(sequence=v[0], qpc=v[1], sample=v[2], input_sequence=v[3],
                         kind=v[4], thread=v[5], code=v[6], state=v[7],
                         values=list(v[8:48]), text=v[48].split(b'\0')[0].decode('ascii')))
    require(rows[0]['kind'] == 1 and rows[0]['code'] == 4 and rows[0]['text'] == HEADER,
            'Requires separate v4 input header')
    verify_trace(rows)
    require(all(r['kind'] in range(1, 11) for r in rows), 'Unknown v4 record kind')
    return rows, hashlib.sha256(raw).hexdigest()


def form_id(v, index):
    lo, hi = v[index:index + 2]
    require(all(x == int(x) and 0 <= x <= 65535 for x in (lo, hi)), 'Invalid form identity')
    result = int(lo) | (int(hi) << 16)
    require(result != 0, 'Missing checked form identity')
    return f'{result:08x}'


def analyze_rows(rows):
    observed = [r for r in rows if r['kind'] == 10]
    setters = [r for r in observed if r['text'] == 'setter']
    snapshots = [r for r in observed if r['text'] == 'launch']
    launches = [r for r in rows if r['kind'] == 9]
    require(len(setters) == len(snapshots) == len(launches) == 1,
            'Requires exactly one setter and launch pair')
    setter, snapshot, launch = setters[0], snapshots[0], launches[0]
    require(setter['code'] & SETTER_REQUIRED == SETTER_REQUIRED, 'Invalid setter input fields')
    require(snapshot['code'] & LAUNCH_REQUIRED == LAUNCH_REQUIRED, 'Invalid launch input fields')
    v, w = setter['values'], snapshot['values']
    require(all(math.isfinite(x) for x in v + w), 'Nonfinite v4 input')
    require(v[2] == 0x2b and v[3] in (0, 1) and v[1] == int(v[1]) and 0 <= v[1] <= 65535,
            'Unsupported NPC/reference fields')
    require(v[0] == v[12], 'Jump setting changed across setter')
    require(all(v[i] == w[i] for i in (0, 1, 2, 3, 4, 5, 6, 11, 13, 14, 16, 17, 18, 19, 20, 21, 22)),
            'Loaded inputs changed between setter and launch')
    require(setter['qpc'] <= launch['qpc'] and setter['sample'] <= launch['sample'],
            'Setter did not precede launch')
    reference = f32(v[1] * v[22])
    npc = f32(v[5 + int(v[3])] * v[4])
    scale = f32(reference * npc)
    predicted_setter = f32(scale * v[0])
    require(predicted_setter == v[7], 'Raw-factor prediction differs from exact setter argument')
    predicted_store = f32(predicted_setter * v[11])
    require(predicted_store == v[9] == w[8], 'Setter post-store or launch input differs')
    require(v[10] == 1, 'Native setter did not select jumping state')
    air = [r for r in rows if r['kind'] == 8 and r['sample'] == launch['sample'] and
           r['values'][16] == 2]
    require(len(air) == 1 and air[0]['code'] == 3, 'Missing first nested air callback')
    a = air[0]['values']
    require(a[11] == predicted_store and launch['values'][11] == predicted_store,
            'Stored height did not reach launch/air callbacks')
    gravity = abs(f32(a[39] * a[10]))
    predicted_velocity = f32(math.sqrt(f32(f32(gravity * abs(predicted_store)) * 2.0)))
    require(abs(predicted_velocity - a[6]) <= 1e-6, 'Field-based launch prediction differs')
    return {'status': 'passed', 'validity': {'setter': setter['code'], 'launch': snapshot['code']},
            'inputs': {'jump_height_min': v[0], 'reference_u16': int(v[1]),
                       'reference_factor': reference, 'npc_type': int(v[2]), 'sex_index': int(v[3]),
                       'npc_height': v[4], 'race_heights': v[5:7], 'npc_race_factor': npc,
                       'combined_scale': scale, 'game_to_havok': v[11], 'reference_unit': v[22],
                       'swim_setting': v[13], 'actor_flag_bit_1': int(v[14]),
                       'reference_form_id': form_id(v, 16), 'npc_form_id': form_id(v, 18),
                       'race_form_id': form_id(v, 20)},
            'setter_call_rva': hex(setter['state']), 'setter_argument': v[7], 'predicted_setter_argument': predicted_setter,
            'controller_height': v[9], 'predicted_controller_height': predicted_store,
            'launch_havok_velocity': a[6], 'predicted_launch_havok_velocity': predicted_velocity,
            'launch_creation_velocity': a[6] * a[36], 'havok_to_creation': a[36],
            'effective_gravity_havok': gravity, 'controller_dt': a[2],
            'first_step_havok_velocity': a[24],
            'claim_scope': 'Exact ordinary-ground field product and setter store. Launch tolerance 1e-6 Havok velocity. No fitted coefficients or trajectory refit.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    rows, digest = decode(args.trace)
    result = dict(trace_sha256=digest, **analyze_rows(rows))
    output = json.dumps(result, indent=2, allow_nan=False) + '\n'
    if args.output:
        args.output.write_text(output)
    print(output, end='')


if __name__ == '__main__':
    main()
