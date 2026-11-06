#!/usr/bin/env python3
"""Score the production Rust WALK transition against the sealed native schedules.

The Rust input contains a training-only effective height and native dt schedules.
Observed future height, velocity, history, support and apex stay in this process.
This replay uses an idealized floor. The separate Rust headless export measures
actual production Bevy/Rapier contact at its unchanged fixed 60 Hz timestep.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import zipfile

from evaluate_skyrim_jump import decode


ROOT = Path(__file__).resolve().parents[1]
REFERENCE = ROOT / 'tools/skyrim-recorder/reference'
EVIDENCE = ROOT / 'docs/research/evidence/skyrim-physics-221'
CORPORA = ('jump-v3', 'jump-cadence-v3')
FROZEN_SEAL = 'c1026fb434bb72d0bb4b255888fd340dd202590e0f9a73d7a19b6a6e0607d575'
RULES = {'window_game_seconds': 1.15, 'rmse_gate': 1, 'peak_error_gate': 3}
BRIDGE = 'physics::jump_replay_tests::export_production_jump_replay'
SOURCES = ('scripts/check_skyrim_jump_runtime.py',
           'crates/engine/src/physics/jump_replay_tests.rs',
           'crates/engine/src/physics.rs', 'crates/engine/src/physics/vertical.rs',
           'crates/engine/Cargo.toml', 'Cargo.lock')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()


def write_json(path, value):
    data = canonical(value) + b'\n'
    path.write_bytes(data)
    return sha(data)


def load_json(path):
    return json.loads(path.read_bytes())


def validate_sources(corpus, frozen_bytes, receipts):
    with zipfile.ZipFile(EVIDENCE / 'captured-source.zip') as archive:
        archived = {name: archive.read(name) for name in archive.namelist()}
    if corpus == 'jump-cadence-v3':
        with zipfile.ZipFile(EVIDENCE / 'cadence-source.zip') as archive:
            archived.update({name: archive.read(name) for name in archive.namelist()})
    plan = f'tools/skyrim-recorder/fixtures/{corpus}.json'
    for receipt_name in receipts:
        path = REFERENCE / corpus / receipt_name
        receipt = load_json(path)
        require(sha(archived[plan]) == receipt['plan_sha256'], f'{path}: archived plan changed')
        for name, expected in receipt['sources'].items():
            require(sha(archived[name]) == expected, f'{path}: archived source changed: {name}')
        expected_frozen = receipt['frozen_sha256']
        require(expected_frozen is None if receipt_name == 'train-capture.json'
                else expected_frozen == sha(frozen_bytes), f'{path}: frozen capture receipt changed')
    return archived


def qualified_observations(path, run):
    rows, samples, frequency = decode(path, run['sha256'])
    jump_input = [r for r in rows if r['kind'] == 2 and r['label'] == 'Jump' and r['state'] == 0]
    presses = [r for r in jump_input if r['v'][0] > 0 and r['v'][1] == 0]
    releases = [r for r in jump_input if r['v'][0] == 0 and r['v'][1] > 0]
    require(len(presses) == len(releases) == 1 and presses[0]['qpc'] < releases[0]['qpc'],
            f"{path}: requires one delivered jump press/release")
    press = presses[0]
    press_time = (press['qpc'] - rows[0]['qpc']) / frequency
    samples = [s for s in samples if s['qpc'] >= press_time - .75]
    require(all(b['qpc'] - a['qpc'] < .15 for a, b in zip(samples, samples[1:])),
            f'{path}: camera gap')
    stationary = [s for s in samples if s['sample'] <= press['sample']][-15:]
    require(len(stationary) >= 5 and max(s['z'] for s in stationary) -
            min(s['z'] for s in stationary) <= .2, f'{path}: stationary baseline missing')
    baseline = statistics.median(s['z'] for s in stationary)
    launches = [r for r in rows if r['kind'] == 9 and r['sample'] >= press['sample']]
    air = [r for r in rows if r['kind'] == 8 and r['v'][16] == 2 and r['sample'] >= press['sample']]
    require(len(launches) == 1 and air, f'{path}: launch callback missing or repeated')
    launch, first_air = launches[0], air[0]
    require(launch['v'][16] == 1 and first_air['qpc'] <= launch['qpc'] and
            first_air['sample'] == launch['sample'], f'{path}: invalid nested launch order')
    tail = [s for s in samples if s['sample'] >= first_air['sample']]
    require(tail and tail[0]['sample'] == first_air['sample'], f'{path}: launch boundary missing')
    apex = max(range(len(tail)), key=lambda index: tail[index]['z'])
    require(0 < apex < len(tail) - 3 and tail[apex]['z'] - baseline >= 10,
            f'{path}: clear interior apex missing')
    require(all(abs(s['z'] - baseline) <= 2 for s in tail[-3:]), f'{path}: settled landing missing')
    active_counts = {}
    for row in air:
        if row['v'][34] == 2:
            active_counts[row['sample']] = active_counts.get(row['sample'], 0) + 1
    require(active_counts and all(n == 1 for n in active_counts.values()) and
            sorted(active_counts) == list(range(tail[0]['sample'], max(active_counts) + 1)),
            f'{path}: one controller update per camera assumption failed')
    return {'samples': tail, 'baseline': baseline, 'launch': launch,
            'observed_sampled_apex': tail[apex]['z'] - baseline}


def prepare():
    observations, unique, training = {}, {}, {}
    provenance = {'corpora': {}, 'source_archives': {}}
    frozen_reference = None
    for corpus in CORPORA:
        root = REFERENCE / corpus
        frozen_bytes = (root / 'frozen.json').read_bytes()
        frozen = json.loads(frozen_bytes)
        seal = frozen.pop('seal_sha256')
        require(seal == FROZEN_SEAL and sha(canonical(frozen)) == seal, f'{corpus}: frozen seal changed')
        require(sha(canonical(frozen['models'])) == frozen['model_sha256'], f'{corpus}: model hash changed')
        require(all(frozen['rules'][key] == value for key, value in RULES.items()),
                f'{corpus}: scoring gates changed')
        require(sha((ROOT / 'scripts/evaluate_skyrim_jump.py').read_bytes()) == frozen['evaluator_sha256'],
                f'{corpus}: sealed decoder source changed')
        if frozen_reference is None:
            frozen_reference = frozen_bytes
        require(frozen_bytes == frozen_reference, f'{corpus}: frozen artifact differs between corpora')
        receipts = ['heldout-capture.json']
        if corpus == 'jump-v3':
            receipts.append('train-capture.json')
        archived = validate_sources(corpus, frozen_bytes, receipts)
        require(sha(archived['scripts/evaluate_skyrim_jump.py']) == frozen['evaluator_sha256'],
                f'{corpus}: archived evaluator differs from seal')
        path = root / 'manifest.json'
        manifest = load_json(path)
        require(manifest['schema'] == 1 and manifest['runtime_sha256'] == frozen['runtime_sha256'],
                f'{corpus}: manifest schema/runtime changed')
        declared_training = {(r['id'], r['sha256']) for r in frozen['training']}
        actual_training = {(r['id'], r['sha256']) for r in manifest['runs'] if r['split'] == 'train'}
        require(actual_training == declared_training, f'{corpus}: training declaration changed')
        ids, hashes = set(), set()
        for run in manifest['runs']:
            require(run['id'] not in ids and run['sha256'] not in hashes, f'{corpus}: duplicate run')
            ids.add(run['id'])
            hashes.add(run['sha256'])
            require(run['split'] in ('train', 'heldout'), f'{corpus}: unknown split')
            require(run['frame_cap'] in (30, 60, 120) and run['time_multiplier'] in (1, .5),
                    f'{corpus}: invalid run condition')
            relative = Path(run['path'])
            require(not relative.is_absolute() and '..' not in relative.parts, f'{corpus}: unsafe trace path')
            proof = run['proof']
            require(proof['status'] == 'passed' and proof['received_down_up'] and
                    proof['forward_input_accepted'] and proof['source_save_unchanged'],
                    f'{corpus}/{run["id"]}: rejected acquisition')
            require({key: proof[key] for key in frozen['provenance_contract']} == frozen['provenance_contract'],
                    f'{corpus}/{run["id"]}: acquisition source contract changed')
            require(run['split'] == 'train' or all(run['id'] != ident and run['sha256'] != digest
                    for ident, digest in declared_training), f'{corpus}: training/heldout overlap')
            observed = qualified_observations(root / relative, run)
            if run['sha256'] in unique:
                require(run['split'] == 'train' and unique[run['sha256']][1] == run,
                        f'{corpus}: only identical training traces may be repeated')
                continue
            key = f'{corpus}/{run["id"]}'
            unique[run['sha256']] = (key, run)
            observations[key] = (run, observed)
            if run['split'] == 'train':
                training[key] = (run, observed)
        require(all(attempt['exit_code'] == 0 and attempt['attempt'] == 1
                    for attempt in manifest['attempts']), f'{corpus}: rejected acquisition attempt')
        provenance['corpora'][corpus] = {
            'manifest_sha256': sha(path.read_bytes()), 'frozen_file_sha256': sha(frozen_bytes),
            'seal_sha256': seal,
            'capture_receipt_sha256': {name: sha((root / name).read_bytes()) for name in receipts},
        }
    require(len(training) == 3 and len(observations) == 15, 'expected 3 unique train and 12 unique heldout traces')
    heights = [observed['launch']['v'][11] * observed['launch']['v'][36]
               for _, observed in training.values()]
    require(all(math.isfinite(height) for height in heights), 'invalid training effective jump height')
    profile = {'effective_jump_height_creation': statistics.mean(heights)}
    profile_source = {
        'method': 'mean of direct original-training launch callback v[11] * v[36]; no fitting',
        'field_semantics': 'controller jump-height in Havok length times recorded native Creation conversion',
        'runs': [{'id': key, 'trace_sha256': run['sha256'],
                  'effective_jump_height_creation': observed['launch']['v'][11] * observed['launch']['v'][36]}
                 for key, (run, observed) in training.items()],
    }
    payload = {'schema': 1, 'profile': profile, 'runs': []}
    for key, (_, observed) in observations.items():
        payload['runs'].append({'id': key, 'initial_floor_height': 0.0,
                               'controller_dt_seconds': [s['controller_dt'] for s in observed['samples'][1:]]})
    for name in ('captured-source.zip', 'cadence-source.zip'):
        provenance['source_archives'][name] = sha((EVIDENCE / name).read_bytes())
    provenance['profile_source'] = profile_source
    provenance['trace_sha256'] = {key: run['sha256'] for key, (run, _) in observations.items()}
    return payload, observations, provenance


def score(output, observations):
    require(output['schema'] == 1, 'Rust bridge output schema changed')
    predicted = {run['id']: run['heights'] for run in output['runs']}
    require(len(predicted) == len(output['runs']) and predicted.keys() == observations.keys(),
            'Rust predictions omit, duplicate or add runs')
    results = []
    for key, (run, observed) in observations.items():
        samples = observed['samples']
        heights = predicted[key]
        require(len(heights) == len(samples) and all(math.isfinite(value) for value in heights),
                f'{key}: invalid predicted heights')
        require(heights[0] == 0.0, f'{key}: changed initial predicted floor height')
        times = [sample['controller'] - samples[0]['controller'] for sample in samples]
        errors = [height - (sample['z'] - observed['baseline'])
                  for height, sample, time in zip(heights, samples, times)
                  if 0 <= time <= RULES['window_game_seconds']]
        require(errors, f'{key}: empty score window')
        rmse = math.sqrt(statistics.mean(error * error for error in errors))
        max_abs = max(map(abs, errors))
        covered = times[-1] >= RULES['window_game_seconds']
        results.append({'id': key, 'split': run['split'], 'frame_cap_request': run['frame_cap'],
                        'time_multiplier': run['time_multiplier'], 'n': len(errors),
                        'rmse': rmse, 'max_abs': max_abs, 'window_covered': covered,
                        'observed_sampled_apex': observed['observed_sampled_apex'],
                        'predicted_sampled_apex': max(heights),
                        'passes_frozen_report_gate': covered and rmse <= RULES['rmse_gate'] and
                        max_abs <= RULES['peak_error_gate']})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--test-binary', type=Path, help='existing engine library test executable; skips Cargo')
    parser.add_argument('--require-heldout-pass', action='store_true')
    args = parser.parse_args()
    destination = args.output.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    payload, observations, provenance = prepare()
    input_path = destination / 'input.json'
    input_hash = write_json(input_path, payload)
    sources = {name: sha((ROOT / name).read_bytes()) for name in SOURCES}
    source_hash = sha(canonical(sources))
    provenance.update({'schema': 1, 'input_sha256': input_hash, 'source_sha256': source_hash, 'sources': sources})
    source_receipt_hash = write_json(destination / 'source.json', provenance)
    if args.prepare_only:
        print(json.dumps({'prepared': str(input_path), 'input_sha256': input_hash,
                          'unique_training': 3, 'unique_heldout': 12,
                          'profile': payload['profile']}))
        return 0
    prediction_path = destination / 'predictions.json'
    prediction_path.unlink(missing_ok=True)
    command = ([str(args.test_binary.resolve()), BRIDGE, '--ignored', '--exact', '--nocapture']
               if args.test_binary else
               ['cargo', 'test', '-p', 'engine', '--lib', BRIDGE, '--', '--ignored', '--exact', '--nocapture'])
    env = dict(os.environ, MUDCRAB_JUMP_REPLAY_INPUT=str(input_path),
               MUDCRAB_JUMP_REPLAY_OUTPUT=str(prediction_path))
    subprocess.run(command, cwd=ROOT, env=env, check=True)
    require(sources == {name: sha((ROOT / name).read_bytes()) for name in SOURCES},
            'production replay sources changed during execution')
    output = load_json(prediction_path)
    results = score(output, observations)
    heldout = [run for run in results if run['split'] == 'heldout']
    report = {'schema': 1, 'model': output['model'], 'profile_height_consumed': output['profile_height_consumed'],
              'rules': RULES, 'input_sha256': input_hash, 'source_sha256': source_hash,
              'source_receipt_sha256': source_receipt_hash, 'prediction_sha256': sha(prediction_path.read_bytes()),
              'command': command, 'runs': results,
              'heldout': {'n': len(heldout), 'passes': sum(run['passes_frozen_report_gate'] for run in heldout),
                          'maximum_rmse': max(run['rmse'] for run in heldout),
                          'maximum_pointwise_error': max(run['max_abs'] for run in heldout)},
              'limitations': [
                  'Production-function replay uses native observed dt, not the research fixed-60-Hz Mudcrab candidate.',
                  'Only the recorded dt and original-training effective jump height enter Rust; no future observed state.',
                  'Idealized floor contact is prediction-owned. Real contact is covered by export_headless_jump separately.',
                  'Native launch boundary is conditional and unchanged; input latency and scheduler ownership are untested.',
                  'Existing test-binary selection is caller-owned; its file hash is recorded when supplied.',
              ]}
    if args.test_binary:
        report['test_binary_sha256'] = sha(args.test_binary.read_bytes())
    write_json(destination / 'result.json', report)
    print(json.dumps({'result': str(destination / 'result.json'), **report['heldout']}))
    return int(args.require_heldout_pass and report['heldout']['passes'] != len(heldout))


if __name__ == '__main__':
    raise SystemExit(main())
