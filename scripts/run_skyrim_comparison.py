#!/usr/bin/env python3
"""Run Fiji's isolated headless Skyrim session and capture a verified loaded save."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

ROOT = Path('/home/taylor/Games/MudcrabComparison')
DISPLAY_SERVICE = 'mudcrab-comparison-display.service'
GAMESCOPE_CAPTURE_PATH = Path('/tmp/gamescope.png')


def run(*args, env=None, check=True):
    return subprocess.run(args, env=env, check=check, text=True, capture_output=True, timeout=45)


def display_environment_is_current(values, invocation_id):
    return bool(invocation_id and values.get('INVOCATION_ID') == invocation_id and values.get('DISPLAY'))


def display_socket_exists(display):
    number = display.rsplit(':', 1)[-1].split('.', 1)[0]
    return Path('/tmp/.X11-unix/X' + number).exists()


def latest_completed_save(content):
    events = list(re.finditer(r'\bsave name is\s*([^\r\n]*)', content, re.IGNORECASE))
    if not events:
        return None
    latest = events[-1]
    if not re.search(r'dispatch message \(3\)', content[latest.end():], re.IGNORECASE):
        return None
    return Path(latest.group(1).strip().strip('"')).stem


def _output_text(value):
    if value is None:
        return ''
    if isinstance(value, bytes):
        return value.decode(errors='replace')
    return str(value)


def _command_text(command):
    if isinstance(command, (tuple, list)):
        return [str(part) for part in command]
    return str(command)


def _error_details(error):
    result = {'type': type(error).__name__, 'message': str(error)}
    if isinstance(error, subprocess.CalledProcessError):
        result.update(command=_command_text(error.cmd), returncode=error.returncode,
                      stdout=_output_text(error.stdout), stderr=_output_text(error.stderr))
    elif isinstance(error, subprocess.TimeoutExpired):
        result.update(command=_command_text(error.cmd), timeout_seconds=error.timeout,
                      stdout=_output_text(error.stdout), stderr=_output_text(error.stderr))
    return result


def write_failure_receipt(output, state, error):
    diagnostic_errors = []
    log_path = state.get('log_path')
    if log_path:
        source = Path(log_path)
        if source.is_file():
            try:
                shutil.copyfile(source, output / 'skse64.log')
            except OSError as copy_error:
                diagnostic_errors.append({'operation': 'copy-skse-log', 'error': str(copy_error)})

    screenshots = []
    for image_path in sorted(output.glob('*.png')):
        try:
            data = image_path.read_bytes()
        except OSError as read_error:
            diagnostic_errors.append({'operation': 'hash-screenshot', 'file': image_path.name,
                                      'error': str(read_error)})
            continue
        screenshots.append({'file': image_path.name, 'size_bytes': len(data),
                            'sha256': hashlib.sha256(data).hexdigest()})

    receipt = {
        'kind': 'skyrim-headless-startup-smoke',
        'status': 'failed',
        'phase': state.get('phase', 'initialization'),
        'error': _error_details(error),
        'screenshots': screenshots,
    }
    if diagnostic_errors:
        receipt['diagnostic_errors'] = diagnostic_errors
    for key in ('expected_save', 'display', 'display_invocation_id'):
        if state.get(key) is not None:
            receipt[key] = state[key]

    log_copy = output / 'skse64.log'
    if log_copy.is_file():
        try:
            log_data = log_copy.read_bytes()
        except OSError as read_error:
            receipt.setdefault('diagnostic_errors', []).append(
                {'operation': 'hash-skse-log', 'error': str(read_error)})
        else:
            receipt['skse_log'] = {'file': log_copy.name, 'size_bytes': len(log_data),
                                   'sha256': hashlib.sha256(log_data).hexdigest()}

    destination = output / 'failure.json'
    temporary = output / 'failure.json.tmp'
    temporary.write_text(json.dumps(receipt, indent=2) + '\n')
    os.replace(temporary, destination)
    return receipt


def run_session(output, timeout, state):
    state['phase'] = 'tool-discovery'
    tools = [Path(p) / 'bin' for p in (ROOT / 'artifacts/automation-tools.paths').read_text().splitlines()]

    def executable(name):
        return str(next(p / name for p in tools if (p / name).exists()))

    xdotool, xprop = executable('xdotool'), executable('xprop')
    ocr = Path((ROOT / 'artifacts/ocr-tool.path').read_text().strip()) / 'bin/tesseract'
    if not ocr.exists():
        raise RuntimeError('Pinned OCR tool is missing')

    manifest = json.loads((ROOT / 'session.json').read_text())
    expected = Path(manifest['save']['save_filename']).stem
    state['expected_save'] = expected
    log = ROOT / 'compatdata/pfx/drive_c/users/steamuser/Documents/My Games/Skyrim Special Edition/SKSE/skse64.log'
    state['log_path'] = str(log)

    state['phase'] = 'display-start'
    run('systemctl', '--user', 'start', DISPLAY_SERVICE)
    invocation = run('systemctl', '--user', 'show', '--property=InvocationID', '--value', DISPLAY_SERVICE)
    invocation_id = invocation.stdout.strip()
    if not invocation_id:
        raise RuntimeError('Headless display service has no systemd invocation ID')
    state['display_invocation_id'] = invocation_id

    env = os.environ.copy()
    deadline = time.monotonic() + timeout
    display_file = ROOT / 'artifacts/display.env'
    state['phase'] = 'display-readiness'
    while time.monotonic() < deadline:
        if display_file.exists():
            values = dict(line.split('=', 1) for line in display_file.read_text().splitlines() if '=' in line)
            display = values.get('DISPLAY', '')
            if display_environment_is_current(values, invocation_id) and display_socket_exists(display):
                env.update({key: values[key] for key in ['DISPLAY', 'WAYLAND_DISPLAY'] if values.get(key)})
                state['display'] = display
                break
        time.sleep(1)
    else:
        raise RuntimeError('Headless display failed to publish a current ready environment')

    def capture(path):
        # X11 window imports can return an old swapchain image under Gamescope.
        # Request the current game plane from the compositor instead.
        state['phase'] = 'capture-request'
        source = GAMESCOPE_CAPTURE_PATH
        previous = source.stat().st_mtime_ns if source.exists() else None
        run(xprop, '-root', '-f', 'GAMESCOPECTRL_REQUEST_SCREENSHOT', '32c',
            '-set', 'GAMESCOPECTRL_REQUEST_SCREENSHOT', '1', env=env)
        capture_deadline = time.monotonic() + 10
        while time.monotonic() < capture_deadline:
            if source.exists() and source.stat().st_mtime_ns != previous:
                time.sleep(0.2)
                data = source.read_bytes()
                if data.startswith(b'\x89PNG\r\n\x1a\n') and data[-8:-4] == b'IEND':
                    path.write_bytes(data)
                    return
            time.sleep(0.1)
        raise RuntimeError('Gamescope did not produce a fresh complete PNG')

    state['phase'] = 'steam-start'
    run('systemctl', '--user', 'start', 'mudcrab-comparison-steam.service')
    # Pass the task-owned display to the game without changing the user manager environment.
    state['phase'] = 'game-state-check'
    active = run('systemctl', '--user', 'is-active', 'mudcrab-comparison-game.service', check=False)
    if active.returncode:
        run('systemctl', '--user', 'reset-failed', 'mudcrab-comparison-game.service', check=False)
        state['phase'] = 'game-start'
        run('systemctl', '--user', 'start', 'mudcrab-comparison-game.service')

    confirmed = False
    state['phase'] = 'save-load-wait'
    while time.monotonic() < deadline:
        window = run(xdotool, 'search', '--onlyvisible', '--name', '^Skyrim Special Edition$', env=env, check=False)
        window_ids = window.stdout.split()
        if window.returncode or not window_ids:
            time.sleep(2)
            continue
        wid = window_ids[-1]
        capture(output / 'current.png')
        state['phase'] = 'screen-recognition'
        text = run(str(ocr), str(output / 'current.png'), 'stdout', '--psm', '11').stdout.lower()
        # Only acknowledge the known achievements notice. Missing-content dialogs are never accepted.
        if not confirmed and 'achievements' in text and 'disabled' in text and 'continue loading' in text:
            run(xdotool, 'windowactivate', '--sync', wid, 'key', 'Return', env=env)
            confirmed = True
        if 'enable survival mode' in text:
            run(xdotool, 'windowactivate', '--sync', wid, 'key', 'Right', 'Return', env=env)
            time.sleep(2)
            continue

        content = log.read_text(errors='replace') if log.exists() else ''
        if latest_completed_save(content) != expected:
            time.sleep(3)
            continue
        if any(term in text for term in ['continue loading', 'level 1', 'new save', 'missing content', 'survival mode']):
            time.sleep(3)
            continue

        # Let initial terrain/LOD and mod initialization settle. Recheck the newest completed load afterward.
        state['phase'] = 'scene-settle'
        time.sleep(10)
        settled_content = log.read_text(errors='replace') if log.exists() else ''
        if latest_completed_save(settled_content) != expected:
            continue

        state['phase'] = 'settled-capture'
        capture(output / 'loaded-save.png')
        state['phase'] = 'final-verification'
        final_text = run(str(ocr), str(output / 'loaded-save.png'), 'stdout', '--psm', '11').stdout.lower()
        final_log = log.read_bytes() if log.exists() else b''
        final_content = final_log.decode(errors='replace')
        if latest_completed_save(final_content) != expected:
            continue
        if any(term in final_text for term in ['survival mode', 'continue loading', 'missing content']):
            continue

        (output / 'skse64.log').write_bytes(final_log)
        data = (output / 'loaded-save.png').read_bytes()
        receipt = {
            'kind': 'skyrim-headless-startup-smoke',
            'save': manifest['save'],
            'autoload_log_verified': True,
            'post_load_message_verified': True,
            'latest_completed_save': expected,
            'achievements_notice_acknowledged': confirmed,
            'screenshot': 'loaded-save.png',
            'screenshot_sha256': hashlib.sha256(data).hexdigest(),
            'skse_log_sha256': hashlib.sha256(final_log).hexdigest(),
            'display': env['DISPLAY'],
            'display_invocation_id': invocation_id,
            'capture_backend': 'Gamescope base plane',
            'matched_camera_verified': False,
        }
        (output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
        print(json.dumps({'status': 'loaded-and-captured', 'output': str(output)}), flush=True)
        return

    raise RuntimeError('Save loading timed out; diagnostic screenshots retained')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--timeout', type=float, default=300)
    args = parser.parse_args(argv)
    try:
        args.output.mkdir(parents=True, exist_ok=False)
    except OSError as error:
        print(f'Cannot create fresh output directory {args.output}: {error}', file=sys.stderr)
        return 2

    state = {'phase': 'initialization'}
    try:
        run_session(args.output, args.timeout, state)
        return 0
    except Exception as error:
        try:
            receipt = write_failure_receipt(args.output, state, error)
            print(json.dumps({'status': 'failed', 'failure_receipt': str(args.output / 'failure.json'),
                              'phase': receipt['phase']}), file=sys.stderr)
        except Exception as receipt_error:
            print(f'Could not write failure receipt: {receipt_error}', file=sys.stderr)
        print(f'Skyrim comparison failed during {state.get("phase", "initialization")}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
