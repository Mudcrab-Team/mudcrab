#!/usr/bin/env python3
"""Capture the declared jump campaign with the #520 recorder and retain numeric evidence."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import sys

import evaluate_skyrim_jump as evaluator
from record_skyrim_motion import stopped_capture_services
from skyrim_capture_attempts import (claim_private_output, corpus_lock, private_attempt_paths, record_attempt,
                                     recover_attempts, run_recorder, save_manifest, verify_retained_trace)

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write(path, value, *, exclusive=False):
    data = (json.dumps(value, indent=2, allow_nan=False) + '\n').encode()
    with path.open('xb' if exclusive else 'wb') as output:
        output.write(data)
    return evaluator.digest(data)


def provenance(session):
    return {key: session[key] for key in ('exe_sha256', 'source_save_sha256', 'input_sha256', 'proton_sha256')} | {
        'dll_sha256': session['build']['dll_sha256']}


def validate_frozen(data, session):
    frozen = json.loads(data)
    seal = frozen.pop('seal_sha256', None)
    if seal != evaluator.digest(evaluator.canonical(frozen)):
        raise ValueError('frozen seal mismatch')
    if (frozen['evaluator_sha256'] != digest(Path(evaluator.__file__)) or
            frozen['model_sha256'] != evaluator.digest(evaluator.canonical(frozen['models'])) or
            frozen['schema'] != 1 or frozen['rules'] != evaluator.RULES):
        raise ValueError('frozen evaluator or model mismatch')
    if frozen['runtime_sha256'] != session['exe_sha256'] or frozen['provenance_contract'] != provenance(session):
        raise ValueError('frozen fit differs from the prepared session')


def retain_failure(output, attempt, failure):
    with stopped_capture_services(output, attempt):
        attempt['failure'] = failure


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--session', type=Path, required=True)
    parser.add_argument('--private-output', type=Path, required=True)
    parser.add_argument('--corpus', type=Path, required=True)
    parser.add_argument('--ocr-prefix', type=Path, required=True)
    parser.add_argument('--plan', type=Path, default=ROOT / 'tools/skyrim-recorder/fixtures/jump-v3.json')
    parser.add_argument('--split', choices=('train', 'heldout'), required=True)
    parser.add_argument('--frozen', type=Path)
    args = parser.parse_args()
    plan_bytes = args.plan.read_bytes()
    plan = json.loads(plan_bytes)
    session_dir = args.session.resolve()
    session_bytes = (session_dir / 'session.json').read_bytes()
    session = json.loads(session_bytes)
    if (session.get('profile_name') is not None or
            session['exe_sha256'] != plan['runtime_sha256'] or
            session['source_save_sha256'] != plan['source_save_sha256']):
        parser.error('session does not match the preregistered v3 executable/save/profile')
    if args.split == 'heldout' and (args.frozen is None or not args.frozen.is_file()):
        parser.error('held-out collection requires an existing frozen fit')
    frozen_bytes = args.frozen.read_bytes() if args.frozen else None
    if args.split == 'heldout':
        validate_frozen(frozen_bytes, session)
    if session['build']['source_sha256'] != digest(ROOT / 'tools/skyrim-recorder/probe.cpp'):
        parser.error('Prepared recorder source differs; prepare the session again')
    args.corpus = args.corpus.resolve()
    args.private_output = args.private_output.resolve()
    args.private_output.mkdir(parents=True, exist_ok=True)
    args.corpus.mkdir(parents=True, exist_ok=True)
    with corpus_lock(args.corpus) as lock_fd:
        (args.corpus / 'traces').mkdir(exist_ok=True)
        sources = ['scripts/capture_skyrim_jump.py', 'scripts/evaluate_skyrim_jump.py', 'scripts/record_skyrim_motion.py', 'scripts/skyrim_capture_attempts.py',
                   'tools/skyrim-recorder/probe.cpp', 'tools/skyrim-recorder/runtime.json',
                   'tools/skyrim-recorder/build.py', 'tools/skyrim-recorder/input.c']
        receipt = {'plan_sha256': evaluator.digest(plan_bytes), 'session_sha256': evaluator.digest(session_bytes),
                   'sources': {name: digest(ROOT / name) for name in sources},
                   'frozen_sha256': evaluator.digest(frozen_bytes) if frozen_bytes is not None else None}
        receipt_path = args.corpus / (args.split + '-capture.json')
        manifest_path = args.corpus / 'manifest.json'
        manifest = (json.loads(manifest_path.read_text()) if manifest_path.exists() else
                    {'schema': 1, 'runtime_sha256': plan['runtime_sha256'], 'runs': [], 'attempts': []})
        claim_private_output(args.private_output, args.corpus)
        if receipt_path.exists():
            if json.loads(receipt_path.read_text()) != receipt:
                parser.error('capture source, plan, session or frozen model changed; use a new corpus')
        else:
            save_manifest(receipt_path, receipt)
        recover_attempts(manifest_path, manifest, args.private_output, retain_failure)
        conditions = plan['training' if args.split == 'train' else 'heldout']
        for repeat in range(1, plan['repeats'] + 1):
            for condition in conditions:
                name = f"{args.split}-{condition['frame_cap']}hz-{condition['time_multiplier']:g}x-{repeat}"
                accepted = next((run for run in manifest['runs'] if run['id'] == name), None)
                if accepted is not None:
                    verify_retained_trace(args.corpus, accepted)
                    continue
                attempt = 1 + sum(row['id'] == name for row in manifest['attempts'])
                output, inputs = private_attempt_paths(args.private_output, name, attempt)
                fixture = dict(plan['fixture'], time_multiplier=condition['time_multiplier'])
                fixture_path = inputs / (name + '.json')
                command = [sys.executable, str(ROOT / 'scripts/record_skyrim_motion.py'), 'run',
                           '--session', str(session_dir), '--output', str(output.resolve()),
                           '--fixture', str(fixture_path.resolve()), '--frame-cap', str(condition['frame_cap']),
                           '--ocr-prefix', str(args.ocr_prefix.resolve())]
                with record_attempt(manifest_path, manifest, name, attempt, output, retain_failure,
                                    private_session=session_dir) as attempt_row:
                    inputs.mkdir()
                    expected_fixture = write(fixture_path, fixture, exclusive=True)
                    print(f'Recording {name}, attempt {attempt}', flush=True)
                    with Path(str(output) + '.log').open('x') as log:
                        exit_code = run_recorder(command, stdout=log, attempt=attempt_row, lock_fd=lock_fd)
                    attempt_row['exit_code'] = exit_code
                    save_manifest(manifest_path, manifest)
                    if exit_code:
                        raise RuntimeError(f'Capture failed; preserved {output} and log. Resume explicitly.')
                    proof = json.loads((output / 'proof.json').read_text())
                    if proof['session'] != session:
                        raise ValueError('recorded session differs from capture receipt')
                    if proof['fixture_sha256'] != expected_fixture:
                        raise ValueError('recorded fixture differs from planned fixture')
                    trace = output / 'trace.bin'
                    evaluator.qualify(trace, {'sha256': proof['trace_sha256']})
                    raw = trace.read_bytes()
                    if hashlib.sha256(raw).hexdigest() != proof['trace_sha256']:
                        raise ValueError('capture changed after native proof')
                    destination = args.corpus / 'traces' / (name + '.bin.gz')
                    destination.write_bytes(gzip.compress(raw, mtime=0))
                    public_proof = {key: proof[key] for key in
                                    ('status', 'received_down_up', 'forward_input_accepted',
                                     'source_save_unchanged', 'frame_cap_request', 'dxvk_config', 'ini', 'fixture_sha256')}
                    if 'camera_cadence' in proof:
                        public_proof['camera_cadence'] = proof['camera_cadence']
                    public_proof.update(provenance(session))
                    manifest['runs'].append(dict(id=name, split=args.split, repeat=repeat,
                                                 path=str(destination.relative_to(args.corpus)),
                                                 sha256=proof['trace_sha256'], proof=public_proof, **condition))
                    save_manifest(manifest_path, manifest)
                    print(f'Accepted native recording {name}', flush=True)


if __name__ == '__main__':
    main()
