"""The ordinary scripts CI job must score the committed v3 traces and frozen models."""
import json
import gzip
from pathlib import Path
import shutil
import re
from collections import Counter
import tempfile
import unittest
from unittest.mock import patch

import evaluate_skyrim_jump as evaluator
import report_skyrim_jump as report


class RetainedJumpEvidenceTests(unittest.TestCase):
    def test_report_keeps_corpus_identifiers_out_of_executable_markup(self):
        reference = Path(__file__).resolve().parents[1] / 'tools/skyrim-recorder/reference/jump-v3'
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory) / 'corpus'
            shutil.copytree(reference, corpus)
            manifest = json.loads((corpus / 'manifest.json').read_text())
            run = next(r for r in manifest['runs'] if r['split'] == 'heldout')
            malicious = '</script><script>alert(1)</script> SUMMARY REJECTED'
            run['id'] = malicious
            (corpus / 'manifest.json').write_text(json.dumps(manifest))
            output = Path(directory) / 'output'
            evaluator.score(corpus / 'manifest.json', corpus / 'frozen.json', output)
            with patch('sys.argv', ['report', '--corpus', str(corpus), '--results', str(output)]):
                report.main()
            html = (output / 'report.html').read_text()
            self.assertEqual(html.count('</script>'), 1)
            self.assertNotIn(malicious, html)
            curves = json.loads(re.search(r'const curves=(.*?);const summary=', html).group(1))
            self.assertIn(malicious, [curve['id'] for curve in curves])

    def test_report_rejects_changed_traces_or_a_mismatched_score_seal(self):
        reference = Path(__file__).resolve().parents[1] / 'tools/skyrim-recorder/reference/jump-v3'
        for mode in ('heldout-trace', 'training-trace', 'seal', 'training-frame-cap', 'training-timescale'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                corpus = Path(directory) / 'corpus'
                shutil.copytree(reference, corpus)
                output = Path(directory) / 'output'
                evaluator.score(corpus / 'manifest.json', corpus / 'frozen.json', output)
                if mode == 'seal':
                    scored = json.loads((output / 'report.json').read_text())
                    scored['frozen_sha256'] = '0' * 64
                    (output / 'report.json').write_text(json.dumps(scored))
                elif mode in ('training-frame-cap', 'training-timescale'):
                    manifest = json.loads((corpus / 'manifest.json').read_text())
                    run = next(r for r in manifest['runs'] if r['split'] == 'train')
                    run['frame_cap' if mode == 'training-frame-cap' else 'time_multiplier'] = (
                        30 if mode == 'training-frame-cap' else .5)
                    (corpus / 'manifest.json').write_text(json.dumps(manifest))
                else:
                    manifest = json.loads((corpus / 'manifest.json').read_text())
                    split = 'train' if mode == 'training-trace' else 'heldout'
                    run = next(r for r in manifest['runs'] if r['split'] == split)
                    jump = evaluator.qualify(corpus / run['path'], run)
                    peak = max(jump['samples'], key=lambda s: s['z'])['sample']
                    raw = gzip.decompress((corpus / run['path']).read_bytes())
                    rows = []
                    for row in evaluator.WIRE.iter_unpack(raw):
                        row = list(row)
                        if row[4] == 3 and row[2] == peak:
                            row[10] += 1
                        rows.append(evaluator.WIRE.pack(*row))
                    changed = b''.join(rows)
                    (corpus / run['path']).write_bytes(gzip.compress(changed))
                    run['sha256'] = evaluator.digest(changed)
                    (corpus / 'manifest.json').write_text(json.dumps(manifest))
                with patch('sys.argv', ['report', '--corpus', str(corpus), '--results', str(output)]):
                    with self.assertRaisesRegex(ValueError, 'corpus|seal|training'):
                        report.main()

    def test_report_displays_rejections_with_actual_capture_counts(self):
        reference = Path(__file__).resolve().parents[1] / 'tools/skyrim-recorder/reference/jump-v3'
        for all_rejected in (False, True):
            with self.subTest(all_rejected=all_rejected), tempfile.TemporaryDirectory() as directory:
                corpus = Path(directory) / 'corpus'
                shutil.copytree(reference, corpus)
                manifest = json.loads((corpus / 'manifest.json').read_text())
                if all_rejected:
                    manifest['runs'] = [r for r in manifest['runs'] if r['split'] == 'heldout']
                rejected = [r for r in manifest['runs'] if r['split'] == 'heldout']
                if not all_rejected:
                    rejected = rejected[:1]
                for run in rejected:
                    run['sha256'] = evaluator.digest(run['id'].encode())
                (corpus / 'manifest.json').write_text(json.dumps(manifest))
                output = Path(directory) / 'output'
                evaluator.score(corpus / 'manifest.json', corpus / 'frozen.json', output)
                with patch('sys.argv', ['report', '--corpus', str(corpus), '--results', str(output)]):
                    report.main()
                html = (output / 'report.html').read_text()
                self.assertIn('trace hash mismatch', html)
                self.assertNotIn('Each condition has three independent captures', html)
                self.assertEqual(len(json.loads((output / 'rejections.json').read_text())), len(rejected))
                summary = json.loads((output / 'summary.json').read_text())
                self.assertEqual(sum(r['n'] for r in summary), len(manifest['runs']) - len(rejected))

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
        manifest = json.loads((corpus / 'manifest.json').read_text())
        for run in manifest['runs']:
            self.assertEqual(run['proof']['status'], 'passed')
            jump = evaluator.qualify(corpus / run['path'], run)
            samples = jump['samples']
            apex = max(range(len(samples)), key=lambda i: samples[i]['z'])
            landing = next(s['sample'] for s in samples[apex + 1:]
                           if abs(s['z'] - jump['baseline']) <= 2)
            callbacks = Counter(row['sample'] for row in jump['rows']
                                if row['kind'] == 8 and row['v'][16] == 2)
            for sample in range(jump['origin']['sample'], landing):
                self.assertEqual(callbacks[sample], 1, (run['id'], sample))
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
