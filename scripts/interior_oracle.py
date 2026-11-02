#!/usr/bin/env python3
"""Exact-source controller for the separately admitted, asset-free interior Metal oracle.

Reuse pinned GEOMETRY sampling and MATERIALS process identity/cleanup in place.
`check` is source-only. `seal` needs a successful admitted build. Legacy `run` is disabled.
"""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time

GIB = 1024 ** 3
TEST = 'world::environment::interior::native_tests::interior_transition_hdr_readback'


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def save(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def helpers(packet):
    loaded = {}
    for name, item in packet['tooling'].items():
        require(sha(item['path']) == item['sha256'], 'shared helper changed: ' + name)
        spec = importlib.util.spec_from_file_location('interior_reuse_' + name, item['path'])
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        loaded[name] = module
    return loaded['geometry'], loaded['materials']


def source(packet):
    repo = Path(packet['checkout'])
    geo, materials = helpers(packet)
    require(geo.command(['git', 'rev-parse', 'HEAD'], repo).strip() == packet['head'], 'HEAD changed')
    require(sha(__file__) == packet['files']['scripts/interior_oracle.py'], 'controller identity')
    for name, digest in packet['files'].items():
        require(sha(repo / name) == digest, 'source changed: ' + name)
    untracked = geo.command(['git', 'ls-files', '--others', '--exclude-standard'], repo).splitlines()
    require(all(name == 'AGENTS.md' or name in packet['files'] for name in untracked), 'unfrozen untracked file')
    require(hashlib.sha256(geo.source_patch(repo, packet['files'])).hexdigest() == packet['patch_sha256'], 'source patch changed')
    return geo, materials, repo


def caps(elapsed, rss, footprint, log_bytes):
    require(elapsed < 120, '120 second wall limit')
    require(rss < 4 * GIB, '4 GiB process-tree RSS limit')
    require(footprint < 6 * GIB, '6 GiB process-tree physical footprint limit')
    require(log_bytes < 16 * 1024 ** 2, '16 MiB log limit')


def held_build_leases(locks):
    """Count the kernel leases used by workspace.py, including isolated builds.

    lsof is an open-file inventory, not a flock inventory. Its directory scan can
    return status 1 and cannot establish which open descriptors hold a lease.
    Admission uses the same nonblocking flock operation as the workspace wrapper.
    Artifact retirement keeps the wrapper's separate lsof check.
    """
    require(locks.is_dir() and not locks.is_symlink(), 'regular lock directory required')
    held = set()
    for path in sorted(locks.glob('*.lock')):
        if not path.name.startswith(('isolated-', 'slot-')):
            continue
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        try:
            require(stat.S_ISREG(os.fstat(descriptor).st_mode), 'regular lease file required')
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                held.add(str(path))
            else:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)
    return held


def lease_capacity(held, own):
    require(str(own) in held and len(held) <= 2, 'global two-command lease capacity')


def check_lease_inventory():
    """Pure mocks cover held/free slots, isolated leases, and fail-closed I/O."""
    from types import SimpleNamespace
    from unittest.mock import patch
    names = [Path('/locks/allocation.lock'), Path('/locks/slot-0.lock'),
             Path('/locks/isolated-one.lock'), Path('/locks/isolated-two.lock')]
    directory = SimpleNamespace(is_dir=lambda: True, is_symlink=lambda: False,
                                glob=lambda pattern: names)
    descriptors = {str(path): index + 100 for index, path in enumerate(names)}
    busy = {descriptors[str(names[1])], descriptors[str(names[2])]}

    def open_lock(path, flags):
        require(flags & os.O_NOFOLLOW, 'lease symlink rejection required')
        return descriptors[str(path)]

    def flock(descriptor, flags):
        if flags != fcntl.LOCK_UN and descriptor in busy:
            raise BlockingIOError()

    with patch.object(os, 'open', side_effect=open_lock), patch.object(os, 'close') as close, \
            patch.object(os, 'fstat', return_value=SimpleNamespace(st_mode=stat.S_IFREG)), \
            patch.object(fcntl, 'flock', side_effect=flock):
        held = held_build_leases(directory)
        require(held == {str(names[1]), str(names[2])} and close.call_count == 3,
                'held/free isolated and slot inventory')
        lease_capacity(held, names[2])
        for rejected, own in [(held | {str(names[3])}, names[2]), (held, names[3])]:
            try:
                lease_capacity(rejected, own)
            except RuntimeError:
                pass
            else:
                raise RuntimeError('invalid capacity admitted')
        busy.clear()
        require(not held_build_leases(directory), 'free leases must not count')
    with patch.object(os, 'open', side_effect=PermissionError('mock unreadable lock')):
        try:
            held_build_leases(directory)
        except PermissionError:
            pass
        else:
            raise RuntimeError('lock scan I/O error was suppressed')


def check(packet):
    _, materials, _ = source(packet)
    caps(119, 4 * GIB - 1, 6 * GIB - 1, 16 * 1024 ** 2 - 1)
    for values in [(120, 0, 0, 0), (0, 4 * GIB, 0, 0), (0, 0, 6 * GIB, 0), (0, 0, 0, 16 * 1024 ** 2)]:
        try:
            caps(*values)
        except RuntimeError:
            pass
        else:
            raise RuntimeError('cap boundary did not fail closed')
    rows = materials.parse_processes('10 1 10 100 Sun Nov 1 06:00:00 2026 engine-test\n11 10 11 20 Sun Nov 1 06:00:01 2026 child')
    require(materials.family(rows, 10, {10: rows[10]['birth']}) == {10, 11}, 'escaped descendant identity')
    require(not materials.family(rows, 10, {10: 'different birth'}), 'PID reuse rejection')
    check_lease_inventory()
    print('Source identities, seven pure control checks and five lease-inventory checks passed. No native process launched.')


def seal(args, packet):
    geo, _, repo = source(packet)
    events = [json.loads(line) for line in args.cargo_log.read_text().splitlines() if line.startswith('{')]
    require(args.build_exit == 0 and any(e.get('reason') == 'build-finished' and e['success'] for e in events), 'successful build receipt required')
    binaries = {e['executable'] for e in events if e.get('reason') == 'compiler-artifact'
                and e.get('executable') and e['target']['name'] == 'engine'
                and 'lib' in e['target']['kind'] and e['profile']['test']}
    require(len(binaries) == 1, 'one engine library test executable required')
    binary = Path(binaries.pop()).resolve()
    lease = lease_check(packet, geo, repo)
    require(binary.is_relative_to(lease), 'binary outside owned cache')
    destination = args.receipt.parent / 'interior-engine-tests'
    require(not destination.exists(), 'retained binary already exists')
    shutil.copy2(binary, destination)
    require(sha(destination) == sha(binary), 'binary copy mismatch')
    save(args.receipt, dict(head=packet['head'], source_sha256=sha(args.packet),
                           cargo_log=str(args.cargo_log.resolve()), cargo_log_sha256=sha(args.cargo_log),
                           exit_code=0, binary=str(destination.resolve()), binary_sha256=sha(destination)))


def lease_check(packet, geo, repo):
    require(float(os.environ.get('MUDCRAB_BUILD_BUDGET_GIB', '60')) == 60
            and float(os.environ.get('MUDCRAB_RESERVE_GIB', '20')) == 20, 'unchanged storage limits required')
    require(os.environ.get('CARGO_INCREMENTAL') == '0', 'isolated validation required')
    lease = Path(os.environ.get('MUDCRAB_BUILD_LEASE', ''))
    storage = Path(os.environ.get('MUDCRAB_STORAGE', ''))
    require(lease.is_absolute() and storage.is_absolute() and lease.parent == storage / 'builds', 'managed foreground lease required')
    marker = json.loads((lease / 'build.json').read_text())
    require(marker['checkout'] == str(repo) and marker['commit'] == packet['head'], 'lease identity')
    lock_path = storage / 'locks' / (lease.name + '.lock')
    with lock_path.open('rb') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            pass
        else:
            fcntl.flock(lock, fcntl.LOCK_UN)
            raise RuntimeError('foreground lease not held')
    lease_capacity(held_build_leases(storage / 'locks'), lock_path)
    require(shutil.disk_usage(storage).free >= 20 * GIB, '20 GiB disk reserve')
    return lease


def grant_check(args, packet):
    admission = json.loads(args.admission.read_text())
    require(admission['source_sha256'] == sha(args.packet), 'grant/source mismatch')
    require(admission['gpu_owner'] == 'interior' and admission['grant_reference'].strip(), 'named interior GPU grant required')
    require(time.time() < admission['expires_epoch'] <= time.time() + 3600, 'grant expired or exceeds one hour')
    require(admission['other_gpu_campaigns'] == 0, 'another GPU campaign declared')
    ledger = json.loads(Path(packet['ledger']).read_text())
    require(ledger['resourcePolicy']['gpuOwner'] == 'interior', 'live GPU owner is not interior')
    return admission


def run(args, packet):
    raise RuntimeError(
        'Legacy interior GPU launcher is disabled. Use a separately frozen successor '
        'with a new exact-source native grant.'
    )

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--packet', type=Path, required=True)
    sub = parser.add_subparsers(dest='action', required=True)
    sub.add_parser('check')
    seal_parser = sub.add_parser('seal')
    seal_parser.add_argument('--cargo-log', type=Path, required=True)
    seal_parser.add_argument('--build-exit', type=int, required=True)
    seal_parser.add_argument('--receipt', type=Path, required=True)
    run_parser = sub.add_parser('run')
    run_parser.add_argument('--admission', type=Path, required=True)
    run_parser.add_argument('--receipt', type=Path, required=True)
    run_parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    packet = json.loads(args.packet.read_text())
    if args.action == 'check':
        check(packet)
    elif args.action == 'seal':
        seal(args, packet)
    else:
        run(args, packet)


if __name__ == '__main__':
    sys.dont_write_bytecode = True
    main()
