#!/usr/bin/env python3
"""Recompute all v4 input products, controls, source receipts and trace identities."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path

from analyze_skyrim_jump_inputs import analyze_rows, decode, require
from publish_skyrim_evidence import source_digest
from capture_skyrim_jump_inputs import (ROOT, SOURCES, control_prediction, read_capture_sources, validate_campaign_session,
                                        validate_capture_proof)

LEGACY_CAPTURE_SHA256 = '6e85fa44b471005a5179a88fb220d4a1c12e8ab6341cadc640250d93ebc8d9ad'
LEGACY_SOURCES = set(SOURCES) - {'scripts/publish_skyrim_evidence.py', 'scripts/skyrim_capture_attempts.py'}


def verify(corpus, plan_path):
    manifest_bytes = (corpus / 'manifest.json').read_bytes()
    manifest = json.loads(manifest_bytes)
    plan_bytes = plan_path.read_bytes()
    plan = json.loads(plan_bytes)
    plan_sha256 = hashlib.sha256(plan_bytes).hexdigest()
    require(manifest['schema'] == 1 and manifest['profile'] == 'jump-inputs-v4', 'Wrong corpus profile')
    require(manifest['plan_sha256'] == plan_sha256, 'Declared plan differs')
    require(manifest['runtime_sha256'] == plan['runtime_sha256'], 'Corpus runtime differs')
    conditions = plan['conditions']
    require([r['id'] for r in manifest['runs']] == [c['id'] for c in conditions],
            'Campaign is incomplete or conditions reordered')
    accepted = 0
    next_attempt = 1
    for attempt in manifest['attempts']:
        require(accepted < len(conditions) and attempt['id'] == conditions[accepted]['id']
                and type(attempt['attempt']) is int and attempt['attempt'] == next_attempt
                and attempt.get('status') != 'pending', 'Attempt ledger is out of sequence or pending')
        if 'failure' in attempt:
            next_attempt += 1
        else:
            require(attempt['exit_code'] == 0, 'Attempt ledger has an unclassified attempt')
            accepted += 1
            next_attempt = 1
    require(accepted == len(conditions), 'Attempt ledger is incomplete')
    baseline_run = manifest['runs'][0]
    validate_campaign_session(manifest['runs'], baseline_run['receipt']['session_sha256'], baseline_run['proof'])
    baseline = baseline_run['result']
    captured_sources = read_capture_sources(corpus / 'capture-source.zip')
    capture = 'scripts/capture_skyrim_jump_inputs.py'
    require(capture in captured_sources, 'Missing capture source')
    acquisition = source_digest(corpus / 'capture-source.zip', capture, captured_sources[capture])
    required_sources = LEGACY_SOURCES if acquisition == LEGACY_CAPTURE_SHA256 else set(SOURCES)
    require(set(captured_sources) == required_sources, 'Archived source set is incomplete or unexpected')
    trace_hashes = set()
    results = []
    for run, condition in zip(manifest['runs'], conditions):
        fixture = dict(plan['fixture'], **{k: v for k, v in condition.items() if k != 'id'})
        require(run['fixture'] == fixture, 'Stored fixture differs from plan')
        path = Path(run['path'])
        require(not path.is_absolute() and '..' not in path.parts, 'Trace escapes corpus')
        rows, raw_digest = decode(corpus / path)
        cadence = validate_capture_proof(rows, run['proof'], plan['frame_cap'], fixture)
        if acquisition != LEGACY_CAPTURE_SHA256 or 'camera_cadence' in run['proof']:
            require(run['proof'].get('camera_cadence') == cadence, 'Stored camera cadence differs from trace')
        require(run['sha256'] == raw_digest, 'Numeric trace identity differs')
        require(raw_digest not in trace_hashes, 'Native trace reused across conditions')
        trace_hashes.add(raw_digest)
        result = analyze_rows(rows)
        require(result == run['result'], 'Stored input result differs from recomputation')
        require(run['proof']['exe_sha256'] == plan['runtime_sha256'], 'Recorded runtime differs')
        require(run['proof']['source_save_sha256'] == plan['source_save_sha256'], 'Recorded save differs')
        require(run['proof']['source_save_unchanged'], 'Source save was modified')
        require(set(run['receipt']['sources']) == required_sources, 'Receipt source set is incomplete or unexpected')
        for source, expected in run['receipt']['sources'].items():
            require(source_digest(corpus / 'capture-source.zip', source, captured_sources[source]) == expected,
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
        if run['id'] == 'repeat-baseline':
            require(result['inputs'] == baseline['inputs'], 'Repeat baseline inputs differ')
        results.append({'id': run['id'], 'trace_sha256': raw_digest,
                        'setting': result['inputs']['jump_height_min'],
                        'reference_u16': result['inputs']['reference_u16'],
                        'setter_argument': result['setter_argument'],
                        'controller_height': result['controller_height'],
                        'launch_havok_velocity': result['launch_havok_velocity'],
                        'launch_prediction_error': result['launch_havok_velocity'] - result['predicted_launch_havok_velocity']})
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
            'plan_sha256': plan_sha256, 'manifest_sha256': hashlib.sha256(manifest_bytes).hexdigest()}


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
