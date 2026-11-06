#!/usr/bin/env python3
"""Independent, sealed Skyrim v3 jump evaluation. No production physics imports."""
import argparse
import gzip
import hashlib
import json
import math
from pathlib import Path
import statistics
import struct

WIRE = struct.Struct('<4Q4I40f48s')
RUNTIME = '846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f'
RULES = {'window_game_seconds': 1.15, 'stationary_tolerance': 0.2,
         'minimum_rise': 10, 'landing_tolerance': 2, 'rmse_gate': 1,
         'peak_error_gate': 3, 'floor': 'idealized baseline clip',
         'prefix': 'discard initial non-first-person prefix only; require every pair thereafter',
         'launch': 'camera boundary preceding first nested air callback; phase unresolved'}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()


def check(ok, message):
    if not ok:
        raise ValueError(message)


def manifest(path):
    path = Path(path)
    m = json.loads(path.read_text())
    check(m.get('schema') == 1, 'manifest schema must be 1')
    check(m.get('runtime_sha256') == RUNTIME, 'unsupported runtime_sha256')
    ids, hashes = set(), set()
    for r in m['runs']:
        check(r['id'] not in ids and r['sha256'] not in hashes, 'duplicate run identity/hash')
        ids.add(r['id']); hashes.add(r['sha256'])
        check(r['split'] in ('train', 'heldout'), 'invalid split')
        check(r['frame_cap'] in (30, 60, 120) and r['time_multiplier'] in (1, .5), 'invalid condition')
        check(not Path(r['path']).is_absolute(), 'trace path must be relative')
        check('..' not in Path(r['path']).parts, 'trace path escapes corpus')
        if 'proof' in r:
            check(r['proof']['exe_sha256'] == RUNTIME, 'run runtime differs from manifest')
    return m, path.parent


def decode(path, expected):
    data = gzip.open(path, 'rb').read() if str(path).endswith('.gz') else Path(path).read_bytes()
    check(digest(data) == expected, 'trace hash mismatch')
    check(data and len(data) % WIRE.size == 0, 'truncated trace')
    rows = []
    prior_qpc = 0
    input_sequence = 0
    for i, raw in enumerate(WIRE.iter_unpack(data), 1):
        seq, qpc, sample, inp, kind, thread, code, state = raw[:8]
        v, label = raw[8:48], raw[48].split(b'\0')[0].decode('ascii', 'replace')
        check(seq == i, 'dropped/reordered record sequence')
        check(qpc >= prior_qpc, 'nonmonotonic QPC')
        prior_qpc = qpc
        if kind == 2:
            input_sequence += 1
        check(inp == input_sequence, 'invalid input association')
        check(kind in range(1, 10), 'unknown record kind')
        check(all(math.isfinite(x) for x in v), 'nonfinite consumed field')
        check(kind != 4 or (code == 0 and label == 'hooks installed'), 'recorder diagnostic error')
        rows.append({'qpc': qpc, 'sample': sample, 'kind': kind, 'code': code,
                     'state': state, 'v': v, 'label': label})
    check(rows[0]['kind'] == 1 and rows[0]['code'] == 3 and
          rows[0]['label'] == 'mudcrab-motion-256-v3', 'requires v3 header')
    check(rows[-1]['kind'] == 5 and sum(r['kind'] == 5 for r in rows) == 1, 'missing/invalid footer')
    check(sum(r['kind'] == 1 for r in rows) == 1, 'duplicate header')
    frequency = rows[0]['sample']
    check(frequency > 0, 'invalid QPC frequency')
    cameras, springs, chars = {}, {}, {}
    for r in rows:
        k, s = r['kind'], r['sample']
        target = {3: cameras, 6: springs, 7: chars}.get(k)
        if target is not None:
            check(s not in target, 'duplicate camera pair')
            target[s] = r
        if k in (8, 9):
            check(r['code'] == 3 and r['v'][2] > 0 and r['v'][20] > 0,
                  'invalid callback snapshot')
    check(list(cameras) == list(range(1, len(cameras) + 1)), 'dropped camera sample')
    check(cameras.keys() == chars.keys(), 'missing camera/character pair')
    valid_start = next((s for s in cameras if s in springs and springs[s]['code']==3 and cameras[s]['state']==0), None)
    check(valid_start is not None, 'no valid first person prefix boundary')
    selected = {s:r for s,r in cameras.items() if s >= valid_start}
    check(set(selected) <= springs.keys(), 'missing camera/spring pair after first person boundary')
    samples, camera_time, controller_time = [], 0., 0.
    for s, r in selected.items():
        spring, char = springs[s], chars[s]
        check(r['code'] & 1 and r['state'] == 0 and spring['code'] == 3 and char['code'] == 1,
              'invalid camera/spring/character snapshot')
        dt = spring['v'][32]
        check(0 < dt <= .25 and abs(dt - spring['v'][17]) < 1e-6, 'invalid paired camera dt')
        controller_dt = char['v'][2]
        check(0 < controller_dt <= .25 and abs(controller_dt * char['v'][3] - 1) < .001,
              'invalid controller dt/reciprocal')
        camera_time += dt
        controller_time += controller_dt
        samples.append({'sample': s, 'qpc': (r['qpc'] - rows[0]['qpc']) / frequency,
                        'camera': camera_time, 'z': r['v'][2], 'dt': dt,
                        'controller': controller_time, 'controller_dt': controller_dt,
                        'proxy_z': char['v'][0], 'scale': char['v'][21]})
    check(len(samples) >= 20, 'insufficient camera samples')
    check(all(b['qpc'] > a['qpc'] for a, b in zip(samples, samples[1:])), 'nonincreasing camera QPC')
    check(rows[-1]['sample'] == samples[-1]['sample'], 'footer sample mismatch')
    return rows, samples, frequency


def qualify(path, run):
    rows, samples, frequency = decode(path, run['sha256'])
    jump = [r for r in rows if r['kind'] == 2 and r['label'] == 'Jump' and r['state'] == 0]
    presses = [r for r in jump if r['v'][0] > 0 and r['v'][1] == 0]
    releases = [r for r in jump if r['v'][0] == 0 and r['v'][1] > 0]
    check(len(presses) == len(releases) == 1 and presses[0]['qpc'] < releases[0]['qpc'],
          'requires one delivered Jump press/release')
    press_sample = presses[0]['sample']
    press_time = (presses[0]['qpc'] - rows[0]['qpc']) / frequency
    samples = [s for s in samples if s['qpc'] >= press_time - .75]
    check(all(b['qpc'] - a['qpc'] < .15 for a,b in zip(samples,samples[1:])),
          'camera gap in jump window')
    pre = [s for s in samples if s['sample'] <= press_sample][-15:]
    check(len(pre) >= 5 and max(s['z'] for s in pre) - min(s['z'] for s in pre) <= .2,
          'missing stationary preinput baseline')
    baseline = statistics.median(s['z'] for s in pre)
    air = [r for r in rows if r['kind'] == 8 and r['v'][16] == 2 and r['sample'] >= press_sample]
    launches = [r for r in rows if r['kind'] == 9 and r['sample'] >= press_sample]
    check(air and len(launches) == 1, 'requires exactly one direct native launch event')
    first_air = air[0]
    # Air emits before the enclosing launch callback. Require that nesting order.
    check(first_air['qpc'] <= launches[0]['qpc'] and first_air['sample'] == launches[0]['sample'],
          'first air callback not nested in launch')
    check(launches[0]['v'][16] == 1, 'launch callback prestate must be jumping')
    origin_index = next((i for i,s in enumerate(samples) if s['sample']==first_air['sample']), None)
    check(origin_index is not None, 'launch boundary outside qualified camera region')
    origin = samples[origin_index]
    tail = samples[origin_index:]
    peak = max(s['z'] for s in tail)
    check(peak - baseline >= 10, 'no clear jump')
    apex_i = max(range(len(tail)), key=lambda i: tail[i]['z'])
    check(0 < apex_i < len(tail) - 3, 'no interior apex')
    check(all(abs(s['z'] - baseline) <= 2 for s in tail[-3:]), 'missing settled landing')
    onset_i = next(i for i, s in enumerate(samples) if s['sample'] >= press_sample and s['z'] > baseline + .2)
    landing_i = next(i for i in range(origin_index + apex_i + 1, len(samples)) if abs(samples[i]['z'] - baseline) <= 2)
    def interval(i):
        return {clock: [samples[max(0, i-1)][clock], samples[min(len(samples)-1, i+1)][clock]]
                for clock in ('qpc', 'camera')}
    return {'samples': samples, 'origin': origin, 'baseline': baseline,
            'peak': peak - baseline, 'apex_index': origin_index + apex_i,
            'events': {'onset': interval(onset_i), 'apex': interval(origin_index+apex_i), 'landing': interval(landing_i)},
            'rows': rows, 'launch_callback': launches[0], 'launch_air': first_air, 'launch_pre_velocity': first_air['v'][6] * first_air['v'][36],
            'phase_uncertainty': {c: samples[min(origin_index+1,len(samples)-1)][c]-origin[c] for c in ('qpc','camera')}}


def solve3(points):
    a = [[sum(t ** (i+j) for t, y in points) for j in range(3)] +
         [sum(y*t**i for t, y in points)] for i in range(3)]
    check(len(points) >= 4, 'insufficient fit points')
    for i in range(3):
        pivot = max(range(i, 3), key=lambda j: abs(a[j][i]))
        a[i], a[pivot] = a[pivot], a[i]
        check(abs(a[i][i]) > 1e-12, 'singular fit')
        d = a[i][i]; a[i] = [x/d for x in a[i]]
        for j in range(3):
            if j != i:
                d = a[j][i]; a[j] = [x-d*y for x,y in zip(a[j],a[i])]
    return [r[3] for r in a]


def fit(jumps, clock):
    up, down, launch = [], [], []
    for jump in jumps:
        points_up, points_down = [], []
        for i,s in enumerate(jump['samples']):
            t = s[clock]-jump['origin'][clock]; y = s['z']-jump['baseline']
            if t > .04 and y > 5:
                (points_up if i <= jump['apex_index'] else points_down).append((t,y))
        u, d = solve3(points_up), solve3(points_down)
        up.append(-2*u[2]); down.append(-2*d[2]); launch.append(u[1])
    gu, gd, v = map(statistics.mean, (up, down, launch))
    check(gu > 0 and gd > 0 and v > 0, 'nonphysical fitted trajectory')
    return [{'law': 'ballistic', 'clock': clock, 'g': gu, 'v': v},
            {'law': 'two_acceleration', 'clock': clock, 'g': gu, 'g_down': gd, 'v': v},
            {'law': 'mudcrab', 'clock': clock, 'g': 900., 'v': 340., 'h': 1/60}]


def native_forecast(model, steps):
    """Forward controller recurrence consumes dt only, never measured height/velocity."""
    z = maximum = 0.
    velocity = model['v']
    result = []
    for dt in steps:
        if dt is None:
            result.append(0.)
            continue
        multiplier = model['fall_multiplier'] if z <= maximum else 1.
        maximum = max(maximum, z)
        velocity -= model['g'] * multiplier * dt
        z = max(0., z + velocity * dt)
        if z == 0 and velocity < 0:
            velocity = 0.
        result.append(z)
    return result


def forecast(model, times):
    """Only model parameters and clock offsets cross this forecast boundary."""
    g, v = model['g'], model['v']
    result = []
    for t in times:
        if t < 0:
            y = 0.
        elif model['law'] == 'mudcrab':
            n = math.floor(t/model['h'] + 1e-9)
            y = n*v*model['h'] - g*model['h']**2*n*(n-1)/2
        elif model['law'] == 'two_acceleration' and t > v/g:
            y = v*v/(2*g)-model['g_down']*(t-v/g)**2/2
        else:
            y = v*t-g*t*t/2
        result.append(max(0., y))
    return result


def distribution(xs):
    xs = sorted(xs)
    return {'n': len(xs), 'min': xs[0], 'median': statistics.median(xs),
            'max': xs[-1], 'mean': statistics.mean(xs)} if xs else None


def metrics(errors):
    return {'n': len(errors), 'rmse': math.sqrt(statistics.mean(e*e for e in errors)),
            'bias': statistics.mean(errors), 'max_abs': max(map(abs, errors))} if errors else None


def evaluate(jump, run, models):
    samples, origin = jump['samples'], jump['origin']
    report = {'id': run['id'], 'split': run['split'], 'frame_cap': run['frame_cap'],
              'time_multiplier': run['time_multiplier'], 'observed_sampled_apex': jump['peak'],
              'events': jump['events'], 'launch_phase_uncertainty': jump['phase_uncertainty'],
              'direct_launch_velocity_units_per_second': jump['launch_pre_velocity'], 'models': []}
    v = jump['launch_air']['v']
    report['native_launch'] = {'pre_velocity':v[6]*v[36], 'post_velocity':v[24]*v[36],
        'dt':v[2], 'height_gate_equal_or_below':v[15]<=v[12],
        'predicted_first_dv':v[39]*v[10]*v[2]*v[37]*v[36],
        'sqrt_2gh_launch':math.sqrt(2*abs(v[39]*v[10])*abs(v[11]))*v[36]}
    active_counts = {}
    for row in jump['rows']:
        if (row['kind']==8 and row['v'][16]==2 and row['v'][34]==2 and
                row['sample'] >= origin['sample']):
            active_counts[row['sample']] = active_counts.get(row['sample'],0)+1
    report['native_callbacks_per_camera_sample'] = distribution(list(active_counts.values()))
    report['one_controller_update_per_camera_during_flight'] = bool(active_counts and
        all(n == 1 for n in active_counts.values()) and
        sorted(active_counts) == list(range(origin['sample'], max(active_counts) + 1)))
    report['clocks'] = {k: distribution(v) for k,v in {
        'camera_dt': [s['dt'] for s in samples], 'qpc_dt': [b['qpc']-a['qpc'] for a,b in zip(samples,samples[1:])],
        'controller_dt': [s['controller_dt'] for s in samples],
        'camera_to_qpc_ratio': [b['dt']/(b['qpc']-a['qpc']) for a,b in zip(samples,samples[1:])]}.items()}
    diagnostics = {'rising': [], 'history_gate': []}
    for r in jump['rows']:
        if (r['kind'] == 8 and r['v'][16] == 2 and r['v'][34] == 2 and
                r['sample'] >= origin['sample']):
            v = r['v']; gate = v[15] <= v[12]
            predicted = v[39]*v[10]*v[2]*(v[37] if gate else 1)
            actual = v[24]-v[6]
            diagnostics['history_gate' if gate else 'rising'].append((actual-predicted)*v[36])
    report['clocks']['controller_minus_previous_camera_dt'] = distribution([b['controller_dt']-a['dt'] for a,b in zip(samples,samples[1:])])
    report['clocks']['controller_minus_same_camera_dt'] = distribution([s['controller_dt']-s['dt'] for s in samples])
    report['position_scale_delta_residual'] = metrics([
        (b['proxy_z']-a['proxy_z'])*b['scale'] - (b['z']-a['z'])
        for a,b in zip(samples,samples[1:]) if b['sample'] > origin['sample']])
    report['native_air_dv_residual_units_per_second'] = {k:metrics(v) for k,v in diagnostics.items()}
    for model in models:
        c = model['clock']; times = [s[c]-origin[c] for s in samples]
        predicted = (native_forecast(model, [s['controller_dt'] if s['sample'] > origin['sample'] else None for s in samples])
                     if model['law']=='native_discrete' else forecast(model, times))
        errors = [p-(s['z']-jump['baseline']) for p,s in zip(predicted,samples)]
        # Window is declared in game seconds. QPC hypothesis scales only its window.
        end = RULES['window_game_seconds'] / (run['time_multiplier'] if c == 'qpc' else 1)
        selected = [i for i,t in enumerate(times) if 0 <= t <= end]
        m = {'model': model, 'window': metrics([errors[i] for i in selected]),
             'whole_flight': metrics([e for e,t,s in zip(errors,times,samples) if t>=0 and s['z']>jump['baseline']+2]),
             'ascent': metrics([errors[i] for i in selected if i <= jump['apex_index']]),
             'descent': metrics([errors[i] for i in selected if i > jump['apex_index']]),
             'predicted_peak': max(predicted), 'sampled_peak_error': max(predicted)-jump['peak'],
             'window_covered': times[-1] >= end}
        m['passes_frozen_report_gate'] = bool(m['window_covered'] and m['window'] and
            m['window']['rmse'] <= RULES['rmse_gate'] and
            m['window']['max_abs'] <= RULES['peak_error_gate'] and
            (model['law'] != 'native_discrete' or report['one_controller_update_per_camera_during_flight']))
        report['models'].append(m)
    return report


def freeze(manifest_path, output):
    m, root = manifest(manifest_path)
    training = [r for r in m['runs'] if r['split'] == 'train']
    check(training, 'no training runs')
    contract_keys = ('exe_sha256', 'dll_sha256', 'source_save_sha256', 'input_sha256', 'proton_sha256')
    provenance = {k: training[0].get('proof', {}).get(k) for k in contract_keys}
    check(all({k: r.get('proof', {}).get(k) for k in contract_keys} == provenance for r in training),
          'training source provenance differs')
    jumps = [qualify(root/r['path'], r) for r in training]
    models = fit(jumps, 'camera') + fit(jumps, 'qpc')
    native_launch, native_gravity, native_multiplier = [], [], []
    for jump in jumps:
        first = next(r for r in jump['rows'] if r['kind']==9 and r['sample']==jump['origin']['sample'])
        v = first['v']
        native_launch.append(jump['launch_pre_velocity'])
        native_gravity.append(abs(v[39]*v[10]*v[36]))
        native_multiplier.append(v[37])
    models.append({'law':'native_discrete','clock':'controller','g':statistics.mean(native_gravity),
                   'v':statistics.mean(native_launch),'fall_multiplier':statistics.mean(native_multiplier),
                   'assumption':'one controller update per camera; dt schedule observed, own height history'})
    frozen = {'schema': 1, 'runtime_sha256': m['runtime_sha256'],
              'evaluator_sha256': digest(Path(__file__).read_bytes()), 'rules': RULES,
              'provenance_contract': provenance,
              'training': [{'id':r['id'],'sha256':r['sha256']} for r in training], 'models': models,
              'training_scores': [evaluate(j,r,models) for j,r in zip(jumps,training)]}
    frozen['model_sha256'] = digest(canonical(models))
    frozen['seal_sha256'] = digest(canonical(frozen))
    Path(output).write_text(json.dumps(frozen, indent=2)+'\n')
    return frozen


def score(manifest_path, frozen_path, output):
    f = json.loads(Path(frozen_path).read_text())
    seal = f.pop('seal_sha256')
    check(digest(canonical(f)) == seal, 'frozen seal mismatch')
    check(f['evaluator_sha256'] == digest(Path(__file__).read_bytes()), 'evaluator changed since freeze')
    check(f['model_sha256'] == digest(canonical(f['models'])), 'model hash mismatch')
    m, root = manifest(manifest_path)
    check(m['runtime_sha256'] == f['runtime_sha256'], 'runtime mismatch')
    training_ids = {r['id'] for r in f['training']}; training_hashes = {r['sha256'] for r in f['training']}
    heldout = [r for r in m['runs'] if r['split'] == 'heldout']
    check(heldout, 'no heldout runs')
    check(all(r['id'] not in training_ids and r['sha256'] not in training_hashes for r in heldout), 'training/heldout overlap')
    results = []
    for r in heldout:
        try:
            check({k: r.get('proof', {}).get(k) for k in f['provenance_contract']} == f['provenance_contract'],
                  'heldout source provenance differs from calibration')
            results.append(evaluate(qualify(root/r['path'],r),r,f['models']))
        except (ValueError, OSError) as e:
            results.append({'id':r['id'], 'rejected':str(e)})
    report = {'schema':1,'frozen_sha256':seal,'rules':f['rules'],'runs':results,
              'limitations':['Camera dt is a clock hypothesis, not proven controller tick ownership.',
                              'Origin is preceding camera boundary; no heldout phase fitting.',
                              'Two acceleration switch uses predicted apex, approximating native height-history gate.']}
    out = Path(output); out.mkdir(parents=True,exist_ok=True)
    (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command',required=True)
    for name in ('freeze','score'):
        p=sub.add_parser(name); p.add_argument('--manifest',required=True); p.add_argument('--output',required=True)
        if name=='score': p.add_argument('--frozen',required=True)
    a=parser.parse_args()
    try:
        value=freeze(a.manifest,a.output) if a.command=='freeze' else score(a.manifest,a.frozen,a.output)
        print(json.dumps({'command':a.command,'output':a.output,'seal':value.get('seal_sha256',value.get('frozen_sha256'))}))
    except (ValueError,KeyError,OSError,json.JSONDecodeError) as e:
        parser.exit(2, f'evaluation rejected: {e}\n')


if __name__=='__main__': main()
