import contextlib
import gzip
import hashlib
import io
import json
from pathlib import Path
import shutil
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zipfile

import capture_skyrim_jump as v3
import capture_skyrim_jump_inputs as v4
from verify_skyrim_jump_inputs import verify


def campaign(module, root, frame_cap=None):
    plan_path = module.ROOT / ('tools/skyrim-recorder/fixtures/jump-inputs-v4.json' if module is v4
                               else 'tools/skyrim-recorder/fixtures/jump-v3.json')
    plan = json.loads(plan_path.read_text())
    if frame_cap:
        plan['frame_cap'] = frame_cap
        plan_path = root / 'plan.json'
        plan_path.write_text(json.dumps(plan))
    session = dict(exe_sha256=plan['runtime_sha256'], source_save_sha256=plan['source_save_sha256'],
                   input_sha256='test-input', proton_sha256='test-proton',
                   build=dict(source_sha256=module.digest(module.ROOT / 'tools/skyrim-recorder/probe.cpp'),
                              extension_sha256=module.digest(module.ROOT / 'tools/skyrim-recorder/jump_inputs.h'),
                              dll_sha256='test-dll'))
    if module is v4:
        session['profile_name'] = 'inputs-v4'
    session_dir = root / 'session'
    session_dir.mkdir()
    (session_dir / 'session.json').write_text(json.dumps(session))
    argv = ['capture', '--session', str(session_dir), '--corpus', str(root / 'corpus'),
            '--private-output', str(root / 'private'), '--ocr-prefix', str(root), '--plan', str(plan_path)]
    argv += ['--condition', 'baseline'] if module is v4 else ['--split', 'train']
    return argv, session


class CaptureAttemptTests(unittest.TestCase):
    def test_interrupted_adoption_recovers_the_imported_trace(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, _ = campaign(v4, root)
            imported = root / 'external-baseline'
            imported.mkdir()
            raw = b'imported trace before acceptance'
            (imported / 'trace.bin').write_bytes(raw)
            argv += ['--adopt-baseline', str(imported)]
            manifest_path = root / 'corpus/manifest.json'
            snapshots = []
            original_write = v4.write

            def interrupt_adoption(path, value):
                original_write(path, value)
                if path.name.endswith('-receipt.json'):
                    snapshots.append(manifest_path.read_bytes())
                    raise KeyboardInterrupt()

            with patch('sys.argv', argv), patch.object(v4, 'write', interrupt_adoption), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(KeyboardInterrupt):
                    v4.main()
                # Resume from the durable pre-acceptance snapshot, as after a hard kill.
                manifest_path.write_bytes(snapshots[0])
                with self.assertRaises(KeyboardInterrupt):
                    v4.main()
            ledger = json.loads(manifest_path.read_text())['attempts']
            self.assertEqual([row['attempt'] for row in ledger], [1, 2])
            self.assertTrue(all('private_output' not in row for row in ledger))
            recovered = ledger[0]
            self.assertEqual(recovered['failure']['phase'], 'interrupted')
            self.assertEqual(gzip.decompress((root / 'corpus' / recovered['path']).read_bytes()), raw)
            self.assertEqual(recovered['trace_sha256'], hashlib.sha256(raw).hexdigest())
            self.assertEqual((imported / 'trace.bin').read_bytes(), raw)

    def test_interrupted_recorders_preserve_logs_and_resume_at_the_next_attempt(self):
        for module in (v3, v4):
            with self.subTest(module=module.__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                argv, _ = campaign(module, root)
                manifest_path = root / 'corpus/manifest.json'
                first = {}

                def interrupted(command, **kwargs):
                    self.assertTrue(manifest_path.exists(), 'running attempt must be saved before launch')
                    first['manifest'] = manifest_path.read_bytes()
                    self.assertEqual(json.loads(first['manifest'])['attempts'][0]['status'], 'pending')
                    output = Path(command[command.index('--output') + 1])
                    output.mkdir()
                    (output / 'trace.bin').write_bytes(b'partial trace')
                    kwargs['stdout'].write('original interrupted log\n')
                    first['output'] = output
                    raise KeyboardInterrupt()

                with patch('sys.argv', argv), patch.object(module.subprocess, 'run', interrupted), \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(KeyboardInterrupt):
                        module.main()
                log = Path(str(first['output']) + '.log')
                original_log = log.read_bytes()
                # This is the actual durable snapshot from inside collection, as after a hard kill.
                manifest_path.write_bytes(first['manifest'])

                def retry(command, **kwargs):
                    output = Path(command[command.index('--output') + 1])
                    self.assertNotEqual(output, first['output'])
                    self.assertTrue(str(output).endswith('-attempt-2'))
                    output.mkdir()
                    kwargs['stdout'].write('new attempt log\n')
                    return SimpleNamespace(returncode=1)

                with patch('sys.argv', argv), patch.object(module.subprocess, 'run', retry), \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(RuntimeError):
                        module.main()
                ledger = json.loads(manifest_path.read_text())['attempts']
                self.assertEqual([row['attempt'] for row in ledger], [1, 2])
                self.assertEqual(ledger[0]['failure']['phase'], 'interrupted')
                self.assertNotIn('status', ledger[0])
                self.assertEqual(log.read_bytes(), original_log)
                self.assertEqual((first['output'] / 'trace.bin').read_bytes(), b'partial trace')

    def test_adoption_rejects_failed_proofs_and_false_cadence_claims(self):
        reference = v4.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        observed = json.loads((reference / 'manifest.json').read_text())['runs'][0]
        raw = gzip.decompress((reference / observed['path']).read_bytes())
        for mode in ('failed', 'cadence'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                argv, session = campaign(v4, root, frame_cap=120 if mode == 'cadence' else None)
                output = root / 'adopted'
                output.mkdir()
                (output / 'trace.bin').write_bytes(raw)
                proof = dict(observed['proof'], session=session,
                             trace_sha256=hashlib.sha256(raw).hexdigest(),
                             runner_sha256=v4.digest(v4.ROOT / 'scripts/record_skyrim_motion.py'))
                proof['status'] = 'failed' if mode == 'failed' else 'passed'
                proof['frame_cap_request'] = 120 if mode == 'cadence' else 60
                (output / 'proof.json').write_text(json.dumps(proof))
                argv += ['--adopt-baseline', str(output)]
                with patch('sys.argv', argv), patch.object(v4.subprocess, 'run') as recorder, \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(ValueError, 'proof|cadence'):
                        v4.main()
                    recorder.assert_not_called()
                self.assertEqual(json.loads((root / 'corpus/manifest.json').read_text())['runs'], [])

    def test_offline_verification_rejects_a_failed_accepted_proof(self):
        reference = v4.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = v4.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory) / 'corpus'
            shutil.copytree(reference, corpus)
            manifest = json.loads((corpus / 'manifest.json').read_text())
            # A fresh test campaign uses current sources, without historical publication transforms.
            with zipfile.ZipFile(corpus / 'capture-source.zip', 'w') as archive:
                for name in v4.SOURCES:
                    archive.writestr(name, (v4.ROOT / name).read_bytes())
            for run in manifest['runs']:
                run['receipt']['sources'] = {name: v4.digest(v4.ROOT / name) for name in v4.SOURCES}
            manifest['runs'][0]['proof']['status'] = 'failed'
            (corpus / 'manifest.json').write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, 'proof'):
                verify(corpus, plan)


if __name__ == '__main__':
    unittest.main()
