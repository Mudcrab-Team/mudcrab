import gzip
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_skyrim_jump_runtime as replay
import report_skyrim_jump_runtime as chart


class RetainedEvidenceTests(unittest.TestCase):
    def test_published_corpus_keeps_all_unique_native_recordings(self):
        _, observations, _ = replay.prepare()
        self.assertEqual(len(observations), 15)
        self.assertEqual(sum(run['split'] == 'heldout' for run, _ in observations.values()), 12)

    def test_replacing_trace_and_its_manifest_hash_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = root / 'tools/skyrim-recorder/reference/jump-v3'
            shutil.copytree(replay.REFERENCE / 'jump-v3', corpus)
            manifest_path = corpus / 'manifest.json'
            manifest = json.loads(manifest_path.read_bytes())
            run = next(run for run in manifest['runs'] if run['split'] == 'heldout')
            path = corpus / run['path']
            raw = bytearray(gzip.decompress(path.read_bytes()))
            raw[-1] ^= 1
            path.write_bytes(gzip.compress(raw, mtime=0))
            run['sha256'] = hashlib.sha256(raw).hexdigest()
            manifest_path.write_text(json.dumps(manifest))
            with patch.object(replay, 'ROOT', root):
                with self.assertRaisesRegex(ValueError, 'Pinned evidence identity changed'):
                    replay.prepare()

    def test_omitting_an_acquisition_source_from_a_receipt_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in replay.PINNED_EVIDENCE:
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(replay.ROOT / name, destination)
            receipt = root / 'tools/skyrim-recorder/reference/jump-v3/train-capture.json'
            changed = json.loads(receipt.read_bytes())
            changed['sources'].popitem()
            receipt.write_text(json.dumps(changed))
            with patch.object(replay, 'ROOT', root):
                with self.assertRaisesRegex(ValueError, 'Pinned evidence identity changed: .*train-capture'):
                    replay.prepare()

    def test_existing_replay_directory_preserves_its_results(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / 'result.json'
            receipt.write_bytes(b'previous run')
            with patch.object(sys, 'argv', ['replay', '--output', directory]):
                with self.assertRaises(SystemExit) as error:
                    replay.main()
            self.assertEqual(error.exception.code, 2)
            self.assertEqual(list(Path(directory).iterdir()), [receipt])
            self.assertEqual(receipt.read_bytes(), b'previous run')

    def test_existing_chart_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'chart.html'
            output.write_bytes(b'previous chart')
            with patch.object(sys, 'argv', ['chart', '--output', str(output)]), patch.object(chart, 'render', return_value='new chart'):
                with self.assertRaises(FileExistsError):
                    chart.main()
            self.assertEqual(output.read_bytes(), b'previous chart')


if __name__ == '__main__':
    unittest.main()
