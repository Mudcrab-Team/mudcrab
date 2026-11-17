"""Persist capture ownership before collection and recover unfinished attempts."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import tempfile


def save_manifest(path, manifest):
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as output:
            temporary = Path(output.name)
            output.write(json.dumps(manifest, indent=2, allow_nan=False) + '\n')
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def fail_attempt(row, output, failure, retain):
    if retain:
        retain(output, row, failure)
    else:
        row['failure'] = failure


def recover_attempts(path, manifest, private_output, retain=None):
    pending = [row for row in manifest['attempts'] if row.get('status') == 'pending']
    for row in pending:
        if any(run['id'] == row['id'] for run in manifest['runs']):
            row['exit_code'] = 0
        else:
            row['recorder_exit_code'] = row.get('exit_code')
            row['exit_code'] = -1
            output = (Path(row['private_output']) if 'private_output' in row else
                      private_output / f"{row['id']}-attempt-{row['attempt']}")
            fail_attempt(row, output, {'phase': 'interrupted',
                                      'error': 'Capture interrupted before attempt acceptance'}, retain)
        row.pop('status')
        row.pop('private_output', None)
    if pending:
        save_manifest(path, manifest)


@contextmanager
def record_attempt(path, manifest, name, number, output, retain=None):
    row = dict(id=name, attempt=number, exit_code=None, status='pending',
               private_output=str(output.resolve()))
    manifest['attempts'].append(row)
    save_manifest(path, manifest)
    try:
        yield row
    except BaseException as error:
        if row['exit_code'] is None:
            row['exit_code'] = -1
        if 'failure' not in row:
            fail_attempt(row, output, {'phase': 'interrupted' if isinstance(error, (KeyboardInterrupt, SystemExit)) else 'capture',
                                      'error': type(error).__name__}, retain)
        raise
    finally:
        row.pop('status')
        row.pop('private_output', None)
        save_manifest(path, manifest)
