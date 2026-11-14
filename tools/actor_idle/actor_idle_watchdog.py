"""Actor-only Python adapter. Import/check never creates a process or samples a host.

Native execution needs a new exact-plan grant and memory recovery. Shared lifecycle,
process identity, cleanup and sampler implementations retain their qualification pins.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import sys
import time

from actor_idle_tools import workspace_root, load_json, need, sha, shared
from actor_idle_oracle import check

GIB = 1024**3
# INTERIOR's qualification allows up to 25 seconds for one shared cleanup call.
# Keep another five seconds inside the nominal campaign budget for receipt handling.
CLEANUP_RESERVE_SECONDS = 30
control = shared('geometry_watchdog')
materials = shared('materials_process')
lifecycle = shared('lifecycle')
lease_control = shared('lease_admission')
RUST_SUFFIXES = ('.rs', '.toml', '.lock', '.wgsl', '.json', '.ron')


def check_admission(plan, plan_hash, admission, ledger, now):
    need(plan['version'] == 2 and plan['runnable'] is True, 'closed preparation template')
    need(plan['memory_gate']['status'] == 'admitted', 'native memory block remains')
    need(admission['memory_recovery_sha256'] == plan['memory_gate']['sha256']
         and isinstance(plan['memory_gate']['sha256'], str)
         and re.fullmatch('[a-f0-9]{64}', plan['memory_gate']['sha256']), 'named memory recovery proof required')
    need(admission['plan_sha256'] == plan_hash, 'admission plan mismatch')
    need(admission['gpu_owner'] == 'animals' and admission['grant_reference'].strip(), 'named ANIMALS GPU grant required')
    need(now < admission['expires_epoch'] <= now + 1800, 'expired/overlong admission')
    need(admission['other_gpu_campaigns'] == 0, 'another GPU campaign declared')
    need(ledger['resourcePolicy']['gpuOwner'] == 'animals', 'ANIMALS GPU ownership missing/revoked')
    need(plan['case_names'] and len(set(plan['case_names'])) == len(plan['case_names']) <= 3, 'case denominator')
    need(all(re.fullmatch('[a-z0-9-]{1,48}', name) for name in plan['case_names']), 'case/output key')
    need(plan['wall_seconds'] == 180 and plan['per_child_seconds'] == 60, 'unreviewed wall limits')


def held_leases(storage):
    return lease_control.held_build_leases(Path(storage) / 'locks')


def live_guard(plan, plan_path, admission, owned=None):
    need(sha(plan_path) == admission['plan_sha256'], 'plan changed during campaign')
    check_admission(plan, admission['plan_sha256'], admission, load_json(plan['ledger']), time.time())
    storage = Path(os.environ.get('MUDCRAB_STORAGE', ''))
    lease = Path(os.environ.get('MUDCRAB_BUILD_LEASE', ''))
    need(storage.is_absolute() and storage.resolve() == workspace_root().resolve(), 'storage root mismatch')
    lease_control.verify_lease_admission(storage, lease, Path(plan['checkout']), plan['head'])
    need(shutil.disk_usage(storage).free >= 20 * GIB, 'disk reserve')
    rows = materials.processes()  # Exact qualified lstart/PID/RSS parser and backend.
    own = owned.child.pid if owned is not None and owned.child is not None else None
    for pid, row in rows.items():
        name = Path(row['command']).name
        if name == 'actor_idle_probe' or control.GAME.fullmatch(name):
            need(pid == own and owned.known.get(pid) == row['birth'], 'existing/unowned game or fixture; leave untouched')
        need(name not in ('cargo', 'rustc'), 'active compiler; request a quiet native lane')
    return rows


def verify_compiled(plan):
    item = plan['compiled_inputs']
    need(sha(item['path']) == item['sha256'], 'compiled input manifest changed')
    manifest = load_json(item['path'])
    repo = Path(plan['checkout'])
    need(repo.is_absolute() and manifest['checkout'] == str(repo) and manifest['head'] == plan['head'], 'compiled owner')
    need(control.command(['git', 'rev-parse', 'HEAD'], repo).strip() == manifest['head'], 'HEAD changed')
    for name, digest in manifest['files'].items():
        key = Path(name)
        need(not key.is_absolute() and '..' not in key.parts and key.suffix in RUST_SUFFIXES, 'compiled input path')
        need(sha(repo / name) == digest, 'compiled input changed: ' + name)
    names = control.command(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], repo).split('\0')
    actual = {n for n in names if n and Path(n).suffix in RUST_SUFFIXES}
    need(actual == set(manifest['files']), 'compiled input inventory changed')
    return repo


def verify_python(plan):
    item = plan['controller']
    need(sha(item['path']) == item['sha256'], 'Python controller manifest changed')
    manifest = load_json(item['path'])
    need(manifest['version'] == 2 and str(Path(__file__).resolve()) in manifest['files'], 'Python entry identity')
    for name, digest in manifest['files'].items():
        path = Path(name)
        need(path.is_absolute() and not path.is_symlink() and sha(path) == digest, 'Python/helper input changed: ' + name)
    # Full shared-module file pins cover the function globals as well as their ASTs.
    for module in ('geometry_watchdog', 'geometry_oracle', 'materials_process', 'lifecycle', 'lease_admission'):
        shared(module)
    return manifest


def verify_inputs(plan, admission):
    repo = verify_compiled(plan)
    verify_python(plan)
    binary = Path(plan['binary']['path'])
    need(binary.is_absolute() and not binary.is_symlink() and os.access(binary, os.X_OK), 'retained executable required')
    need(sha(binary) == plan['binary']['sha256'] == admission['binary_sha256'], 'binary mismatch')
    proof = plan['binary']['build_receipt']
    need(sha(proof['path']) == proof['sha256'], 'build receipt changed')
    built = load_json(proof['path'])
    need(built['exit_code'] == 0 and built['example'] == 'actor_idle_probe'
         and built['head'] == plan['head'] and built['checkout'] == plan['checkout']
         and built['binary_sha256'] == plan['binary']['sha256']
         and built['compiled_inputs_sha256'] == plan['compiled_inputs']['sha256'], 'build/Rust provenance')
    # A later Python-only successor does not need a new Rust build/source receipt.
    need(sha(plan['contract']) == plan['contract_sha256'], 'contract changed')
    memory = plan['memory_gate']
    need(memory['status'] == 'admitted' and sha(memory['path']) == memory['sha256'], 'memory recovery proof changed')
    for path, digest in plan['assets'].items():
        need(Path(path).is_absolute() and sha(path) == digest, 'asset changed: ' + path)
    return binary, repo


def save(path, value):
    temporary = path.with_name(path.name + '.partial')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def run_case(binary, repo, name, directory, plan, backend, guard, campaign_deadline):
    """New actor wiring around exact qualified lifecycle/process/cleanup functions."""
    start = time.monotonic()
    allowance = min(60, campaign_deadline - start - CLEANUP_RESERVE_SECONDS)
    need(allowance > 0, 'no remaining child/cleanup budget')
    directory.mkdir()
    receipt = {'command': None, 'status': 'starting', 'samples': 0}
    owned = lifecycle.OwnedChild(materials)
    selector = None
    close_error = None
    try:
        owned.install()  # Install before arming or entering the Popen ownership interval.
        owned.arm(allowance)
        initial = backend.host()
        control.check_host(initial, initial['swap'], admission=True)
        guard(None)  # Final revocation/lease/process check before creating anything.
        args = [str(binary), '--assets', plan['assets_root'], '--contract', plan['contract'],
                '--case', name, '--output', str(directory)]
        receipt['command'] = args
        with (directory / 'engine.log').open('xb') as log, (directory / 'samples.jsonl').open('x') as samples:
            child = owned.spawn(args, cwd=repo, env=dict(os.environ, WGPU_BACKEND='metal', RUST_LOG='info',
                                MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256=plan['contract_sha256']),
                                start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, bufsize=0)
            receipt['pid'] = child.pid
            os.set_blocking(child.stdout.fileno(), False)
            selector = selectors.DefaultSelector()
            selector.register(child.stdout, selectors.EVENT_READ)
            total, tail = 0, b''
            while True:
                guard(owned)
                _, family = owned.observe()
                need(family <= {child.pid}, 'unexpected probe descendant')
                if selector.select(timeout=0):
                    data = os.read(child.stdout.fileno(), 65536)
                    total += len(data)
                    log.write(data[:max(0, 16*1024**2 - log.tell())])
                    need(not control.FAULT.search(tail + data), 'renderer fault')
                    tail = (tail + data)[-256:]
                elapsed = time.monotonic() - start
                if child.poll() is not None:
                    need(child.returncode == 0, 'probe exit ' + str(child.returncode))
                    while True:
                        try:
                            data = os.read(child.stdout.fileno(), 65536)
                        except BlockingIOError:
                            raise ValueError('probe left an inherited pipe open')
                        if not data:
                            break
                        total += len(data)
                        log.write(data[:max(0, 16*1024**2 - log.tell())])
                        control.check_sample(time.monotonic()-start, {'rss': 0, 'footprint': 0}, total)
                        need(not control.FAULT.search(tail + data), 'renderer fault at exit')
                        tail = (tail + data)[-256:]
                    need(receipt['samples'] > 0, 'no process memory sample')
                    control.check_sample(time.monotonic()-start, {'rss': 0, 'footprint': 0}, total)
                    control.check_host(backend.host(), initial['swap'])
                    break
                need(child.pid in family, 'live probe identity missing')
                own, controller = backend.memory(child.pid), backend.memory(os.getpid())
                aggregate = {key: own[key] + controller[key] for key in ('rss', 'footprint')}
                control.check_sample(elapsed, aggregate, total)
                host = backend.host()
                control.check_host(host, initial['swap'])
                samples.write(json.dumps({'seconds': elapsed, 'child': own, 'controller': controller, 'host': host}) + '\n')
                samples.flush()
                receipt['samples'] += 1
                time.sleep(.25)
        receipt['status'] = 'readback-pending-oracle'
    except BaseException as exc:
        receipt.update(status='failed', reason=str(exc) or type(exc).__name__)
        raise
    finally:
        # Shared cleanup masks all four handled signals, including SIGALRM. Never retry it.
        owned.mask_cleanup()
        if selector is not None:
            try:
                selector.close()
            except BaseException as exc:
                close_error = exc
                receipt.update(status='failed', selector_error=str(exc))
        try:
            need(not (owned.attempted and owned.child is None), 'ambiguous Popen result; keep foreground lease')
            owned.cleanup()
        except BaseException as exc:
            receipt.update(status='cleanup-failed', cleanup_error=str(exc), ownership=owned.description())
            # Exact shared passive hold never re-signals or silently releases the foreground lease.
            owned.hold(lambda: save(directory / 'control.json', receipt))
            raise RuntimeError('passive hold unexpectedly returned')
        receipt.update(exit_code=owned.child.returncode if owned.child is not None else None,
                       elapsed_seconds=time.monotonic()-start, ownership=owned.description())
        try:
            save(directory / 'control.json', receipt)
        except BaseException as exc:
            receipt.update(status='receipt-failed', receipt_error=str(exc))
            owned.hold(lambda: save(directory / 'lease-hold.json', receipt))
            raise RuntimeError('passive hold unexpectedly returned')
        owned.restore()
        if close_error is not None:
            raise close_error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plan', type=Path, required=True)
    parser.add_argument('--admission', type=Path, required=True)
    args = parser.parse_args()
    plan, admission = load_json(args.plan), load_json(args.admission)
    # Closed plans and memory blocks reject before any host/process sampling or signal installation.
    check_admission(plan, sha(args.plan), admission, load_json(plan['ledger']), time.time())
    root = Path(plan['output'])
    need(root.is_absolute() and not root.exists() and root.parent.resolve().is_relative_to((workspace_root()/'evidence').resolve()), 'new owned evidence directory required')
    root.mkdir()
    result = {'status': 'failed', 'plan_sha256': sha(args.plan), 'cases': []}
    deadline = time.monotonic() + 180
    previous = {sig: lifecycle.signal.getsignal(sig) for sig in lifecycle.HANDLED}
    for sig in lifecycle.HANDLED:
        lifecycle.signal.signal(sig, control.deadline_signal)
    lifecycle.signal.setitimer(lifecycle.signal.ITIMER_REAL, 180-CLEANUP_RESERVE_SECONDS)
    try:
        binary, repo = verify_inputs(plan, admission)
        backend = control.Darwin()
        live_guard(plan, args.plan, admission)
        for name in plan['case_names']:
            verify_inputs(plan, admission)
            live_guard(plan, args.plan, admission)
            run_case(binary, repo, name, root / name, plan, backend,
                     lambda owned: live_guard(plan, args.plan, admission, owned), deadline)
            lifecycle.signal.setitimer(lifecycle.signal.ITIMER_REAL,
                                      max(.001, deadline-time.monotonic()-CLEANUP_RESERVE_SECONDS))
            result['cases'].append(check(plan['contract'], name, root / name))
            verify_inputs(plan, admission)
            live_guard(plan, args.plan, admission)
        result['status'] = 'selected-actor-fixtures-passed-world-acceptance-unverified'
    except BaseException as exc:
        result['reason'] = str(exc) or type(exc).__name__
        raise
    finally:
        lifecycle.signal.setitimer(lifecycle.signal.ITIMER_REAL, 0)
        for sig, handler in previous.items():
            lifecycle.signal.signal(sig, handler)
        result['elapsed_seconds'] = 180 - (deadline-time.monotonic())
        save(root / 'campaign.json', result)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, RuntimeError, OSError, KeyError, control.Refused, subprocess.SubprocessError) as exc:
        print('REFUSED/FAILED: ' + str(exc), file=sys.stderr)
        raise SystemExit(1)
