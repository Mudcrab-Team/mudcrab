import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


class EvidenceJobSelectionTests(unittest.TestCase):
    def test_evidence_inputs_select_both_validators(self):
        workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/run_tests.yml').read_text()
        start = workflow.index('          check_pattern() {')
        script = textwrap.dedent(workflow[start:workflow.index('    outputs:', start)])
        paths = [
            'tools/skyrim-recorder/reference/jump-inputs-v4/manifest.json',
            'tools/skyrim-recorder/reference/jump-inputs-v4/traces/baseline.bin.gz',
            'tools/skyrim-recorder/reference/jump-v3/frozen.json',
            'tools/skyrim-recorder/reference/jump-cadence-v3/traces/heldout-30hz-1x-1.bin.gz',
            'tools/skyrim-recorder/fixtures/jump-inputs-v4.json',
            'tools/skyrim-recorder/probe.cpp',
            'docs/research/evidence/skyrim-physics-221/captured-source.zip',
            'scripts/check_skyrim_jump_runtime.py',
        ]
        for changed in paths + ['docs/unrelated.md']:
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / 'outputs'
                subprocess.run(['bash', '-c', script], check=True,
                               env=dict(os.environ, CHANGED_FILES=changed, GITHUB_OUTPUT=str(output)))
                flags = dict(line.split('=', 1) for line in output.read_text().splitlines())
                expected = 'false' if changed == 'docs/unrelated.md' else 'true'
                self.assertEqual(flags['check_scripts'], expected)
                self.assertEqual(flags['check_rust'], expected)


if __name__ == '__main__':
    unittest.main()
