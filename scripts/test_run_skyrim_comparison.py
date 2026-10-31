import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from unittest.mock import patch

import run_skyrim_comparison as runner


class RunnerEvidenceTests(unittest.TestCase):
    def test_latest_completed_load_wins_over_historical_save_events(self):
        content = (
            'save name is Expected.ess\n'
            'dispatch message (3)\n'
            'save name is Different.ess\n'
            'dispatch message (3)\n'
        )
        self.assertEqual(runner.latest_completed_save(content), 'Different')

    def test_incomplete_latest_load_does_not_reuse_an_older_completion(self):
        content = (
            'save name is Expected\ndispatch message (3)\n'
            'save name is Different\n'
        )
        self.assertIsNone(runner.latest_completed_save(content))

    def test_display_environment_must_match_the_current_systemd_invocation(self):
        stale = {'DISPLAY': ':7', 'INVOCATION_ID': 'old'}
        current = {'DISPLAY': ':7', 'INVOCATION_ID': 'current'}
        self.assertFalse(runner.display_environment_is_current(stale, 'current'))
        self.assertTrue(runner.display_environment_is_current(current, 'current'))

    def test_capture_failure_writes_receipt_and_retains_diagnostics(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / 'run'
            artifacts = root / 'artifacts'
            artifacts.mkdir()
            (artifacts / 'automation-tools.paths').write_text(str(root / 'tools') + '\n')
            (root / 'tools' / 'bin').mkdir(parents=True)
            for name in ('xdotool', 'xprop'):
                (root / 'tools' / 'bin' / name).write_text('')
            (artifacts / 'ocr-tool.path').write_text(str(root / 'ocr') + '\n')
            (root / 'ocr' / 'bin').mkdir(parents=True)
            (root / 'ocr' / 'bin' / 'tesseract').write_text('')
            (root / 'session.json').write_text(json.dumps({'save': {'save_filename': 'Expected.ess'}}))

            log = root / 'compatdata/pfx/drive_c/users/steamuser/Documents/My Games/Skyrim Special Edition/SKSE/skse64.log'
            log.parent.mkdir(parents=True)
            log.write_text('save name is Expected.ess\ndispatch message (3)\n')

            display_file = artifacts / 'display.env'
            source = root / 'gamescope.png'
            png = b'\x89PNG\r\n\x1a\nsynthetic\x00\x00\x00\x00IEND\xaeB`\x82'
            calls = {'xprop': 0, 'search': 0}

            def fake_run(*command, env=None, check=True):
                # Compare basenames: Path() renders with backslashes on Windows.
                executable = Path(command[0]).name
                if command[:3] == ('systemctl', '--user', 'start') and command[-1] == runner.DISPLAY_SERVICE:
                    display_file.write_text('DISPLAY=:99\nINVOCATION_ID=current\n')
                    return subprocess.CompletedProcess(command, 0, '', '')
                if command[:3] == ('systemctl', '--user', 'show'):
                    return subprocess.CompletedProcess(command, 0, 'current\n', '')
                if command[:3] == ('systemctl', '--user', 'is-active'):
                    return subprocess.CompletedProcess(command, 0, 'active\n', '')
                if executable == 'xdotool' and command[1] == 'search':
                    calls['search'] += 1
                    if calls['search'] <= 2:
                        self.assertEqual(calls['xprop'], 0)
                        return subprocess.CompletedProcess(command, 0,
                                                           '' if calls['search'] == 1 else '  \n', '')
                    return subprocess.CompletedProcess(command, 0, '99\n100\n', '')
                if executable == 'tesseract':
                    return subprocess.CompletedProcess(command, 0, '', '')
                if executable == 'xprop':
                    calls['xprop'] += 1
                    if calls['xprop'] == 1:
                        source.write_bytes(png)
                        return subprocess.CompletedProcess(command, 0, '', '')
                    raise subprocess.CalledProcessError(1, command, stderr='synthetic compositor failure')
                return subprocess.CompletedProcess(command, 0, '', '')

            stdout, stderr = StringIO(), StringIO()
            with patch.object(runner, 'ROOT', root), \
                 patch.object(runner, 'GAMESCOPE_CAPTURE_PATH', source), \
                 patch.object(runner, 'display_socket_exists', return_value=True), \
                 patch.object(runner.time, 'sleep', return_value=None), \
                 patch.object(runner, 'run', side_effect=fake_run), \
                 redirect_stdout(stdout), redirect_stderr(stderr):
                result = runner.main(['--output', str(output)])

            self.assertEqual(calls['search'], 3)
            self.assertEqual(result, 1)
            receipt = json.loads((output / 'failure.json').read_text())
            self.assertEqual(receipt['phase'], 'capture-request')
            self.assertEqual(receipt['error']['command'][0], str(root / 'tools' / 'bin' / 'xprop'))
            self.assertEqual(receipt['error']['stderr'], 'synthetic compositor failure')
            self.assertEqual(receipt['display_invocation_id'], 'current')
            self.assertEqual([image['file'] for image in receipt['screenshots']], ['current.png'])
            self.assertTrue((output / 'skse64.log').is_file())
            self.assertIn('failure.json', stderr.getvalue())


if __name__ == '__main__':
    unittest.main()
