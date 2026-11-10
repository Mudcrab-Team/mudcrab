import gzip
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

import check_skyrim_jump_runtime as replay


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


if __name__ == '__main__':
    unittest.main()
