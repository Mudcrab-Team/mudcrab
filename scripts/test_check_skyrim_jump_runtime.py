import gzip
import hashlib
import json
import math
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_skyrim_jump_runtime as replay
import report_skyrim_jump_runtime as chart


class ReplayScoreTests(unittest.TestCase):
    def score_predictions(self, heights, *, duration=replay.RULES['window_game_seconds'],
                          prediction_ids=('jump',)):
        samples = [{'controller': duration * (i / (len(heights) - 1)), 'z': 0.0}
                   for i in range(len(heights))]
        run = {'split': 'heldout', 'frame_cap': 60, 'time_multiplier': 1}
        observed = {'baseline': 0.0, 'samples': samples, 'observed_sampled_apex': 0.0}
        output = {'schema': 1, 'runs': [{'id': key, 'heights': heights} for key in prediction_ids]}
        return replay.score(output, {'jump': (run, observed)})[0]

    def test_perfect_prediction_requires_full_window(self):
        window = replay.RULES['window_game_seconds']
        for duration, passes in [(window, True), (math.nextafter(window, 0), False)]:
            with self.subTest(duration=duration):
                result = self.score_predictions([0, 0], duration=duration)
                self.assertEqual(result['window_covered'], passes)
                self.assertEqual(result['passes_frozen_report_gate'], passes)

    def test_rmse_limit_is_inclusive(self):
        limit = replay.RULES['rmse_gate']
        for rmse, passes in [(limit, True), (math.nextafter(limit, math.inf), False)]:
            with self.subTest(rmse=rmse):
                # One error of 2 * rmse across four samples gives the desired RMSE.
                result = self.score_predictions([0, 0, 0, 2 * rmse])
                self.assertEqual(result['rmse'], rmse)
                self.assertLess(result['max_abs'], replay.RULES['peak_error_gate'])
                self.assertEqual(result['passes_frozen_report_gate'], passes)

    def test_peak_error_limit_is_inclusive(self):
        limit = replay.RULES['peak_error_gate']
        for peak, passes in [(limit, True), (math.nextafter(limit, math.inf), False)]:
            with self.subTest(peak=peak):
                result = self.score_predictions([0] * 15 + [peak])
                self.assertEqual(result['max_abs'], peak)
                self.assertLess(result['rmse'], replay.RULES['rmse_gate'])
                self.assertEqual(result['passes_frozen_report_gate'], passes)

    def test_missing_and_duplicate_predictions_are_rejected(self):
        for ids in [(), ('jump', 'jump')]:
            with self.subTest(ids=ids):
                with self.assertRaisesRegex(ValueError, 'Rust predictions omit, duplicate or add runs'):
                    self.score_predictions([0, 0], prediction_ids=ids)


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

    def test_prepare_only_cannot_satisfy_heldout_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'replay'
            with patch.object(sys, 'argv', ['replay', '--output', str(output),
                                            '--prepare-only', '--require-heldout-pass']):
                with self.assertRaises(SystemExit) as error:
                    replay.main()
            self.assertEqual(error.exception.code, 2)
            self.assertFalse(output.exists())

    def test_required_gate_rejects_a_supplied_binary_before_preparing_output(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'replay'
            with patch.object(sys, 'argv', ['replay', '--output', str(output),
                                            '--require-heldout-pass', '--test-binary', sys.executable]):
                with self.assertRaises(SystemExit) as error:
                    replay.main()
            self.assertEqual(error.exception.code, 2)
            self.assertFalse(output.exists())

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
