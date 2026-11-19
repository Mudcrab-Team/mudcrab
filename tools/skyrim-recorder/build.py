#!/usr/bin/env python3
"""Build the one-runtime recorder with LLVM, without a Windows SDK or CRT."""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess


def build(exe, output, profile_name='v3'):
    source = Path(__file__).resolve().parent
    profile = json.loads((source / 'runtime.json').read_text())
    if profile_name == 'inputs-v4':
        profile.update(json.loads((source / 'runtime-inputs-v4.json').read_text()))
    data = exe.read_bytes()
    if hashlib.sha256(data).hexdigest() != profile['sha256']:
        raise ValueError('Unsupported Skyrim executable hash')
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    count, optional_size = struct.unpack_from('<H12xH', data, pe + 6)
    sections = pe + 24 + optional_size

    def at(rva, size):
        for i in range(count):
            virtual_size, address, raw_size, offset = struct.unpack_from(
                '<IIII', data, sections + i * 40 + 8)
            if address <= rva and rva + size <= address + raw_size:
                return data[offset + rva - address:offset + rva - address + size]
        raise ValueError(f'Unmapped RVA {rva:x}')

    def checked_signature(name):
        identity = profile[name]
        stem = name.removesuffix('_signature')
        rva = profile[stem if stem.endswith('_call') else stem + '_function']
        local = at(rva, identity['length'])
        if hashlib.sha256(local).hexdigest() != identity['sha256']:
            raise ValueError(f'{stem} local executable range differs')
        return local

    for stem in ('movement', 'jump', 'camera', 'controller_getter', 'controller_gate', 'air', 'launch'):
        checked_signature(stem + '_signature')
    checked_signature('camera_call_signature')
    if profile_name == 'inputs-v4':
        checked_signature('setter_signature')
        call = checked_signature('setter_call_signature')
        player_call = checked_signature('player_setter_call_signature')
        if profile['player_setter_call'] + 5 + struct.unpack_from('<i', player_call, 1)[0] != profile['setter_function']:
            raise ValueError('Player jump setter call mismatch')
        if profile['setter_call'] + 5 + struct.unpack_from('<i', call, 1)[0] != profile['setter_function']:
            raise ValueError('Jump setter call target mismatch')
    for stem in ('movement', 'jump', 'air', 'launch'):
        slot = struct.unpack('<Q', at(profile[stem + '_vtable_slot'], 8))[0]
        if slot != profile['preferred_base'] + profile[stem + '_function']:
            raise ValueError(f'{stem} vtable mismatch')
    output.mkdir(parents=True, exist_ok=True)
    header = []
    for name, value in profile.items():
        if isinstance(value, int):
            header.append(f'constexpr unsigned long long {name} = {value}ULL;')
        elif name.endswith('_signature'):
            values = ','.join(str(v) for v in checked_signature(name))
            header.append(f'constexpr unsigned char {name}[] = {{{values}}};')
    if profile_name == 'inputs-v4':
        header.append('#define MUDCRAB_INPUTS_V4 1')
    (output / 'runtime.generated.h').write_text('\n'.join(header) + '\n')
    imports = ['LoadLibraryW', 'GetProcAddress', 'GetSystemDirectoryW', 'GetModuleHandleW',
               'GetEnvironmentVariableA', 'CreateFileA', 'WriteFile', 'CloseHandle',
               'QueryPerformanceCounter', 'QueryPerformanceFrequency', 'GetCurrentThreadId',
               'GetCurrentProcess', 'ReadProcessMemory', 'VirtualProtect', 'VirtualAlloc',
               'FlushInstructionCache', 'AcquireSRWLockExclusive', 'ReleaseSRWLockExclusive']
    definition = output / 'kernel32.def'
    definition.write_text('LIBRARY KERNEL32.dll\nEXPORTS\n' + '\n'.join(imports) + '\n')
    subprocess.run(['llvm-dlltool', '-m', 'i386:x86-64', '-d', str(definition),
                    '-l', str(output / 'kernel32.lib')], check=True)
    subprocess.run(['clang-cl', '/nologo', '/c', '/O2', '/GS-', '/GR-', '/Zl',
                    '/std:c++20', '/W4', '/WX', '/I' + str(output),
                    '/Fo' + str(output / 'probe.obj'), str(source / 'probe.cpp')], check=True)
    subprocess.run(['lld-link', '/dll', '/noentry', '/nodefaultlib', '/machine:x64',
                    '/export:DirectInput8Create', '/out:' + str(output / 'dinput8.dll'),
                    str(output / 'probe.obj'), str(output / 'kernel32.lib')], check=True)
    receipt = {'profile': profile, 'dll_sha256': hashlib.sha256((output / 'dinput8.dll').read_bytes()).hexdigest(),
               'source_sha256': hashlib.sha256((source / 'probe.cpp').read_bytes()).hexdigest()}
    if profile_name == 'inputs-v4':
        receipt['extension_sha256'] = hashlib.sha256((source / 'jump_inputs.h').read_bytes()).hexdigest()
        receipt['profile_name'] = profile_name
    (output / 'build.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(output / 'dinput8.dll')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--profile', choices=('v3', 'inputs-v4'), default='v3')
    args = parser.parse_args()
    build(args.exe, args.output.resolve(), args.profile)
