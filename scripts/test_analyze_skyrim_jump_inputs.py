import contextlib
import gzip
import hashlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
from unittest.mock import patch
import unittest
import zipfile

from analyze_skyrim_jump_inputs import (HEADER, LAUNCH_REQUIRED, SETTER_REQUIRED,
                                        analyze_rows, decode, f32)
import capture_skyrim_jump_inputs as capture
from capture_skyrim_jump_inputs import control_prediction
from verify_skyrim_jump_inputs import verify
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


class CaptureResumeTests(unittest.TestCase):
    def test_failed_attempt_then_resume_produces_a_verifiable_campaign(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        observed = json.loads((reference / 'manifest.json').read_text())
        plan = capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        declaration = json.loads(plan.read_text())
        baseline = gzip.decompress((reference / observed['runs'][0]['path']).read_bytes())
        for mode in ('proof', 'runner', 'startup'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                session_dir = root / 'session'
                session_dir.mkdir()
                session = dict(profile_name='inputs-v4',
                               exe_sha256=declaration['runtime_sha256'],
                               source_save_sha256=declaration['source_save_sha256'],
                               input_sha256='test-input', proton_sha256='test-proton',
                               build=dict(source_sha256=capture.digest(capture.ROOT / 'tools/skyrim-recorder/probe.cpp'),
                                          extension_sha256=capture.digest(capture.ROOT / 'tools/skyrim-recorder/jump_inputs.h'),
                                          dll_sha256='test-dll'))
                capture.write(session_dir / 'session.json', session)
                corpus = root / 'corpus'
                argv = ['capture', '--session', str(session_dir), '--corpus', str(corpus),
                        '--private-output', str(root / 'private'), '--ocr-prefix', '/', '--plan', str(plan)]
                failed = False

                def record(command, **_kwargs):
                    nonlocal failed
                    output = Path(command[command.index('--output') + 1])
                    output.mkdir()
                    name = Path(command[command.index('--fixture') + 1]).stem
                    run = next(row for row in observed['runs'] if row['id'] == name)
                    raw = gzip.decompress((reference / run['path']).read_bytes())
                    proof = dict(run['proof'], session=session,
                                 runner_sha256=capture.digest(capture.ROOT / 'scripts/record_skyrim_motion.py'),
                                 trace_sha256=hashlib.sha256(raw).hexdigest())
                    if not failed:
                        failed = True
                        if mode == 'proof':
                            raw = raw[:-1]
                        else:
                            if mode == 'runner':
                                (output / 'trace.bin').write_bytes(raw)
                            capture.write(output / 'failure.json', dict(phase='fixture-readiness', error='test interruption'))
                            return SimpleNamespace(returncode=1)
                    (output / 'trace.bin').write_bytes(raw)
                    capture.write(output / 'proof.json', proof)
                    return SimpleNamespace(returncode=0)

                with patch('sys.argv', argv), patch.object(capture.subprocess, 'run', record), contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises((ValueError, RuntimeError)):
                        capture.main()
                    manifest = json.loads((corpus / 'manifest.json').read_text())
                    self.assertEqual(manifest['runs'], [])
                    failure = manifest['attempts'][0]
                    if mode == 'startup':
                        self.assertEqual(failure['trace_status'], 'not-created')
                    else:
                        raw = baseline[:-1] if mode == 'proof' else baseline
                        self.assertEqual(gzip.decompress((corpus / failure['path']).read_bytes()), raw)
                        self.assertEqual(failure['trace_sha256'], hashlib.sha256(raw).hexdigest())
                    capture.main()
                result = verify(corpus, plan)
                self.assertEqual(result['status'], 'passed')
                self.assertEqual(len(result['runs']), 4)
                self.assertEqual(result['attempts'], 5)
                self.assertEqual(result['failed_attempts'], [failure])
                if mode == 'proof':
                    archive_path = corpus / 'capture-source.zip'
                    with zipfile.ZipFile(archive_path, 'a') as archive:
                        archive.writestr('unexpected-source.py', 'changed source set')
                    archived = archive_path.read_bytes()
                    manifest_bytes = (corpus / 'manifest.json').read_bytes()
                    error = io.StringIO()
                    with patch('sys.argv', argv), contextlib.redirect_stderr(error):
                        with self.assertRaises(SystemExit):
                            capture.main()
                    self.assertIn('Capture sources changed', error.getvalue())
                    self.assertEqual(archive_path.read_bytes(), archived)
                    self.assertEqual((corpus / 'manifest.json').read_bytes(), manifest_bytes)
                if mode != 'startup':
                    (corpus / failure['path']).write_bytes(gzip.compress(b'changed'))
                    with self.assertRaisesRegex(ValueError, 'Failed trace identity'):
                        verify(corpus, plan)
                else:
                    manifest = json.loads((corpus / 'manifest.json').read_text())
                    manifest['attempts'][0]['exit_code'] = 0
                    capture.write(corpus / 'manifest.json', manifest)
                    with self.assertRaisesRegex(ValueError, 'Missing trace is only valid'):
                        verify(corpus, plan)


if __name__ == '__main__':
    unittest.main()
