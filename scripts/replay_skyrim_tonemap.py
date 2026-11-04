#!/usr/bin/env python3
"""Replay the audited group-26/27 pixel arithmetic from explicit sampled inputs.

This is a CPU reference, not a texture sampler or a renderer. A sample must
contain the values returned by the captured SRVs and samplers, plus the actual
interpolated pixel-shader v1.xy. No sRGB conversion, assumed 0.18 pivot, assumed
unit adaptation, inferred Vulkan-to-D3D binding map, output blending, or render
target conversion is added here. Only the two pinned retail shaders qualify.

Input JSON has schema_version=1, shader_group, shader_sha256, constants, samples,
and provenance. Constants are explicit float4 rows named cb2[0], cb2[2], cb2[3],
cb2[4], cb12[42], cb12[43], cb12[44], and cb2[5] for group 27. Each sample has
label, uv, sampled_t0_rgb, sampled_t1_rgb, sampled_t2_xy, and optionally
expected_shader_rgba. Those expected values must be shader outputs before
render-target conversion and blending, not decoded screenshot pixels.

provenance.kind is synthetic or captured. Captured input requires capture_sha256,
event_id, slot_mapping_evidence, sample_evidence, and bindings for t0/t1/t2.
These are caller-supplied evidence references, not independently verified facts.
Run with --help for output and arithmetic options.
"""

import argparse
import ctypes
import hashlib
import json
import math
from pathlib import Path
import struct
import sys


SHADERS = {
    26: '1def29cca7e582b149a09bd15524793593293ae2d94fc20b3d030bf9f91e39fc',
    27: '781978201a0b01ba4107b11abbe52456b5f20b49b998f1d0ba7aa9d057133ea1',
}
LUMINANCE = (0.21250000596046448, 0.715399980545044, 0.07209999859333038)
REQUIRED_ROWS = ('cb2[0]', 'cb2[2]', 'cb2[3]', 'cb2[4]',
                 'cb12[42]', 'cb12[43]', 'cb12[44]')
LIMITATIONS = [
    'Caller-supplied provenance and binding evidence are recorded, not independently verified.',
    'Inputs must already include captured SRV conversion, swizzle, filtering, addressing and mip selection.',
    'uv must be interpolated pixel-shader v1.xy; no pixel-center, vertex, orientation or viewport mapping is inferred.',
    'D3D permits arithmetic refactoring and GPU-specific division, log2, exp2 and dot-product accuracy.',
    'The chosen CPU MAD and dot-product rounding model does not promise bitwise GPU equivalence.',
    'Output is the shader return before output-merger blending, target conversion and later passes.',
    'No display transfer or artistic sRGB conversion is inferred or added.',
]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def f32(value):
    """Round to IEEE binary32; keep sample subnormals until an arithmetic op."""
    try:
        return struct.unpack('<f', struct.pack('<f', value))[0]
    except OverflowError:
        return math.copysign(math.inf, value)


def bits32(value):
    return struct.unpack('<I', struct.pack('<f', value))[0]


def flush(value):
    return math.copysign(0.0, value) if 0 < abs(value) < 2 ** -126 else value


def encode(value):
    """Encode nonfinites explicitly so output remains standards-compliant JSON."""
    if isinstance(value, float) and not math.isfinite(value):
        return 'NaN' if math.isnan(value) else ('Infinity' if value > 0 else '-Infinity')
    if isinstance(value, dict):
        return {key: encode(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [encode(item) for item in value]
    return value


def scalar(value, name):
    if isinstance(value, str) and value in ('NaN', 'Infinity', '-Infinity'):
        return {'NaN': math.nan, 'Infinity': math.inf, '-Infinity': -math.inf}[value]
    require(type(value) in (int, float), f'{name} must be a number or explicit nonfinite string')
    try:
        return f32(value)
    except (TypeError, ValueError, OverflowError) as error:
        raise ValueError(f'{name} is not representable as binary32') from error


def vector(value, size, name):
    require(isinstance(value, list) and len(value) == size, f'{name} must have {size} components')
    return [scalar(item, f'{name}[{index}]') for index, item in enumerate(value)]


def decode_r11g11b10_float_word(word):
    """Decode one storage word; this does not apply an SRV or sampler.

    DXGI R11G11B10_FLOAT / Vulkan B10G11R11_UFLOAT_PACK32 stores unsigned
    5-bit exponents with bias 15. Red and green have six fraction bits; blue
    has five. This preserves subnormals and exposes infinities/NaNs.
    """
    require(type(word) is int and 0 <= word <= 0xFFFFFFFF, 'Expected one unsigned 32-bit storage word')

    def component(value, fraction_bits):
        fraction = value & ((1 << fraction_bits) - 1)
        exponent = value >> fraction_bits
        if exponent == 0:
            return math.ldexp(fraction, 1 - 15 - fraction_bits)
        if exponent == 31:
            return math.nan if fraction else math.inf
        return math.ldexp(1 + fraction / (1 << fraction_bits), exponent - 15)

    return [component(word & 0x7FF, 6), component((word >> 11) & 0x7FF, 6),
            component(word >> 22, 5)]


class Arithmetic:
    """Explicit binary32 reference policy; not a claim of native GPU rounding."""

    def __init__(self, mad_mode='separate', dot_mode='separate'):
        require(mad_mode in ('separate', 'fused'), 'Unknown MAD mode')
        require(dot_mode in ('separate', 'fused-chain'), 'Unknown dot-product mode')
        self.mad_mode = mad_mode
        self.dot_mode = dot_mode
        self.fmaf = None
        if mad_mode == 'fused' or dot_mode == 'fused-chain':
            try:
                self.fmaf = ctypes.CDLL(None).fmaf
                self.fmaf.argtypes = [ctypes.c_float] * 3
                self.fmaf.restype = ctypes.c_float
            except (AttributeError, OSError, TypeError) as error:
                # Windows cannot load the process image by passing None, so the
                # probe raises TypeError there even when no fmaf is reachable.
                raise ValueError('System C99 fmaf unavailable; use explicit separate MAD mode') from error

    def add(self, a, b):
        return flush(f32(flush(a) + flush(b)))

    def mul(self, a, b):
        return flush(f32(flush(a) * flush(b)))

    def mad(self, a, b, c):
        if self.mad_mode == 'separate':
            return self.add(self.mul(a, b), c)
        return flush(float(self.fmaf(flush(a), flush(b), flush(c))))

    def dot3(self, a, b):
        # Retain the dot as an identified reference policy rather than claim
        # this particular accumulation order is required by D3D or the GPU.
        # The captured DXVK SPIR-V explicitly uses first mul, then two FMAs.
        if self.dot_mode == 'fused-chain':
            value = self.mul(a[0], b[0])
            value = flush(float(self.fmaf(flush(a[1]), flush(b[1]), value)))
            return flush(float(self.fmaf(flush(a[2]), flush(b[2]), value)))
        return self.add(self.add(self.mul(a[0], b[0]), self.mul(a[1], b[1])),
                        self.mul(a[2], b[2]))

    def div(self, a, b):
        a, b = flush(a), flush(b)
        if b == 0:
            if a == 0 or math.isnan(a):
                return math.nan
            return math.copysign(math.inf, math.copysign(1, a) * math.copysign(1, b))
        return flush(f32(a / b))

    def log2(self, value):
        value = flush(value)
        if value == 0:
            return -math.inf
        if value < 0:
            return math.nan
        return flush(f32(math.log2(value)))

    def exp2(self, value):
        try:
            return flush(f32(2 ** flush(value)))
        except OverflowError:
            return math.inf

    @staticmethod
    def maximum(a, b):
        a, b = flush(a), flush(b)
        return b if math.isnan(a) else a if math.isnan(b) else max(a, b)

    @staticmethod
    def minimum(a, b):
        a, b = flush(a), flush(b)
        return b if math.isnan(a) else a if math.isnan(b) else min(a, b)

    def saturate(self, value):
        return self.minimum(self.maximum(value, 0.0), 1.0)


def coordinates(uv, constants, arithmetic):
    """Return normalized coordinates before sampler addressing, not texel indices."""
    a = arithmetic
    scale, bounds = constants['cb12[43]'], constants['cb12[44]']
    adjusted = [a.maximum(a.mul(uv[index], scale[index]), 0.0) for index in (0, 1)]
    adjusted = [a.minimum(adjusted[0], bounds[2]), a.minimum(adjusted[1], scale[1])]
    return {'t0': adjusted if constants['cb2[0]'][0] > 0.5 else list(uv),
            't1': adjusted, 't2': list(uv)}


def replay_pixel(sample, constants, shader_group=26, mad_mode='separate', dot_mode='separate'):
    """Replay instructions in audited order, including the unselected curve."""
    a = Arithmetic(mad_mode, dot_mode)
    scene = vector(sample.get('sampled_t1_rgb'), 3, 'sampled_t1_rgb')
    bloom = vector(sample.get('sampled_t0_rgb'), 3, 'sampled_t0_rgb')
    adaptation = vector(sample.get('sampled_t2_xy'), 2, 'sampled_t2_xy')
    uv = vector(sample.get('uv'), 2, 'uv')
    hdr, grade, tint = constants['cb2[2]'], constants['cb2[3]'], constants['cb2[4]']

    # 13-18. R*Y in the MAD below is not replaced with the rounded Z.
    y = a.maximum(a.dot3(LUMINANCE, scene), f32(0.00001))
    ratio = a.div(adaptation[1], adaptation[0])
    z = a.mul(y, ratio)
    filmic_input = a.mad(ratio, y, -f32(0.004))
    denominator = a.mad(ratio, y, 1.0)
    # 19-27. Both curves execute before the shader's select instruction.
    filmic_input = a.maximum(filmic_input, 0.0)
    b = a.mad(filmic_input, f32(6.2), 0.5)
    c = a.mad(filmic_input, f32(6.2), f32(1.7))
    numerator = a.mul(filmic_input, b)
    divisor = a.mad(filmic_input, c, f32(0.06))
    filmic = a.div(numerator, divisor)
    filmic = a.log2(filmic)
    filmic = a.mul(filmic, f32(2.2))
    filmic = a.exp2(filmic)
    filmic = a.mul(filmic, hdr[1])
    # 28-31.
    reinhard = a.mad(z, hdr[1], 1.0)
    reinhard = a.mul(reinhard, z)
    reinhard = a.div(reinhard, denominator)
    q = filmic if hdr[2] > 0.5 else reinhard
    # 32-36.
    scale = a.div(q, y)
    mix = a.saturate(a.add(-q, hdr[0]))
    weighted_bloom = [a.mul(component, mix) for component in bloom]
    rgb = [a.mad(component, scale, weighted_bloom[index])
           for index, component in enumerate(scene)]
    k = a.dot3(rgb, LUMINANCE)
    # 37-43. Alpha participates in saturation, tint, brightness and contrast.
    value = rgb + [1.0]
    value = [a.add(component, -k) for component in value]
    value = [a.mad(grade[0], component, k) for component in value]
    delta = [a.mad(k, tint[index], -component) for index, component in enumerate(value)]
    value = [a.mad(tint[3], delta[index], component) for index, component in enumerate(value)]
    value = [a.mad(grade[3], component, -adaptation[0]) for component in value]
    value = [a.mad(grade[2], component, adaptation[0]) for component in value]
    contrasted = list(value)
    # 44-49 (ordinary) or 44-50 (fade). Keep log2/mul/exp2 distinct.
    clamped = [a.saturate(component) for component in value[:3]]
    output = [a.exp2(a.mul(a.log2(component), constants['cb12[42]'][0]))
              for component in clamped] + [value[3]]
    powered = list(output)
    if shader_group == 27:
        fade = constants['cb2[5]']
        delta = [a.add(fade[index], -component) for index, component in enumerate(output)]
        output = [a.mad(fade[3], delta[index], component) for index, component in enumerate(output)]

    return {'label': sample['label'], 'uv': uv,
            'sample_coordinates_before_sampler_addressing': coordinates(uv, constants, a),
            'sampled_t0_rgb': bloom, 'sampled_t1_rgb': scene, 'sampled_t2_xy': adaptation,
            'curve': 'filmic' if hdr[2] > 0.5 else 'reinhard',
            'trace': {'Y': y, 'exposure_ratio': ratio, 'Z': z,
                      'filmic_Q': filmic, 'reinhard_Q': reinhard, 'selected_Q': q,
                      'scene_scale': scale, 'bloom_mix': mix, 'rgb_after_bloom': rgb,
                      'K': k, 'contrast_pivot': adaptation[0],
                      'contrasted_rgba': contrasted, 'clamped_rgb': clamped,
                      'gamma_exponent': constants['cb12[42]'][0],
                      'after_power_rgba': powered},
            'shader_rgba': output,
            'shader_rgba_bits32': [f'{bits32(component):08x}' for component in output]}


def validate(document):
    require(isinstance(document, dict) and document.get('schema_version') == 1,
            'Expected input schema_version=1 object')
    group = document.get('shader_group')
    require(type(group) is int and group in SHADERS, 'Only audited shader groups 26 and 27 are supported')
    require(document.get('shader_sha256') == SHADERS[group], 'Pixel shader hash does not match audited group')
    require(document.get('sample_value_domain') == 'post_srv_sampler',
            'sample_value_domain must explicitly be post_srv_sampler')
    raw = document.get('constants')
    require(isinstance(raw, dict), 'constants must contain explicit constant-buffer float4 rows')
    required = REQUIRED_ROWS + (('cb2[5]',) if group == 27 else ())
    constants = {name: vector(raw.get(name), 4, name) for name in required}
    provenance = document.get('provenance')
    require(isinstance(provenance, dict) and provenance.get('kind') in ('synthetic', 'captured'),
            'provenance.kind must be synthetic or captured')
    if provenance['kind'] == 'captured':
        capture_hash = provenance.get('capture_sha256')
        require(isinstance(capture_hash, str) and len(capture_hash) == 64
                and all(c in '0123456789abcdef' for c in capture_hash), 'Captured input requires capture_sha256')
        require(type(provenance.get('event_id')) is int and provenance['event_id'] > 0,
                'Captured input requires a positive event_id')
        for name in ('slot_mapping_evidence', 'sample_evidence'):
            require(isinstance(provenance.get(name), str) and provenance[name].strip(),
                    f'Captured input requires {name}')
        bindings = document.get('bindings')
        require(isinstance(bindings, dict), 'Captured input requires explicit bindings')
        for slot in range(3):
            binding = bindings.get(f't{slot}')
            require(isinstance(binding, dict) and binding.get('resource_id')
                    and type(binding.get('sampler_slot')) is int and binding['sampler_slot'] == slot
                    and binding.get('srv_format') and binding.get('sampler_evidence'),
                    f'Captured t{slot} requires resource_id, sampler_slot={slot}, srv_format and sampler_evidence')
    samples = document.get('samples')
    require(isinstance(samples, list) and 0 < len(samples) <= 100000,
            'samples must contain 1 through 100000 explicitly sampled pixels')
    labels = set()
    for sample in samples:
        require(isinstance(sample, dict) and isinstance(sample.get('label'), str) and sample['label'],
                'Each sample requires a nonempty label')
        require(sample['label'] not in labels, 'Sample labels must be unique')
        labels.add(sample['label'])
        for name, count in (('uv', 2), ('sampled_t0_rgb', 3), ('sampled_t1_rgb', 3), ('sampled_t2_xy', 2)):
            vector(sample.get(name), count, f"{sample['label']}.{name}")
        if 'expected_shader_rgba' in sample:
            vector(sample['expected_shader_rgba'], 4, 'expected_shader_rgba')
    return constants


def compare(actual, expected, atol, rtol):
    """Nonfinite matches stay explicit; NaN never counts as a successful check."""
    components = []
    for observed, reference in zip(actual, expected):
        difference = abs(observed - reference)
        matches = (not math.isnan(observed) and not math.isnan(reference)
                   and ((math.isfinite(observed) and math.isfinite(reference)
                         and difference <= atol + rtol * abs(reference))
                        or (math.isinf(observed) and observed == reference)))
        components.append({'actual': observed, 'expected': reference,
                           'absolute_error': difference, 'within_tolerance': matches})
    return {'within_tolerance': all(component['within_tolerance'] for component in components),
            'components': components}


def replay(document, mad_mode='separate', atol=1e-5, rtol=1e-5, dot_mode='separate'):
    require(math.isfinite(atol) and math.isfinite(rtol) and atol >= 0 and rtol >= 0,
            'Comparison tolerances must be finite and nonnegative')
    constants = validate(document)
    results = []
    for sample in document['samples']:
        result = replay_pixel(sample, constants, document['shader_group'], mad_mode, dot_mode)
        if 'expected_shader_rgba' in sample:
            result['comparison'] = compare(result['shader_rgba'], vector(sample['expected_shader_rgba'], 4,
                                                                        'expected_shader_rgba'), atol, rtol)
        results.append(result)
    comparisons = [result['comparison'] for result in results if 'comparison' in result]
    return encode({'schema_version': 1, 'status': 'complete',
                   'evidence_kind': document['provenance']['kind'],
                   'shader_group': document['shader_group'], 'shader_sha256': document['shader_sha256'],
                   'provenance': document['provenance'], 'bindings': document.get('bindings'),
                   'constants': constants,
                   'arithmetic': {'storage': 'IEEE binary32', 'subnormal_arithmetic': 'flush_to_signed_zero',
                                  'mad_mode': mad_mode, 'dot_mode': dot_mode,
                                  'dot3': ('first product; two C99 fused multiply-add operations'
                                           if dot_mode == 'fused-chain' else
                                           'separate products; left-associated binary32 sums'),
                                  'log2_exp2': 'system binary64 math rounded to binary32 after each instruction',
                                  'absolute_tolerance': atol, 'relative_tolerance': rtol},
                   'sample_count': len(results), 'comparison_count': len(comparisons),
                   'all_comparisons_within_tolerance': (all(item['within_tolerance'] for item in comparisons)
                                                         if comparisons else None),
                   'samples': results, 'limitations': LIMITATIONS})


def reject_json_constant(value):
    raise ValueError(f'Nonstandard JSON number {value}; use an explicit nonfinite string')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--input', type=Path, required=True, help='Explicit sampled-input JSON')
    parser.add_argument('--output', type=Path, required=True, help='Fresh replay receipt JSON; never overwritten')
    parser.add_argument('--mad-mode', choices=('separate', 'fused'), default='separate',
                        help='CPU reference MAD policy; fused requires system C99 fmaf')
    parser.add_argument('--dot-mode', choices=('separate', 'fused-chain'), default='separate',
                        help='CPU reference dot3 policy; fused-chain follows observed DXVK SPIR-V')
    parser.add_argument('--atol', type=float, default=1e-5)
    parser.add_argument('--rtol', type=float, default=1e-5)
    args = parser.parse_args(argv)
    try:
        require(args.input.resolve() != args.output.resolve(), 'Input and output paths must differ')
        require(not args.output.exists(), 'Output exists; use a fresh receipt path')
        raw = args.input.read_bytes()
        document = json.loads(raw, parse_constant=reject_json_constant)
        result = replay(document, args.mad_mode, args.atol, args.rtol, args.dot_mode)
        result['input'] = {'path': str(args.input.resolve()), 'sha256': hashlib.sha256(raw).hexdigest()}
        result['script_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        with args.output.open('x') as stream:
            json.dump(result, stream, indent=2, allow_nan=False)
            stream.write('\n')
        print(json.dumps({'status': result['status'], 'output': str(args.output),
                          'sample_count': result['sample_count'], 'comparison_count': result['comparison_count'],
                          'all_comparisons_within_tolerance': result['all_comparisons_within_tolerance']}))
        return 1 if result['all_comparisons_within_tolerance'] is False else 0
    except (OSError, ValueError, TypeError, KeyError) as error:
        print(f'replay_skyrim_tonemap: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
