"""Analytical cases and evidence-contract failures for the native CPU reference."""

import copy
import importlib.util
import json
import math
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('replay_skyrim_tonemap.py')
SPEC = importlib.util.spec_from_file_location('replay_skyrim_tonemap', SCRIPT)
REPLAY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPLAY)


def fixture(group=26):
    return {'schema_version': 1, 'shader_group': group, 'shader_sha256': REPLAY.SHADERS[group],
            'sample_value_domain': 'post_srv_sampler', 'provenance': {'kind': 'synthetic'},
            'constants': {'cb2[0]': [0, 0, 0, 0], 'cb2[2]': [0, 1, 0, 0],
                          'cb2[3]': [1, 0, 1, 1], 'cb2[4]': [1, 1, 1, 0],
                          'cb2[5]': [0.2, 0.4, 0.6, 0.25],
                          'cb12[42]': [1, 0, 0, 0], 'cb12[43]': [1, 1, 0, 0],
                          'cb12[44]': [0, 0, 1, 0]},
            'samples': [{'label': 'analytical', 'uv': [0.5, 0.5],
                         'sampled_t0_rgb': [0, 0, 0], 'sampled_t1_rgb': [0.5, 0.5, 0.5],
                         'sampled_t2_xy': [1, 1]}]}


def pixel(document, **kwargs):
    return REPLAY.replay(document, **kwargs)['samples'][0]


class NativeArithmeticTests(unittest.TestCase):
    def assertVectorAlmostEqual(self, actual, expected, places=6):
        self.assertEqual(len(actual), len(expected))
        for index, (observed, reference) in enumerate(zip(actual, expected)):
            with self.subTest(component=index):
                self.assertAlmostEqual(observed, reference, places=places)

    def test_reinhard_uses_luminance_not_independent_rgb_curves(self):
        data = fixture()
        data['constants']['cb2[2]'][1] = 0
        data['samples'][0]['sampled_t1_rgb'] = [0.5, 0.25, 0.125]
        out = pixel(data)
        # With zero bloom and neutral grade, a single luminance scale preserves
        # these RGB ratios. Per-channel Reinhard would not preserve them.
        red, green, blue = out['shader_rgba'][:3]
        self.assertAlmostEqual(red / green, 2, places=5)
        self.assertAlmostEqual(green / blue, 2, places=5)

    def test_actual_adaptation_ratio_controls_white_scaled_reinhard(self):
        data = fixture()
        data['constants']['cb2[2]'][1] = 0.5
        data['samples'][0]['sampled_t2_xy'] = [0.25, 0.5]
        out = pixel(data)
        # Y=.5, ratio=2, Z=1, k=.5: Q=1*(1+.5)/(1+1)=.75.
        self.assertEqual(out['trace']['exposure_ratio'], 2)
        self.assertAlmostEqual(out['trace']['selected_Q'], 0.75, places=6)
        self.assertVectorAlmostEqual(out['shader_rgba'], [0.75, 0.75, 0.75, 1])

    def test_same_exposure_ratio_different_px_changes_contrast_pivot(self):
        data = fixture()
        data['constants']['cb2[2]'][1] = 0.5
        data['constants']['cb2[3]'][2] = 0.5
        data['samples'][0]['sampled_t2_xy'] = [0.25, 0.5]
        first = pixel(data)
        data['samples'][0]['sampled_t2_xy'] = [0.75, 1.5]
        second = pixel(data)
        self.assertEqual(first['trace']['exposure_ratio'], second['trace']['exposure_ratio'])
        self.assertEqual(first['trace']['selected_Q'], second['trace']['selected_Q'])
        self.assertVectorAlmostEqual(first['shader_rgba'][:3], [0.5] * 3)
        self.assertVectorAlmostEqual(second['shader_rgba'][:3], [0.75] * 3)

    def test_bloom_weight_is_saturated_threshold_minus_tonemapped_luminance(self):
        data = fixture()
        data['constants']['cb2[2]'] = [0.8, 0, 0, 0]
        data['samples'][0]['sampled_t0_rgb'] = [0.25, 0.125, 0.5]
        out = pixel(data)
        q = 1 / 3
        mix = 0.8 - q
        self.assertAlmostEqual(out['trace']['selected_Q'], q, places=6)
        self.assertAlmostEqual(out['trace']['bloom_mix'], mix, places=6)
        self.assertVectorAlmostEqual(out['shader_rgba'][:3], [q + mix * c for c in (0.25, 0.125, 0.5)])
        data['constants']['cb2[2]'][0] = -1
        self.assertEqual(pixel(data)['trace']['bloom_mix'], 0)
        data['constants']['cb2[2]'][0] = 4
        self.assertEqual(pixel(data)['trace']['bloom_mix'], 1)

    def test_saturation_and_tint_target_original_post_bloom_luminance(self):
        data = fixture()
        data['samples'][0]['sampled_t1_rgb'] = [0.5, 0.25, 0.125]
        data['constants']['cb2[3]'][0] = 0
        desaturated = pixel(data)
        k = desaturated['trace']['K']
        self.assertVectorAlmostEqual(desaturated['shader_rgba'][:3], [k] * 3)
        data['constants']['cb2[3]'][0] = 2
        data['constants']['cb2[4]'] = [0.25, 0.5, 0.75, 1]
        tinted = pixel(data)
        self.assertVectorAlmostEqual(tinted['shader_rgba'][:3], [k * c for c in (0.25, 0.5, 0.75)])

    def test_brightness_precedes_contrast_and_final_power(self):
        data = fixture()
        data['samples'][0]['sampled_t1_rgb'] = [0.25] * 3
        data['samples'][0]['sampled_t2_xy'] = [0.125, 0.125]
        data['constants']['cb2[3]'][2:] = [0.5, 2]
        data['constants']['cb12[42]'][0] = 0.5
        # White coefficient=1 makes the tone curve identity. Correct order is
        # sqrt(.125 + .5 * (2*.25 - .125)), not grading after the power.
        self.assertVectorAlmostEqual(pixel(data)['shader_rgba'][:3], [math.sqrt(0.3125)] * 3)

    def test_no_unproved_srgb_decode_or_encode(self):
        data = fixture()
        data['samples'][0]['sampled_t1_rgb'] = [0.25] * 3
        self.assertVectorAlmostEqual(pixel(data)['shader_rgba'][:3], [0.25] * 3)

    def test_final_rgb_clamps_before_power_but_alpha_does_not_clamp(self):
        data = fixture()
        data['samples'][0]['sampled_t1_rgb'] = [-0.5, 0.25, 2]
        data['constants']['cb2[3]'][3] = 2
        data['constants']['cb12[42]'][0] = 0.5
        output = pixel(data)['shader_rgba']
        self.assertEqual(output[0], 0)
        self.assertEqual(output[2], 1)
        self.assertEqual(output[3], 2)

    def test_alpha_participates_in_cinematic_path(self):
        data = fixture()
        data['constants']['cb2[3]'][0] = 0
        self.assertAlmostEqual(pixel(data)['shader_rgba'][3], 0.5)
        data['constants']['cb2[4]'][3] = 1
        # Tint's fourth component is both tint alpha and interpolation weight.
        self.assertAlmostEqual(pixel(data)['shader_rgba'][3], 0.5)

    def test_filmic_selection_strictly_greater_than_half(self):
        data = fixture()
        data['constants']['cb2[2]'][2] = 0.5
        self.assertEqual(pixel(data)['curve'], 'reinhard')
        data['constants']['cb2[2]'][2] = REPLAY.f32(0.50000006)
        out = pixel(data)
        self.assertEqual(out['curve'], 'filmic')
        # Independent closed form for the positive finite filmic case.
        h = (0.496 * (6.2 * 0.496 + 0.5)) / (0.496 * (6.2 * 0.496 + 1.7) + 0.06)
        self.assertAlmostEqual(out['trace']['selected_Q'], h ** 2.2, places=6)

    def test_unselected_filmic_branch_does_not_replace_reinhard(self):
        data = fixture()
        data['samples'][0]['sampled_t1_rgb'] = [0.001] * 3
        out = pixel(data)
        self.assertEqual(out['trace']['filmic_Q'], 0)
        self.assertVectorAlmostEqual(out['shader_rgba'][:3], [0.001] * 3)

    def test_fade_occurs_after_power_affects_alpha_and_has_no_new_clamp(self):
        data = fixture(27)
        data['samples'][0]['sampled_t1_rgb'] = [0.25] * 3
        data['constants']['cb12[42]'][0] = 0.5
        data['constants']['cb2[5]'] = [2, 0, -1, 0.5]
        self.assertVectorAlmostEqual(pixel(data)['shader_rgba'], [1.25, 0.25, -0.25, 0.75])

    def test_coordinate_transform_and_t0_branch_use_actual_bounds(self):
        data = fixture()
        data['samples'][0]['uv'] = [-0.25, 1.5]
        data['constants']['cb12[43]'][:2] = [0.8, 0.6]
        data['constants']['cb12[44]'][2] = 0.7
        plain = pixel(data)['sample_coordinates_before_sampler_addressing']
        self.assertVectorAlmostEqual(plain['t1'], [0, 0.6])
        self.assertEqual(plain['t0'], [-0.25, 1.5])
        self.assertEqual(plain['t2'], [-0.25, 1.5])
        data['constants']['cb2[0]'][0] = 1
        adjusted = pixel(data)['sample_coordinates_before_sampler_addressing']
        self.assertVectorAlmostEqual(adjusted['t0'], [0, 0.6])

    def test_explicit_fused_and_separate_policies_differ_for_cancellation(self):
        separate = REPLAY.Arithmetic('separate')
        try:
            fused = REPLAY.Arithmetic('fused')
        except ValueError:
            self.skipTest('System C99 fmaf unavailable')
        a, b = 1 + 2 ** -23, 1 - 2 ** -23
        self.assertEqual(separate.mad(a, b, -1), 0)
        self.assertEqual(fused.mad(a, b, -1), -(2 ** -46))

    def test_explicit_dxvk_dot_policy_uses_two_fused_accumulations(self):
        separate = REPLAY.Arithmetic()
        try:
            observed = REPLAY.Arithmetic('separate', 'fused-chain')
        except ValueError:
            self.skipTest('System C99 fmaf unavailable')
        a, b = 1 + 2 ** -23, 1 - 2 ** -23
        self.assertEqual(separate.dot3([-1, a, 0], [1, b, 0]), 0)
        self.assertEqual(observed.dot3([-1, a, 0], [1, b, 0]), -(2 ** -46))
        # The scalar MAD policy stays independent of the dot-product policy.
        self.assertEqual(observed.mad(a, b, -1), 0)
        receipt = REPLAY.replay(fixture(), dot_mode='fused-chain')
        self.assertEqual(receipt['arithmetic']['dot_mode'], 'fused-chain')

    def test_zero_adaptation_and_zero_power_expose_nonfinites(self):
        data = fixture()
        data['samples'][0]['sampled_t2_xy'] = [0, 0]
        out = REPLAY.replay(data)
        self.assertEqual(out['samples'][0]['trace']['exposure_ratio'], 'NaN')
        json.dumps(out, allow_nan=False)
        # log2(0)*0 remains NaN; the shader is not silently replaced with
        # Python pow(0,0), whose value would be one.
        data = fixture()
        data['samples'][0]['sampled_t1_rgb'] = [0, 0, 0]
        data['constants']['cb12[42]'][0] = 0
        self.assertEqual(pixel(data)['shader_rgba'][:3], ['NaN'] * 3)


class EvidenceContractTests(unittest.TestCase):
    def test_unknown_shader_missing_constants_and_sampling_domain_fail(self):
        for mutation in ('shader', 'constant', 'domain', 'uv', 'sample', 'duplicate'):
            with self.subTest(mutation=mutation):
                data = fixture()
                if mutation == 'shader':
                    data['shader_sha256'] = '0' * 64
                elif mutation == 'constant':
                    del data['constants']['cb12[43]']
                elif mutation == 'domain':
                    data['sample_value_domain'] = 'raw_texture'
                elif mutation == 'uv':
                    del data['samples'][0]['uv']
                elif mutation == 'sample':
                    del data['samples'][0]['sampled_t2_xy']
                else:
                    data['samples'].append(copy.deepcopy(data['samples'][0]))
                with self.assertRaises(ValueError):
                    REPLAY.replay(data)

    def test_capture_requires_evidenced_explicit_resource_slot_mapping(self):
        data = fixture()
        data['provenance'] = {'kind': 'captured'}
        with self.assertRaisesRegex(ValueError, 'capture_sha256'):
            REPLAY.replay(data)
        data['provenance'].update(capture_sha256='a' * 64, event_id=123,
                                  slot_mapping_evidence='receipt.json#native-slot-map',
                                  sample_evidence='pixel-debug.json#shader-sample-results')
        with self.assertRaisesRegex(ValueError, 'explicit bindings'):
            REPLAY.replay(data)
        data['bindings'] = {f't{slot}': {'resource_id': f'ResourceId::{slot + 10}', 'sampler_slot': slot,
                                        'srv_format': 'R16G16B16A16_SFLOAT',
                                        'sampler_evidence': f'receipt.json#sampler-{slot}'}
                            for slot in range(3)}
        out = REPLAY.replay(data)
        self.assertEqual(out['evidence_kind'], 'captured')
        self.assertEqual(out['provenance'], data['provenance'])
        data['bindings']['t2']['sampler_slot'] = 0
        with self.assertRaisesRegex(ValueError, 'Captured t2'):
            REPLAY.replay(data)

    def test_expected_shader_values_compare_and_nan_never_passes(self):
        data = fixture()
        data['samples'][0]['expected_shader_rgba'] = [0.5, 0.5, 0.5, 1]
        out = REPLAY.replay(data)
        self.assertEqual(out['comparison_count'], 1)
        self.assertTrue(out['all_comparisons_within_tolerance'])
        data['samples'][0]['expected_shader_rgba'][0] = 0.6
        self.assertFalse(REPLAY.replay(data)['all_comparisons_within_tolerance'])
        self.assertFalse(REPLAY.compare([math.nan], [math.nan], 0, 0)['within_tolerance'])

    def test_no_expected_output_means_no_parity_claim(self):
        out = REPLAY.replay(fixture())
        self.assertEqual(out['comparison_count'], 0)
        self.assertIsNone(out['all_comparisons_within_tolerance'])

    def test_tolerance_and_component_contract_reject_bool_and_invalid_values(self):
        for atol in (-1, math.inf, math.nan):
            with self.subTest(atol=atol), self.assertRaises(ValueError):
                REPLAY.replay(fixture(), atol=atol)
        data = fixture()
        data['constants']['cb2[2]'][0] = True
        with self.assertRaises(ValueError):
            REPLAY.replay(data)

    def test_cli_records_input_hash_preserves_files_and_reports_mismatch(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / 'input.json', root / 'receipt.json'
            data = fixture()
            data['samples'][0]['expected_shader_rgba'] = [0.6, 0.5, 0.5, 1]
            raw = json.dumps(data).encode()
            source.write_bytes(raw)
            with patch('sys.stdout'), patch('sys.stderr'):
                code = REPLAY.main(['--input', str(source), '--output', str(output)])
            self.assertEqual(code, 1)
            receipt = json.loads(output.read_text())
            self.assertEqual(receipt['input']['sha256'], REPLAY.hashlib.sha256(raw).hexdigest())
            self.assertEqual(receipt['script_sha256'], REPLAY.hashlib.sha256(SCRIPT.read_bytes()).hexdigest())
            self.assertEqual(source.read_bytes(), raw)
            saved = output.read_bytes()
            with patch('sys.stdout'), patch('sys.stderr'):
                self.assertEqual(REPLAY.main(['--input', str(source), '--output', str(output)]), 2)
                self.assertEqual(REPLAY.main(['--input', str(source), '--output', str(source)]), 2)
            self.assertEqual(output.read_bytes(), saved)
            self.assertEqual(source.read_bytes(), raw)

    def test_nonstandard_json_nonfinite_literals_rejected(self):
        with self.assertRaises(ValueError):
            json.loads('{"value":NaN}', parse_constant=REPLAY.reject_json_constant)


class PackedFloatStorageTests(unittest.TestCase):
    def test_unsigned_components_exhaustively_match_padded_ieee_half(self):
        # Each packed component has exactly the positive binary16 exponent
        # layout, with the low fraction bits omitted. Compare every encoding
        # to Python's independent IEEE half decoder, including special values.
        for fraction_bits, shift, size in ((6, 0, 11), (6, 11, 11), (5, 22, 10)):
            component = {0: 0, 11: 1, 22: 2}[shift]
            for encoded in range(1 << size):
                expected = struct.unpack('<e', struct.pack('<H', encoded << (10 - fraction_bits)))[0]
                decoded = REPLAY.decode_r11g11b10_float_word(encoded << shift)[component]
                if math.isnan(expected):
                    self.assertTrue(math.isnan(decoded))
                else:
                    self.assertEqual(decoded, expected)

    def test_channel_order_and_independent_bit_boundaries(self):
        # Exponent15, fraction0 is one in every component; mixed values must
        # not swap the low red bits with the high blue bits.
        packed = (15 << 6) | ((16 << 6) << 11) | ((17 << 5) << 22)
        self.assertEqual(REPLAY.decode_r11g11b10_float_word(packed), [1, 2, 4])
        self.assertEqual(REPLAY.decode_r11g11b10_float_word(0), [0, 0, 0])

    def test_non_word_values_rejected(self):
        for value in (-1, 2 ** 32, True, 0.5, '0'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                REPLAY.decode_r11g11b10_float_word(value)


if __name__ == '__main__':
    unittest.main()
