"""Persist capture ownership before collection and recover unfinished attempts."""
from contextlib import contextmanager
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import zlib


RECORDER_SHUTDOWN_TIMEOUT = 120


@contextmanager
def corpus_lock(corpus):
    with (corpus / '.capture.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield lock.fileno()


def claim_private_output(private, corpus):
    """Bind a campaign's private directory before inspecting any previous attempts."""
    owner = private / '.capture-corpus'
    if not owner.is_symlink():
        if any(private.glob('*-attempt-*')):
            raise ValueError(f'Private output contains unclaimed attempts: {private}; use a fresh private directory')
        try:
            owner.symlink_to(corpus.resolve(), target_is_directory=True)
        except FileExistsError:
            pass  # A competing corpus may have installed its claim.
    if not owner.is_symlink() or owner.readlink() != corpus.resolve():
        raise ValueError(f'Private output belongs to another corpus: {private}')


def private_attempt_paths(private, name, number, adopted=None):
    if not name or Path(name).name != name or name in ('.', '..'):
        raise ValueError(f'Invalid private attempt name: {name!r}')
    label = f'{name}-attempt-{number}'
    output = private / label
    inputs = private / (label + '.inputs')
    reserved = (inputs,) if adopted else (inputs, output, private / (label + '.log'))
    for path in reserved:
        if os.path.lexists(path):
            raise FileExistsError(f'Private attempt path already exists: {path}')
    return (adopted or output), inputs


def run_recorder(command, *, stdout, attempt, lock_fd):
    """Forward campaign cancellation and reap the recorder while the corpus is locked."""
    cancellation = None
    waiting = False

    def cancel(signum, _frame):
        nonlocal cancellation
        if cancellation is None:
            cancellation = signum
            if waiting:
                raise SystemExit(128 + signum)

    previous = {}
    try:
        for sig in (signal.SIGINT, signal.SIGTERM):
            if sig == signal.SIGINT and signal.getsignal(sig) == signal.SIG_IGN:
                continue
            previous[sig] = signal.signal(sig, cancel)
        with subprocess.Popen(command, stdout=stdout, stderr=subprocess.STDOUT,
                              start_new_session=True, pass_fds=(lock_fd,)) as child:
            try:
                waiting = True
                if cancellation is None:
                    child.wait()
            finally:
                waiting = False
                if child.returncode is None:
                    child.send_signal(cancellation or signal.SIGTERM)
                    try:
                        child.wait(timeout=RECORDER_SHUTDOWN_TIMEOUT)
                    except subprocess.TimeoutExpired:
                        try:
                            os.killpg(child.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                attempt['exit_code'] = child.wait()
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        if cancellation is not None:
            raise SystemExit(128 + cancellation)
    return attempt['exit_code']


def verify_retained_trace(corpus, run):
    path = Path(run['path'])
    if path.is_absolute() or '..' in path.parts:
        raise ValueError(f'Retained trace escapes corpus: {path}')
    retained = corpus / path
    try:
        raw = gzip.decompress(retained.read_bytes())
    except (OSError, EOFError, zlib.error) as error:
        raise ValueError(f'Cannot read retained trace {retained}: {error}') from error
    if hashlib.sha256(raw).hexdigest() != run['sha256']:
        raise ValueError(f'Retained trace hash mismatch: {retained}')


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
            if retain and 'private_output' not in row:
                raise ValueError(f"Pending attempt {row['id']} lacks its recorded private output path")
            row.setdefault('recorder_exit_code', row.get('exit_code'))
            row['exit_code'] = -1
            output = (Path(row['private_output']) if 'private_output' in row else
                      private_output / f"{row['id']}-attempt-{row['attempt']}")
            failure = row.get('failure', {'phase': 'interrupted',
                                         'error': 'Capture interrupted before attempt acceptance'})
            fail_attempt(row, output, failure, retain)
        row.pop('status')
        row.pop('private_output', None)
        row.pop('private_session', None)
    if pending:
        save_manifest(path, manifest)


@contextmanager
def record_attempt(path, manifest, name, number, output, retain=None, *, private_session=None):
    row = dict(id=name, attempt=number, exit_code=None, status='pending',
               private_output=str(output.resolve()))
    if private_session is not None:
        row['private_session'] = str(private_session.resolve())
    manifest['attempts'].append(row)
    save_manifest(path, manifest)
    durable = None
    finalized = False
    try:
        yield row
    except BaseException as error:
        durable = json.loads(path.read_text())
        if any(run['id'] == name for run in durable['runs']):
            row['exit_code'] = 0
            finalized = True
        else:
            manifest['runs'] = durable['runs']
            row['recorder_exit_code'] = row['exit_code']
            if row['exit_code'] in (None, 0):
                row['exit_code'] = -1
            if 'failure' not in row:
                fail_attempt(row, output, {'phase': 'interrupted' if isinstance(error, (KeyboardInterrupt, SystemExit)) else 'capture',
                                          'error': type(error).__name__}, retain)
                finalized = True
            else:
                finalized = not retain or 'path' in row or row.get('trace_status') == 'not-created'
        raise
    else:
        finalized = True
    finally:
        if finalized:
            row.pop('status')
            row.pop('private_output', None)
            row.pop('private_session', None)
        if finalized or durable is not None:
            save_manifest(path, manifest)
