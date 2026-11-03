"""Check a frozen actor-only pose/capture contract. This does not launch a renderer."""
import argparse
import json
import math
from pathlib import Path

from actor_idle_tools import load_json, need, sha, shared


def finite_vector(value):
    return (isinstance(value, list) and len(value) == 3
            and all(type(x) in (int, float) and math.isfinite(x) for x in value))


def evaluate(contract, case, report, images):
    """Images are decoded by the pinned shared PNG reader before this pure check."""
    need(report['version'] == 1 and report['scope'] == 'actor-only-unlit-skinning', 'probe scope/version')
    for key in ('name', 'base_form_id', 'race_id', 'sex', 'body_scene', 'source', 'clip_scene'):
        need(report[key] == case[key], 'actor identity mismatch: ' + key)
    need(report['body_sha256'] == case['body_sha256'] and report['clip_sha256'] == case['clip_sha256'],
         'asset checksum mismatch')
    need(report['contract_sha256'] == contract['self_sha256'], 'frozen contract mismatch')
    need(report['fixture_spawn'] == 'catalog-plan-with-explicit-live-idle', 'spawn scope changed')
    need(report['actor_drive'] is False, 'fixture acquired locomotion')
    need(report['graph_clip_matches'] is True, 'player graph selected another clip')
    need(type(report['curve_targets']) is int and report['curve_targets'] > 0, 'empty clip targets')
    need(type(report['unbound_targets']) is int and report['unbound_targets'] == 0, 'unbound animation targets')
    need(type(report['skinned_meshes']) is int and report['skinned_meshes'] > 0, 'no rendered skin')
    need(type(report['skinned_vertices_total']) is int and 0 < report['skinned_vertices_total'] <= 1_000_000
         and type(report['readback_frames']) is int and report['readback_frames'] == 2, 'empty skin/readback')
    need(report['skinned_meshes'] == case['expected_skinned_meshes']
         and report['skinned_vertices_total'] == case['expected_skin_vertices'], 'incomplete body primitive denominator')
    need(report['adapter']['backend'] == 'Metal', 'expected native Metal renderer')
    poses = report['poses']
    need(len(poses) == len(images) == 2, 'exactly two pose readbacks required')
    need(poses[0]['seconds'] == 0 and abs(poses[1]['seconds'] - case['sample_seconds']) <= 1e-5,
         'source sample time mismatch')
    need(all(p['settled_frames'] >= contract['settle_frames'] for p in poses), 'unsettled pose')
    camera = poses[0]['camera_matrix']
    need(isinstance(camera, list) and len(camera) == 16
         and all(type(x) in (int, float) and math.isfinite(x) for x in camera)
         and poses[1]['camera_matrix'] == camera, 'camera changed/nonfinite')
    a, b = (p['vertices'] for p in poses)
    need(0 < len(a) == len(b) <= contract['maximum_sample_vertices'], 'sample count changed/empty')
    need([x['id'] for x in a] == [x['id'] for x in b], 'sampled vertices changed')
    need(len({x['id'] for x in a}) == len(a), 'duplicate sampled vertex')
    need(all(finite_vector(x['position']) for x in a + b), 'nonfinite skin coordinates')
    displacements = [math.dist(x['position'], y['position']) for x, y in zip(a, b)]
    moved = sum(value > contract['vertex_motion_epsilon'] for value in displacements)
    need(moved >= contract['minimum_moving_vertices'], 'sampled skin did not deform')
    masks = []
    for width, height, pixels in images:
        need([width, height] == contract['dimensions'], 'image dimensions')
        # White unlit geometry on black excludes a changing environment/material as the oracle.
        need(len(pixels) == width * height * 3, 'image byte count')
        border = [i for i in range(width)] + [(height - 1) * width + i for i in range(width)]
        border += [y * width for y in range(1, height - 1)]
        border += [y * width + width - 1 for y in range(1, height - 1)]
        need(all(max(pixels[3*i:3*i+3]) <= contract['background_max_u8'] for i in border),
             'nonblack or clipped capture border')
        mask = [max(pixels[i:i+3]) > contract['foreground_min_u8'] for i in range(0, len(pixels), 3)]
        need(contract['minimum_foreground_pixels'] <= sum(mask) <= width * height * .75,
             'blank or oversized foreground')
        masks.append(mask)
    changed = sum(max(abs(x-y) for x, y in zip(images[0][2][i:i+3], images[1][2][i:i+3]))
                  > contract['pixel_difference_u8'] for i in range(0, len(images[0][2]), 3))
    need(changed >= contract['minimum_changed_pixels'], 'rendered skin poses did not change')
    return {'status': 'selected-native-skinning-motion-detected-uncalibrated', 'case': case['name'],
            'sampled_vertices': len(a), 'moving_sample_vertices': moved, 'changed_pixels': changed,
            'moving_sample_fraction': moved/len(a),
            'maximum_sample_displacement': max(displacements),
            'maximum_displacement_epsilon_ratio': max(displacements)/contract['vertex_motion_epsilon'],
            'changed_pixel_fraction': changed/(images[0][0]*images[0][1]),
            'thresholds': {key: contract[key] for key in ('vertex_motion_epsilon', 'minimum_moving_vertices',
                                                        'pixel_difference_u8', 'minimum_changed_pixels')},
            'thresholds_calibrated': False,
            'foreground_pixels': list(map(sum, masks)),
            'limits': contract['limits']}


def check(contract_path, name, directory):
    contract = load_json(contract_path)
    contract['self_sha256'] = sha(contract_path)
    cases = [case for case in contract['cases'] if case['name'] == name]
    need(len(cases) == 1, 'unknown/duplicate case')
    directory = Path(directory)
    report = load_json(directory / 'probe.json')
    reader = shared('geometry_oracle')
    images = [reader.read_png(directory / f'pose-{i}.png', contract) for i in range(2)]
    return evaluate(contract, cases[0], report, images)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--contract', type=Path, required=True)
    parser.add_argument('--case', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(check(args.contract, args.case, args.output), indent=2))
