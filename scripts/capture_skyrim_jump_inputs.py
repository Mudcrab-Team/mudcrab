#!/usr/bin/env python3
"""Capture the declared v4 input controls with immutable per-run numeric receipts."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import sys

from analyze_skyrim_jump_inputs import analyze_rows, decode, f32

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ['scripts/record_skyrim_motion.py', 'scripts/capture_skyrim_jump_inputs.py',
           'scripts/analyze_skyrim_jump_inputs.py', 'scripts/inspect_skyrim_jump_inputs.py',
           'tools/skyrim-recorder/probe.cpp', 'tools/skyrim-recorder/jump_inputs.h',
           'tools/skyrim-recorder/build.py', 'tools/skyrim-recorder/runtime.json',
           'tools/skyrim-recorder/runtime-inputs-v4.json', 'tools/skyrim-recorder/input.c']


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + '\n')


def control_prediction(baseline, condition):
    inputs = baseline['inputs']
    setting = condition.get('jump_height_min', inputs['jump_height_min'])
    raw_scale = (round(condition['reference_scale'] * 100) if 'reference_scale' in condition
                 else inputs['reference_u16'])
    reference = f32(raw_scale * inputs['reference_unit'])
    scale = f32(reference * inputs['npc_race_factor'])
    argument = f32(scale * setting)
    return {'jump_height_min': setting, 'reference_u16': raw_scale,
            'setter_argument': argument,
            'controller_height': f32(argument * inputs['game_to_havok']),
            'method': 'Baseline raw factors, requested typed control, native float32 order; no fit.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--session', type=Path, required=True)
    parser.add_argument('--private-output', type=Path, required=True)
    parser.add_argument('--corpus', type=Path, required=True)
    parser.add_argument('--ocr-prefix', type=Path, required=True)
    parser.add_argument('--plan', type=Path, default=ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json')
    parser.add_argument('--condition', choices=('baseline', 'setting-64', 'reference-125', 'repeat-baseline'))
    parser.add_argument('--adopt-baseline', type=Path,
                        help='Import an already completed baseline from the same prepared v4 session')
    args = parser.parse_args()
    plan = json.loads(args.plan.read_text())
    session = json.loads((args.session / 'session.json').read_text())
    if (session.get('profile_name') != 'inputs-v4' or session['exe_sha256'] != plan['runtime_sha256'] or
            session['source_save_sha256'] != plan['source_save_sha256']):
        parser.error('Session differs from declared v4 runtime/save/profile')
    if session['build']['source_sha256'] != digest(ROOT / 'tools/skyrim-recorder/probe.cpp'):
        parser.error('Prepared recorder source differs')
    if session['build']['extension_sha256'] != digest(ROOT / 'tools/skyrim-recorder/jump_inputs.h'):
        parser.error('Prepared extension source differs')
    args.private_output.mkdir(parents=True, exist_ok=True)
    args.corpus.mkdir(parents=True, exist_ok=True)
    (args.corpus / 'traces').mkdir(exist_ok=True)
    manifest_path = args.corpus / 'manifest.json'
    manifest = (json.loads(manifest_path.read_text()) if manifest_path.exists() else
                {'schema': 1, 'profile': 'jump-inputs-v4', 'plan_sha256': digest(args.plan),
                 'runtime_sha256': plan['runtime_sha256'], 'runs': [], 'attempts': []})
    if manifest['plan_sha256'] != digest(args.plan):
        parser.error('Declared plan changed; use a new corpus')
    for condition in plan['conditions']:
        name = condition['id']
        if args.condition and name != args.condition:
            continue
        if any(run['id'] == name for run in manifest['runs']):
            continue
        attempt = 1 + sum(row['id'] == name for row in manifest['attempts'])
        adopted = name == 'baseline' and args.adopt_baseline
        output = args.adopt_baseline if adopted else args.private_output / f'{name}-attempt-{attempt}'
        fixture = dict(plan['fixture'], **{k: v for k, v in condition.items() if k != 'id'})
        fixture_path = args.private_output / (name + '.json')
        write(fixture_path, fixture)
        prediction = None
        if name != 'baseline':
            baseline = next((r for r in manifest['runs'] if r['id'] == 'baseline'), None)
            if baseline is None:
                parser.error('Collect and accept baseline before controls')
            prediction = control_prediction(baseline['result'], condition)
            write(args.private_output / (name + '-prediction.json'), prediction)
        command = [sys.executable, str(ROOT / 'scripts/record_skyrim_motion.py'), 'run',
                   '--session', str(args.session.resolve()), '--output', str(output.resolve()),
                   '--fixture', str(fixture_path.resolve()), '--frame-cap', str(plan['frame_cap']),
                   '--ocr-prefix', str(args.ocr_prefix.resolve())]
        sources = {name: digest(ROOT / name) for name in SOURCES}
        receipt = {'session_sha256': digest(args.session / 'session.json'), 'sources': sources,
                   'command': command, 'prediction_before_capture': prediction, 'adopted': bool(adopted)}
        write(args.private_output / f'{name}-attempt-{attempt}-receipt.json', receipt)
        print(f'Recording {name}, attempt {attempt}', flush=True)
        exit_code = 0
        if not adopted:
            with Path(str(output) + '.log').open('w') as log:
                exit_code = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT).returncode
        attempt_row = {'id': name, 'attempt': attempt, 'exit_code': exit_code}
        manifest['attempts'].append(attempt_row)
        write(manifest_path, manifest)
        if exit_code:
            failure = json.loads((output / 'failure.json').read_text())
            attempt_row['failure'] = {key: failure[key] for key in ('phase', 'error')}
            write(manifest_path, manifest)
            raise RuntimeError(f'Capture failed; preserved {output}')
        proof = json.loads((output / 'proof.json').read_text())
        try:
            rows, raw_digest = decode(output / 'trace.bin')
            result = analyze_rows(rows)
            if raw_digest != proof['trace_sha256'] or proof['session'] != session:
                raise ValueError('Recorded session or native trace differs')
            if proof['runner_sha256'] != sources['scripts/record_skyrim_motion.py']:
                raise ValueError('Recorder source changed during capture')
            if prediction:
                for key in ('jump_height_min', 'reference_u16'):
                    if prediction[key] != result['inputs'][key]:
                        raise ValueError('Requested control did not reach measured memory field')
                for key in ('setter_argument', 'controller_height'):
                    if prediction[key] != result[key]:
                        raise ValueError('Pre-capture field-product prediction failed')
                original = baseline['result']['inputs']
                for key in ('npc_type', 'sex_index', 'npc_height', 'race_heights', 'npc_form_id', 'race_form_id'):
                    if original[key] != result['inputs'][key]:
                        raise ValueError('Control changed an unrequested base factor')
        except ValueError as error:
            attempt_row['failure'] = {'phase': 'v4-input-proof', 'error': str(error)}
            write(manifest_path, manifest)
            raise
        destination = args.corpus / 'traces' / (name + '.bin.gz')
        destination.write_bytes(gzip.compress((output / 'trace.bin').read_bytes(), mtime=0))
        public_proof = {key: proof[key] for key in ('status', 'received_down_up', 'forward_input_accepted',
                        'source_save_unchanged', 'frame_cap_request', 'dxvk_config', 'ini', 'fixture_sha256')}
        public_proof.update(exe_sha256=session['exe_sha256'], source_save_sha256=session['source_save_sha256'],
                            dll_sha256=session['build']['dll_sha256'], input_sha256=session['input_sha256'],
                            proton_sha256=session['proton_sha256'])
        manifest['runs'].append({'id': name, 'path': str(destination.relative_to(args.corpus)),
                                 'sha256': raw_digest, 'fixture': fixture, 'proof': public_proof,
                                 'receipt': receipt, 'result': result})
        write(manifest_path, manifest)
        print(json.dumps({'id': name, 'result': result}), flush=True)


if __name__ == '__main__':
    main()
