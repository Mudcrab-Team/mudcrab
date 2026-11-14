"""Exactly A1/A2 CPU binding qualification. Closed authorization performs no host/control work."""
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import sys
import time

GIB = 1024**3
MIB = 1024**2
BOUNDS = {'nominal_seconds': 90, 'per_interval_seconds': 45, 'active_seconds': 15,
          'cleanup_reserve_seconds': 30, 'cleanup_abort_seconds': 25,
          'helper_groups': 2, 'simultaneous_helpers': 1, 'helper_payload_bytes': MIB,
          'helper_alarm_seconds': 8, 'a1_after_ack_ms': 750,
          'helper_rss_bytes': 64*MIB, 'helper_footprint_bytes': 128*MIB,
          'controller_rss_bytes': 256*MIB, 'controller_footprint_bytes': 512*MIB,
          'host_admission_bytes': 4*GIB, 'host_abort_bytes': 2*GIB,
          'swap_growth_bytes': 256*MIB, 'log_bytes': 16384, 'sample_bytes': 262144,
          'output_bytes': MIB, 'growth_gib': .05, 'disk_reserve_gib': 20,
          'build_budget_gib': 60, 'managed_commands': 2,
          'serial_telemetry_processes': 1, 'peak_processes_including_wrapper': 4,
          'estimated_campaign_rss_bytes': 512*MIB, 'estimated_campaign_footprint_bytes': GIB}
REVOCATION = 'planned A2 actor guard revocation after first sample'


class Refused(RuntimeError):
    pass


class PlannedRevocation(ValueError):
    pass


def need(ok, message):
    if not ok:
        raise Refused(message)


def sha(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(MIB), b''):
            h.update(block)
    return h.hexdigest()


def read_json(path, cap=2*MIB):
    path = Path(path)
    need(not path.is_symlink() and path.is_file() and path.stat().st_size <= cap, 'JSON path/size')
    def pairs(items):
        result = {}
        for key, value in items:
            need(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    def nonfinite(value):
        raise Refused('nonfinite JSON')
    return json.loads(path.read_text(), object_pairs_hook=pairs, parse_constant=nonfinite)


def save(path, value):
    temporary = path.with_name(path.name + '.partial')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def authorize(plan, plan_hash, manifest_hash, auth, ledger, now, initial=True):
    need(auth.get('authorized') is True and auth.get('action') == 'animals_actor_adapter_cpu_qualification',
         'closed CPU authorization')
    need(plan['version'] == 1 and plan['cases'] == ['a1', 'a2'] and plan['bounds'] == BOUNDS, 'case/bounds changed')
    need(auth['plan_sha256'] == plan_hash and auth['qualification_manifest_sha256'] == manifest_hash, 'authorization identity')
    need(auth['grant_reference'].strip(), 'named CPU grant required')
    margin = BOUNDS['nominal_seconds'] if initial else BOUNDS['cleanup_reserve_seconds']
    need(now + margin < auth['expires_epoch'] <= now + 1800, 'CPU grant expiry/cleanup margin')
    recovery = auth['host_recovery']
    need(recovery['meaningful_change'].strip() and recovery['reference'].strip()
         and recovery['confirmed_epoch'] >= plan['prepared_epoch']
         and 0 <= now-recovery['confirmed_epoch'] <= 1800, 'CPU host recovery is not confirmed')
    policy = ledger['resourcePolicy']
    grant = policy['cpuControlGrant']
    need(grant.get('executionAuthorized') is True and grant['owner'] == 'animals'
         and grant['name'] == auth['grant_reference'] and grant['planSha256'] == plan_hash, 'live CPU grant missing/revoked')
    need(policy['maxManagedCommands'] == 2 and policy['buildBudgetGiB'] == 60 and policy['reserveGiB'] == 20,
         'resource policy changed')
    need(policy['gpuOwner'] is None, 'another GPU campaign is admitted')


class CpuPolicy:
    """Explicit CPU execution context. Actual measurements are never replaced by invented values."""
    def __init__(self, native):
        self.FAULT = native.FAULT
    @staticmethod
    def check_host(host, initial_swap, admission=False):
        floor = BOUNDS['host_admission_bytes'] if admission else BOUNDS['host_abort_bytes']
        need(host['available'] >= floor, 'CPU host floor')
        need(host['swap']-initial_swap < BOUNDS['swap_growth_bytes'], 'CPU swap growth')
    @staticmethod
    def check_sample(elapsed, memory, log_bytes):
        need(elapsed < BOUNDS['active_seconds'], 'CPU active deadline')
        need(memory['rss'] < BOUNDS['helper_rss_bytes']+BOUNDS['controller_rss_bytes'], 'CPU aggregate RSS')
        need(memory['footprint'] < BOUNDS['helper_footprint_bytes']+BOUNDS['controller_footprint_bytes'], 'CPU aggregate footprint')
        need(log_bytes < BOUNDS['log_bytes'], 'CPU log cap')


@contextmanager
def cpu_context(adapter):
    previous = adapter.control
    adapter.control = CpuPolicy(previous)
    try:
        yield
    finally:
        adapter.control = previous


class CpuSampler:
    def __init__(self, actual):
        self.actual = actual
        self.initial_swap = None
    def host(self):
        value = self.actual.host()
        if self.initial_swap is None:
            self.initial_swap = value['swap']
        need(value['swap']-self.initial_swap < BOUNDS['swap_growth_bytes'], 'CPU campaign swap growth')
        return value
    def memory(self, pid):
        result = self.actual.memory(pid)
        prefix = 'controller' if pid == os.getpid() else 'helper'
        need(0 < result['rss'] < BOUNDS[prefix+'_rss_bytes']
             and result['footprint'] < BOUNDS[prefix+'_footprint_bytes'], 'CPU '+prefix+' memory')
        return dict(result, pid=pid)


def import_file(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_adapter(manifest):
    for path, digest in manifest['files'].items():
        need(Path(path).is_absolute() and not Path(path).is_symlink() and sha(path) == digest, 'qualification source changed')
    for name in ('actor_idle_tools', 'actor_idle_oracle'):
        if name in sys.modules:
            path = str(Path(sys.modules[name].__file__).resolve())
            need(path in manifest['files'] and sha(path) == manifest['files'][path], 'preloaded module identity')
    sys.path.insert(0, str(Path(manifest['controller_entrypoint']).parent))
    adapter = import_file(manifest['controller_entrypoint'], 'actor_cpu_qualified_adapter')
    need(sha(manifest['controller_manifest']['path']) == manifest['controller_manifest']['sha256'], 'frozen controller manifest changed')
    need(adapter.CLEANUP_RESERVE_SECONDS == BOUNDS['cleanup_reserve_seconds'], 'cleanup context mismatch')
    return adapter


def first_sample(directory, pid):
    path = directory/'samples.jsonl'
    if not path.exists() or path.stat().st_size == 0:
        return None
    need(not path.is_symlink() and path.stat().st_size <= BOUNDS['sample_bytes'], 'sample log cap')
    lines = path.read_bytes().splitlines(keepends=True)
    if not lines or not lines[0].endswith(b'\n'):
        return None
    raw = lines[0].rstrip(b'\r\n')
    value = json.loads(raw)
    need(value['child']['pid'] == pid and value['controller']['pid'] == os.getpid(), 'CPU sample identity')
    need(value['child']['rss'] > 0 and value['controller']['rss'] > 0, 'empty CPU sample')
    return hashlib.sha256(raw).hexdigest()


def helper_protocol(directory, name, owned, plan):
    path = directory/'helper-ready.json'
    if not path.exists():
        return None
    ready = read_json(path, 16384)
    expected = ['--assets', plan['assets_root'], '--contract', plan['contract'],
                '--case', name, '--output', str(directory)]
    need(ready['scope'] == 'cpu-helper-only' and ready['case'] == name and ready['pid'] == owned.child.pid,
         'helper identity')
    need(ready['argv'] == expected and ready['assets_opened'] is False and ready['payload_bytes'] == MIB,
         'helper argv/output/payload wiring')
    need(ready['contract_sha256'] == plan['contract_sha256']
         and ready['environment'] == {'WGPU_BACKEND':'metal', 'RUST_LOG':'info',
                                      'MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256':plan['contract_sha256']}, 'helper environment wiring')
    need(ready['python_executable'] == plan['interpreter'] and ready['python_version'] == sys.version, 'helper interpreter')
    return ready


def run_binding_case(adapter, backend, plan, name, directory, guard, deadline):
    seen = []
    acked = False
    last_guard = None
    def actor_guard(owned):
        nonlocal acked, last_guard
        guard(owned)
        if owned is None:
            return
        last_guard = time.monotonic()
        if not seen:
            seen.append(owned)
        need(seen[0] is owned and owned.known and not owned.cleanup_attempted, 'ownership interval changed')
        sample = first_sample(directory, owned.child.pid)
        ready = helper_protocol(directory, name, owned, plan)
        if sample and ready:
            if name == 'a2':
                raise PlannedRevocation(REVOCATION)
            if not acked:
                with (directory/'sample-ack.json').open('x') as stream:
                    json.dump({'case':'a1','pid':owned.child.pid,'sample_sha256':sample}, stream)
                acked = True
    expected_failure = False
    with cpu_context(adapter):
        try:
            adapter.run_case(Path(plan['helper']), Path(plan['checkout']), name, directory,
                             plan, backend, actor_guard, min(deadline, time.monotonic()+45))
        except PlannedRevocation as exc:
            need(name == 'a2' and str(exc) == REVOCATION, 'unexpected revocation')
            expected_failure = True
    need(len(seen) == 1 and (name != 'a1' or acked), 'helper/sample was not bound')
    # Includes final observation/drain as well as cleanup; this is a conservative upper bound.
    post_guard_seconds = time.monotonic()-last_guard
    need(post_guard_seconds < BOUNDS['cleanup_abort_seconds'], 'post-guard/cleanup elapsed limit')
    owned = seen[0]
    receipt = read_json(directory/'control.json')
    ready = helper_protocol(directory, name, owned, plan)
    need(ready is not None and receipt['samples'] >= 1 and owned.cleanup_attempted, 'receipt/sample/cleanup incomplete')
    need(receipt['command'] == [plan['helper']]+ready['argv'], 'recorded command mismatch')
    need(receipt['ownership']['cleanup_attempted'] is True and receipt['ownership']['spawning'] is False, 'cleanup state')
    need(receipt['exit_code'] == (0 if name == 'a1' else -signal.SIGTERM), 'helper exit contract')
    if name == 'a1':
        need(not expected_failure and receipt['status'] == 'readback-pending-oracle', 'A1 outcome')
    else:
        need(expected_failure and receipt['status'] == 'failed' and receipt['reason'] == REVOCATION, 'A2 outcome')
    _, remaining = owned.observe()
    if remaining:
        owned.hold(lambda: save(directory/'lease-hold.json', {'reason':'post-case survivor','ownership':owned.description()}))
        raise Refused('passive hold unexpectedly returned')
    need(Path(directory/'engine.log').stat().st_size < BOUNDS['log_bytes'], 'helper log cap')
    return {'id':name.upper(), 'status':'passed_cpu_process_protocol_only', 'adapter_outcome':receipt['status'],
            'samples':receipt['samples'], 'ownership':receipt['ownership'], 'exit_code':receipt['exit_code'],
            'expected_failure':expected_failure, 'cleanup_attempted_once':True,
            'post_guard_cleanup_upper_bound_seconds':post_guard_seconds,
            'remaining_owned_processes':[], 'native_oracle_called':False,
            'receipt_sha256':sha(directory/'control.json'), 'helper_ready_sha256':sha(directory/'helper-ready.json')}


def two_cases(run_one, completed):
    results = []
    for name in ('a1','a2'):
        value = run_one(name)
        need(value['id'] == name.upper() and value['status'] == 'passed_cpu_process_protocol_only', 'case result identity')
        results.append(value)
        completed(list(results))
    return results


def output_bytes(root):
    total = 0
    for path in Path(root).rglob('*'):
        need(not path.is_symlink(), 'output symlink')
        if path.is_file():
            total += path.stat().st_size
            need(total <= BOUNDS['output_bytes'], 'CPU output cap')
    return total


def active_guard(plan, plan_path, manifest_hash, auth, auth_path, auth_hash, adapter, ledger_path, deadline, owned=None):
    need(time.monotonic() < deadline-30, 'qualification active budget exhausted')
    need(sha(plan_path) == auth['plan_sha256'] and sha(auth_path) == auth_hash, 'plan/authorization changed')
    need(sha(plan['qualification_manifest']) == manifest_hash, 'qualification manifest changed')
    manifest = read_json(plan['qualification_manifest'])
    need(all(sha(path) == digest for path,digest in manifest['files'].items()), 'source changed during qualification')
    ledger = read_json(ledger_path)
    authorize(plan, auth['plan_sha256'], manifest_hash, auth, ledger, time.time(), initial=False)
    storage = Path(os.environ.get('MUDCRAB_STORAGE',''))
    lease = Path(os.environ.get('MUDCRAB_BUILD_LEASE',''))
    need(str(storage) == plan['storage'] and os.environ.get('CARGO_INCREMENTAL') == '0', 'managed isolated context')
    need(float(os.environ.get('MUDCRAB_BUILD_BUDGET_GIB','60')) == 60
         and float(os.environ.get('MUDCRAB_RESERVE_GIB','20')) == 20, 'storage limits changed')
    adapter.lease_control.verify_lease_admission(storage, lease, Path(plan['checkout']), plan['head'])
    remaining = max(0, ledger['resourcePolicy'].get('combinedRemainingGrantedGrowthGiB',0))
    evidence = max(0, ledger['resourcePolicy'].get('separateEvidenceGrowthGiB',0))
    need(shutil.disk_usage(storage).free >= (20+remaining+evidence)*GIB, 'disk reserve/growth')
    output_bytes(plan['output'])
    rows = adapter.materials.processes()
    forbidden = ('engine','actor_idle_probe','converter','cluster_fixture','cargo','rustc')
    need(not any(Path(row['command']).name.lower() in forbidden
                 or Path(row['command']).name.lower().startswith(('skyrim','mudcrab')) for row in rows.values()),
         'existing build/native process; leave untouched')
    if owned is not None:
        need(owned.child.pid in owned.known, 'child identity missing')
    return rows


def execute(args):
    plan, manifest, auth = read_json(args.plan), read_json(args.manifest), read_json(args.authorization)
    plan_hash, manifest_hash, auth_hash = sha(args.plan), sha(args.manifest), sha(args.authorization)
    ledger = read_json(plan['ledger'])
    # A closed grant or absent meaningful recovery refuses before module import/sampling/signals.
    authorize(plan, plan_hash, manifest_hash, auth, ledger, time.time())
    need(manifest_hash == plan['qualification_manifest_sha256'] and sha(__file__) == manifest['driver_sha256'], 'driver/manifest changed')
    need(sys.executable == plan['interpreter'] and sys.version == manifest['python_version'], 'driver interpreter changed')
    need(all(os.environ.get(key) == value for key,value in plan['locale'].items()), 'qualified locale changed')
    need(sha(plan['helper']) == manifest['helper_sha256'] and os.access(plan['helper'],os.X_OK), 'helper source/mode')
    need(sha(plan['contract']) == plan['contract_sha256'], 'helper contract changed')
    need(Path(plan['assets_root']).name == 'no-assets' and not Path(plan['assets_root']).exists(), 'sentinel asset path')
    output = Path(plan['output'])
    need(output.is_absolute() and output.parent == Path(manifest['packet_dir'])
         and not output.exists() and not output.is_symlink(), 'new owned output required')
    adapter = load_adapter(manifest)
    output.mkdir()  # First admission/attempt is retained; never overwrite/retry it.
    result = {'status':'failed','plan_sha256':plan_hash,'qualification_manifest_sha256':manifest_hash,
              'controller_manifest_sha256':manifest['controller_manifest']['sha256'], 'cases':[],
              'attempted_cases':[], 'real_helper_attempts_max':2,'native_oracle_called':False}
    started = time.monotonic(); deadline = started+90
    previous = {}
    owned_last = None
    def checkpoint():
        result['elapsed_seconds'] = time.monotonic()-started
        result['recorded_helper_identities'] = [read_json(path)['ownership']
            for path in sorted(output.glob('*/control.json')) if path.is_file()]
        save(output/'receipt.json',result)
    try:
        checkpoint()
        previous = {sig:signal.getsignal(sig) for sig in adapter.lifecycle.HANDLED}
        def interrupted(signum, frame):
            raise Refused('qualification signal '+str(signum))
        for sig in previous:
            signal.signal(sig,interrupted)
        signal.setitimer(signal.ITIMER_REAL,60)
        guard = lambda owned=None: active_guard(plan,args.plan,manifest_hash,auth,args.authorization,auth_hash,
                                               adapter,plan['ledger'],deadline,owned)
        guard()
        need(sha(plan['workspace_wrapper']) == manifest['workspace_wrapper_sha256'], 'wrapper identity')
        workspace = import_file(plan['workspace_wrapper'], 'actor_cpu_workspace_inventory')
        inventory = workspace.inventory(Path(plan['storage'])/'builds',set())
        need(not inventory['errors'], 'build inventory errors')
        growth = ledger['resourcePolicy']['combinedRemainingGrantedGrowthGiB']
        need(inventory['allocated_bytes']+growth*GIB <= 60*GIB, 'managed build budget/reservations')
        backend = CpuSampler(adapter.control.Darwin())
        initial = backend.host()
        CpuPolicy.check_host(initial,initial['swap'],admission=True)
        result['initial_host'] = initial
        result['cpu_policy'] = BOUNDS
        def run_one(name):
            nonlocal owned_last
            guard()
            result['attempted_cases'].append(name.upper())
            checkpoint()
            def case_guard(owned):
                nonlocal owned_last
                if owned is not None:
                    owned_last = owned
                guard(owned)
            return run_binding_case(adapter,backend,plan,name,output/name,case_guard,deadline)
        def completed(cases):
            result['cases'] = cases
            signal.setitimer(signal.ITIMER_REAL,max(.001,deadline-time.monotonic()-30))
            checkpoint()
        two_cases(run_one,completed)
        need([case['id'] for case in result['cases']] == ['A1','A2'], 'qualification denominator')
        result['status'] = 'two_actor_cpu_process_protocol_cases_passed_only'
    except BaseException as exc:
        result['reason'] = str(exc) or type(exc).__name__
        raise
    finally:
        signal.setitimer(signal.ITIMER_REAL,0)
        try:
            checkpoint()
        except BaseException:
            holder = owned_last or adapter.lifecycle.OwnedChild(adapter.materials)
            holder.hold(lambda: save(output/'lease-hold.json',result))
            raise Refused('passive hold unexpectedly returned')
        for sig, handler in previous.items():
            signal.signal(sig,handler)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',type=Path,required=True)
    parser.add_argument('--plan',type=Path,required=True)
    parser.add_argument('--authorization',type=Path,required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(execute(args),indent=2))
    except (Refused,ValueError,TypeError,OSError,KeyError) as exc:
        print('REFUSED/FAILED: '+str(exc),file=sys.stderr)
        raise SystemExit(1)


if __name__ == '__main__':
    sys.dont_write_bytecode=True
    main()
