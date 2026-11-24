import contextlib
import gzip
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import capture_skyrim_jump as capture
from test_capture_skyrim_attempts import campaign
from test_evaluate_skyrim_jump import synthetic


class CampaignProfileTests(unittest.TestCase):
    def test_v4_session_is_rejected_before_starting_or_writing_a_campaign(self):
        self.check_profile('inputs-v4')

    def test_historical_v3_session_reaches_the_recorder(self):
        self.check_profile(None)

    def check_profile(self, profile):
        plan = json.loads((capture.ROOT / 'tools/skyrim-recorder/fixtures/jump-v3.json').read_text())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            session = root / 'session'
            session.mkdir()
            config = {'exe_sha256': plan['runtime_sha256'],
                      'source_save_sha256': plan['source_save_sha256'],
                      'build': {'source_sha256': capture.digest(capture.ROOT / 'tools/skyrim-recorder/probe.cpp')}}
            if profile is not None:
                config['profile_name'] = profile
            (session / 'session.json').write_text(json.dumps(config))
            corpus, private = root / 'corpus', root / 'private'
            argv = ['capture', '--session', str(session), '--corpus', str(corpus),
                    '--private-output', str(private), '--ocr-prefix', str(root), '--split', 'train']
            error = io.StringIO()
            with patch('sys.argv', argv), contextlib.redirect_stderr(error), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    patch.object(capture, 'run_recorder', side_effect=RuntimeError('recorder started')) as run:
                if profile is not None:
                    with self.assertRaises(SystemExit) as stopped:
                        capture.main()
                    self.assertEqual(stopped.exception.code, 2)
                    self.assertIn('v3 executable/save/profile', error.getvalue())
                    run.assert_not_called()
                    self.assertFalse(corpus.exists())
                    self.assertFalse(private.exists())
                else:
                    with self.assertRaisesRegex(RuntimeError, 'recorder started'):
                        capture.main()
                    run.assert_called_once()


class CampaignEvidenceTests(unittest.TestCase):
    def test_heldout_requires_sealed_fit_from_compatible_session(self):
        reference = capture.ROOT / 'tools/skyrim-recorder/reference/jump-v3/frozen.json'
        frozen = json.loads(reference.read_bytes())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, session = campaign(capture, root)
            argv[-1] = 'heldout'
            model = root / 'model.json'
            model.write_text('{}')
            with patch('sys.argv', argv + ['--frozen', str(model)]), \
                    patch.object(capture, 'run_recorder', side_effect=RuntimeError('started')) as recorder:
                with self.assertRaisesRegex(ValueError, 'seal'):
                    capture.main()
                model.write_bytes(reference.read_bytes())
                with self.assertRaisesRegex(ValueError, 'prepared session'):
                    capture.main()
                recorder.assert_not_called()
                self.assertFalse((root / 'corpus').exists())
                contract = frozen['provenance_contract']
                session['build']['dll_sha256'] = contract['dll_sha256']
                session.update(proton_sha256=contract['proton_sha256'], input_sha256=contract['input_sha256'])
                capture.write(root / 'session/session.json', session)
                with self.assertRaisesRegex(RuntimeError, 'started'):
                    capture.main()
                self.assertEqual(json.loads((root / 'corpus/heldout-capture.json').read_bytes())['frozen_sha256'],
                                 capture.digest(model))

    def test_plan_hash_and_dispatch_use_the_preflight_snapshots(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, _ = campaign(capture, root)
            plan = root / 'plan.json'
            plan.write_bytes(Path(argv[argv.index('--plan') + 1]).read_bytes())
            original_digest = capture.digest(plan)
            argv[argv.index('--plan') + 1] = str(plan)
            current = root / 'current'
            session = root / 'session'
            current.symlink_to(session)
            argv[argv.index('--session') + 1] = str(current)
            read = Path.read_bytes

            def change_after_preflight(path):
                data = read(path)
                if path == session / 'session.json':
                    changed = json.loads(read(plan))
                    changed['fixture']['hold_ms'] = 600
                    capture.write(plan, changed)
                    current.unlink()
                    current.symlink_to(root / 'other-session')
                return data

            with patch('sys.argv', argv), patch.object(Path, 'read_bytes', change_after_preflight), \
                    patch.object(capture, 'run_recorder', side_effect=RuntimeError('dispatched')) as recorder:
                with self.assertRaisesRegex(RuntimeError, 'dispatched'):
                    capture.main()
            command, = recorder.call_args.args
            self.assertEqual(Path(command[command.index('--session') + 1]), session)
            fixture = Path(command[command.index('--fixture') + 1])
            self.assertEqual(json.loads(fixture.read_bytes())['hold_ms'], 500)
            receipt = json.loads((root / 'corpus/train-capture.json').read_bytes())
            self.assertEqual(receipt['plan_sha256'], original_digest)


    def test_failed_jump_and_changed_session_are_retryable_and_resume_checks_retained_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, session = campaign(capture, root)
            plan = json.loads(Path(argv[argv.index('--plan') + 1]).read_bytes())
            plan['repeats'] = 1
            plan_file = root / 'plan.json'
            capture.write(plan_file, plan)
            argv[argv.index('--plan') + 1] = str(plan_file)
            session_link = root / 'current'
            session_link.symlink_to(root / 'session')
            argv[argv.index('--session') + 1] = str(session_link)
            proof_session = dict(session, proton_sha256='other')
            raw = synthetic(no_jump=True)
            change_fixture = False

            def record(command, **kwargs):
                self.assertEqual(Path(command[command.index('--session') + 1]), root / 'session')
                output = Path(command[command.index('--output') + 1])
                output.mkdir()
                (output / 'trace.bin').write_bytes(raw)
                fixture = Path(command[command.index('--fixture') + 1])
                if change_fixture:
                    changed = json.loads(fixture.read_bytes())
                    changed['hold_ms'] = 600
                    capture.write(fixture, changed)
                capture.write(output / 'proof.json', dict(session=proof_session,
                              trace_sha256=capture.evaluator.digest(raw), status='passed', received_down_up=True,
                              forward_input_accepted=True, source_save_unchanged=True, frame_cap_request=60,
                              dxvk_config={}, ini={}, fixture_sha256=capture.digest(fixture)))
                return 0

            with patch('sys.argv', argv), patch.object(capture, 'run_recorder', side_effect=record) as recorder:
                manifest = root / 'corpus/manifest.json'
                with self.assertRaisesRegex(ValueError, 'recorded session'):
                    capture.main()
                proof_session = session
                with self.assertRaisesRegex(ValueError, 'no clear jump'):
                    capture.main()
                self.assertEqual(json.loads(manifest.read_bytes())['runs'], [])
                self.assertTrue((root / 'private/train-60hz-1x-1-attempt-2/trace.bin').is_file())
                raw = synthetic()
                change_fixture = True
                with self.assertRaisesRegex(ValueError, 'planned fixture'):
                    capture.main()
                self.assertEqual(json.loads(manifest.read_bytes())['runs'], [])
                change_fixture = False
                capture.main()
                receipt = root / 'corpus/train-capture.json'
                receipt_stat = receipt.stat()
                run, = json.loads(manifest.read_bytes())['runs']
                trace = root / 'corpus' / run['path']
                capture.main()
                self.assertEqual(receipt.stat().st_mtime_ns, receipt_stat.st_mtime_ns)
                self.assertEqual(receipt.stat().st_ino, receipt_stat.st_ino)
                trace.write_bytes(gzip.compress(b'other trace'))
                with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                    capture.main()
                trace.unlink()
                with self.assertRaisesRegex(ValueError, 'Cannot read retained trace'):
                    capture.main()
                self.assertEqual(recorder.call_count, 4)


if __name__ == '__main__':
    unittest.main()
