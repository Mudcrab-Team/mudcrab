"""The ordinary scripts CI job must score the committed v3 traces and frozen models."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import evaluate_skyrim_jump as evaluator
import report_skyrim_jump as report


class RetainedJumpEvidenceTests(unittest.TestCase):
    def assert_summary_matches(self, actual, expected):
        if isinstance(expected, float):
            self.assertAlmostEqual(actual, expected, delta=1e-9)
        elif isinstance(expected, dict):
            self.assertEqual(actual.keys(), expected.keys())
            for key in expected:
                self.assert_summary_matches(actual[key], expected[key])
        elif isinstance(expected, list):
            self.assertEqual(len(actual), len(expected))
            for a, b in zip(actual, expected):
                self.assert_summary_matches(a, b)
        else:
            self.assertEqual(actual, expected)

    def check_corpus(self, name):
        corpus = Path(__file__).resolve().parents[1] / 'tools/skyrim-recorder/reference' / name
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            scored = evaluator.score(corpus / 'manifest.json', corpus / 'frozen.json', output)
            self.assertFalse([run for run in scored['runs'] if 'rejected' in run], 'retained native run rejected')
            for run in scored['runs']:
                native = next(model for model in run['models']
                              if model['model']['law'] == 'native_discrete' and model['model']['clock'] == 'controller')
                self.assertTrue(native['passes_frozen_report_gate'], run['id'])
            with patch('sys.argv', ['report', '--corpus', str(corpus), '--results', str(output)]):
                report.main()
            self.assert_summary_matches(json.loads((output / 'summary.json').read_text()),
                                        json.loads((corpus / 'result/summary.json').read_text()))

    def test_committed_jump_v3_matches_frozen_expected_results(self):
        self.check_corpus('jump-v3')

    def test_committed_cadence_v3_matches_frozen_expected_results(self):
        self.check_corpus('jump-cadence-v3')


if __name__ == '__main__':
    unittest.main()
