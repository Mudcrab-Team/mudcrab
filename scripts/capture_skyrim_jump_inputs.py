#!/usr/bin/env python3
"""Capture the declared v4 input controls with immutable per-run numeric receipts."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import zipfile
from functools import partial

from analyze_skyrim_jump_inputs import analyze_rows, decode_bytes, f32
from publish_skyrim_evidence import public_value
from record_skyrim_motion import stopped_capture_services, verify_trace
from skyrim_capture_attempts import (claim_private_output, corpus_lock, private_attempt_paths, record_attempt,
                                     recover_attempts, run_recorder, save_manifest, verify_retained_trace)

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ['scripts/publish_skyrim_evidence.py', 'scripts/record_skyrim_motion.py', 'scripts/capture_skyrim_jump_inputs.py',
           'scripts/skyrim_capture_attempts.py',
           'scripts/analyze_skyrim_jump_inputs.py', 'scripts/inspect_skyrim_jump_inputs.py',
           'tools/skyrim-recorder/probe.cpp', 'tools/skyrim-recorder/jump_inputs.h',
           'tools/skyrim-recorder/build.py', 'tools/skyrim-recorder/runtime.json',
           'tools/skyrim-recorder/runtime-inputs-v4.json', 'tools/skyrim-recorder/input.c']


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write(path, value, *, exclusive=False):
    with path.open('x' if exclusive else 'w') as output:
        output.write(json.dumps(value, indent=2, allow_nan=False) + '\n')


def read_capture_sources(path):
    with zipfile.ZipFile(path) as archive:
        members = archive.infolist()
        if len({member.filename for member in members}) != len(members):
            raise ValueError(f'Duplicate source archive member in {path}')
        return {member.filename: archive.read(member) for member in members}


def validate_campaign_session(runs, session_sha256, expected_proof):
    for run in runs:
        if (run['receipt']['session_sha256'] != session_sha256 or
                any(run['proof'][key] != expected_proof[key]
                    for key in ('dll_sha256', 'input_sha256', 'proton_sha256'))):
            raise ValueError(f"Capture session or tool identity differs for {run['id']}")


def control_prediction(baseline, condition):
    inputs = baseline['inputs']
    setting = condition.get('jump_height_min', inputs['jump_height_min'])
    raw_scale = (round(condition['reference_scale'] * 100) if 'reference_scale' in condition
                 else inputs['reference_u16'])
    if 'jump_height_min' in condition and setting == inputs['jump_height_min']:
        raise ValueError('Requested jump height already matches baseline')
    if 'reference_scale' in condition and raw_scale == inputs['reference_u16']:
        raise ValueError('Requested reference scale already matches baseline')
    reference = f32(raw_scale * inputs['reference_unit'])
    scale = f32(reference * inputs['npc_race_factor'])
    argument = f32(scale * setting)
    return {'jump_height_min': setting, 'reference_u16': raw_scale,
            'setter_argument': argument,
            'controller_height': f32(argument * inputs['game_to_havok']),
            'method': 'Baseline raw factors, requested typed control, native float32 order; no fit.'}


def validate_capture_proof(rows, proof, frame_cap, fixture):
    if proof['status'] != 'passed':
        raise ValueError('Capture proof did not pass')
    if not proof['source_save_unchanged']:
        raise ValueError('Capture proof source save was modified')
    if proof['frame_cap_request'] != frame_cap:
        raise ValueError('Capture proof requested a different cadence')
    fixture_bytes = (json.dumps(fixture, indent=2, allow_nan=False) + '\n').encode()
    if proof['fixture_sha256'] != hashlib.sha256(fixture_bytes).hexdigest():
        raise ValueError('Capture proof fixture differs from plan')
    measured = verify_trace(rows, requested_fps=frame_cap)
    if measured['status'] != 'passed':
        raise ValueError('Measured camera cadence did not meet the plan')
    return measured['camera_cadence']


def stopped_trace(output, attempt):
    """Read a finished private trace or confirm the service can no longer write it."""
    trace = output / 'trace.bin'
    try:
        proof = json.loads((output / 'proof.json').read_bytes())
    except (FileNotFoundError, ValueError):
        proof = {}
    if (proof.get('status') == 'passed' and proof.get('services_stopped') is True and
            proof.get('runner_sha256') == digest(ROOT / 'scripts/record_skyrim_motion.py')):
        raw = trace.read_bytes()
        if hashlib.sha256(raw).hexdigest() == proof['trace_sha256']:
            return raw

    with stopped_capture_services(output, attempt):
        try:
            return trace.read_bytes()
        except FileNotFoundError:
            return None


def retain_failure(corpus, output, attempt, failure):
    """Keep available failed bytes, or explicitly record that capture never created them."""
    attempt['failure'] = public_value(failure)
    raw = stopped_trace(output, attempt)
    if raw is not None:
        kept = corpus / 'traces' / f"failed-{attempt['id']}-attempt-{attempt['attempt']}.bin.gz"
        kept.write_bytes(gzip.compress(raw, mtime=0))
        attempt.update(path=str(kept.relative_to(corpus)), trace_sha256=hashlib.sha256(raw).hexdigest())
    else:
        attempt['trace_status'] = 'not-created'


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
    plan_bytes = args.plan.read_bytes()
    plan_hash = hashlib.sha256(plan_bytes).hexdigest()
    plan = json.loads(plan_bytes)
    session_dir = args.session.resolve()
    session_bytes = (session_dir / 'session.json').read_bytes()
    session = json.loads(session_bytes)
    if (session.get('profile_name') != 'inputs-v4' or session['exe_sha256'] != plan['runtime_sha256'] or
            session['source_save_sha256'] != plan['source_save_sha256']):
        parser.error('Session differs from declared v4 runtime/save/profile')
    if session['build']['source_sha256'] != digest(ROOT / 'tools/skyrim-recorder/probe.cpp'):
        parser.error('Prepared recorder source differs')
    if session['build']['extension_sha256'] != digest(ROOT / 'tools/skyrim-recorder/jump_inputs.h'):
        parser.error('Prepared extension source differs')
    args.corpus = args.corpus.resolve()
    args.private_output = args.private_output.resolve()
    args.private_output.mkdir(parents=True, exist_ok=True)
    args.corpus.mkdir(parents=True, exist_ok=True)
    with corpus_lock(args.corpus) as lock_fd:
        (args.corpus / 'traces').mkdir(exist_ok=True)
        manifest_path = args.corpus / 'manifest.json'
        manifest = (json.loads(manifest_path.read_text()) if manifest_path.exists() else
                    {'schema': 1, 'profile': 'jump-inputs-v4', 'plan_sha256': plan_hash,
                     'runtime_sha256': plan['runtime_sha256'], 'runs': [], 'attempts': []})
        if manifest['plan_sha256'] != plan_hash:
            parser.error('Declared plan changed; use a new corpus')
        session_sha256 = hashlib.sha256(session_bytes).hexdigest()
        tools = {'dll_sha256': session['build']['dll_sha256'], 'input_sha256': session['input_sha256'],
                 'proton_sha256': session['proton_sha256']}
        validate_campaign_session(manifest['runs'], session_sha256, tools)
        for run in manifest['runs']:
            verify_retained_trace(args.corpus, run)
        claim_private_output(args.private_output, args.corpus)
        source_bytes = {name: (ROOT / name).read_bytes() for name in SOURCES}
        archive_path = args.corpus / 'capture-source.zip'
        if archive_path.exists():
            if read_capture_sources(archive_path) != source_bytes:
                parser.error('Capture sources changed; use a new corpus')
        else:
            if manifest['attempts']:
                parser.error('Existing attempts lack a source archive; use a new corpus')
            with tempfile.TemporaryDirectory(dir=args.corpus) as staging:
                staged = Path(staging) / archive_path.name
                with zipfile.ZipFile(staged, 'x', compression=zipfile.ZIP_DEFLATED) as archive:
                    for name, data in source_bytes.items():
                        archive.writestr(name, data)
                staged.replace(archive_path)
        sources = {name: hashlib.sha256(data).hexdigest() for name, data in source_bytes.items()}
        recover_attempts(manifest_path, manifest, args.private_output, partial(retain_failure, args.corpus))
        for condition in plan['conditions']:
            name = condition['id']
            if args.condition and name != args.condition:
                continue
            if any(run['id'] == name for run in manifest['runs']):
                continue
            next_condition = next(c['id'] for c in plan['conditions']
                                  if not any(run['id'] == c['id'] for run in manifest['runs']))
            if name != next_condition:
                parser.error(f'Collect declared conditions in plan order; next is {next_condition}')
            attempt = 1 + sum(row['id'] == name for row in manifest['attempts'])
            adopted = name == 'baseline' and args.adopt_baseline
            output, inputs = private_attempt_paths(args.private_output, name, attempt, adopted)
            fixture = dict(plan['fixture'], **{k: v for k, v in condition.items() if k != 'id'})
            fixture_path = inputs / (name + '.json')
            prediction = None
            if name != 'baseline':
                baseline = next((r for r in manifest['runs'] if r['id'] == 'baseline'), None)
                if baseline is None:
                    parser.error('Collect and accept baseline before controls')
                prediction = control_prediction(baseline['result'], condition)
            command = [sys.executable, str(ROOT / 'scripts/record_skyrim_motion.py'), 'run',
                       '--session', str(session_dir), '--output', str(output.resolve()),
                       '--fixture', str(fixture_path.resolve()), '--frame-cap', str(plan['frame_cap']),
                       '--ocr-prefix', str(args.ocr_prefix.resolve())]
            receipt = {'session_sha256': session_sha256, 'sources': sources,
                       'command': command, 'prediction_before_capture': prediction, 'adopted': bool(adopted)}
            with record_attempt(manifest_path, manifest, name, attempt, output, partial(retain_failure, args.corpus),
                                private_session=session_dir) as attempt_row:
                inputs.mkdir()
                write(fixture_path, fixture, exclusive=True)
                if prediction is not None:
                    write(inputs / (name + '-prediction.json'), prediction, exclusive=True)
                write(inputs / f'{name}-attempt-{attempt}-receipt.json', receipt, exclusive=True)
                print(f'Recording {name}, attempt {attempt}', flush=True)
                exit_code = 0
                if not adopted:
                    with Path(str(output) + '.log').open('x') as log:
                        exit_code = run_recorder(command, stdout=log, attempt=attempt_row, lock_fd=lock_fd)
                attempt_row['exit_code'] = exit_code
                save_manifest(manifest_path, manifest)
                if exit_code:
                    try:
                        failure = json.loads((output / 'failure.json').read_text())
                    except FileNotFoundError:
                        failure = {'phase': 'recorder',
                                   'error': f'Recorder exited with code {exit_code} without failure.json; see private log'}
                    retain_failure(args.corpus, output, attempt_row,
                                   {key: failure[key] for key in ('phase', 'error')})
                    save_manifest(manifest_path, manifest)
                    raise RuntimeError(f'Capture failed; preserved {output}')
                try:
                    proof = json.loads((output / 'proof.json').read_text())
                    if adopted and proof.get('services_stopped') is not True:
                        raise ValueError('Adopted proof lacks successful recorder shutdown')
                    if proof['runner_sha256'] != sources['scripts/record_skyrim_motion.py']:
                        raise ValueError('Recorder source changed during capture')
                    raw = (output / 'trace.bin').read_bytes()
                    rows, raw_digest = decode_bytes(raw)
                    if any(run['sha256'] == raw_digest for run in manifest['runs']):
                        raise ValueError('Native trace reused across conditions')
                    cadence = validate_capture_proof(rows, proof, plan['frame_cap'], fixture)
                    result = analyze_rows(rows)
                    if raw_digest != proof['trace_sha256'] or proof['session'] != session:
                        raise ValueError('Recorded session or native trace differs')
                    if prediction:
                        for key in ('jump_height_min', 'reference_u16'):
                            if prediction[key] != result['inputs'][key]:
                                raise ValueError('Requested control did not reach measured memory field')
                        for key in ('setter_argument', 'controller_height'):
                            if prediction[key] != result[key]:
                                raise ValueError('Pre-capture field-product prediction failed')
                        original = baseline['result']['inputs']
                        if name == 'repeat-baseline' and result['inputs'] != original:
                            raise ValueError('Repeat baseline inputs differ from the original baseline')
                        for key in ('npc_type', 'sex_index', 'npc_height', 'race_heights', 'npc_form_id', 'race_form_id'):
                            if original[key] != result['inputs'][key]:
                                raise ValueError('Control changed an unrequested base factor')
                    public_proof = {key: proof[key] for key in ('status', 'received_down_up', 'forward_input_accepted',
                                    'source_save_unchanged', 'frame_cap_request', 'dxvk_config', 'ini', 'fixture_sha256')}
                    public_proof['camera_cadence'] = cadence
                    public_proof.update(exe_sha256=session['exe_sha256'], source_save_sha256=session['source_save_sha256'],
                                        **tools)
                except Exception as error:
                    retain_failure(args.corpus, output, attempt_row,
                                   {'phase': 'v4-input-proof', 'error': f'{type(error).__name__}: {error}'})
                    save_manifest(manifest_path, manifest)
                    raise
                destination = args.corpus / 'traces' / (name + '.bin.gz')
                destination.write_bytes(gzip.compress(raw, mtime=0))
                manifest['runs'].append({'id': name, 'path': str(destination.relative_to(args.corpus)),
                                         'sha256': raw_digest, 'fixture': fixture, 'proof': public_proof,
                                         'receipt': public_value(receipt), 'result': result})
                save_manifest(manifest_path, manifest)
                print(json.dumps({'id': name, 'result': result}), flush=True)


if __name__ == '__main__':
    main()
