import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


class EvidenceJobSelectionTests(unittest.TestCase):
    def test_evidence_inputs_select_both_validators(self):
        workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/run_tests.yml').read_text()
        start = workflow.index('          set -euo pipefail', workflow.index('name: Filter changed files'))
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
            'tools/skyrim-recorder/reference/café\twith\nnewline.zip',
        ]
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory) / 'repo'
            repo.mkdir()
            def git(*args):
                return subprocess.check_output(['git', '-C', str(repo), *args], text=True).strip()

            git('init', '-q')
            git('config', 'user.name', 'CI filter test')
            git('config', 'user.email', 'ci@example.invalid')
            (repo / 'README').touch()
            git('add', 'README')
            git('commit', '-qm', 'base')
            output = Path(directory) / 'outputs'
            for changed in paths + ['docs/unrelated.md', 'docs/archived-capture.zip']:
                with self.subTest(changed=changed):
                    before = git('rev-parse', 'HEAD')
                    destination = repo / changed
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    if changed == 'docs/archived-capture.zip':
                        git('mv', paths[-1], changed)
                    else:
                        destination.touch()
                        git('add', changed)
                    git('commit', '-qm', 'change tracked path')
                    output.write_text('')
                    subprocess.run(['bash', '-c', script], cwd=repo, check=True,
                                   env=dict(os.environ, GITHUB_BASE_REF='', PUSH_BEFORE=before,
                                            GITHUB_SHA=git('rev-parse', 'HEAD'), RUNNER_TEMP=directory,
                                            GITHUB_OUTPUT=str(output)))
                    flags = dict(line.split('=', 1) for line in output.read_text().splitlines())
                    expected_rust = 'false' if changed == 'docs/unrelated.md' else 'true'
                    # Markdown under docs/ now feeds the retail-claim gate in the scripts job.
                    expected_scripts = 'true'
                    self.assertEqual(flags['check_scripts'], expected_scripts)
                    self.assertEqual(flags['check_rust'], expected_rust)

            git('branch', 'available-base', 'HEAD')
            git('remote', 'add', 'origin', str(repo))
            (repo / 'README').write_text('docs only')
            git('commit', '-qam', 'update README')
            for branch, expected in [('available-base', 'false'), ('missing-base', 'true')]:
                with self.subTest(base=branch):
                    output.write_text('')
                    subprocess.run(['bash', '-c', script], cwd=repo, check=True, capture_output=True,
                                   env=dict(os.environ, GITHUB_BASE_REF=branch, GITHUB_SHA=git('rev-parse', 'HEAD'),
                                            RUNNER_TEMP=directory, GITHUB_OUTPUT=str(output)))
                    flags = dict(line.split('=', 1) for line in output.read_text().splitlines())
                    self.assertEqual(flags['check_scripts'], expected)
                    self.assertEqual(flags['check_rust'], expected)

if __name__ == '__main__':
    unittest.main()
