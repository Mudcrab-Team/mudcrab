import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import capture_skyrim_jump as capture


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
                      'source_save_sha256': plan['source_save_sha256']}
            if profile is not None:
                config['profile_name'] = profile
            (session / 'session.json').write_text(json.dumps(config))
            corpus, private = root / 'corpus', root / 'private'
            argv = ['capture', '--session', str(session), '--corpus', str(corpus),
                    '--private-output', str(private), '--ocr-prefix', str(root), '--split', 'train']
            error = io.StringIO()
            with patch('sys.argv', argv), contextlib.redirect_stderr(error), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    patch.object(capture.subprocess, 'run', side_effect=RuntimeError('recorder started')) as run:
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


if __name__ == '__main__':
    unittest.main()
