from concurrent.futures import ThreadPoolExecutor
import contextlib
import fcntl
from functools import partial
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
from threading import Barrier
import unittest
from unittest.mock import patch
import zipfile

import capture_skyrim_jump as v3
import capture_skyrim_jump_inputs as v4
import skyrim_capture_attempts as attempts
from verify_skyrim_jump_inputs import verify
import record_skyrim_motion as recorder
from skyrim_capture_attempts import claim_private_output, record_attempt, recover_attempts, save_manifest


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


def install_fake_recorder(module, root, fake_recorder):
    (root / 'tools').symlink_to(module.ROOT / 'tools')
    scripts = root / 'scripts'
    scripts.mkdir()
    for name in set(v4.SOURCES) | {'scripts/capture_skyrim_jump.py', 'scripts/evaluate_skyrim_jump.py'}:
        if name.startswith('scripts/'):
            shutil.copyfile(module.ROOT / name, root / name)
    recorder = scripts / 'record_skyrim_motion.py'
    recorder.rename(scripts / 'real_recorder.py')
    recorder.write_text("if __name__ == '__main__':\n" + textwrap.indent(fake_recorder, '    ') +
                        "else:\n    from real_recorder import *\n")
    return scripts / Path(module.__file__).name


class CaptureAttemptTests(unittest.TestCase):
    def test_campaign_failure_and_recovery_wait_for_all_detached_services(self):
        for module in (v3, v4):
            for role in ('game', 'display', 'prefix'):
                with self.subTest(module=module.__name__, role=role), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    argv, _ = campaign(module, root)
                    session = root / 'session'
                    corpus = root / 'corpus'
                    display, prefix, game = recorder.session_units(session)
                    active = {'game': game, 'display': display, 'prefix': prefix}[role]

                    def killed(command, **kwargs):
                        output = Path(command[command.index('--output') + 1])
                        output.mkdir()
                        save_manifest(output / 'trace-writer.json', dict(session=str(session), game_unit=game))
                        (output / 'trace.bin').write_bytes(b'partial trace')
                        raise SystemExit(143)

                    with patch('sys.argv', argv), patch.object(module, 'run_recorder', side_effect=killed) as run, \
                            patch.object(recorder, 'unit_stopped', side_effect=lambda unit: unit != active):
                        for _ in range(2):
                            with self.assertRaisesRegex(RuntimeError, active):
                                module.main()
                            row, = json.loads((corpus / 'manifest.json').read_bytes())['attempts']
                            self.assertEqual(row['status'], 'pending')
                            self.assertNotIn('path', row)
                        run.assert_called_once()
                    retain = partial(v4.retain_failure, corpus) if module is v4 else v3.retain_failure
                    path = corpus / 'manifest.json'
                    with patch.object(recorder, 'unit_stopped', return_value=True) as stopped:
                        recover_attempts(path, json.loads(path.read_bytes()), root / 'private', retain)
                    self.assertEqual({call.args[0] for call in stopped.call_args_list}, {display, prefix, game})
                    row, = json.loads(path.read_bytes())['attempts']
                    self.assertNotIn('status', row)
                    self.assertIn('failure', row)

    def test_only_one_corpus_claims_an_unused_private_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            private = root / 'private'
            private.mkdir()
            corpora = [root / 'a', root / 'b']
            for corpus in corpora:
                corpus.mkdir()
            start = Barrier(2)

            def claim(corpus):
                start.wait()
                try:
                    claim_private_output(private, corpus)
                    return corpus
                except ValueError:
                    return None

            with ThreadPoolExecutor(max_workers=2) as pool:
                winners = [result for result in pool.map(claim, corpora) if result is not None]
            self.assertEqual(len(winners), 1)
            claim_private_output(private, winners[0])

    def test_foreign_private_attempt_is_rejected_before_capture_or_evidence_retention(self):
        for module in (v3, v4):
            with self.subTest(module=module.__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                argv, _ = campaign(module, root)
                other = argv.copy()
                other[other.index('--corpus') + 1] = str(root / 'other')
                fixtures = []

                def recorder(command, **kwargs):
                    fixtures.append(Path(command[command.index('--fixture') + 1]))
                    output = Path(command[command.index('--output') + 1])
                    output.mkdir()
                    (output / 'trace.bin').write_bytes(b'private trace from original campaign')
                    with patch('sys.argv', other), self.assertRaisesRegex(ValueError, 'another corpus'):
                        module.main()
                    return 1

                with patch('sys.argv', argv), patch.object(module, 'run_recorder', side_effect=recorder), \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(RuntimeError, 'Capture failed'):
                        module.main()
                    fixtures[0].write_bytes(b'preserved fixture')
                    with self.assertRaisesRegex(RuntimeError, 'Capture failed'):
                        module.main()
                self.assertEqual(len(set(fixtures)), 2)
                self.assertEqual(fixtures[0].read_bytes(), b'preserved fixture')
                self.assertFalse((root / 'other/manifest.json').exists())
                self.assertFalse(list((root / 'other/traces').glob('*')))
                attempts = json.loads((root / 'corpus/manifest.json').read_bytes())['attempts']
                self.assertEqual([row['attempt'] for row in attempts], [1, 2])
                if module is v4:
                    self.assertEqual(gzip.decompress((root / 'corpus' / attempts[0]['path']).read_bytes()),
                                     b'private trace from original campaign')

                # An unmarked, older private directory must also keep its old files.
                private = root / 'private'
                (private / '.capture-corpus').unlink()
                with patch('sys.argv', other), patch.object(module, 'run_recorder') as record, \
                        self.assertRaisesRegex(ValueError, 'use a fresh private directory'):
                    module.main()
                record.assert_not_called()
                self.assertFalse((root / 'other/manifest.json').exists())
                # Existing attempts in the caller's manifest also cannot establish private ownership.
                before = (root / 'corpus/manifest.json').read_bytes()
                with patch('sys.argv', argv), patch.object(module, 'run_recorder') as record, \
                        self.assertRaisesRegex(ValueError, 'use a fresh private directory'):
                    module.main()
                self.assertFalse((private / '.capture-corpus').is_symlink())
                self.assertEqual((root / 'corpus/manifest.json').read_bytes(), before)
                record.assert_not_called()

    def test_unidentified_pending_attempt_never_adopts_a_matching_private_filename(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stale = root / 'baseline-attempt-1'
            stale.mkdir()
            (stale / 'trace.bin').write_bytes(b'unrelated trace')
            manifest = dict(runs=[], attempts=[dict(id='baseline', attempt=1, status='pending')])
            path = root / 'manifest.json'
            save_manifest(path, manifest)
            before = path.read_bytes()
            with patch.object(v4, 'retain_failure') as retain, self.assertRaisesRegex(ValueError, 'private output path'):
                recover_attempts(path, manifest, root, retain)
            retain.assert_not_called()
            self.assertEqual(path.read_bytes(), before)

    def test_preexisting_next_attempt_path_never_becomes_failed_evidence(self):
        for module in (v3, v4):
            with self.subTest(module=module.__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                argv, _ = campaign(module, root)
                private, corpus = root / 'private', root / 'corpus'
                private.mkdir()
                corpus.mkdir()
                claim_private_output(private, corpus)
                label = 'baseline' if module is v4 else 'train-60hz-1x-1'
                stale = private / f'{label}-attempt-1'
                stale.mkdir()
                (stale / 'trace.bin').write_bytes(b'unrelated trace')
                with patch('sys.argv', argv), patch.object(module, 'run_recorder') as record, \
                        self.assertRaisesRegex(FileExistsError, 'attempt path already exists'):
                    module.main()
                record.assert_not_called()
                self.assertEqual((stale / 'trace.bin').read_bytes(), b'unrelated trace')
                self.assertFalse((corpus / 'manifest.json').exists())
                self.assertFalse(list((corpus / 'traces').glob('*')))

    def test_failure_waits_for_independent_native_writer_and_retries_unknown_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'traces').mkdir()
            output = root / 'private'
            output.mkdir()
            session = root / 'session'
            session.mkdir()
            game_unit = 'mudcrab-record-game-' + hashlib.sha256(str(session).encode()).hexdigest()[:24]
            (output / 'trace-writer.json').write_text(json.dumps(dict(game_unit=game_unit, session=str(session))))
            writer_code = """import sys
with open(sys.argv[1], 'wb') as trace:
    trace.write(b'first')
    trace.flush()
    print('writing', flush=True)
    sys.stdin.readline()
    trace.write(b' last')
"""
            path = root / 'manifest.json'
            retain = partial(v4.retain_failure, root)
            manifest = dict(runs=[], attempts=[])
            with subprocess.Popen([sys.executable, '-c', writer_code, str(output / 'trace.bin')],
                                  start_new_session=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE) as writer:
                try:
                    self.assertEqual(writer.stdout.readline().strip(), b'writing')
                    def stopped(unit):
                        return unit != game_unit or writer.poll() is not None
                    with patch.object(recorder, 'unit_stopped', stopped):
                        with self.assertRaisesRegex(RuntimeError, 'service .* still active'):
                            with record_attempt(path, manifest, 'baseline', 1, output, retain, private_session=session):
                                raise RuntimeError('recorder died')
                    saved = path.read_bytes()
                    self.assertEqual(json.loads(saved)['attempts'][0]['status'], 'pending')
                    with patch.object(recorder, 'unit_stopped', side_effect=OSError('manager unreachable')):
                        with self.assertRaisesRegex(OSError, 'manager unreachable'):
                            recover_attempts(path, json.loads(saved), root, retain)
                    with (session / '.recording.lock').open('a') as busy:
                        fcntl.flock(busy, fcntl.LOCK_EX)
                        with self.assertRaises(BlockingIOError):
                            recover_attempts(path, json.loads(saved), root, retain)
                    self.assertEqual(list((root / 'traces').iterdir()), [])
                    self.assertEqual(path.read_bytes(), saved)
                finally:
                    writer.stdin.close()
                    writer.wait(timeout=3)
            with patch.object(recorder, 'unit_stopped', stopped):
                recover_attempts(path, json.loads(saved), root, retain)
            row, = json.loads(path.read_text())['attempts']
            self.assertNotIn('status', row)
            self.assertNotIn('private_session', row)
            self.assertEqual(gzip.decompress((root / row['path']).read_bytes()), b'first last')

    def test_completed_proof_allows_retention_after_session_removal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'traces').mkdir()
            raw = b'finished trace'
            (root / 'trace.bin').write_bytes(raw)
            proof = dict(services_stopped=True, status='passed', trace_sha256=hashlib.sha256(raw).hexdigest(),
                         runner_sha256=v4.digest(v4.ROOT / 'scripts/record_skyrim_motion.py'))
            v4.write(root / 'proof.json', proof)
            manifest = dict(runs=[], attempts=[])
            with self.assertRaises(KeyboardInterrupt):
                with record_attempt(root / 'manifest', manifest, 'baseline', 1, root, partial(v4.retain_failure, root),
                                    private_session=root / 'removed'):
                    raise KeyboardInterrupt()
            row, = json.loads((root / 'manifest').read_text())['attempts']
            self.assertNotIn('status', row)
            self.assertEqual(gzip.decompress((root / row['path']).read_bytes()), raw)

    def test_cancellation_kills_stopped_recorder_process_group(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_code = """import os, signal, sys
from pathlib import Path
signal.signal(signal.SIGTERM, lambda *_: None)
pid = os.fork()
if pid == 0:
    signal.pause()
else:
    Path(sys.argv[1]).write_text(f'{pid} {os.getpgrp()}')
    os.kill(os.getppid(), signal.SIGTERM)
    os.kill(os.getpid(), signal.SIGSTOP)
"""
            wrapper_code = """import sys
from pathlib import Path
import skyrim_capture_attempts as a
root = Path(sys.argv[1])
a.RECORDER_SHUTDOWN_TIMEOUT = 0.2
with a.corpus_lock(root) as lock, (root / 'recorder.log').open('w') as log:
    a.run_recorder([sys.executable, '-c', sys.argv[2], str(root / 'descendant')], stdout=log, attempt={}, lock_fd=lock)
"""
            try:
                proc = subprocess.run([sys.executable, '-B', '-c', wrapper_code, str(root), child_code],
                                      cwd=Path(attempts.__file__).parent, capture_output=True, timeout=5)
                self.assertEqual(proc.returncode, 128 + signal.SIGTERM, proc.stderr)
                pid, _ = (root / 'descendant').read_text().split()
                try:
                    state = (Path('/proc') / pid / 'stat').read_text().split()[2]
                except FileNotFoundError:
                    state = None
                self.assertIn(state, (None, 'Z'))
            finally:
                if (root / 'descendant').exists():
                    _, group = (root / 'descendant').read_text().split()
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(int(group), signal.SIGKILL)
            with attempts.corpus_lock(root):
                pass

    def test_recovery_waits_for_orphaned_recorder_to_finish_its_trace(self):
        fake_recorder = """import os, signal, sys
from pathlib import Path
output = Path(sys.argv[sys.argv.index('--output') + 1])
output.mkdir()
if output.name != 'baseline-attempt-1':
    sys.exit(42)
signal.alarm(15)
with (output / 'trace.bin').open('wb') as trace:
    trace.write(b'first chunk')
    trace.flush()
    os.kill(os.getppid(), signal.SIGKILL)
    sys.stdin.buffer.read()
    trace.write(b' and last chunk')
"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, _ = campaign(v4, root)
            wrapper = install_fake_recorder(v4, root, fake_recorder)
            command = [sys.executable, '-B', str(wrapper), *argv[1:]]
            corpus = root / 'corpus'
            manifest = corpus / 'manifest.json'
            with subprocess.Popen(command, cwd=root, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL) as process:
                self.assertEqual(process.wait(timeout=8), -signal.SIGKILL)
                checkpoint = manifest.read_bytes()
                attempt, = json.loads(checkpoint)['attempts']
                self.assertEqual(attempt['status'], 'pending')
                self.assertEqual((Path(attempt['private_output']) / 'trace.bin').read_bytes(), b'first chunk')
                blocked = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=8)
                self.assertNotEqual(blocked.returncode, 0)
                self.assertIn('BlockingIOError', blocked.stderr)
                self.assertEqual(manifest.read_bytes(), checkpoint)
                self.assertEqual(list((corpus / 'traces').iterdir()), [])
                process.stdin.close()
                # Synchronize with the orphan's exit before issuing another campaign command.
                with (corpus / '.capture.lock').open('a') as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX)
            retried = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=8)
            self.assertNotEqual(retried.returncode, 0)
            recovered, retry = json.loads(manifest.read_bytes())['attempts']
            self.assertNotIn('status', recovered)
            self.assertEqual(recovered['failure']['phase'], 'interrupted')
            self.assertEqual(gzip.decompress((corpus / recovered['path']).read_bytes()), b'first chunk and last chunk')
            self.assertEqual((retry['attempt'], retry['exit_code']), (2, 42))

    def test_wrapper_signal_waits_for_recorder_before_releasing_corpus(self):
        fake_recorder = """import fcntl, os, signal, sys
from pathlib import Path
root = Path.cwd()
signum = int((root / 'signal').read_text())
def cancel(sig, frame):
    with (root / 'corpus/.capture.lock').open('a') as contender:
        try:
            fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            (root / 'stopped-under-lock').write_text(str(sig))
    raise SystemExit(128 + sig)
signal.signal(signum, cancel)
signal.alarm(3)
with (root / 'session/.recording.lock').open('a') as session:
    fcntl.flock(session, fcntl.LOCK_EX)
    os.kill(os.getppid(), signum)
    signal.pause()
"""
        for module in (v3, v4):
            for sig in (signal.SIGTERM, signal.SIGINT):
                with self.subTest(module=module.__name__, signal=sig), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    argv, _ = campaign(module, root)
                    (root / 'signal').write_text(str(int(sig)))
                    wrapper = install_fake_recorder(module, root, fake_recorder)
                    result = subprocess.run([sys.executable, '-B', str(wrapper), *argv[1:]],
                                            cwd=root, capture_output=True, text=True, timeout=8,
                                            preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))
                    self.assertEqual(result.returncode, 128 + sig, result.stderr)
                    self.assertEqual((root / 'stopped-under-lock').read_text(), str(int(sig)))
                    attempt, = json.loads((root / 'corpus/manifest.json').read_text())['attempts']
                    self.assertEqual(attempt['recorder_exit_code'], 128 + sig)
                    self.assertEqual(attempt['failure']['phase'], 'interrupted')
                    with (root / 'session/.recording.lock').open('a') as session:
                        fcntl.flock(session, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_failed_classification_preserves_the_last_pending_checkpoint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'traces').mkdir()
            output = root / 'capture'
            output.mkdir()
            (output / 'trace.bin').write_bytes(b'captured trace')
            manifest = dict(runs=[], attempts=[])
            path = root / 'manifest.json'
            retain = partial(v4.retain_failure, root)
            with patch.object(Path, 'read_text', side_effect=OSError('temporary manifest read error')):
                with self.assertRaises(OSError):
                    with record_attempt(path, manifest, 'baseline', 1, output, retain, private_session=root) as attempt:
                        attempt['exit_code'] = 0
                        manifest['runs'].append(dict(id='baseline'))
                        raise KeyboardInterrupt()
            pending = json.loads(path.read_text())
            self.assertEqual(pending['runs'], [])
            self.assertEqual(pending['attempts'][0]['status'], 'pending')
            self.assertEqual(pending['attempts'][0]['private_output'], str(output))
            recover_attempts(path, pending, root, retain)
            failed = json.loads(path.read_text())['attempts'][0]
            self.assertNotIn('status', failed)
            self.assertNotEqual(failed['exit_code'], 0)
            self.assertEqual(gzip.decompress((root / failed['path']).read_bytes()), b'captured trace')

    def test_v3_receipt_rejects_a_changed_prepared_recorder_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, session = campaign(v3, root)
            session['build']['source_sha256'] = '0' * 64
            (root / 'session/session.json').write_text(json.dumps(session))
            with patch('sys.argv', argv), patch.object(v3, 'run_recorder',
                                                     side_effect=AssertionError('recorder started')) as recorder, \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(SystemExit, '2'):
                    v3.main()
                recorder.assert_not_called()
            self.assertFalse((root / 'corpus/train-capture.json').exists())

    def test_interrupted_acceptance_rolls_back_unsaved_runs_and_allows_retry(self):
        for error in (KeyboardInterrupt, OSError):
            with self.subTest(error=error.__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'traces').mkdir()
                output = root / 'capture'
                output.mkdir()
                (output / 'trace.bin').write_bytes(b'captured trace')
                prior = dict(id='prior')
                manifest = dict(runs=[prior], attempts=[dict(id='prior', attempt=1, exit_code=0)])
                path = root / 'manifest.json'
                retain = partial(v4.retain_failure, root)
                with self.assertRaises(error):
                    with record_attempt(path, manifest, 'baseline', 1, output, retain, private_session=root) as attempt:
                        attempt['exit_code'] = 0
                        manifest['runs'].append(dict(id='baseline'))
                        with patch('skyrim_capture_attempts.os.replace', side_effect=error):
                            save_manifest(path, manifest)
                saved = json.loads(path.read_text())
                self.assertEqual(saved['runs'], [prior])
                failed = saved['attempts'][-1]
                self.assertNotEqual(failed['exit_code'], 0)
                self.assertEqual(gzip.decompress((root / failed['path']).read_bytes()), b'captured trace')
                with record_attempt(path, saved, 'baseline', 2, output, retain, private_session=root) as attempt:
                    attempt['exit_code'] = 0
                    saved['runs'].append(dict(id='baseline'))
                    save_manifest(path, saved)
                accepted = json.loads(path.read_text())
                self.assertEqual(accepted['runs'], [prior, dict(id='baseline')])
                self.assertEqual([r['exit_code'] for r in accepted['attempts']], [0, -1, 0])

    def test_corpus_lock_rejects_a_second_capture_before_manifest_mutation(self):
        for module in (v3, v4):
            with self.subTest(module=module.__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                argv, _ = campaign(module, root)
                corpus = root / 'corpus'
                corpus.mkdir()
                with (corpus / '.capture.lock').open('a') as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    with patch('sys.argv', argv), patch.object(module, 'run_recorder',
                                                               return_value=1) as recorder:
                        with self.assertRaises(BlockingIOError):
                            module.main()
                        recorder.assert_not_called()
                self.assertFalse((corpus / 'manifest.json').exists())

    def test_failed_retention_remains_pending_and_recovers_after_storage_returns(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'traces').mkdir()
            output = root / 'capture'
            output.mkdir()
            raw = b'failed capture bytes'
            (output / 'trace.bin').write_bytes(raw)
            manifest = dict(runs=[], attempts=[])
            path = root / 'manifest.json'
            retain = partial(v4.retain_failure, root)
            with patch.object(Path, 'write_bytes', side_effect=OSError('storage full')):
                with self.assertRaises(OSError):
                    with record_attempt(path, manifest, 'baseline', 1, output, retain, private_session=root) as attempt:
                        attempt['exit_code'] = 1
                        retain(output, attempt, dict(phase='recorder', error='capture failed'))
            saved = json.loads(path.read_text())
            self.assertEqual(saved['attempts'][0]['status'], 'pending')
            self.assertEqual(saved['attempts'][0]['private_output'], str(output))
            recover_attempts(path, saved, root, retain)
            recovered = saved['attempts'][0]
            self.assertNotIn('status', recovered)
            self.assertEqual(recovered['failure']['phase'], 'recorder')
            self.assertEqual(gzip.decompress((root / recovered['path']).read_bytes()), raw)
            before = path.read_bytes()
            recover_attempts(path, saved, root, retain)
            self.assertEqual(path.read_bytes(), before)

    def test_progress_failure_cannot_reclassify_a_durably_accepted_attempt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = dict(runs=[], attempts=[])
            path = root / 'manifest.json'
            with self.assertRaises(BrokenPipeError):
                with record_attempt(path, manifest, 'baseline', 1, root) as attempt:
                    attempt['exit_code'] = 0
                    manifest['runs'].append(dict(id='baseline'))
                    save_manifest(path, manifest)
                    raise BrokenPipeError('closed progress output')
            accepted = json.loads(path.read_text())['attempts'][0]
            self.assertEqual(accepted['exit_code'], 0)
            self.assertNotIn('failure', accepted)
            self.assertNotIn('status', accepted)

    def test_selected_conditions_must_follow_plan_order_before_recording(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, session = campaign(v4, root)
            plan_path = Path(argv[argv.index('--plan') + 1])
            plan = json.loads(plan_path.read_text())
            corpus = root / 'corpus'
            corpus.mkdir()
            reference = v4.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4/manifest.json'
            (corpus / 'traces').symlink_to(reference.parent / 'traces')
            baseline = json.loads(reference.read_text())['runs'][0]
            baseline['receipt']['session_sha256'] = v4.digest(root / 'session/session.json')
            baseline['proof'].update(dll_sha256=session['build']['dll_sha256'], input_sha256=session['input_sha256'],
                                     proton_sha256=session['proton_sha256'])
            manifest = dict(schema=1, profile='jump-inputs-v4', plan_sha256=v4.digest(plan_path),
                            runtime_sha256=plan['runtime_sha256'], runs=[baseline], attempts=[])
            (corpus / 'manifest.json').write_text(json.dumps(manifest))
            argv[argv.index('--condition') + 1] = 'reference-125'
            with patch('sys.argv', argv), patch.object(v4, 'run_recorder',
                                                     side_effect=AssertionError('recording began out of order')) as recorder, \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(SystemExit, '2'):
                    v4.main()
                recorder.assert_not_called()
            self.assertEqual(json.loads((corpus / 'manifest.json').read_text()), manifest)

    def test_missing_import_can_retry_and_pass_offline_verification(self):
        reference = v4.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        observed = json.loads((reference / 'manifest.json').read_text())['runs'][0]
        raw = gzip.decompress((reference / observed['path']).read_bytes())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv, session = campaign(v4, root)
            plan = json.loads(Path(argv[argv.index('--plan') + 1]).read_text())
            plan['conditions'] = plan['conditions'][:1]
            plan_path = root / 'plan.json'
            plan_path.write_text(json.dumps(plan))
            argv[argv.index('--plan') + 1] = str(plan_path)
            imported = root / 'imported'
            imported.mkdir()
            argv += ['--adopt-baseline', str(imported)]
            with patch('sys.argv', argv), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(FileNotFoundError):
                    v4.main()
                (imported / 'trace.bin').write_bytes(raw)
                proof = dict(observed['proof'], session=session, services_stopped=True,
                             trace_sha256=hashlib.sha256(raw).hexdigest(),
                             runner_sha256=v4.digest(v4.ROOT / 'scripts/record_skyrim_motion.py'))
                (imported / 'proof.json').write_text(json.dumps(proof))
                v4.main()
            corpus = root / 'corpus'
            self.assertEqual(verify(corpus, plan_path)['status'], 'passed')
            attempts = json.loads((corpus / 'manifest.json').read_text())['attempts']
            self.assertEqual([row['attempt'] for row in attempts], [1, 2])
            self.assertNotEqual(attempts[0]['exit_code'], 0)
            self.assertEqual(attempts[0]['recorder_exit_code'], 0)
            self.assertEqual(attempts[0]['trace_status'], 'not-created')
            self.assertEqual(attempts[1]['exit_code'], 0)

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

            def interrupt_adoption(path, value, **kwargs):
                original_write(path, value, **kwargs)
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

                with patch('sys.argv', argv), patch.object(module, 'run_recorder', interrupted), \
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
                    return 1

                # Resume in a new private directory, while recovering the exact previous recording.
                previous = root / 'private'
                (previous / '.capture-corpus').unlink()
                argv[argv.index('--private-output') + 1] = str(root / 'new-private')
                with patch('sys.argv', argv), patch.object(module, 'run_recorder', retry), \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(RuntimeError):
                        module.main()
                ledger = json.loads(manifest_path.read_text())['attempts']
                self.assertEqual([row['attempt'] for row in ledger], [1, 2])
                self.assertEqual(ledger[0]['failure']['phase'], 'interrupted')
                self.assertNotIn('status', ledger[0])
                self.assertEqual(log.read_bytes(), original_log)
                self.assertEqual((first['output'] / 'trace.bin').read_bytes(), b'partial trace')
                if module is v4:
                    self.assertEqual(gzip.decompress((root / 'corpus' / ledger[0]['path']).read_bytes()), b'partial trace')

    def test_adoption_rejects_failed_proofs_false_cadence_and_wrong_fixtures(self):
        reference = v4.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        observed = json.loads((reference / 'manifest.json').read_text())['runs'][0]
        raw = gzip.decompress((reference / observed['path']).read_bytes())
        for mode in ('failed', 'cadence', 'fixture', 'changed-save', 'cleanup-failed'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                argv, session = campaign(v4, root, frame_cap=120 if mode == 'cadence' else None)
                output = root / 'adopted'
                output.mkdir()
                (output / 'trace.bin').write_bytes(raw)
                proof = dict(observed['proof'], session=session, services_stopped=True,
                             trace_sha256=hashlib.sha256(raw).hexdigest(),
                             runner_sha256=v4.digest(v4.ROOT / 'scripts/record_skyrim_motion.py'))
                if mode == 'cleanup-failed':
                    proof.pop('services_stopped')
                proof['status'] = 'failed' if mode == 'failed' else 'passed'
                proof['frame_cap_request'] = 120 if mode == 'cadence' else 60
                if mode == 'changed-save':
                    proof['source_save_unchanged'] = False
                if mode == 'fixture':
                    proof['fixture'] = dict(observed['fixture'], yaw_degrees=180)
                    proof['fixture_sha256'] = hashlib.sha256(
                        (json.dumps(proof['fixture'], indent=2) + '\n').encode()).hexdigest()
                (output / 'proof.json').write_text(json.dumps(proof))
                argv += ['--adopt-baseline', str(output)]
                with patch('sys.argv', argv), patch.object(v4, 'run_recorder') as recorder, \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(ValueError, 'proof|cadence|fixture'):
                        v4.main()
                    recorder.assert_not_called()
                self.assertEqual(json.loads((root / 'corpus/manifest.json').read_text())['runs'], [])

    def test_offline_verification_rejects_failed_proofs_and_wrong_fixtures(self):
        reference = v4.ROOT / 'tools/skyrim-recorder/reference/jump-inputs-v4'
        plan = v4.ROOT / 'tools/skyrim-recorder/fixtures/jump-inputs-v4.json'
        for mode in ('failed', 'fixture-hash', 'fixture-label', 'missing-source'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                corpus = Path(directory) / 'corpus'
                shutil.copytree(reference, corpus)
                manifest = json.loads((corpus / 'manifest.json').read_text())
                # A fresh test campaign uses current sources, without historical publication transforms.
                sources = v4.SOURCES[:-1] if mode == 'missing-source' else v4.SOURCES
                with zipfile.ZipFile(corpus / 'capture-source.zip', 'w') as archive:
                    for name in sources:
                        archive.writestr(name, (v4.ROOT / name).read_bytes())
                for run in manifest['runs']:
                    run['receipt']['sources'] = {name: v4.digest(v4.ROOT / name) for name in sources}
                baseline = manifest['runs'][0]
                if mode == 'failed':
                    baseline['proof']['status'] = 'failed'
                elif mode == 'fixture-hash':
                    other = dict(baseline['fixture'], yaw_degrees=180)
                    baseline['proof']['fixture_sha256'] = hashlib.sha256(
                        (json.dumps(other, indent=2) + '\n').encode()).hexdigest()
                elif mode == 'fixture-label':
                    baseline['fixture']['yaw_degrees'] = 180
                (corpus / 'manifest.json').write_text(json.dumps(manifest))
                with self.assertRaisesRegex(ValueError, 'proof|fixture|source'):
                    verify(corpus, plan)


if __name__ == '__main__':
    unittest.main()
