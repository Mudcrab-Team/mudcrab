import gzip
import json
from pathlib import Path
import tempfile
import unittest

from analyze_skyrim_jump_inputs import (HEADER, LAUNCH_REQUIRED, SETTER_REQUIRED,
                                        analyze_rows, decode, f32)
from capture_skyrim_jump_inputs import control_prediction
from record_skyrim_motion import RECORD, read_fixture
from test_record_skyrim_motion import recording


def input_rows():
    v = [0.0] * 40
    v[:7] = [76.0, 100.0, 43.0, 0.0, 1.0, f32(1.03), f32(1.03)]
    v[7] = f32(f32(f32(v[1] * f32(.01)) * f32(v[4] * v[5])) * v[0])
    v[9] = f32(v[7] * f32(.0142875))
    v[10:16] = [1.0, f32(.0142875), 76.0, 2.0, 0.0, 1.0]
    v[16:23] = [20.0, 0.0, 7.0, 0.0, 14150.0, 1.0, f32(.01)]
    w = v.copy()
    w[8] = v[9]
    a = [0.0] * 40
    a[6] = f32((f32(f32(abs(f32(-9.8)) * v[9]) * 2.0)) ** .5)
    a[11], a[16], a[39], a[10] = v[9], 2.0, f32(-9.8), 1.0
    launch = [0.0] * 40
    launch[11] = v[9]
    return [dict(kind=10, text='setter', code=SETTER_REQUIRED, state=0x6752ba,
                 values=v, qpc=1, sample=1),
            dict(kind=8, code=3, values=a, qpc=2, sample=1),
            dict(kind=9, values=launch, qpc=3, sample=1),
            dict(kind=10, text='launch', code=LAUNCH_REQUIRED, values=w, qpc=4, sample=1)]


class JumpInputsProofTests(unittest.TestCase):
    def test_native_order_and_separate_form_identity(self):
        result = analyze_rows(input_rows())
        self.assertEqual(result['setter_argument'], f32(76 * f32(1.03)))
        self.assertEqual(result['inputs']['race_form_id'], '00013746')
        self.assertEqual(result['inputs']['npc_form_id'], '00000007')
        self.assertEqual(result['setter_call_rva'], '0x6752ba')

    def test_invalid_read_does_not_become_a_zero_measurement(self):
        rows = input_rows()
        rows[0]['code'] &= ~2
        with self.assertRaisesRegex(ValueError, 'Invalid setter'):
            analyze_rows(rows)

    def test_invalid_form_identity_read_fails(self):
        rows = input_rows()
        rows[0]['code'] &= ~32768
        with self.assertRaisesRegex(ValueError, 'Invalid setter'):
            analyze_rows(rows)

    def test_wrong_setter_argument_cannot_pass_product_check(self):
        rows = input_rows()
        rows[0]['values'][7] += 1
        with self.assertRaisesRegex(ValueError, 'Raw-factor prediction'):
            analyze_rows(rows)

    def test_store_must_match_native_conversion(self):
        rows = input_rows()
        rows[0]['values'][9] += .1
        with self.assertRaisesRegex(ValueError, 'Setter post-store'):
            analyze_rows(rows)

    def test_launch_snapshot_cannot_hide_changed_gmst(self):
        rows = input_rows()
        rows[-1]['values'][0] = 64
        with self.assertRaisesRegex(ValueError, 'Loaded inputs changed'):
            analyze_rows(rows)

    def test_duplicate_setter_cannot_pass(self):
        rows = input_rows()
        rows.append(rows[0])
        with self.assertRaisesRegex(ValueError, 'exactly one setter'):
            analyze_rows(rows)

    def test_control_prediction_uses_baseline_raw_factors(self):
        baseline = analyze_rows(input_rows())
        setting = control_prediction(baseline, {'jump_height_min': 64})
        reference = control_prediction(baseline, {'reference_scale': 1.25})
        self.assertEqual(setting['setter_argument'], f32(f32(1.03) * 64))
        self.assertEqual(reference['reference_u16'], 125)
        self.assertEqual(reference['setter_argument'], f32(f32(1.25 * f32(1.03)) * 76))

    def test_v4_decoder_requires_its_own_version_and_complete_record(self):
        rows = recording()
        rows[0].update(text=HEADER, code=4)
        raw = b''.join(RECORD.pack(row['sequence'], row['qpc'], row['sample'], row['input_sequence'],
                                 row['kind'], row['thread'], row['code'], row['state'],
                                 *row['values'], row['text'].encode()) for row in rows)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'trace.bin.gz'
            path.write_bytes(gzip.compress(raw))
            decoded, _ = decode(path)
            self.assertEqual(decoded[0]['text'], HEADER)
            path.write_bytes(gzip.compress(raw[:-1]))
            with self.assertRaisesRegex(ValueError, 'Truncated v4'):
                decode(path)
            rows[0]['code'] = 3
            wrong = bytearray(raw)
            wrong[40:44] = (3).to_bytes(4, 'little')
            path.write_bytes(gzip.compress(wrong))
            with self.assertRaisesRegex(ValueError, 'separate v4'):
                decode(path)

    def test_typed_controls_reject_arbitrary_console_commands(self):
        fixture = dict(schema=1, kind='jump', position=[0, 0, 0], yaw_degrees=0,
                       hold_ms=500, jump_height_min='64; save altered')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'fixture.json'
            path.write_text(json.dumps(fixture))
            with self.assertRaisesRegex(ValueError, 'Unsupported jump-input intervention'):
                read_fixture(path)


if __name__ == '__main__':
    unittest.main()
