import contextlib
from io import StringIO
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zlib

import capture_skyrim_tonemap_state as collector


def chunk(kind, payload=b''):
    return (struct.pack('>I', len(payload)) + kind + payload
            + struct.pack('>I', zlib.crc32(kind + payload) & 0xffffffff))


def png():
    return (collector.PNG_SIGNATURE
            + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(b'\x00\xff\xff\xff'))
            + chunk(b'IEND'))


def file_stat(mtime_ns, size=100, inode=1, mode=stat.S_IFREG | 0o600):
    return SimpleNamespace(st_mtime_ns=mtime_ns, st_size=size, st_ino=inode, st_mode=mode)


class ConsoleEvidenceTests(unittest.TestCase):
    def test_complete_png_requires_valid_chunk_crcs(self):
        image = png()
        self.assertEqual(collector.validate_png(image), (1, 1))
        corrupt = bytearray(image)
        corrupt[45] ^= 1
        with self.assertRaisesRegex(RuntimeError, 'CRC'):
            collector.validate_png(corrupt)

    def test_png_rejects_partial_capture_and_trailing_bytes(self):
        for image in (png()[:-2], png() + b'old-frame-tail'):
            with self.subTest(image=image), self.assertRaises(RuntimeError):
                collector.validate_png(image)

    def test_png_rejects_signature_and_iend_without_image_data(self):
        with self.assertRaises(RuntimeError):
            collector.validate_png(collector.PNG_SIGNATURE + chunk(b'IEND'))

    def test_existing_frame_cannot_satisfy_a_new_request(self):
        old = file_stat(100)
        self.assertFalse(collector.capture_is_fresh(old, old, 101))
        self.assertFalse(collector.capture_is_fresh(file_stat(99, inode=2), old, 101))
        self.assertTrue(collector.capture_is_fresh(file_stat(102), old, 101))
        self.assertFalse(collector.capture_is_fresh(
            file_stat(102, mode=stat.S_IFLNK | 0o777), old, 101))

    def test_capture_times_out_instead_of_copying_stale_png(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source.png'
            source.write_bytes(png())
            os.utime(source, ns=(1, 1))
            with patch.object(collector, 'GAMESCOPE_CAPTURE_PATH', source), \
                    patch.object(collector, 'run'), \
                    patch.object(collector.time, 'monotonic', side_effect=[0, 0, 2]), \
                    patch.object(collector.time, 'sleep'):
                with self.assertRaisesRegex(RuntimeError, 'No fresh complete'):
                    collector.capture_frame(root / 'result.png', 'xprop', {}, timeout=1)
            self.assertFalse((root / 'result.png').exists())

    def test_capture_accepts_only_a_new_complete_frame(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source.png'
            source.write_bytes(png())
            os.utime(source, ns=(1, 1))

            def request(*command, env=None):
                source.write_bytes(png())
                os.utime(source, ns=(200, 200))

            with patch.object(collector, 'GAMESCOPE_CAPTURE_PATH', source), \
                    patch.object(collector, 'run', side_effect=request), \
                    patch.object(collector.time, 'time_ns', return_value=100):
                result = collector.capture_frame(root / 'result.png', 'xprop', {})
            self.assertEqual(result['sha256'], collector.sha256(png()))
            self.assertEqual(result['source_mtime_ns'], 200)

    def test_allowlist_rejects_setting_changes_and_chained_queries(self):
        collector.validate_command('PrintHDRParam')
        for command in ('setini "bUseFilmicCurve:Display" 1', 'fw 81a',
                        'PrintHDRParam; quit', 'getini "fUnreviewed:Display"'):
            with self.subTest(command=command), self.assertRaisesRegex(RuntimeError, 'allowlist'):
                collector.validate_command(command)

    def test_return_is_held_and_released_after_typing_with_slower_events(self):
        calls = []
        with patch.object(collector, 'run', side_effect=lambda *args, **kwargs: calls.append(args)), \
                patch.object(collector.time, 'sleep') as hold:
            collector.send_query('PrintHDRParam', 'xdotool', {})
        self.assertEqual(calls, [
            ('xdotool', 'type', '--clearmodifiers', '--delay', '60', '--', 'PrintHDRParam'),
            ('xdotool', 'keydown', '--clearmodifiers', 'Return'),
            ('xdotool', 'keyup', '--clearmodifiers', 'Return'),
        ])
        hold.assert_called_once_with(0.2)

    def test_return_is_released_when_the_hold_is_interrupted(self):
        calls = []
        with patch.object(collector, 'run', side_effect=lambda *args, **kwargs: calls.append(args)), \
                patch.object(collector.time, 'sleep', side_effect=RuntimeError('interrupted')):
            with self.assertRaisesRegex(RuntimeError, 'interrupted'):
                collector.send_query('PrintHDRParam', 'xdotool', {})
        self.assertEqual(calls[-1], ('xdotool', 'keyup', '--clearmodifiers', 'Return'))

    def test_return_release_is_attempted_even_if_keydown_reports_an_error(self):
        calls = []

        def fail_keydown(*args, **kwargs):
            calls.append(args)
            if args[1] == 'keydown':
                raise subprocess.CalledProcessError(1, args, stderr='keydown failed')

        with patch.object(collector, 'run', side_effect=fail_keydown):
            with self.assertRaises(subprocess.CalledProcessError):
                collector.send_query('PrintHDRParam', 'xdotool', {})
        self.assertEqual(calls[-1], ('xdotool', 'keyup', '--clearmodifiers', 'Return'))

    def test_wine_main_process_is_verified_by_executable_mapping(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            process = root / '123'
            process.mkdir()
            executable = Path('/home/taylor/Games/MudcrabComparison/SkyrimSE/SkyrimSE.exe')
            (process / 'comm').write_text('Main\n')
            (process / 'cmdline').write_bytes(
                b'Z:\\home\\taylor\\Games\\MudcrabComparison\\SkyrimSE\\SkyrimSE.exe\0')
            (process / 'maps').write_text(f'6ffff000-6fffffff r-xp 0000 00:01 1 {executable}\n')
            with patch.object(collector, 'PROC_ROOT', root):
                result = collector.inspect_game_process('123', executable)
            self.assertTrue(result['isolated_mapping_verified'])
            self.assertTrue(result['isolated_argument_verified'])

    def test_window_bound_to_another_installation_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            process = root / '123'
            process.mkdir()
            (process / 'cmdline').write_bytes(b'/other/SkyrimSE.exe\0')
            (process / 'maps').write_text('1000-2000 r-xp 0000 00:01 1 /other/SkyrimSE.exe\n')
            with patch.object(collector, 'PROC_ROOT', root):
                with self.assertRaisesRegex(RuntimeError, 'does not map the isolated'):
                    collector.inspect_game_process('123', Path('/isolated/SkyrimSE.exe'))

    def test_executable_symlink_outside_isolation_is_rejected_before_hash(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            game = root / 'SkyrimSE'
            game.mkdir()
            external = root / 'external.exe'
            external.write_bytes(b'executable')
            (game / 'SkyrimSE.exe').symlink_to(external)
            with patch.object(collector, 'ROOT', root):
                with self.assertRaisesRegex(RuntimeError, 'escapes isolated directory'):
                    collector.verify_isolated_files()

    def test_documents_symlink_to_normal_prefix_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root, normal = directory / 'isolated', directory / 'normal-documents'
            normal.mkdir()
            (root / 'SkyrimSE').mkdir(parents=True)
            executable = root / 'SkyrimSE/SkyrimSE.exe'
            executable.write_bytes(b'executable')
            documents = root / 'compatdata/pfx/drive_c/users/steamuser/Documents'
            documents.parent.mkdir(parents=True)
            documents.symlink_to(normal)
            with patch.object(collector, 'ROOT', root), \
                    patch.object(collector, 'EXPECTED_EXE_SHA256', collector.sha256(b'executable')):
                with self.assertRaisesRegex(RuntimeError, 'Documents escapes'):
                    collector.verify_isolated_files()

    def test_stale_display_invocation_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'artifacts').mkdir()
            (root / 'artifacts/display.env').write_text('DISPLAY=:99\nINVOCATION_ID=old\n')
            with patch.object(collector, 'ROOT', root), \
                    patch.object(collector, 'run', return_value=subprocess.CompletedProcess(
                        ['systemctl'], 0, 'current\n', '')):
                with self.assertRaisesRegex(RuntimeError, 'different service invocation'):
                    collector.verified_display_environment()

    def test_current_invocation_still_requires_a_live_display_socket(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'artifacts').mkdir()
            (root / 'artifacts/display.env').write_text(
                'DISPLAY=:999999987\nINVOCATION_ID=current\n')
            with patch.object(collector, 'ROOT', root), \
                    patch.object(collector, 'run', return_value=subprocess.CompletedProcess(
                        ['systemctl'], 0, 'current\n', '')):
                with self.assertRaisesRegex(RuntimeError, 'X11 socket is missing'):
                    collector.verified_display_environment()

    def test_existing_output_is_never_reused(self):
        with tempfile.TemporaryDirectory() as temporary, \
                contextlib.redirect_stderr(StringIO()), patch.object(collector, 'collect') as collect:
            result = collector.main(['--output', temporary, '--console-open'])
            self.assertEqual(result, 2)
            collect.assert_not_called()

    def test_console_assertion_is_required_before_output_or_input(self):
        with tempfile.TemporaryDirectory() as temporary, \
                contextlib.redirect_stderr(StringIO()), patch.object(collector, 'collect') as collect:
            output = Path(temporary) / 'new'
            with self.assertRaises(SystemExit):
                collector.main(['--output', str(output)])
            self.assertFalse(output.exists())
            collect.assert_not_called()

    def test_error_receipt_preserves_command_failure_and_raw_artifacts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'artifacts').mkdir()
            output = root / 'result'

            def fail(destination, state):
                state['phase'] = 'query-curve-selection'
                (destination / '00-initial-console.png').write_bytes(png())
                raise subprocess.CalledProcessError(1, ['xprop', '-root'], stderr='capture failed')

            with patch.object(collector, 'ROOT', root), \
                    patch.object(collector, 'collect', side_effect=fail), \
                    contextlib.redirect_stderr(StringIO()):
                result = collector.main(['--output', str(output), '--console-open'])
            self.assertEqual(result, 1)
            receipt = json.loads((output / 'failure.json').read_text())
            self.assertEqual(receipt['phase'], 'query-curve-selection')
            self.assertEqual(receipt['error']['stderr'], 'capture failed')
            self.assertEqual(receipt['retained_artifacts'][0]['sha256'], collector.sha256(png()))
            self.assertFalse(receipt['gpu_inputs_observed'])

    def test_ocr_history_is_retained_without_numeric_parsing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            evidence = {'file': 'frame.png', 'sha256': 'image-hash'}
            raw = 'old getini output: 1.4\nnew query: fReinhardWhiteScale:Display\n'
            with patch.object(collector, 'capture_frame', return_value=evidence), \
                    patch.object(collector, 'run', return_value=subprocess.CompletedProcess(
                        ['tesseract'], 0, raw, '')):
                result = collector.capture_evidence(output, 'frame', {'xprop': 'xprop',
                                                    'tesseract': 'tesseract'}, {})
            self.assertEqual((output / 'frame.ocr.txt').read_text(), raw)
            self.assertFalse(result['current_numeric_values_parsed'])
            self.assertNotIn('value', result)

    def test_initial_dialog_stops_collection_before_any_typing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            calls = []

            def fake_run(*command, env=None):
                calls.append(command)
                value = {'search': '123\n', 'getwindowpid': '456\n',
                         'getactivewindow': '123\n'}.get(command[1], '')
                if command[0] == 'tesseract':
                    value = 'Missing content. Continue loading?'
                return subprocess.CompletedProcess(command, 0, value, '')

            state = {'queries': []}
            with patch.object(collector, 'verify_isolated_files', return_value=(Path('/game.exe'), {})), \
                    patch.object(collector, 'discover_tools', return_value={
                        'xdotool': 'xdotool', 'xprop': 'xprop', 'tesseract': 'tesseract'}), \
                    patch.object(collector, 'verified_display_environment',
                                 return_value=({'DISPLAY': ':99'}, 'current')), \
                    patch.object(collector, 'inspect_game_process', return_value={}), \
                    patch.object(collector, 'capture_frame', return_value={}), \
                    patch.object(collector, 'run', side_effect=fake_run):
                with self.assertRaisesRegex(RuntimeError, 'Possible save/startup dialog'):
                    collector.collect(output, state)
            self.assertFalse(any(command[1] == 'type' for command in calls))
            self.assertEqual(state['queries'], [])

    def test_collection_sends_only_fixed_queries_and_preserves_each_raw_frame(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            calls, frames = [], []

            def fake_run(*command, env=None):
                calls.append(command)
                value = {'search': '123\n', 'getwindowpid': '456\n',
                         'getactivewindow': '123\n'}.get(command[1], '')
                if command[0] == 'tesseract':
                    value = 'Old console value 1.4\nNew console value unreadable\n'
                return subprocess.CompletedProcess(command, 0, value, '')

            def fake_capture(destination, xprop, env):
                destination.write_bytes(png())
                frames.append(destination.name)
                return {'file': destination.name, 'sha256': collector.sha256(png())}

            state = {'queries': []}
            with patch.object(collector, 'verify_isolated_files', return_value=(Path('/game.exe'), {})), \
                    patch.object(collector, 'discover_tools', return_value={
                        'xdotool': 'xdotool', 'xprop': 'xprop', 'tesseract': 'tesseract'}), \
                    patch.object(collector, 'verified_display_environment',
                                 return_value=({'DISPLAY': ':99'}, 'current')), \
                    patch.object(collector, 'inspect_game_process', return_value={}), \
                    patch.object(collector, 'capture_frame', side_effect=fake_capture), \
                    patch.object(collector, 'run', side_effect=fake_run), \
                    patch.object(collector.time, 'sleep'):
                collector.collect(output, state)
            typed = [command[-1] for command in calls if command[1] == 'type']
            self.assertEqual(typed, [command for _, command in collector.QUERIES])
            self.assertEqual(len(frames), len(collector.QUERIES) + 1)
            self.assertEqual(len(set(frames)), len(frames))
            self.assertTrue(all(entry['command_sent'] for entry in state['queries']))
            self.assertTrue(all(not entry['current_numeric_values_parsed'] for entry in state['queries']))
            self.assertEqual(state['status'], 'captured-raw-evidence')
            self.assertTrue((output / 'receipt.json').exists())


if __name__ == '__main__':
    unittest.main()
