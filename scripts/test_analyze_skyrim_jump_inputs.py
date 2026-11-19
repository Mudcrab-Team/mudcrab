import contextlib
import gzip
import hashlib
import io
import json
import shutil
from pathlib import Path
import tempfile
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
from test_capture_skyrim_attempts import campaign


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
            dict(kind=9, code=3, values=launch, qpc=3, sample=1, sequence=3, thread=7),
            dict(kind=10, text='launch', code=LAUNCH_REQUIRED, values=w, qpc=4, sample=1, sequence=4, thread=7)]


class JumpInputsProofTests(unittest.TestCase):
    def test_native_order_and_separate_form_identity(self):
        result = analyze_rows(input_rows())
        self.assertEqual(result['setter_argument'], f32(76 * f32(1.03)))
        self.assertEqual(result['inputs']['race_form_id'], '00013746')
        self.assertEqual(result['inputs']['npc_form_id'], '00000007')
        self.assertEqual(result['setter_call_rva'], '0x6752ba')

    def test_float32_product_rounds_nonunit_factors_and_selects_the_female_race_height(self):
        rows = input_rows()
        setter, air, launch, snapshot = (r['values'] for r in rows)
        setter[1], setter[3], setter[4] = 99, 1, f32(1.06)
        setter[5:7] = [f32(1.13), f32(.97)]
        # Native float32 products round after each operation. A single final rounding gives 77.361763.
        setter[7] = 77.36177062988281
        setter[9] = f32(setter[7] * setter[11])
        snapshot[:] = setter
        air[11] = launch[11] = snapshot[8] = setter[9]
        air[6] = f32((f32(f32(abs(air[39]) * setter[9]) * 2.0)) ** .5)
        self.assertEqual(analyze_rows(rows)['setter_argument'], 77.36177062988281)

    def test_launch_snapshot_must_immediately_follow_its_own_valid_callback(self):
        for index, field, value, reason in ((3, 'sample', 2, 'snapshot'), (3, 'sequence', 5, 'snapshot'),
                                            (3, 'thread', 6, 'snapshot'), (3, 'qpc', 2, 'snapshot'),
                                            (2, 'code', 1, 'launch callback'), (0, 'state', 0, 'setter path')):
            with self.subTest(field=field):
                rows = input_rows()
                rows[index][field] = value
                with self.assertRaisesRegex(ValueError, reason):
                    analyze_rows(rows)

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

    def test_control_requires_a_measured_change_from_baseline(self):
        baseline = analyze_rows(input_rows())
        for condition in ({'jump_height_min': 76}, {'reference_scale': 1.0}):
            with self.subTest(condition=condition), self.assertRaisesRegex(ValueError, 'matches baseline'):
                control_prediction(baseline, condition)

    def test_setter_observer_can_emit_after_launch(self):
        rows = input_rows()
        rows[0]['qpc'] = rows[2]['qpc'] + 2
        self.assertEqual(analyze_rows(rows)['status'], 'passed')

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
            for record in (raw[:RECORD.size], raw[-RECORD.size:]):
                path.write_bytes(gzip.compress(raw[:RECORD.size] + record + raw[RECORD.size:]))
                with self.assertRaisesRegex(ValueError, 'Duplicate header or footer'):
                    decode(path)
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
    def test_capture_retries_partial_archive_and_keeps_only_verified_trace_snapshots(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        evidence = {run['id']: run for run in json.loads((reference / 'manifest.json').read_bytes())['runs']}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, session = campaign(capture, root)
            argv = argv[:-2]  # collect every declared condition
            corpus = root / 'corpus'
            private_trace = None
            drift = True
            decode_snapshot = capture.decode_bytes

            def record(command, **kwargs):
                nonlocal private_trace
                name = Path(command[command.index('--fixture') + 1]).stem
                run = evidence[name]
                raw = gzip.decompress((reference / run['path']).read_bytes())
                if drift and name == 'repeat-baseline':
                    rows = []
                    for values in RECORD.iter_unpack(raw):
                        row = list(values)
                        if row[4] == 10:
                            row[8 + 13] += 1  # observed swim setting at setter and launch
                        rows.append(RECORD.pack(*row))
                    raw = b''.join(rows)
                output = Path(command[command.index('--output') + 1])
                output.mkdir()
                private_trace = output / 'trace.bin'
                private_trace.write_bytes(raw)
                capture.write(output / 'proof.json', dict(run['proof'], session=session,
                              trace_sha256=hashlib.sha256(raw).hexdigest(),
                              runner_sha256=capture.digest(capture.ROOT / 'scripts/record_skyrim_motion.py')))
                return 0

            def replace_after_read(raw):
                if private_trace.parent.name == 'baseline-attempt-1':
                    private_trace.write_bytes(b'changed by another process')
                return decode_snapshot(raw)

            with patch('sys.argv', argv), patch.object(capture, 'run_recorder', side_effect=record) as recorder, \
                    patch.object(capture, 'decode_bytes', side_effect=replace_after_read):
                with patch.object(zipfile.ZipFile, 'writestr', side_effect=KeyboardInterrupt), \
                        self.assertRaises(KeyboardInterrupt):
                    capture.main()
                self.assertFalse((corpus / 'capture-source.zip').exists())
                recorder.assert_not_called()
                with self.assertRaisesRegex(ValueError, 'Repeat baseline inputs'):
                    capture.main()
                manifest = json.loads((corpus / 'manifest.json').read_bytes())
                self.assertEqual([r['id'] for r in manifest['runs']], ['baseline', 'setting-64', 'reference-125'])
                baseline = manifest['runs'][0]
                self.assertEqual(gzip.decompress((corpus / baseline['path']).read_bytes()),
                                 gzip.decompress((reference / evidence['baseline']['path']).read_bytes()))
                trace = corpus / baseline['path']
                retained = trace.read_bytes()
                trace.unlink()
                for source in (None, gzip.compress(b'wrong bytes')):
                    if source is not None:
                        trace.write_bytes(source)
                    with patch('sys.argv', argv + ['--condition', 'repeat-baseline']), \
                            self.assertRaisesRegex(ValueError, 'baseline.bin.gz'):
                        capture.main()
                    self.assertEqual(recorder.call_count, 4)
                trace.write_bytes(retained)
                drift = False
                capture.main()
            plan_path = Path(argv[argv.index('--plan') + 1])
            self.assertEqual(verify(corpus, plan_path)['status'], 'passed')
            manifest_path = corpus / 'manifest.json'
            manifest = json.loads(manifest_path.read_bytes())
            del manifest['runs'][0]['proof']['camera_cadence']
            capture.write(manifest_path, manifest)
            with self.assertRaisesRegex(ValueError, 'Stored camera cadence'):
                verify(corpus, plan_path)

    def test_two_condition_plan_verifies_snapshot_identities_and_rejects_false_cadence(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = json.loads((capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json').read_bytes())
        plan['conditions'] = plan['conditions'][:2]
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory)
            for name in ('traces', 'capture-source.zip'):
                (corpus / name).symlink_to(reference / name)
            plan_path, path = corpus / 'plan.json', corpus / 'manifest.json'
            capture.write(plan_path, plan)
            expected_plan = capture.digest(plan_path)
            manifest = json.loads((reference / 'manifest.json').read_bytes())
            manifest['runs'] = manifest['runs'][:2]
            ids = {run['id'] for run in manifest['runs']}
            manifest['attempts'] = [a for a in manifest['attempts'] if a['id'] in ids]
            manifest['plan_sha256'] = expected_plan
            capture.write(path, manifest)
            expected_manifest = capture.digest(path)

            def replace_inputs(archive):
                path.write_text('{}')
                plan_path.write_text('{}')
                return capture.read_capture_sources(archive)

            with patch('verify_skyrim_jump_inputs.read_capture_sources', side_effect=replace_inputs):
                result = verify(corpus, plan_path)
            self.assertEqual([r['id'] for r in result['runs']], ['baseline', 'setting-64'])
            self.assertEqual(result['manifest_sha256'], expected_manifest)
            self.assertEqual(result['plan_sha256'], expected_plan)
            manifest['runs'][0]['proof']['camera_cadence'] = {'achieved_fps': 30}
            capture.write(plan_path, plan)
            capture.write(path, manifest)
            with self.assertRaisesRegex(ValueError, 'Stored camera cadence'):
                verify(corpus, plan_path)

    def test_ambiguous_source_archive_fails_verification(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory)
            for name in ('manifest.json', 'capture-source.zip'):
                shutil.copyfile(reference / name, corpus / name)
            with zipfile.ZipFile(corpus / 'capture-source.zip', 'a') as archive:
                name = archive.namelist()[0]
                with self.assertWarns(UserWarning):
                    archive.writestr(name, archive.read(name))
            with self.assertRaisesRegex(ValueError, 'Duplicate source archive member'):
                verify(corpus, plan)

    def test_public_failure_redacts_home_path_and_preserves_trace(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / 'private'
            output.mkdir()
            (output / 'trace.bin').write_bytes(b'partial numeric trace')
            corpus = root / 'corpus'
            (corpus / 'traces').mkdir(parents=True)
            attempt = {'id': 'baseline', 'attempt': 1, 'exit_code': 1, 'private_session': str(root)}
            failure = {'phase': 'recorder', 'error': '/home/private-operator/game/proof.json missing'}
            capture.retain_failure(corpus, output, attempt, failure)
            self.assertNotIn('/home/private-operator', json.dumps(attempt))
            self.assertIn('<operator-home>/game/proof.json', attempt['failure']['error'])
            self.assertEqual(failure['error'], '/home/private-operator/game/proof.json missing')
            self.assertEqual(gzip.decompress((corpus / attempt['path']).read_bytes()),
                             (output / 'trace.bin').read_bytes())

    def test_failed_attempt_then_resume_produces_a_verifiable_campaign(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        observed = json.loads((reference / 'manifest.json').read_text())
        plan = capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        declaration = json.loads(plan.read_text())
        baseline = gzip.decompress((reference / observed['runs'][0]['path']).read_bytes())
        run_recorder = capture.run_recorder
        overflow = bytearray(baseline)
        for offset in range(0, len(overflow), RECORD.size):
            if RECORD.unpack_from(overflow, offset)[4] == 10:
                overflow[offset + 64:offset + 68] = (0x7f7fffff).to_bytes(4, 'little')
        for mode in ('proof', 'runner', 'startup', 'preflight', 'overflow', 'proof-key', 'public-proof-key'):
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
                        elif mode == 'overflow':
                            raw = bytes(overflow)
                            proof['trace_sha256'] = hashlib.sha256(raw).hexdigest()
                        elif mode == 'proof-key':
                            del proof['trace_sha256']
                        elif mode == 'public-proof-key':
                            del proof['ini']
                        elif mode == 'preflight':
                            output.rmdir()
                            return run_recorder(command, **_kwargs)
                        else:
                            if mode == 'runner':
                                (output / 'trace.bin').write_bytes(raw)
                            capture.write(output / 'failure.json', dict(phase='fixture-readiness', error='test interruption'))
                            return 1
                    (output / 'trace.bin').write_bytes(raw)
                    capture.write(output / 'proof.json', proof)
                    return 0

                with patch('sys.argv', argv), patch.object(capture, 'run_recorder', record), contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises((ValueError, RuntimeError, FileNotFoundError, OverflowError, KeyError)):
                        capture.main()
                    manifest = json.loads((corpus / 'manifest.json').read_text())
                    self.assertEqual(manifest['runs'], [])
                    failure = manifest['attempts'][0]
                    self.assertIn('failure', failure)
                    if mode == 'overflow':
                        self.assertTrue(failure['failure']['error'].startswith('OverflowError:'))
                    if mode in ('proof-key', 'public-proof-key'):
                        self.assertTrue(failure['failure']['error'].startswith('KeyError:'))
                    if mode in ('startup', 'preflight'):
                        self.assertEqual(failure['trace_status'], 'not-created')
                    else:
                        raw = baseline[:-1] if mode == 'proof' else bytes(overflow) if mode == 'overflow' else baseline
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
                    original_archive = archive_path.read_bytes()
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
                    archive_path.write_bytes(original_archive)
                if mode not in ('startup', 'preflight'):
                    (corpus / failure['path']).write_bytes(gzip.compress(b'changed'))
                    with self.assertRaisesRegex(ValueError, 'Failed trace identity'):
                        verify(corpus, plan)
                else:
                    manifest = json.loads((corpus / 'manifest.json').read_text())
                    manifest['attempts'][0]['exit_code'] = 0
                    capture.write(corpus / 'manifest.json', manifest)
                    with self.assertRaisesRegex(ValueError, 'Missing trace is only valid'):
                        verify(corpus, plan)


    def test_resume_rejects_a_changed_prepared_session_before_dispatch(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        baseline = json.loads((reference / 'manifest.json').read_bytes())['runs'][0]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, session = campaign(capture, root)
            adopted = root / 'adopted'
            adopted.mkdir()
            raw = gzip.decompress((reference / baseline['path']).read_bytes())
            (adopted / 'trace.bin').write_bytes(raw)
            capture.write(adopted / 'proof.json', dict(baseline['proof'], session=session, services_stopped=True,
                          trace_sha256=hashlib.sha256(raw).hexdigest(),
                          runner_sha256=capture.digest(capture.ROOT / 'scripts/record_skyrim_motion.py')))
            with patch('sys.argv', argv + ['--adopt-baseline', str(adopted)]):
                capture.main()
            path = root / 'corpus/manifest.json'
            checkpoint = path.read_bytes()
            session['proton_sha256'] = 'a' * 64
            capture.write(root / 'session/session.json', session)
            argv[argv.index('--condition') + 1] = 'setting-64'
            with patch('sys.argv', argv), patch.object(capture, 'run_recorder', return_value=1) as recorder:
                with self.assertRaisesRegex(ValueError, 'session or tool identity'):
                    capture.main()
                recorder.assert_not_called()
            self.assertEqual(path.read_bytes(), checkpoint)

    def test_offline_verification_rejects_mixed_session_and_tool_hashes(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory)
            for name in ('capture-source.zip', 'traces'):
                (corpus / name).symlink_to(reference / name)
            for section, key in [('receipt', 'session_sha256'), ('proof', 'dll_sha256'),
                                 ('proof', 'input_sha256'), ('proof', 'proton_sha256')]:
                with self.subTest(key=key):
                    manifest = json.loads((reference / 'manifest.json').read_bytes())
                    manifest['runs'][1][section][key] = 'a' * 64
                    capture.write(corpus / 'manifest.json', manifest)
                    with self.assertRaisesRegex(ValueError, 'session or tool identity'):
                        verify(corpus, plan)

    def test_missing_failed_attempt_breaks_ledger_continuity(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory)
            manifest = json.loads((reference / 'manifest.json').read_text())
            del manifest['attempts'][0]
            capture.write(corpus / 'manifest.json', manifest)
            for name in ('capture-source.zip', 'traces'):
                (corpus / name).symlink_to(reference / name)
            with self.assertRaisesRegex(ValueError, 'Attempt ledger'):
                verify(corpus, plan)

    def test_repeat_baseline_requires_independent_trace_bytes(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory)
            shutil.copytree(reference / 'traces', corpus / 'traces')
            (corpus / 'capture-source.zip').symlink_to(reference / 'capture-source.zip')
            manifest = json.loads((reference / 'manifest.json').read_text())
            baseline, repeat = manifest['runs'][0], manifest['runs'][-1]
            for key in ('sha256', 'proof', 'result'):
                repeat[key] = baseline[key]
            raw = gzip.decompress((corpus / baseline['path']).read_bytes())
            (corpus / repeat['path']).write_bytes(gzip.compress(raw, mtime=1))
            capture.write(corpus / 'manifest.json', manifest)
            with self.assertRaisesRegex(ValueError, 'trace reused'):
                verify(corpus, plan)

    def test_unclassified_attempt_cannot_disappear_after_a_successful_retry(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory) / 'corpus'
            shutil.copytree(reference, corpus)
            original = (corpus / 'manifest.json').read_text()
            for missing_failure in [index for index, attempt in enumerate(json.loads(original)['attempts'])
                                    if 'failure' in attempt]:
                with self.subTest(attempt=missing_failure):
                    manifest = json.loads(original)
                    del manifest['attempts'][missing_failure]['failure']
                    capture.write(corpus / 'manifest.json', manifest)
                    with self.assertRaisesRegex(ValueError, 'Attempt ledger'):
                        verify(corpus, plan)


if __name__ == '__main__':
    unittest.main()
