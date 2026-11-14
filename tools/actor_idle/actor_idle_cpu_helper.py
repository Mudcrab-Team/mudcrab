#!/Library/Developer/CommandLineTools/usr/bin/python3
"""CPU protocol helper for actor adapter A1/A2. Never reads game assets or starts children."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import sys
import time


def protocol_record(args, pid, environment, contract_hash):
    if args.case not in ('a1', 'a2'):
        raise ValueError('exactly A1/A2 are supported')
    if environment.get('MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256') != contract_hash:
        raise ValueError('contract environment mismatch')
    if environment.get('WGPU_BACKEND') != 'metal':
        raise ValueError('actor environment wiring mismatch')
    return {'version': 1, 'scope': 'cpu-helper-only', 'pid': pid, 'case': args.case,
            'argv': ['--assets', args.assets, '--contract', args.contract,
                     '--case', args.case, '--output', args.output],
            'assets_argument': args.assets, 'assets_opened': False,
            'contract_sha256': contract_hash, 'payload_bytes': 1048576,
            'python_executable': sys.executable, 'python_version': sys.version,
            'environment': {key: environment.get(key) for key in
                            ('WGPU_BACKEND', 'RUST_LOG', 'MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256')}}


def verify_sample_ack(output, value, pid, controller_pid):
    path = output / 'samples.jsonl'
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 262144:
        raise ValueError('sample path/size')
    lines = path.read_bytes().splitlines(keepends=True)
    if not lines or not lines[0].endswith(b'\n'):
        raise ValueError('incomplete first sample')
    raw = lines[0].rstrip(b'\r\n')
    sample = json.loads(raw)
    if (value['case'] != 'a1' or value['pid'] != pid
            or value['sample_sha256'] != hashlib.sha256(raw).hexdigest()
            or sample['child']['pid'] != pid
            or sample['controller']['pid'] != controller_pid
            or sample['child']['rss'] <= 0 or sample['controller']['rss'] <= 0):
        raise ValueError('sample ack mismatch')


def main():
    signal.alarm(8)  # Independent exit even if protocol validation cannot finish.
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ('assets', 'contract', 'case', 'output'):
        parser.add_argument('--' + key, required=True)
    args = parser.parse_args()
    output = Path(args.output)
    if not output.is_absolute() or output.is_symlink() or not output.is_dir():
        raise ValueError('existing owned output directory required')
    contract = Path(args.contract)
    if contract.is_symlink() or not contract.is_file() or contract.stat().st_size > 16384:
        raise ValueError('small task-owned contract required')
    body = contract.read_bytes()
    document = json.loads(body)
    if document != {'version': 1, 'scope': 'actor-adapter-cpu-only', 'payload_bytes': 1048576,
                    'self_alarm_seconds': 8, 'a1_after_ack_ms': 750}:
        raise ValueError('unknown helper contract')
    record = protocol_record(args, os.getpid(), os.environ, hashlib.sha256(body).hexdigest())
    deadline = time.monotonic() + 7
    payload = bytearray(1048576)
    for index in range(0, len(payload), 4096):
        payload[index] = 1
    with (output / 'helper-ready.json').open('x') as stream:
        json.dump(record, stream)
    print('ACTOR_CPU_READY ' + args.case, flush=True)
    if args.case == 'a1':
        ack = output / 'sample-ack.json'
        while not ack.exists():
            if time.monotonic() >= deadline:
                raise RuntimeError('first sample was not acknowledged')
            time.sleep(.025)
        if ack.is_symlink() or ack.stat().st_size > 4096:
            raise ValueError('ack path/size')
        value = json.loads(ack.read_text())
        verify_sample_ack(output, value, os.getpid(), os.getppid())
        time.sleep(.75)
        print('ACTOR_CPU_DONE a1', flush=True)
    else:
        while time.monotonic() < deadline:
            time.sleep(.025)
        raise RuntimeError('A2 planned revocation never arrived')


if __name__ == '__main__':
    main()
