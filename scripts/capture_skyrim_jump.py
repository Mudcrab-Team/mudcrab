#!/usr/bin/env python3
"""Capture the declared jump campaign with the #520 recorder and retain numeric evidence."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + '\n')


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
    plan = json.loads(args.plan.read_text())
    session = json.loads((args.session / 'session.json').read_text())
    if (session['exe_sha256'] != plan['runtime_sha256'] or
            session['source_save_sha256'] != plan['source_save_sha256']):
        parser.error('session does not match the preregistered executable/save')
    if args.split == 'heldout' and (args.frozen is None or not args.frozen.is_file()):
        parser.error('held-out collection requires an existing frozen fit')
    args.private_output.mkdir(parents=True, exist_ok=True)
    args.corpus.mkdir(parents=True, exist_ok=True)
    (args.corpus / 'traces').mkdir(exist_ok=True)
    sources = ['scripts/capture_skyrim_jump.py', 'scripts/record_skyrim_motion.py',
               'tools/skyrim-recorder/probe.cpp', 'tools/skyrim-recorder/runtime.json',
               'tools/skyrim-recorder/build.py', 'tools/skyrim-recorder/input.c']
    receipt = {'plan_sha256': digest(args.plan), 'session_sha256': digest(args.session / 'session.json'),
               'sources': {name: digest(ROOT / name) for name in sources},
               'frozen_sha256': digest(args.frozen) if args.frozen else None}
    receipt_path = args.corpus / (args.split + '-capture.json')
    if receipt_path.exists() and json.loads(receipt_path.read_text()) != receipt:
        parser.error('capture source, plan, session or frozen model changed; use a new corpus')
    write(receipt_path, receipt)
    manifest_path = args.corpus / 'manifest.json'
    manifest = (json.loads(manifest_path.read_text()) if manifest_path.exists() else
                {'schema': 1, 'runtime_sha256': plan['runtime_sha256'], 'runs': [], 'attempts': []})
    conditions = plan['training' if args.split == 'train' else 'heldout']
    for repeat in range(1, plan['repeats'] + 1):
        for condition in conditions:
            name = f"{args.split}-{condition['frame_cap']}hz-{condition['time_multiplier']:g}x-{repeat}"
            if any(run['id'] == name for run in manifest['runs']):
                continue
            attempt = 1 + sum(row['id'] == name for row in manifest['attempts'])
            output = args.private_output / f'{name}-attempt-{attempt}'
            fixture = dict(plan['fixture'], time_multiplier=condition['time_multiplier'])
            fixture_path = args.private_output / (name + '.json')
            write(fixture_path, fixture)
            command = [sys.executable, str(ROOT / 'scripts/record_skyrim_motion.py'), 'run',
                       '--session', str(args.session.resolve()), '--output', str(output.resolve()),
                       '--fixture', str(fixture_path.resolve()), '--frame-cap', str(condition['frame_cap']),
                       '--ocr-prefix', str(args.ocr_prefix.resolve())]
            print(f'Recording {name}, attempt {attempt}', flush=True)
            with Path(str(output) + '.log').open('w') as log:
                result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
            manifest['attempts'].append({'id': name, 'attempt': attempt, 'exit_code': result.returncode})
            write(manifest_path, manifest)
            if result.returncode:
                raise RuntimeError(f'Capture failed; preserved {output} and log. Resume explicitly.')
            proof = json.loads((output / 'proof.json').read_text())
            raw = (output / 'trace.bin').read_bytes()
            if hashlib.sha256(raw).hexdigest() != proof['trace_sha256']:
                raise ValueError('capture changed after native proof')
            destination = args.corpus / 'traces' / (name + '.bin.gz')
            destination.write_bytes(gzip.compress(raw, mtime=0))
            public_proof = {key: proof[key] for key in
                            ('status', 'received_down_up', 'forward_input_accepted',
                             'source_save_unchanged', 'frame_cap_request', 'dxvk_config', 'ini', 'fixture_sha256')}
            if 'camera_cadence' in proof:
                public_proof['camera_cadence'] = proof['camera_cadence']
            public_proof['source_save_sha256'] = session['source_save_sha256']
            public_proof['exe_sha256'] = session['exe_sha256']
            public_proof['dll_sha256'] = session['build']['dll_sha256']
            public_proof['input_sha256'] = session['input_sha256']
            public_proof['proton_sha256'] = session['proton_sha256']
            manifest['runs'].append(dict(id=name, split=args.split, repeat=repeat,
                                         path=str(destination.relative_to(args.corpus)),
                                         sha256=proof['trace_sha256'], proof=public_proof, **condition))
            write(manifest_path, manifest)
            print(f'Accepted native recording {name}', flush=True)


if __name__ == '__main__':
    main()
