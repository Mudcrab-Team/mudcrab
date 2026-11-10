#!/usr/bin/env python3
"""Record and verify one native Skyrim W press in an isolated Gamescope session."""

import argparse
import csv
import ctypes as C
import fcntl
import hashlib
import io
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import struct
import subprocess
import sys
import time
import traceback


RECORD = struct.Struct('<4Q4I40f48s')
ROOT = Path(__file__).resolve().parents[1]
SCREENSHOT_PATH = Path('/tmp/gamescope.png')
SCREENSHOT_LOCK_PATH = Path('/tmp/mudcrab-gamescope-screenshot.lock')


def capture(display, destination):
    # Gamescope shares this output path across otherwise isolated displays.
    with SCREENSHOT_LOCK_PATH.open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        source = SCREENSHOT_PATH
        previous = source.stat().st_mtime_ns if source.exists() else None
        subprocess.run(['xprop', '-root', '-f', 'GAMESCOPECTRL_REQUEST_SCREENSHOT', '32c',
                        '-set', 'GAMESCOPECTRL_REQUEST_SCREENSHOT', '1'],
                       env=dict(os.environ, DISPLAY=display), check=True, capture_output=True)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if source.exists() and source.stat().st_mtime_ns != previous:
                data = source.read_bytes()
                if data.startswith(b'\x89PNG\r\n\x1a\n') and data[-8:-4] == b'IEND':
                    destination.write_bytes(data)
                    return
            time.sleep(0.1)
        raise RuntimeError('Gamescope did not produce a complete fresh screenshot')


class GameWindow:
    """Focus only the exact Skyrim window on the task-owned display."""

    def __init__(self, display):
        self.x = C.CDLL('libX11.so.6')
        declarations = {
            'XOpenDisplay': ([C.c_char_p], C.c_void_p),
            'XDefaultRootWindow': ([C.c_void_p], C.c_ulong),
            'XQueryTree': ([C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong),
                            C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)], C.c_int),
            'XFetchName': ([C.c_void_p, C.c_ulong, C.POINTER(C.c_void_p)], C.c_int),
            'XFree': ([C.c_void_p], C.c_int),
            'XSetInputFocus': ([C.c_void_p, C.c_ulong, C.c_int, C.c_ulong], C.c_int),
            'XGetInputFocus': ([C.c_void_p, C.POINTER(C.c_ulong), C.POINTER(C.c_int)], C.c_int),
            'XRaiseWindow': ([C.c_void_p, C.c_ulong], C.c_int),
            'XSync': ([C.c_void_p, C.c_int], C.c_int),
            'XCloseDisplay': ([C.c_void_p], C.c_int),
        }
        for name, (args, result) in declarations.items():
            f = getattr(self.x, name)
            f.argtypes, f.restype = args, result
        self.display = self.x.XOpenDisplay(display.encode())
        if not self.display:
            raise RuntimeError(f'Cannot open isolated X display {display}')
        self.window = None

    def windows(self, parent=None, depth=0):
        parent = parent or self.x.XDefaultRootWindow(self.display)
        result = []
        root, ancestor, children, count = C.c_ulong(), C.c_ulong(), C.POINTER(C.c_ulong)(), C.c_uint()
        if not self.x.XQueryTree(self.display, parent, C.byref(root), C.byref(ancestor),
                                 C.byref(children), C.byref(count)):
            return result
        try:
            for i in range(count.value):
                window = children[i]
                name = C.c_void_p()
                if self.x.XFetchName(self.display, window, C.byref(name)) and name.value:
                    result.append((window, C.string_at(name).decode(errors='replace')))
                    self.x.XFree(name)
                if depth < 2:
                    result.extend(self.windows(window, depth + 1))
        finally:
            if children:
                self.x.XFree(children)
        return result

    def focus(self):
        matches = [w for w, name in self.windows() if name == 'Skyrim Special Edition']
        if len(matches) != 1:
            raise RuntimeError(f'Expected one Skyrim window; found {len(matches)}')
        self.window = matches[0]
        self.x.XRaiseWindow(self.display, self.window)
        self.x.XSetInputFocus(self.display, self.window, 2, 0)
        self.x.XSync(self.display, 0)
        self.check_focus()

    def check_focus(self):
        focused, revert = C.c_ulong(), C.c_int()
        self.x.XGetInputFocus(self.display, C.byref(focused), C.byref(revert))
        if focused.value != self.window:
            raise RuntimeError('The owned Skyrim window lost keyboard focus')

    def close(self):
        if self.display:
            self.x.XCloseDisplay(self.display)
            self.display = None


def sha256(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def prepare_session(game, save, proton, session, profile_name='v3'):
    profile = json.loads((ROOT / 'tools/skyrim-recorder/runtime.json').read_text())
    if sha256(game / 'SkyrimSE.exe') != profile['sha256']:
        raise ValueError('The recorder supports only the exact executable in runtime.json')
    session.mkdir(parents=True, exist_ok=False)
    destination = session / 'game'
    (destination / 'Data').mkdir(parents=True)
    for path in game.iterdir():
        if path.is_file():
            shutil.copy2(path, destination / path.name)
    for path in (game / 'Data').iterdir():
        (destination / 'Data' / path.name).symlink_to(path, target_is_directory=path.is_dir())
    (session / 'compatdata').mkdir()
    subprocess.run([sys.executable, str(ROOT / 'tools/skyrim-recorder/build.py'),
                    '--exe', str(destination / 'SkyrimSE.exe'), '--output', str(session / 'build'),
                    '--profile', profile_name], check=True)
    shutil.copy2(session / 'build/dinput8.dll', destination / 'dinput8.dll')
    flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'libei-1.0'], text=True).split()
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-O2', str(ROOT / 'tools/skyrim-recorder/input.c'),
                    '-o', str(session / 'build/input'), *flags], check=True)
    config = {'schema': 1, 'source_game': str(game), 'source_save': str(save),
              'source_save_sha256': sha256(save), 'proton': str(proton),
              'proton_sha256': sha256(proton), 'exe_sha256': profile['sha256'],
              'input_sha256': sha256(session / 'build/input'),
              'build': json.loads((session / 'build/build.json').read_text())}
    if profile_name != 'v3':
        config['profile_name'] = profile_name
    (session / 'session.json').write_text(json.dumps(config, indent=2) + '\n')


def initialize_prefix(session, config, environment, output):
    process = subprocess.run([config['proton'], 'run', 'wineboot.exe', '-u'], env=environment,
                             capture_output=True, text=True, timeout=120)
    (output / 'prefix-init.log').write_text(process.stdout + process.stderr)
    process.check_returncode()
    documents = session / 'compatdata/pfx/drive_c/users/steamuser/Documents/My Games/Skyrim Special Edition'
    documents.mkdir(parents=True, exist_ok=True)
    if not documents.resolve().is_relative_to(session):
        raise RuntimeError('Wine Documents escapes the isolated session')
    (documents / 'Saves').mkdir(exist_ok=True)
    shutil.copy2(config['source_save'], documents / 'Saves/MudcrabFixture.ess')
    archives = ', '.join('Skyrim - ' + name + '.bsa' for name in
                         ['Misc', 'Shaders', 'Interface', 'Animations', 'Meshes0', 'Meshes1', 'Sounds'])
    textures = ', '.join('Skyrim - ' + name + '.bsa' for name in ['Voices_en0'] + [f'Textures{i}' for i in range(9)])
    (documents / 'Skyrim.ini').write_text(
        f'[General]\nsLanguage=ENGLISH\nsIntroSequence=\n[Archive]\nsResourceArchiveList={archives}\n'
        f'sResourceArchiveList2={textures}\n[SaveGame]\nbAllowScriptedAutosave=0\n')
    (documents / 'SkyrimPrefs.ini').write_text(
        '[Display]\nbFull Screen=0\nbBorderless=1\niSize W=1280\niSize H=720\n'
        'iVSyncPresentInterval=0\niMultiSample=0\niShadowMapResolution=1024\nbUseTAA=0\n'
        '[MAIN]\nbSaveOnPause=0\nbSaveOnTravel=0\nbSaveOnWait=0\nbSaveOnRest=0\n'
        '[Launcher]\nbEnableFileSelection=1\n[Controls]\nbGamePadEnable=0\n')


def recognize(image, ocr_prefix):
    environment = dict(os.environ, LD_LIBRARY_PATH=str(ocr_prefix / 'usr/lib'),
                       TESSDATA_PREFIX=str(ocr_prefix / 'usr/share/tessdata'))
    process = subprocess.run([str(ocr_prefix / 'usr/bin/tesseract'), str(image), 'stdout', '--psm', '11', 'tsv'],
                             env=environment, capture_output=True, text=True, check=True, timeout=15)
    text, words = parse_ocr(process.stdout)
    return text, words, process.stdout


def parse_ocr(tsv):
    # Tesseract emits literal quote characters, not CSV-escaped quoted fields.
    rows = list(csv.DictReader(io.StringIO(tsv), delimiter='\t', quoting=csv.QUOTE_NONE))
    words = [r for r in rows if r.get('text', '').strip()]
    return ' '.join(r['text'] for r in words).lower(), words


def read_fixture(path):
    fixture = json.loads(path.read_text())
    if fixture.get('schema') != 1 or fixture.get('kind') not in ('survey', 'ascent', 'descent', 'flat', 'jump'):
        raise ValueError('Unsupported motion fixture')
    pose = fixture['position']
    if len(pose) != 3 or not all(math.isfinite(v) and abs(v) < 100000 for v in pose):
        raise ValueError('Invalid fixture position')
    if not math.isfinite(fixture['yaw_degrees']) or not 0 <= fixture['yaw_degrees'] < 360:
        raise ValueError('Invalid fixture yaw')
    if not isinstance(fixture['hold_ms'], int) or not 100 <= fixture['hold_ms'] <= 10000:
        raise ValueError('Fixture hold must be 100–10000 ms')
    if fixture.get('modifier') not in (None, 'walk', 'jump'):
        raise ValueError('Unsupported fixture modifier')
    if fixture.get('time_multiplier', 1.0) not in (0.5, 1.0):
        raise ValueError('Unsupported experimental time multiplier')
    for name, allowed in [('jump_height_min', (64.0, 76.0)), ('reference_scale', (1.0, 1.25))]:
        if name in fixture and (fixture['kind'] != 'jump' or fixture[name] not in allowed):
            raise ValueError(f'Unsupported jump-input intervention {name}')
    return fixture


def run_recording(session, output, ocr_prefix, fixture_path=None, frame_cap=60):
    if frame_cap not in (30, 60, 120):
        raise ValueError('Experimental frame cap must be 30, 60, or 120')
    fixture = read_fixture(fixture_path) if fixture_path else None
    config = json.loads((session / 'session.json').read_text())
    if sha256(session / 'game/SkyrimSE.exe') != config['exe_sha256']:
        raise ValueError('Prepared executable changed')
    if sha256(session / 'game/dinput8.dll') != config['build']['dll_sha256']:
        raise ValueError('Prepared recorder changed')
    if sha256(session / 'build/input') != config['input_sha256']:
        raise ValueError('Prepared input sender changed')
    if sha256(Path(config['proton'])) != config['proton_sha256']:
        raise ValueError('Prepared Proton launcher changed')
    if sha256(Path(config['source_save'])) != config['source_save_sha256']:
        raise ValueError('Source save changed since preparation')
    if subprocess.run(['pgrep', '-x', 'steam'], capture_output=True).returncode:
        raise RuntimeError('Steam must be running and signed in before recording')
    output.mkdir(parents=True, exist_ok=False)
    token = hashlib.sha256(str(output).encode()).hexdigest()[:10]
    display_unit, game_unit = f'mudcrab-record-display-{token}', f'mudcrab-record-game-{token}'
    environment_file = output / 'display.json'
    ready_script = output / 'display_ready.py'
    ready_script.write_text('import os,json,time\nfrom pathlib import Path\n'
                           f'Path({str(environment_file)!r}).write_text(json.dumps({{k:os.environ.get(k, "") '
                           'for k in ["DISPLAY","GAMESCOPE_WAYLAND_DISPLAY","LIBEI_SOCKET","INVOCATION_ID"]}))\n'
                           'while True: time.sleep(3600)\n')
    phase = 'display-start'
    owned = []
    keyboard = None

    def start(unit, argv, environment=None, cwd=None):
        command = ['systemd-run', '--user', '--collect', '--unit=' + unit,
                   '--property=TimeoutStopSec=10']
        if environment:
            command += ['--setenv=' + k + '=' + v for k, v in environment.items()]
        if cwd:
            command += ['--working-directory=' + str(cwd)]
        subprocess.run(command + argv, check=True, capture_output=True)
        owned.append(unit)

    try:
        start(display_unit, ['gamescope', '--backend', 'headless', '-W', '1280', '-H', '720',
                             '-w', '1280', '-h', '720', '-r', str(frame_cap), '--keep-alive', '--',
                             sys.executable, str(ready_script)])
        deadline = time.monotonic() + 30
        while not environment_file.exists():
            if time.monotonic() >= deadline:
                raise RuntimeError('Owned display did not start')
            time.sleep(0.2)
        display = json.loads(environment_file.read_text())
        invocation = subprocess.check_output(['systemctl', '--user', 'show', display_unit,
                                             '-p', 'InvocationID', '--value'], text=True).strip()
        if invocation != display['INVOCATION_ID']:
            raise RuntimeError('Display invocation changed')
        socket = Path(os.environ['XDG_RUNTIME_DIR']) / display['LIBEI_SOCKET']
        environment = dict(os.environ, DISPLAY=display['DISPLAY'],
                           WAYLAND_DISPLAY=display['GAMESCOPE_WAYLAND_DISPLAY'],
                           STEAM_COMPAT_DATA_PATH=str(session / 'compatdata'),
                           STEAM_COMPAT_CLIENT_INSTALL_PATH=str(Path(config['source_game']).parents[2]),
                           STEAM_COMPAT_INSTALL_PATH=str(session / 'game'), SteamAppId='489830',
                           SteamGameId='489830', PROTON_LOG='1', PROTON_LOG_DIR=str(output),
                           DXVK_CONFIG=f'dxvk.maxFrameRate = {frame_cap}', WINEDLLOVERRIDES='dinput8=n,b',
                           MUDCRAB_TRACE_FILE='Z:' + str(output / 'trace.bin').replace('/', '\\'))
        phase = 'prefix-initialize'
        initialize_prefix(session, config, environment, output)
        phase = 'game-start'
        # Forward only task-specific launch variables into the owned service.
        game_environment = {k: environment[k] for k in ['DISPLAY', 'WAYLAND_DISPLAY', 'STEAM_COMPAT_DATA_PATH',
                            'STEAM_COMPAT_CLIENT_INSTALL_PATH', 'STEAM_COMPAT_INSTALL_PATH', 'SteamAppId',
                            'SteamGameId', 'PROTON_LOG', 'PROTON_LOG_DIR', 'DXVK_CONFIG',
                            'WINEDLLOVERRIDES', 'MUDCRAB_TRACE_FILE']}
        start(game_unit, [config['proton'], 'run', str(session / 'game/SkyrimSE.exe')],
              game_environment, session / 'game')
        keyboard = GameWindow(display['DISPLAY'])
        phase = 'main-menu'
        deadline = time.monotonic() + 120
        while True:
            if time.monotonic() > deadline:
                raise RuntimeError('No verified main menu')
            if not any(name == 'Skyrim Special Edition' for _, name in keyboard.windows()):
                time.sleep(0.5)
                continue
            capture(display['DISPLAY'], output / 'startup.png')
            text, _, _ = recognize(output / 'startup.png', ocr_prefix)
            if 'continue' in text and 'new' in text:
                break
            time.sleep(0.5)

        def send(*actions):
            keyboard.focus()
            process = subprocess.run([str(session / 'build/input'), str(socket), *actions],
                                     capture_output=True, text=True, timeout=30)
            with (output / 'issued-input.jsonl').open('a') as log:
                log.write(process.stdout)
            if process.returncode:
                raise RuntimeError(f'Input sender failed: {process.stderr}')

        phase = 'fixture-load'
        mapping = dict(zip('qwertyuiop', range(16, 26))) | dict(zip('asdfghjkl', range(30, 39)))
        mapping |= dict(zip('zxcvbnm', range(44, 51))) | {' ': 57}
        def command(text, *, close=True):
            send('41:200', *[f'{mapping[c]}:60' for c in text], '28:200',
                 *(['41:200'] if close else []))
        command('load mudcrabfixture', close=False)
        loaded_at = time.monotonic()
        deadline = loaded_at + 120
        last_view_toggle = 0.0
        declined_survival = False
        phase = 'gameplay-ready'
        while True:
            if time.monotonic() > deadline:
                raise RuntimeError('No stable first-person gameplay before the deadline')
            capture(display['DISPLAY'], output / 'current.png')
            text, words, tsv = recognize(output / 'current.png', ocr_prefix)
            if 'missing content' in text or 'content is no longer present' in text:
                raise RuntimeError('The fixture references missing content')
            if 'enable survival mode' in text:
                if declined_survival:
                    raise RuntimeError('Survival decline did not take effect')
                no = [r for r in words if r['text'].lower() == 'no' and int(r['top']) > 430]
                if len(no) != 1:
                    raise RuntimeError('Cannot uniquely locate No in the verified survival prompt')
                r = no[0]
                x, y = int(r['left']) + int(r['width']) // 2, int(r['top']) + int(r['height']) // 2
                shutil.copy2(output / 'current.png', output / 'survival-prompt.png')
                (output / 'survival-prompt.tsv').write_text(tsv)
                send(f'click:{x}:{y}')
                declined_survival = True
                last_view_toggle = time.monotonic()
                time.sleep(0.5)
                continue
            rows = read_trace(output / 'trace.bin', live=True) if (output / 'trace.bin').exists() else []
            frames = [r for r in rows if r['kind'] == 3 and r['code'] & 1]
            if len(frames) >= 30 and time.monotonic() - loaded_at > 15:
                tail = frames[-30:]
                frequency = rows[0]['sample']
                # QPC is a native clock; establish freshness by observing another sample.
                time.sleep(0.2)
                newer = read_trace(output / 'trace.bin', live=True)
                advanced = newer[-1]['sequence'] > rows[-1]['sequence']
                stable = max(math.dist(r['values'][:3], tail[-1]['values'][:3]) for r in tail) < 1.0
                if (advanced and stable and frames[-1]['state'] == 9 and
                        time.monotonic() - last_view_toggle > 3):
                    send('33:200')
                    last_view_toggle = time.monotonic()
                    continue
                if advanced and stable and all(r['state'] == 0 for r in tail) and frequency > 0:
                    break
            time.sleep(0.3)
        phase = 'one-w-press'
        if fixture:
            phase = 'fixture-position'
            commands = [f'player.setpos {axis} {value:.6f}' for axis, value in zip('xyz', fixture['position'])]
            commands += ['player.setangle x 0', f"player.setangle z {fixture['yaw_degrees']:.6f}"]
            if 'jump_height_min' in fixture:
                commands += [f"setgs fJumpHeightMin {fixture['jump_height_min']:.6f}"]
            if 'reference_scale' in fixture:
                commands += [f"player.setscale {fixture['reference_scale']:.6f}"]
            commands += [f"sgtm {fixture.get('time_multiplier', 1.0)}"]
            (session / 'game/mudcrabsetup.txt').write_text('\n'.join(commands) + '\n')
            (output / 'fixture.json').write_text(json.dumps(fixture, indent=2) + '\n')
            last_sequence = rows[-1]['sequence']
            command('bat mudcrabsetup')
            deadline = time.monotonic() + 30
            while True:
                rows = read_trace(output / 'trace.bin', live=True)
                frames = [r for r in rows if r['kind'] == 3 and r['sequence'] > last_sequence]
                if len(frames) >= 240:
                    tail = frames[-60:]
                    position = tail[-1]['values'][:3]
                    stable = max(math.dist(r['values'][:3], position) for r in tail) < 1.0
                    eye_offsets = [r['values'][14] - r['values'][2] for r in tail]
                    stable = stable and max(eye_offsets) - min(eye_offsets) < 0.02
                    yaw = tail[-1]['values'][5]
                    yaw_error = abs(math.remainder(yaw - math.radians(fixture['yaw_degrees']), 2 * math.pi))
                    if (stable and all(r['state'] == 0 for r in tail) and yaw_error < 0.01 and
                            math.dist(position[:2], fixture['position'][:2]) < 5 and
                            abs(position[2] - fixture['position'][2]) < 512):
                        break
                if time.monotonic() > deadline:
                    raise RuntimeError('Fixture pose did not settle at the requested position/heading')
                time.sleep(0.2)
        phase = 'one-w-press'
        capture(display['DISPLAY'], output / 'before.png')
        keys = '17'
        if fixture and fixture.get('modifier'):
            keys += '+' + {'walk': '42', 'jump': '57'}[fixture['modifier']]
        send(f"{keys}:{fixture['hold_ms'] if fixture else 500}")
        phase = 'native-proof'
        deadline = time.monotonic() + 10
        while True:
            rows = read_trace(output / 'trace.bin', live=True)
            try:
                proof = verify_trace(rows, requested_fps=frame_cap)
                break
            except ValueError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.2)
        capture(display['DISPLAY'], output / 'after.png')
        if sha256(Path(config['source_save'])) != config['source_save_sha256']:
            raise RuntimeError('Original save changed during recording')
        proof.update(session=config, trace_sha256=sha256(output / 'trace.bin'),
                     runner_sha256=sha256(Path(__file__)),
                     declined_survival=declined_survival, display=display,
                     input_backend='Gamescope libei', source_save_unchanged=True)
        documents = session / 'compatdata/pfx/drive_c/users/steamuser/Documents/My Games/Skyrim Special Edition'
        proof['frame_cap_request'] = frame_cap
        proof['dxvk_config'] = environment['DXVK_CONFIG']
        proof['ini'] = {name: {'sha256': sha256(documents / name), 'text': (documents / name).read_text()}
                        for name in ('Skyrim.ini', 'SkyrimPrefs.ini')}
        if fixture:
            proof['fixture'] = fixture
            proof['fixture_sha256'] = sha256(fixture_path)
        (output / 'proof.json').write_text(json.dumps(proof, indent=2) + '\n')
        (output / 'trace.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in rows))
        print(json.dumps(proof, indent=2))
        phase = 'achieved-cadence'
        if proof['status'] != 'passed':
            raise ValueError('Camera cadence did not meet the requested frame cap; retained proof and trace')
    except BaseException as error:
        (output / 'failure.json').write_text(json.dumps({'phase': phase, 'error': str(error),
                                                        'traceback': traceback.format_exc()}, indent=2) + '\n')
        raise
    finally:
        if keyboard:
            keyboard.close()
        try:
            journal = subprocess.run(['journalctl', '--user', '-u', game_unit, '--no-pager'],
                                     capture_output=True, text=True, timeout=10)
            (output / 'game-journal.log').write_text(journal.stdout + journal.stderr)
        except (OSError, subprocess.SubprocessError) as error:
            (output / 'game-journal.log').write_text(str(error) + '\n')
        cleanup_errors = []
        for unit in reversed(owned):
            try:
                subprocess.run(['systemctl', '--user', 'stop', unit], timeout=20, capture_output=True)
            except subprocess.TimeoutExpired:
                try:
                    subprocess.run(['systemctl', '--user', 'kill', '--signal=KILL', unit],
                                   timeout=10, capture_output=True, check=True)
                except (OSError, subprocess.SubprocessError) as error:
                    cleanup_errors.append(f'{unit}: {error}')
        if cleanup_errors:
            (output / 'cleanup-errors.log').write_text('\n'.join(cleanup_errors) + '\n')


def read_trace(path, *, live=False):
    data = path.read_bytes()
    if not live and len(data) % RECORD.size:
        raise ValueError('Truncated native record')
    rows = []
    for offset in range(0, len(data) - RECORD.size + 1, RECORD.size):
        v = RECORD.unpack_from(data, offset)
        rows.append(dict(sequence=v[0], qpc=v[1], sample=v[2], input_sequence=v[3],
                         kind=v[4], thread=v[5], code=v[6], state=v[7], values=list(v[8:48]),
                         text=v[48].split(b'\0', 1)[0].decode('ascii', errors='replace')))
    return rows


def qualify_cadence(frames, frequency, requested_fps, relative_tolerance=0.1):
    if not math.isfinite(requested_fps) or requested_fps <= 0:
        raise ValueError('Requested camera cadence must be positive and finite')
    if not math.isfinite(relative_tolerance) or not 0 <= relative_tolerance < 1:
        raise ValueError('Camera cadence tolerance must be in [0, 1)')
    intervals = [b['qpc'] - a['qpc'] for a, b in zip(frames, frames[1:])]
    if frequency <= 0 or not intervals or min(intervals) <= 0:
        raise ValueError('Insufficient increasing camera timestamps for cadence qualification')
    median_seconds = statistics.median(intervals) / frequency
    achieved_fps = 1 / median_seconds
    passed = abs(achieved_fps - requested_fps) <= requested_fps * relative_tolerance
    return {'status': 'passed' if passed else 'failed', 'requested_fps': requested_fps,
            'achieved_fps': achieved_fps, 'relative_tolerance': relative_tolerance,
            'clock': 'qpc', 'window': 'first_person_camera_during_W',
            'sample_count': len(frames), 'median_interval_seconds': median_seconds}


def verify_trace(rows, *, requested_fps=None):
    def require(condition, message):
        if not condition:
            raise ValueError(message)

    require(rows and rows[0]['kind'] == 1 and rows[0]['text'] in
            ('mudcrab-motion-256-v1', 'mudcrab-motion-256-v2', 'mudcrab-motion-256-v3', 'mudcrab-jump-inputs-256-v4'),
            'Missing native recorder header')
    require(rows[-1]['kind'] == 5, 'Native recording has not completed')
    frequency = rows[0]['sample']
    require(frequency > 0, 'Invalid native clock')
    require([r['sequence'] for r in rows] == list(range(1, len(rows) + 1)), 'Lost native records')
    require(all(b['qpc'] >= a['qpc'] for a, b in zip(rows, rows[1:])), 'Native clock went backwards')
    require(not any(r['kind'] == 4 and r['code'] for r in rows), 'Native recorder reported failure')
    input_sequence = 0
    sample = 0
    for row in rows:
        input_sequence += row['kind'] == 2
        if row['kind'] == 3:
            sample += 1
            require(row['sample'] == sample, 'Lost camera samples')
        require(row['input_sequence'] == input_sequence, 'Broken input/sample association')
    require(rows[-1]['sample'] == sample, 'Incomplete camera footer')
    events = [r for r in rows if r['kind'] == 2 and r['state'] == 0 and
              r['code'] == 17 and r['text'] == 'Forward']
    require(events, 'Skyrim did not deliver W to its movement handler')
    downs = [r for r in events if r['values'][0] > 0 and r['values'][1] == 0]
    ups = [r for r in events if r['values'][0] == 0 and r['values'][1] > 0]
    require(len(downs) == 1 and len(ups) == 1, 'Expected exactly one native W down/up pair')
    down, up = downs[0], ups[0]
    require(down['qpc'] < up['qpc'], 'Native key release precedes press')
    require(any(r['values'][0] > 0 and r['values'][3] > 0 for r in events),
            'Movement handler did not accept forward input')
    frames = [r for r in rows if r['kind'] == 3 and r['code'] & 1 and r['state'] == 0]
    before = [r for r in frames if r['qpc'] < down['qpc']]
    during = [r for r in frames if down['qpc'] <= r['qpc'] <= up['qpc']]
    after = [r for r in frames if r['qpc'] > up['qpc']]
    require(len(before) >= 10 and len(during) >= 5 and len(after) >= 10,
            'Insufficient first-person samples before/during/after W')
    selected = before[-10:] + during + after[:10]
    require(all(r['code'] & 2 for r in selected), 'Attached NiCamera is missing')
    require(all(math.isfinite(v) for r in selected for v in r['values'][:15]), 'Nonfinite position')
    start, end = before[-1]['values'], after[0]['values']
    delta = [end[i] - start[i] for i in range(3)]
    horizontal = math.hypot(delta[0], delta[1])
    require(horizontal > 1.0, 'No horizontal player movement after accepted W')
    yaw = start[5]
    forward = delta[0] * math.sin(yaw) + delta[1] * math.cos(yaw)
    require(forward > horizontal * 0.5, 'Player movement does not follow the starting forward heading')
    camera_delta = [end[i] - start[i] for i in range(9, 12)]
    require(math.hypot(*camera_delta[:2]) > 1.0, 'Camera root did not follow movement')
    eye_delta = [end[i] - start[i] for i in range(12, 15)]
    require(math.hypot(*eye_delta[:2]) > 1.0, 'Attached NiCamera did not follow movement')
    require(max(math.dist(r['values'][:2], start[:2]) for r in before[-10:]) < 1.0,
            'Player was already moving before the press')
    proof = {'status': 'passed', 'received_down_up': True, 'forward_input_accepted': True,
            'input_threads': sorted({r['thread'] for r in events}),
            'camera_threads': sorted({r['thread'] for r in selected}), 'first_person': True,
            'hold_seconds': (up['qpc'] - down['qpc']) / frequency,
            'player_delta_creation_units': delta, 'horizontal_displacement': horizontal,
            'camera_root_delta_creation_units': camera_delta,
            'nicamera_delta_creation_units': eye_delta,
            'samples': {'before': len(before), 'during': len(during), 'after': len(after)},
            'camera_boundary': 'post_PlayerCamera_Update_before_position_submission',
            'render_consumer_verified': False}
    if requested_fps is not None:
        proof['camera_cadence'] = qualify_cadence(during, frequency, requested_fps)
        proof['status'] = proof['camera_cadence']['status']
    return proof


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    prepare = sub.add_parser('prepare')
    prepare.add_argument('--game', type=Path, required=True)
    prepare.add_argument('--save', type=Path, required=True)
    prepare.add_argument('--proton', type=Path, required=True)
    prepare.add_argument('--session', type=Path, required=True)
    prepare.add_argument('--profile', choices=('v3', 'inputs-v4'), default='v3')
    run = sub.add_parser('run')
    run.add_argument('--session', type=Path, required=True)
    run.add_argument('--output', type=Path, required=True)
    run.add_argument('--ocr-prefix', type=Path, required=True)
    run.add_argument('--fixture', type=Path)
    run.add_argument('--frame-cap', type=int, choices=(30, 60, 120), default=60)
    verify = sub.add_parser('verify')
    verify.add_argument('trace', type=Path)
    verify.add_argument('--frame-cap', type=int, choices=(30, 60, 120))
    args = parser.parse_args()
    if args.action == 'prepare':
        prepare_session(args.game.resolve(), args.save.resolve(), args.proton.resolve(), args.session.resolve(), args.profile)
        return
    if args.action == 'run':
        with (args.session / '.recording.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            run_recording(args.session.resolve(), args.output.resolve(), args.ocr_prefix.resolve(),
                          args.fixture.resolve() if args.fixture else None, args.frame_cap)
        return
    if args.action == 'verify':
        proof = verify_trace(read_trace(args.trace), requested_fps=args.frame_cap)
        print(json.dumps(proof, indent=2))
        if proof['status'] != 'passed':
            raise SystemExit(1)


if __name__ == '__main__':
    main()
