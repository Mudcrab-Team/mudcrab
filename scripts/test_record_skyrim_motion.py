import unittest
import json
import fcntl
import multiprocessing
import signal
import os
import runpy
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import tempfile
import threading
from unittest.mock import patch

import record_skyrim_motion as recorder

from record_skyrim_motion import parse_ocr, qualify_cadence, verify_trace


def recording():
    rows = [dict(kind=1, text='mudcrab-motion-256-v1', sample=1000, qpc=0, code=1,
                 state=0, thread=10, values=[0.0] * 40)]
    for frame in range(40):
        if frame in (12, 22):
            values = [0.0] * 40
            values[:4] = [1.0, 0.0, 0.0, 1.0] if frame == 12 else [0.0, 0.5, 0.0, 0.0]
            rows.append(dict(kind=2, text='Forward', sample=frame, qpc=frame * 50,
                             code=17, state=0, thread=10, values=values))
        values = [0.0] * 40
        values[1] = min(max(frame - 12, 0), 10) * 4.0
        values[10] = values[1]
        values[11] = 120.0
        values[13] = values[1]
        values[14] = 120.0
        rows.append(dict(kind=3, text='', sample=frame, qpc=frame * 50 + 1,
                         code=3, state=0, thread=10, values=values))
    rows.append(dict(kind=5, text='complete after native W release', sample=39, qpc=2000,
                     code=0, state=0, thread=10, values=[0.0] * 40))
    inputs, samples = 0, 0
    for i, row in enumerate(rows, 1):
        row['sequence'] = i
        inputs += row['kind'] == 2
        samples += row['kind'] == 3
        row['input_sequence'] = inputs
        row['sample'] = samples if row['kind'] != 1 else 1000
    return rows


class ScreenshotOwnershipTests(unittest.TestCase):
    def test_two_sessions_receive_their_own_screenshot(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'gamescope.png'
            first_requested = threading.Event()
            second_requested = threading.Event()
            images = {display: b'\x89PNG\r\n\x1a\n' + display.encode() + b'IENDxxxx'
                      for display in (':1', ':2')}

            def request(*args, env, **kwargs):
                display = env['DISPLAY']
                source.write_bytes(images[display])
                if display == ':1':
                    first_requested.set()
                    second_requested.wait(0.2)
                else:
                    second_requested.set()

            with patch.object(recorder, 'SCREENSHOT_PATH', source), \
                    patch.object(recorder, 'SCREENSHOT_LOCK_PATH', root / 'screenshot.lock'), \
                    patch.object(recorder.subprocess, 'run', side_effect=request):
                with ThreadPoolExecutor(max_workers=2) as workers:
                    first = workers.submit(recorder.capture, ':1', root / 'first.png')
                    self.assertTrue(first_requested.wait(2))
                    second = workers.submit(recorder.capture, ':2', root / 'second.png')
                    first.result(timeout=3)
                    second.result(timeout=3)
            self.assertEqual((root / 'first.png').read_bytes(), images[':1'])
            self.assertEqual((root / 'second.png').read_bytes(), images[':2'])


class RecordingLifecycleTests(unittest.TestCase):
    def test_sigterm_stops_owned_units_before_unlocking(self):
        for when in ('launch', 'capture', 'cleanup'):
            with self.subTest(when=when), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                output = root / 'capture'
                lock = root / '.recording.lock'
                config = dict(exe_sha256='test', build=dict(dll_sha256='test'), input_sha256='test',
                              proton_sha256='test', source_save_sha256='test', proton='/proton',
                              source_save='/save', source_game='/games/steam/common/skyrim')
                (root / 'session.json').write_text(json.dumps(config))

                def terminate():
                    os.kill(os.getpid(), signal.SIGTERM)

                def run(argv, **kwargs):
                    if argv[0] == 'systemd-run':
                        if argv[-1].endswith('display_ready.py'):
                            display = dict(DISPLAY=':42', INVOCATION_ID='inv', LIBEI_SOCKET='socket',
                                           GAMESCOPE_WAYLAND_DISPLAY='wayland')
                            (output / 'display.json').write_text(json.dumps(display))
                        elif when == 'launch':
                            terminate()
                    elif argv[:3] == ['systemctl', '--user', 'stop']:
                        with lock.open('a') as contender, self.assertRaises(BlockingIOError):
                            fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        terminate()
                        with (root / 'stops').open('a') as log:
                            print(argv[-1], file=log)
                    return subprocess.CompletedProcess(argv, 0, stdout='', stderr='')

                def game_window(display):
                    if when == 'capture':
                        terminate()
                    raise RuntimeError('end simulated recording')

                def record():
                    signal.signal(signal.SIGTERM, signal.SIG_DFL)
                    args = ['record', 'run', '--session', directory, '--output', str(output), '--ocr-prefix', directory]
                    with patch('sys.argv', args), patch.dict(os.environ, XDG_RUNTIME_DIR=directory), \
                            patch.object(recorder, 'sha256', return_value='test'), \
                            patch.object(recorder.subprocess, 'run', side_effect=run), \
                            patch.object(recorder.subprocess, 'check_output', return_value='inv'), \
                            patch.object(recorder, 'initialize_prefix'), patch.object(recorder, 'GameWindow', game_window):
                        recorder.main()

                child = multiprocessing.get_context('fork').Process(target=record)
                child.start()
                try:
                    child.join(5)
                    self.assertEqual(child.exitcode, 128 + signal.SIGTERM)
                finally:
                    if child.is_alive():
                        child.kill()
                    child.join()
                stops = (root / 'stops').read_text().splitlines()
                self.assertEqual([unit.split('-')[2] for unit in stops], ['game', 'display'])
                with lock.open('a') as contender:
                    fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_display_handshake_and_cleanup_with_unwritable_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / 'capture'
            ready = output / 'display.json'
            config = dict(exe_sha256='test', build=dict(dll_sha256='test'), input_sha256='test',
                          proton_sha256='test', source_save_sha256='test', proton='/proton',
                          source_save='/save', source_game='/games/steam/common/skyrim')
            (root / 'session.json').write_text(json.dumps(config))
            started, stopped, visible_during_write = [], [], []
            write_text = Path.write_text

            def write(path, data, *args, **kwargs):
                if data.startswith('{"DISPLAY":'):
                    path.touch()
                    visible_during_write.append(ready.exists())
                if path.name in ('game-journal.log', 'failure.json'):
                    raise OSError('out of space')
                return write_text(path, data, *args, **kwargs)

            def run(argv, **kwargs):
                if argv[0] == 'systemd-run':
                    started.append(next(arg.removeprefix('--unit=') for arg in argv if arg.startswith('--unit=')))
                    if argv[-1].endswith('display_ready.py'):
                        with patch.object(recorder.time, 'sleep', side_effect=StopIteration):
                            with self.assertRaises(StopIteration):
                                runpy.run_path(argv[-1])
                elif argv[:3] == ['systemctl', '--user', 'stop']:
                    stopped.append(argv[-1])
                return subprocess.CompletedProcess(argv, 0, stdout='', stderr='')

            with patch.dict(os.environ, dict(DISPLAY=':42', INVOCATION_ID='inv', XDG_RUNTIME_DIR=directory)), \
                    patch.object(recorder, 'sha256', return_value='test'), \
                    patch.object(recorder.subprocess, 'run', side_effect=run), \
                    patch.object(recorder.subprocess, 'check_output', return_value='inv'), \
                    patch.object(recorder, 'initialize_prefix'), \
                    patch.object(recorder, 'GameWindow', side_effect=RuntimeError('game window failed')), \
                    patch.object(Path, 'write_text', write):
                with self.assertRaises((OSError, RuntimeError)) as failure:
                    recorder.run_recording(root, output, root)
            with self.subTest('display handshake'):
                self.assertEqual(visible_during_write, [False])
                self.assertEqual(json.loads(ready.read_text())['DISPLAY'], ':42')
            with self.subTest('service ownership'):
                self.assertEqual(len(started), 2)
                self.assertEqual(stopped, started[::-1])
                self.assertEqual(str(failure.exception), 'game window failed')


class MotionProofTests(unittest.TestCase):
    def test_v3_sessions_reject_typed_v4_interventions_before_recording(self):
        for control, value in (('jump_height_min', 64.0), ('reference_scale', 1.25)):
            with self.subTest(control=control), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'session.json').write_text('{}')
                fixture = dict(schema=1, kind='jump', position=[0, 0, 0], yaw_degrees=0,
                               hold_ms=500, modifier='jump', **{control: value})
                path = root / 'fixture.json'
                path.write_text(json.dumps(fixture))
                with patch.object(recorder, 'sha256', side_effect=AssertionError('recorder preflight started')):
                    with self.assertRaisesRegex(ValueError, 'inputs-v4'):
                        recorder.run_recording(root, root / 'output', root, path)
                self.assertFalse((root / 'output').exists())
                (root / 'session.json').write_text(json.dumps(dict(profile_name='inputs-v4')))
                with patch.object(recorder, 'sha256', side_effect=AssertionError('recorder preflight started')):
                    with self.assertRaisesRegex(AssertionError, 'recorder preflight started'):
                        recorder.run_recording(root, root / 'output', root, path)

    def test_dialog_quotes_do_not_swallow_the_no_button(self):
        text, words = parse_ocr('left\ttop\twidth\theight\ttext\n'
                               '10\t20\t30\t40\t"Survival\n'
                               '11\t20\t30\t40\tMode"\n'
                               '671\t539\t17\t17\tNo\n')
        self.assertEqual(text, '"survival mode" no')
        self.assertEqual(words[-1]['text'], 'No')
        self.assertEqual(words[-1]['top'], '539')

    def test_requested_30hz_camera_cadence_passes(self):
        rows = recording()
        rows[0]['sample'] = 1500
        proof = verify_trace(rows, requested_fps=30)
        self.assertEqual(proof['status'], 'passed')
        self.assertEqual(proof['camera_cadence']['achieved_fps'], 30)

    def test_requested_30hz_with_60hz_camera_cadence_fails(self):
        rows = recording()
        rows[0]['sample'] = 3000
        proof = verify_trace(rows, requested_fps=30)
        self.assertEqual(proof['status'], 'failed')
        self.assertEqual(proof['camera_cadence']['achieved_fps'], 60)
        self.assertTrue(proof['received_down_up'])
        self.assertEqual(verify_trace(rows)['status'], 'passed')

    def test_half_timescale_does_not_halve_camera_wall_rate(self):
        rows = recording()
        rows[0]['sample'] = 3000
        with_controller = []
        for row in rows:
            if row['kind'] == 3:
                values = [0.0] * 40
                values[2:4] = [1 / 120, 120]
                with_controller.append(dict(row, kind=7, values=values))
            with_controller.append(row)
        for sequence, row in enumerate(with_controller, 1):
            row['sequence'] = sequence
        proof = verify_trace(with_controller, requested_fps=60)
        self.assertEqual(proof['status'], 'passed')
        self.assertEqual(proof['camera_cadence']['achieved_fps'], 60)

    def test_cadence_tolerance_includes_27_and_33_for_30hz(self):
        frames = [{'qpc': 0}, {'qpc': 100}]
        for frequency in (2700, 3300):
            with self.subTest(frequency=frequency):
                self.assertEqual(qualify_cadence(frames, frequency, 30)['status'], 'passed')
        for frequency in (2690, 3310):
            with self.subTest(frequency=frequency):
                self.assertEqual(qualify_cadence(frames, frequency, 30)['status'], 'failed')

    def test_cadence_uses_median_qpc_interval(self):
        frames = [{'qpc': qpc} for qpc in (0, 100, 200, 500, 600)]
        proof = qualify_cadence(frames, 3000, 30)
        self.assertEqual(proof['status'], 'passed')
        self.assertEqual(proof['achieved_fps'], 30)

    def test_cadence_rejects_nonincreasing_camera_timestamps(self):
        with self.assertRaisesRegex(ValueError, 'increasing camera timestamps'):
            qualify_cadence([{'qpc': 1}, {'qpc': 1}], 1000, 60)

    def test_native_received_press_moves_player_and_camera(self):
        result = verify_trace(recording())
        self.assertEqual(result['status'], 'passed')
        self.assertEqual(result['horizontal_displacement'], 40.0)

    def test_sent_but_unreceived_input_cannot_pass(self):
        rows = [r for r in recording() if r['kind'] != 2]
        for i, row in enumerate(rows, 1):
            row['sequence'] = i
            row['input_sequence'] = 0
        with self.assertRaisesRegex(ValueError, 'did not deliver W'):
            verify_trace(rows)

    def test_received_input_without_gameplay_acceptance_fails(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 2:
                row['values'][3] = 0.0
        with self.assertRaisesRegex(ValueError, 'did not accept forward'):
            verify_trace(rows)

    def test_blocked_player_does_not_count_as_movement(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['values'][1] = 0.0
        with self.assertRaisesRegex(ValueError, 'No horizontal player movement'):
            verify_trace(rows)

    def test_camera_must_follow(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['values'][10] = 0.0
        with self.assertRaisesRegex(ValueError, 'Camera root did not follow'):
            verify_trace(rows)

    def test_third_person_is_rejected(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['state'] = 9
        with self.assertRaisesRegex(ValueError, 'Insufficient first-person'):
            verify_trace(rows)

    def test_input_worker_thread_is_preserved(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 2:
                row['thread'] = 11
        result = verify_trace(rows)
        self.assertEqual(result['input_threads'], [11])
        self.assertEqual(result['camera_threads'], [10])

    def test_input_sample_association_is_required(self):
        rows = recording()
        rows[20]['input_sequence'] = 0
        with self.assertRaisesRegex(ValueError, 'Broken input/sample association'):
            verify_trace(rows)

    def test_attached_camera_is_required(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['code'] = 1
        with self.assertRaisesRegex(ValueError, 'Attached NiCamera is missing'):
            verify_trace(rows)

    def test_missing_release_fails(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 2:
                row['values'][0] = 1.0
        with self.assertRaisesRegex(ValueError, 'down/up pair'):
            verify_trace(rows)

    def test_dropped_records_fail(self):
        rows = recording()
        del rows[10]
        with self.assertRaisesRegex(ValueError, 'Lost native records'):
            verify_trace(rows)

    def test_interrupted_native_capture_fails(self):
        with self.assertRaisesRegex(ValueError, 'has not completed'):
            verify_trace(recording()[:-1])


if __name__ == '__main__':
    unittest.main()
