#!/usr/bin/env python3
"""Collect raw, read-only console evidence from Fiji's running comparison game.

This program never starts a service or changes game settings. Before invoking it,
visually verify that the isolated game's console is open with an empty input line.
OCR is retained as evidence, never interpreted as an effective numeric readback.
"""

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import sys
import time
import zlib

try:
    import fcntl
except ImportError:  # Windows has no flock; msvcrt locks one byte instead.
    fcntl = None
    import msvcrt


ROOT = Path('/home/taylor/Games/MudcrabComparison')
PROC_ROOT = Path('/proc')
DISPLAY_SERVICE = 'mudcrab-comparison-display.service'
GAME_SERVICE = 'mudcrab-comparison-game.service'
EXPECTED_EXE_SHA256 = '846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f'
GAMESCOPE_CAPTURE_PATH = Path('/tmp/gamescope.png')
QUERIES = (
    ('curve-selection', 'getini "bUseFilmicCurve:Display"'),
    ('reinhard-white-scale', 'getini "fReinhardWhiteScale:Display"'),
    ('filmic-white-scale', 'getini "fFilmicWhiteScale:Display"'),
    ('adaptation-speed-scale', 'getini "fGlobalEyeAdaptSpeedScale:Display"'),
    ('adaptation-strength-scale', 'getini "fGlobalEyeAdaptStrengthScale:Display"'),
    ('menu-adaptation-timer', 'getini "fConstHDRAdaptTimerForMenu:Display"'),
    ('gamma-setting', 'getini "fGamma:Display"'),
    ('map-contrast-boost', 'getini "fGlobalMapContrastBoost:Display"'),
    ('map-brightness-boost', 'getini "fGlobalMapBrightnessBoost:Display"'),
    ('selected-base-hdr', 'PrintHDRParam'),
    ('weather-81a-identity', 'GetIsCurrentWeather 81a'),
    ('weather-transition', 'GetCurrentWeatherPercent'),
    ('game-hour', 'show gamehour'),
)
ALLOWED_COMMANDS = frozenset(command for _, command in QUERIES)
PNG_SIGNATURE = b'\x89PNG\r\n\x1a\n'
LIMITATIONS = [
    'Commands sent and raw frames are evidence; numeric values require manual frame review.',
    'Input events completing does not prove that Skyrim accepted or executed a console command.',
    'OCR includes older console history and is never parsed as the current command result.',
    'PrintHDRParam selects override/current base data, not resolved manager data or GPU inputs.',
    'PrintHDRParam Target Luminance prints receiveBloomThreshold followed by selected-base White.',
    'A gamma setting does not establish the shader output exponent or complete display transfer.',
    'Resolved modifiers, adaptation textures/history and selected GPU inputs remain unobserved.',
    'This run does not retroactively establish settings of an older reference screenshot.',
]


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(*command, env=None):
    return subprocess.run(command, env=env, text=True, capture_output=True,
                          check=True, timeout=20)


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    os.replace(temporary, path)


def error_details(error):
    details = {'type': type(error).__name__, 'message': str(error)}
    if isinstance(error, (subprocess.CalledProcessError, subprocess.TimeoutExpired)):
        details['command'] = list(error.cmd)
        for key in ('stdout', 'stderr'):
            value = getattr(error, key, None)
            details[key] = value.decode(errors='replace') if isinstance(value, bytes) else value
        if isinstance(error, subprocess.CalledProcessError):
            details['returncode'] = error.returncode
    return details


def validate_command(command):
    require(command in ALLOWED_COMMANDS, 'Command is outside the fixed read-only allowlist')


def send_query(command, xdotool, env):
    validate_command(command)
    run(xdotool, 'type', '--clearmodifiers', '--delay', '60', '--', command, env=env)
    # Gamescope/Wine can miss xdotool's default short Return pulse at low frame rates.
    # Release the key even when the hold or submission encounters an error.
    try:
        run(xdotool, 'keydown', '--clearmodifiers', 'Return', env=env)
        time.sleep(0.2)
    finally:
        run(xdotool, 'keyup', '--clearmodifiers', 'Return', env=env)


def validate_png(data):
    """Validate chunk framing and CRCs, not merely a PNG-looking suffix."""
    require(data.startswith(PNG_SIGNATURE), 'Gamescope output lacks PNG signature')
    offset, dimensions, saw_idat = len(PNG_SIGNATURE), None, False
    while offset < len(data):
        require(offset + 12 <= len(data), 'Truncated PNG chunk header')
        length = struct.unpack_from('>I', data, offset)[0]
        end = offset + 12 + length
        require(end <= len(data), 'Truncated PNG chunk payload')
        kind = data[offset + 4:offset + 8]
        payload = data[offset + 8:offset + 8 + length]
        expected_crc = struct.unpack_from('>I', data, offset + 8 + length)[0]
        require(zlib.crc32(kind + payload) & 0xffffffff == expected_crc,
                'PNG chunk CRC mismatch')
        if dimensions is None:
            require(kind == b'IHDR' and length == 13, 'PNG must begin with one IHDR')
            dimensions = struct.unpack_from('>II', payload)
            require(all(dimensions), 'PNG dimensions must be positive')
        elif kind == b'IHDR':
            raise RuntimeError('Duplicate PNG IHDR')
        if kind == b'IDAT':
            saw_idat = True
        if kind == b'IEND':
            require(length == 0 and end == len(data) and saw_idat,
                    'PNG IEND must follow image data and end the file')
            return dimensions
        offset = end
    raise RuntimeError('PNG lacks complete IEND')


def capture_is_fresh(current_stat, previous_stat, requested_ns):
    if not stat.S_ISREG(current_stat.st_mode) or current_stat.st_mtime_ns < requested_ns:
        return False
    if previous_stat is None:
        return True
    return ((current_stat.st_ino, current_stat.st_size, current_stat.st_mtime_ns)
            != (previous_stat.st_ino, previous_stat.st_size, previous_stat.st_mtime_ns))


def capture_frame(destination, xprop, env, timeout=10):
    source = GAMESCOPE_CAPTURE_PATH
    previous = source.lstat() if source.exists() else None
    requested_ns = time.time_ns()
    run(xprop, '-root', '-f', 'GAMESCOPECTRL_REQUEST_SCREENSHOT', '32c',
        '-set', 'GAMESCOPECTRL_REQUEST_SCREENSHOT', '1', env=env)
    deadline, last_error = time.monotonic() + timeout, None
    while time.monotonic() < deadline:
        try:
            before = source.lstat()
            if capture_is_fresh(before, previous, requested_ns):
                data = source.read_bytes()
                dimensions = validate_png(data)
                after = source.lstat()
                if (before.st_ino, before.st_mtime_ns, before.st_size) == (
                        after.st_ino, after.st_mtime_ns, after.st_size):
                    destination.write_bytes(data)
                    return {'file': destination.name, 'sha256': sha256(data),
                            'size_bytes': len(data), 'dimensions': list(dimensions),
                            'requested_ns': requested_ns, 'source_mtime_ns': after.st_mtime_ns}
        except (OSError, RuntimeError) as error:
            last_error = str(error)
        time.sleep(0.1)
    raise RuntimeError('No fresh complete Gamescope PNG: ' + (last_error or 'source unchanged'))


def normalized_game_argument(argument):
    value = argument.replace('\\', '/')
    return value[2:] if value[:2].lower() == 'z:' else value


def inspect_game_process(pid, executable):
    """Bind the X11 window to the isolated executable, including Wine's Main comm."""
    require(pid.isdecimal() and int(pid) > 0, 'Game window PID is invalid')
    directory = PROC_ROOT / pid
    command = (directory / 'cmdline').read_bytes().decode(errors='replace').split('\0')
    arguments = [normalized_game_argument(argument) for argument in command if argument]
    command_matches = any(Path(argument) == executable for argument in arguments)
    result = {'pid': int(pid), 'command_line': command, 'isolated_argument_verified': command_matches}
    try:
        maps = (directory / 'maps').read_text()
    except PermissionError as error:
        require(command_matches, 'Cannot bind game process to isolated executable')
        result.update(isolated_mapping_verified=False, maps_unavailable=str(error))
    else:
        paths = [line.split(maxsplit=5)[5] for line in maps.splitlines()
                 if len(line.split(maxsplit=5)) == 6]
        matches = [path for path in paths if path == str(executable)]
        require(matches, 'Game window process does not map the isolated executable')
        result.update(isolated_mapping_verified=True, mapped_executable=str(executable),
                      maps_sha256=sha256(maps.encode()))
    return result


def verify_isolated_files():
    root = ROOT.resolve(strict=True)
    game_directory = (root / 'SkyrimSE').resolve(strict=True)
    require(game_directory.is_relative_to(root), 'Game directory escapes isolated root')
    executable = (game_directory / 'SkyrimSE.exe').resolve(strict=True)
    require(executable.is_relative_to(game_directory), 'Game executable escapes isolated directory')
    digest = sha256(executable.read_bytes())
    require(digest == EXPECTED_EXE_SHA256, 'Game executable differs from pinned retail hash')
    documents = (root / 'compatdata/pfx/drive_c/users/steamuser/Documents').resolve(strict=True)
    require(documents.is_relative_to(root), 'Proton Documents escapes isolated root')
    return executable, {'executable': str(executable), 'executable_sha256': digest,
                        'documents': str(documents)}


def discover_tools():
    directories = [Path(line) / 'bin' for line in
                   (ROOT / 'artifacts/automation-tools.paths').read_text().splitlines() if line]
    result = {}
    for name in ('xdotool', 'xprop'):
        matches = [directory / name for directory in directories
                   if (directory / name).is_file() and os.access(directory / name, os.X_OK)]
        require(matches, f'Pinned {name} is missing')
        result[name] = str(matches[0])
    ocr = Path((ROOT / 'artifacts/ocr-tool.path').read_text().strip()) / 'bin/tesseract'
    require(ocr.is_file() and os.access(ocr, os.X_OK), 'Pinned tesseract is missing')
    result['tesseract'] = str(ocr)
    return result


def verified_display_environment():
    for service in (DISPLAY_SERVICE, GAME_SERVICE):
        run('systemctl', '--user', 'is-active', '--quiet', service)
    invocation = run('systemctl', '--user', 'show', '--property=InvocationID',
                     '--value', DISPLAY_SERVICE).stdout.strip()
    values = dict(line.split('=', 1) for line in
                  (ROOT / 'artifacts/display.env').read_text().splitlines() if '=' in line)
    require(invocation and values.get('INVOCATION_ID') == invocation,
            'Display environment belongs to a different service invocation')
    display = values.get('DISPLAY', '')
    require(re.fullmatch(r':\d+(?:\.\d+)?', display), 'Task-owned local DISPLAY is invalid')
    require(Path('/tmp/.X11-unix/X' + display[1:].split('.')[0]).exists(),
            'Task-owned X11 socket is missing')
    env = os.environ.copy()
    env.update({key: values[key] for key in ('DISPLAY', 'WAYLAND_DISPLAY') if values.get(key)})
    return env, invocation


@contextmanager
def collection_lock():
    with (ROOT / 'artifacts/tonemap-console-capture.lock').open('a') as lock:
        if fcntl is not None:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        else:
            lock.seek(0)
            msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
        try:
            yield
        finally:
            if fcntl is not None:
                fcntl.flock(lock, fcntl.LOCK_UN)
            else:
                lock.seek(0)
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)


def capture_evidence(output, label, tools, env):
    image = output / (label + '.png')
    evidence = capture_frame(image, tools['xprop'], env)
    ocr = run(tools['tesseract'], str(image), 'stdout', '--psm', '11')
    raw = ocr.stdout.encode()
    text_path = output / (label + '.ocr.txt')
    text_path.write_bytes(raw)
    stderr_path = output / (label + '.ocr-stderr.txt')
    stderr_path.write_text(ocr.stderr)
    evidence.update(ocr_file=text_path.name, ocr_sha256=sha256(raw),
                    ocr_stderr_file=stderr_path.name, ocr_stderr_sha256=sha256(ocr.stderr.encode()),
                    captured_at_utc=utc_now(),
                    current_numeric_values_parsed=False)
    # A positive match is grounds to stop; absence is not proof of console state.
    require(not re.search(r'missing content|continue loading|enable survival mode|save game',
                          ocr.stdout, re.IGNORECASE),
            'Possible save/startup dialog in frame; no more commands will be sent')
    return evidence


def collect(output, state):
    state['phase'] = 'isolation-verification'
    executable, state['isolation'] = verify_isolated_files()
    tools = discover_tools()
    state['tools'] = tools
    env, state['display_invocation_id'] = verified_display_environment()
    state['display'] = env['DISPLAY']
    xdotool = tools['xdotool']
    windows = run(xdotool, 'search', '--onlyvisible', '--name', '^Skyrim Special Edition$', env=env)
    ids = windows.stdout.split()
    require(len(ids) == 1 and ids[0].isdecimal(), 'Expected one visible Skyrim game window')
    window = ids[0]
    pid = run(xdotool, 'getwindowpid', window, env=env).stdout.strip()
    state['process'] = inspect_game_process(pid, executable)
    state['window_id'] = window
    run(xdotool, 'windowactivate', '--sync', window, env=env)
    require(run(xdotool, 'getactivewindow', env=env).stdout.strip() == window,
            'Game window does not own keyboard focus')
    state['phase'] = 'initial-console-capture'
    state['initial_console'] = capture_evidence(output, '00-initial-console', tools, env)
    write_json(output / 'progress.json', state)
    for index, (label, command) in enumerate(QUERIES, 1):
        validate_command(command)
        state['phase'] = 'query-' + label
        # Recheck service identity and PID before every input. Never type into a replacement game.
        current_env, invocation = verified_display_environment()
        require(invocation == state['display_invocation_id'] and current_env['DISPLAY'] == env['DISPLAY'],
                'Display service changed during collection')
        require(run(xdotool, 'getwindowpid', window, env=env).stdout.strip() == pid,
                'Game window PID changed during collection')
        inspect_game_process(pid, executable)
        run(xdotool, 'windowactivate', '--sync', window, env=env)
        require(run(xdotool, 'getactivewindow', env=env).stdout.strip() == window,
                'Game window does not own keyboard focus')
        entry = {'label': label, 'command': command, 'sent_at_utc': utc_now(),
                 'command_sent': False, 'game_execution_verified': False,
                 'current_numeric_values_parsed': False}
        state['queries'].append(entry)
        write_json(output / 'progress.json', state)
        send_query(command, xdotool, env)
        entry['command_sent'] = True
        write_json(output / 'progress.json', state)
        time.sleep(0.6)
        entry['evidence'] = capture_evidence(output, f'{index:02d}-{label}', tools, env)
        write_json(output / 'progress.json', state)
    state.update(status='captured-raw-evidence', phase='complete', completed_at_utc=utc_now())
    write_json(output / 'receipt.json', state)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='New directory; existing paths are rejected')
    parser.add_argument('--console-open', action='store_true', required=True,
                        help='Confirm console is visibly open with an empty input line')
    args = parser.parse_args(argv)
    try:
        args.output.mkdir(parents=True, exist_ok=False)
    except OSError as error:
        print(f'Cannot create fresh output directory: {error}', file=sys.stderr)
        return 2
    state = {'kind': 'skyrim-tonemap-console-raw-evidence', 'schema_version': 1,
             'status': 'in-progress', 'phase': 'initialization', 'started_at_utc': utc_now(),
             'script_sha256': sha256(Path(__file__).read_bytes()), 'queries': [],
             'console_open_and_empty_asserted_by_operator': True,
             'selected_base_fields_only': True, 'resolved_manager_values_observed': False,
             'gpu_inputs_observed': False, 'limitations': LIMITATIONS}
    try:
        with collection_lock():
            collect(args.output, state)
    except Exception as error:
        state.update(status='failed', error=error_details(error), failed_at_utc=utc_now())
        state['retained_artifacts'] = []
        for path in sorted(args.output.iterdir()):
            if path.is_file():
                try:
                    state['retained_artifacts'].append(
                        {'file': path.name, 'sha256': sha256(path.read_bytes())})
                except OSError as artifact_error:
                    state.setdefault('diagnostic_errors', []).append(
                        {'file': path.name, 'error': str(artifact_error)})
        write_json(args.output / 'failure.json', state)
        print(json.dumps({'status': 'failed', 'phase': state['phase'],
                          'receipt': str(args.output / 'failure.json')}), file=sys.stderr)
        return 1
    print(json.dumps({'status': state['status'], 'receipt': str(args.output / 'receipt.json')}))
    return 0


if __name__ == '__main__':
    sys.exit(main())
