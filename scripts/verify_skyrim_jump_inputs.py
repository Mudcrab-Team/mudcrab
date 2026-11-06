#!/usr/bin/env python3
"""Recompute all v4 input products, controls, source receipts and trace identities."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import zipfile

from analyze_skyrim_jump_inputs import analyze_rows, decode, require
from capture_skyrim_jump_inputs import ROOT, control_prediction, digest


def verify(corpus, plan_path):
    manifest = json.loads((corpus / 'manifest.json').read_text())
    plan = json.loads(plan_path.read_text())
    require(manifest['schema'] == 1 and manifest['profile'] == 'jump-inputs-v4', 'Wrong corpus profile')
    require(manifest['plan_sha256'] == digest(plan_path), 'Declared plan differs')
    require(manifest['runtime_sha256'] == plan['runtime_sha256'], 'Corpus runtime differs')
    conditions = plan['conditions']
    require([r['id'] for r in manifest['runs']] == [c['id'] for c in conditions],
            'Campaign is incomplete or conditions reordered')
    baseline = manifest['runs'][0]['result']
    with zipfile.ZipFile(corpus / 'capture-source.zip') as archive:
        captured_sources = {name: archive.read(name) for name in archive.namelist()}
    results = []
    for run, condition in zip(manifest['runs'], conditions):
        path = Path(run['path'])
        require(not path.is_absolute() and '..' not in path.parts, 'Trace escapes corpus')
        rows, raw_digest = decode(corpus / path)
        require(sum(r['kind'] == 1 for r in rows) == sum(r['kind'] == 5 for r in rows) == 1,
                'Duplicate header or footer')
        require(run['sha256'] == raw_digest, 'Numeric trace identity differs')
        result = analyze_rows(rows)
        require(result == run['result'], 'Stored input result differs from recomputation')
        require(result['setter_call_rva'] in ('0x6752ba', '0x7cf269'), 'Unknown native setter path')
        require(run['proof']['exe_sha256'] == plan['runtime_sha256'], 'Recorded runtime differs')
        require(run['proof']['source_save_sha256'] == plan['source_save_sha256'], 'Recorded save differs')
        require(run['proof']['source_save_unchanged'], 'Source save was modified')
        for source, expected in run['receipt']['sources'].items():
            require(hashlib.sha256(captured_sources[source]).hexdigest() == expected,
                    f'Archived capture source differs {source}')
        if run['id'] != 'baseline':
            prediction = control_prediction(baseline, condition)
            require(prediction == run['receipt']['prediction_before_capture'], 'Pre-capture prediction differs')
            require(all(result['inputs'][k] == prediction[k] for k in ('jump_height_min', 'reference_u16')),
                    'Typed intervention did not reach measured memory')
            require(all(result[k] == prediction[k] for k in ('setter_argument', 'controller_height')),
                    'Pre-capture product prediction failed')
            require(all(result['inputs'][k] == baseline['inputs'][k] for k in
                        ('npc_type', 'sex_index', 'npc_height', 'race_heights', 'npc_form_id', 'race_form_id')),
                    'Unrequested base factors changed')
        results.append({'id': run['id'], 'trace_sha256': raw_digest,
                        'setting': result['inputs']['jump_height_min'],
                        'reference_u16': result['inputs']['reference_u16'],
                        'setter_argument': result['setter_argument'],
                        'controller_height': result['controller_height'],
                        'launch_havok_velocity': result['launch_havok_velocity'],
                        'launch_prediction_error': result['launch_havok_velocity'] - result['predicted_launch_havok_velocity']})
    require(manifest['runs'][-1]['result']['inputs'] == baseline['inputs'], 'Repeat baseline inputs differ')
    for attempt in manifest['attempts']:
        if 'failure' not in attempt:
            continue
        if attempt.get('trace_status') == 'not-created':
            require(attempt['exit_code'] != 0 and 'path' not in attempt and 'trace_sha256' not in attempt,
                    'Missing trace is only valid for an unsuccessful capture before trace creation')
            continue
        path = Path(attempt['path'])
        require(not path.is_absolute() and '..' not in path.parts, 'Failed trace escapes corpus')
        raw = gzip.decompress((corpus / path).read_bytes())
        require(hashlib.sha256(raw).hexdigest() == attempt['trace_sha256'], 'Failed trace identity differs')
    return {'status': 'passed', 'fit_performed': False, 'runs': results,
            'attempts': len(manifest['attempts']),
            'failed_attempts': [a for a in manifest['attempts'] if 'failure' in a],
            'plan_sha256': digest(plan_path), 'manifest_sha256': digest(corpus / 'manifest.json')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus', type=Path, default=ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4')
    parser.add_argument('--plan', type=Path, default=ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    output = json.dumps(verify(args.corpus, args.plan), indent=2, allow_nan=False) + '\n'
    if args.output:
        args.output.write_text(output)
    print(output, end='')


if __name__ == '__main__':
    main()
