"""Procedural trace fixtures: reject missing/dropped events and verify clock arithmetic."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / 'scripts/profiling'


def native_records():
    records = [{'type': 'trace_begin', 'host_ns': 0},
               {'type': 'health', 'host_ns': 0, 'enabled': True, 'dropped': 0}]
    for aid in range(1, 8):
        stamp = aid * 1_000_000_000
        records.extend([
            {'type': 'acquire_begin', 'acquisition_id': aid, 'host_ns': stamp,
             'thread_cpu_ns': stamp},
            {'type': 'acquire_end', 'acquisition_id': aid, 'end_ns': stamp+10_000_000,
             'thread_cpu_ns': stamp+1_000_000, 'texture': 'fixture-texture'},
            {'type': 'completed', 'cb_id': aid, 'gpu_time_valid': True,
             'gpu_start_s': aid+0.02, 'gpu_end_s': aid+0.03,
             'acquisition_id': aid, 'name': 'procedural-buffer'},
        ])
    records.append({'type': 'health', 'host_ns': 8_000_000_000, 'enabled': False, 'dropped': 0})
    return records


class NativeProfilingTests(unittest.TestCase):
    def run_analyzer(self, records):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        source = root / 'native.ndjson'
        source.write_text(''.join(json.dumps(r)+'\n' for r in records))
        result = subprocess.run([sys.executable, str(TOOLS/'analyze_native.py'),
                                 str(source), '--output', str(root/'summary.json')],
                                capture_output=True, text=True)
        return result, root

    def test_positive_trace_crops_partial_tails_and_retains_cpu_delta(self):
        result, root = self.run_analyzer(native_records())
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads((root/'summary.json').read_text())
        self.assertEqual(summary['window_s'], 4)
        self.assertEqual(summary['acquisition_count'], 7)
        self.assertEqual(summary['trailing_incomplete_acquisition_count'], 0)
        self.assertAlmostEqual(summary['acquire_cpu_ms']['mean'], 1)
        self.assertAlmostEqual(summary['acquire_wait_ms']['mean'], 10)
        self.assertLessEqual(summary['accepted_gpu_interval_union_s'],
                             summary['accepted_gpu_interval_sum_s'])

    def test_missing_provenance_health_and_acquisition_are_rejected(self):
        cases = [[], [{'type':'trace_begin'}],
                 [{'type':'trace_begin'}, {'type':'health'}]]
        for records in cases:
            with self.subTest(records=records):
                result, root = self.run_analyzer(records)
                self.assertEqual(result.returncode, 2)
                self.assertFalse((root/'summary.json').exists())

    def test_incomplete_acquisition_is_rejected(self):
        records = [r for r in native_records()
                   if not (r['type'] == 'acquire_end' and r['acquisition_id'] == 4)]
        result, root = self.run_analyzer(records)
        self.assertEqual(result.returncode, 2)
        self.assertIn('incomplete acquisition pairs', result.stderr)
        self.assertFalse((root/'summary.json').exists())

    def test_trailing_incomplete_acquisition_is_cropped_and_reported(self):
        records = native_records()
        records.append({'type':'acquire_begin', 'acquisition_id':8,
                        'host_ns':8_000_000_000, 'thread_cpu_ns':8_000_000_000})
        result, root = self.run_analyzer(records)
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads((root/'summary.json').read_text())
        self.assertEqual(summary['acquisition_count'], 7)
        self.assertEqual(summary['window_s'], 4)
        self.assertEqual(summary['trailing_incomplete_acquisition_count'], 1)

    def test_trailing_id_cannot_hide_an_incomplete_measured_acquisition(self):
        records = native_records()
        records.append({'type':'acquire_begin', 'acquisition_id':100,
                        'host_ns':4_000_000_000, 'thread_cpu_ns':4_000_000_000})
        result, root = self.run_analyzer(records)
        self.assertEqual(result.returncode, 2)
        self.assertIn('incomplete acquisition pairs', result.stderr)
        self.assertFalse((root/'summary.json').exists())

    def test_orphan_acquisition_end_is_rejected_even_in_shutdown_tail(self):
        records = native_records()
        records.append({'type':'acquire_end', 'acquisition_id':100,
                        'end_ns':8_000_000_000, 'thread_cpu_ns':8_000_000_000})
        result, root = self.run_analyzer(records)
        self.assertEqual(result.returncode, 2)
        self.assertIn('incomplete acquisition pairs', result.stderr)
        self.assertFalse((root/'summary.json').exists())

    def test_unhealthy_middle_record_cannot_be_hidden_by_shutdown_health(self):
        for counter in ('dropped','exceptions','hook_failures',
                        'thread_cpu_clock_failures','trace_config_errors','capture_config_errors'):
            with self.subTest(counter=counter):
                records = native_records()
                records.insert(3, {'type':'health', counter:1})
                result, _ = self.run_analyzer(records)
                self.assertEqual(result.returncode, 2)
                self.assertIn(counter, result.stderr)

    def test_empty_correlation_rejects_instead_of_dividing_by_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp)/'timeline.json'
            source.write_text('{}')
            result = subprocess.run([sys.executable, str(TOOLS/'correlate_drawables.py'),
                                     str(source), '--output', str(Path(tmp)/'out.json')],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn('No acquisitions', result.stderr)

    def test_handoff_fixture_has_unique_repeat_names_and_exact_returns(self):
        fixture = json.loads((TOOLS/'fixtures/riverwood-terrain-handoffs.json').read_text())
        self.assertEqual((fixture['width'],fixture['height']), (1600,900))
        shots = fixture['shots']
        self.assertEqual(len(shots),24)
        self.assertEqual(len({s['name'].casefold() for s in shots}),24)
        for cycle in range(3):
            poses = shots[cycle*8:(cycle+1)*8]
            self.assertEqual(poses[0]['position'],poses[-1]['position'])
            self.assertLess(poses[1]['position'][0],6*4096)
            self.assertGreater(poses[2]['position'][0],6*4096)
            self.assertEqual([s['position'] for s in poses],
                             [s['position'] for s in shots[:8]])


@unittest.skipUnless(sys.platform == 'darwin', 'the native observer requires macOS frameworks')
class NativeTraceConfigurationTests(unittest.TestCase):
    """Load the compiled observer in short processes without rendering or game assets."""

    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        root = Path(cls.directory.name)
        cls.library = root / 'libmetal_trace.dylib'
        subprocess.run([
            'clang', '-fobjc-arc', '-O2', '-Wall', '-Wextra', '-Werror', '-dynamiclib',
            str(TOOLS / 'metal_trace.m'), '-o', str(cls.library),
            '-framework', 'Foundation', '-framework', 'Metal', '-framework', 'QuartzCore',
            '-framework', 'AppKit',
        ], check=True, capture_output=True, text=True)
        host = root / 'observer_host.c'
        host.write_text('''#include <dlfcn.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    if (!dlopen(argv[1], RTLD_NOW | RTLD_LOCAL)) {
        fprintf(stderr, "%s\\n", dlerror());
        return 2;
    }
    // Hooks last for the process lifetime; never dlclose the observer.
    return 0;
}
''')
        cls.host = root / 'observer_host'
        subprocess.run(['clang', '-Wall', '-Wextra', '-Werror', str(host), '-o', str(cls.host)],
                       check=True, capture_output=True, text=True)

    def run_observer(self, overrides):
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp) / 'native.ndjson'
            environment = {name: value for name, value in os.environ.items()
                           if not name.startswith(('MUDCRAB_METAL_', 'MTL_'))}
            environment.update(overrides)
            environment['MUDCRAB_METAL_TRACE_FILE'] = str(trace)
            environment['MUDCRAB_METAL_TRACE_ACQUIRE_ONLY'] = '1'
            result = subprocess.run([str(self.host), str(self.library)], env=environment,
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result, [json.loads(line) for line in trace.read_text().splitlines()]

    def assert_rejected(self, overrides):
        result, records = self.run_observer(overrides)
        errors = [record for record in records if record['type'] == 'trace_config_error']
        self.assertEqual({record['selector'] for record in errors}, set(overrides))
        self.assertEqual(len(errors), len(overrides))
        self.assertIn('observer_disabled=1', result.stderr)
        health = records[-1]
        self.assertEqual(health['type'], 'health')
        self.assertFalse(health['enabled'])
        self.assertEqual(health['trace_config_errors'], len(overrides))
        self.assertEqual(health['factory_calls'], 0)
        self.assertEqual(health['acquired'], 0)
        self.assertEqual(health['rows_attempted'], 0)
        self.assertEqual({record['type'] for record in records}, {'trace_config_error', 'health'})

    def test_unset_trace_limits_keep_defaults(self):
        result, records = self.run_observer({})
        self.assertNotIn('config_error', result.stderr)
        begin = records[0]
        self.assertEqual(begin['type'], 'trace_begin')
        self.assertEqual(begin['max_rows'], 1_000_000)
        self.assertEqual(begin['start_acquisition'], 0)
        self.assertEqual(records[-1]['trace_config_errors'], 0)

    def test_valid_uint64_boundaries_and_zero_start_are_preserved(self):
        maximum = (1 << 64) - 1
        for limit, start in (('1', '0'), ('000123', '000045'),
                             (str(maximum), str(maximum))):
            with self.subTest(limit=limit, start=start):
                result, records = self.run_observer({
                    'MUDCRAB_METAL_TRACE_MAX_ROWS': limit,
                    'MUDCRAB_METAL_TRACE_START_ACQUISITION': start,
                })
                self.assertNotIn('config_error', result.stderr)
                self.assertEqual(records[0]['max_rows'], int(limit))
                self.assertEqual(records[0]['start_acquisition'], int(start))
                self.assertEqual(records[-1]['trace_config_errors'], 0)

    def test_malformed_trace_limits_are_rejected_before_hooks_start(self):
        bad_values = ('', 'abc', '1e6', '1.0', '-1', '+1', ' 1', '1 ', '1\n',
                      str(1 << 64), '9' * 100)
        for parameter in ('MUDCRAB_METAL_TRACE_MAX_ROWS',
                          'MUDCRAB_METAL_TRACE_START_ACQUISITION'):
            for value in bad_values:
                with self.subTest(parameter=parameter, value=value):
                    self.assert_rejected({parameter: value})

    def test_zero_row_limit_is_rejected(self):
        self.assert_rejected({'MUDCRAB_METAL_TRACE_MAX_ROWS': '0'})

    def test_multiple_configuration_errors_are_recorded(self):
        self.assert_rejected({'MUDCRAB_METAL_TRACE_MAX_ROWS': 'abc',
                              'MUDCRAB_METAL_TRACE_START_ACQUISITION': '1e6'})


if __name__ == '__main__':
    unittest.main()
