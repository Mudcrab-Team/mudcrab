"""Probe the pinned owned Skyrim portal/sphere function locally; no game assets are distributed."""
import argparse
import ctypes
import hashlib
import json
import math
import mmap
import platform
import struct
import subprocess
import tempfile
from pathlib import Path

TARGET_SHA256 = '846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f'
FUNCTION_RVA = 0xFE9860
COSINE_CALL_RVA = 0xFE994B

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('executable', type=Path)
parser.add_argument('output', type=Path)
args = parser.parse_args()
if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'AMD64'):
    raise SystemExit('This instruction probe requires Linux x86-64 and a C compiler.')
exe = args.executable
data = exe.read_bytes()
if hashlib.sha256(data).hexdigest() != TARGET_SHA256:
    raise SystemExit('Executable does not match the pinned SkyrimSE 1.7.104.0 build.')
pe = struct.unpack_from('<I', data, 60)[0]
count = struct.unpack_from('<H', data, pe + 6)[0]
optional = struct.unpack_from('<H', data, pe + 20)[0]
sections = []
for i in range(count):
    p = pe + 24 + optional + 40 * i
    size, rva, rawsize, offset = struct.unpack_from('<IIII', data, p + 8)
    sections.append((data[p:p + 8].rstrip(b'\x00'), rva, rawsize, offset))

def off(rva):
    for _, start, size, offset in sections:
        if start <= rva < start + size:
            return offset + rva - start
    raise ValueError(hex(rva))
_, rva, size, offset = next((s for s in sections if s[0] == b'.pdata'))
end = None
for p in range(offset, offset + size - 11, 12):
    start, stop, _ = struct.unpack_from('<III', data, p)
    if start == FUNCTION_RVA:
        end = stop
        break
assert end
code = bytearray(data[off(FUNCTION_RVA):off(FUNCTION_RVA) + end - FUNCTION_RVA])
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    (root / 'probe.c').write_text('''#include <stdint.h>
#include <math.h>
typedef uint8_t (__attribute__((ms_abi)) *test)(const void*,const float*);
uint8_t probe(void* f,const void* p,const float* s) { return ((test)f)(p,s); }
__attribute__((ms_abi)) float cosine(float x) { return cosf(x); }
''')
    subprocess.run(['cc', '-O2', '-fPIC', '-shared', str(root / 'probe.c'), '-o', str(root / 'probe.so'), '-lm'], check=True)
    library = ctypes.CDLL(str(root / 'probe.so'))
    library.probe.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_float)]
    library.probe.restype = ctypes.c_uint8
    memory = mmap.mmap(-1, 4096, prot=mmap.PROT_READ | mmap.PROT_WRITE | mmap.PROT_EXEC)
    address = ctypes.addressof(ctypes.c_char.from_buffer(memory))
    stub = (len(code) + 15) & ~15
    mask = stub + 16
    instruction = code.index(b'\xf3\x0f\x10\x1d')
    struct.pack_into('<i', code, instruction + 4, mask - instruction - 8)
    call = COSINE_CALL_RVA - FUNCTION_RVA
    assert code[call] == 0xE8
    struct.pack_into('<i', code, call + 1, stub - call - 5)
    memory[:len(code)] = code
    memory[stub:stub + 12] = b'H\xb8' + struct.pack('<Q', ctypes.cast(library.cosine, ctypes.c_void_p).value) + b'\xff\xe0'
    memory[mask:mask + 16] = struct.pack('<IIII', 0x7FFFFFFF, 0, 0, 0)
    portal = ctypes.create_string_buffer(0x50)
    struct.pack_into('<9f', portal, 0x1C, 1, 0, 0, 0, 1, 0, 0, 0, 1)
    struct.pack_into('<2f', portal, 0x48, 10, 5)
    cases = []
    for radius in (0.25, 1.0, 4.0):
        for x in (-15.0, -11.0, -10.0, -9.0, -5.0, 0.0, 5.0, 9.0, 10.0, 11.0, 15.0):
            for y in (-3.0, -1.0, -0.5, 0.0, 0.5, 1.0, 3.0):
                for z in (-7.0, -5.5, -5.0, 0.0, 5.0, 5.5, 7.0):
                    sphere = (ctypes.c_float * 4)(x, y, z, radius)
                    native = bool(library.probe(address, portal, sphere))
                    inset = math.cos(abs(abs(y) - radius) / radius) * radius
                    expected = (
                        abs(x) - radius <= 10
                        and abs(z) - radius <= 5
                        and abs(y) - radius <= 0
                        and abs(x) - inset <= 10
                        and abs(y) - inset <= 5
                    )
                    assert native == expected, (x, y, z, radius, native, expected)
                    cases.append(dict(center=[x, z, -y], radius=radius, intersects=native))
    result = dict(
        executable_sha256=TARGET_SHA256,
        function_rva=hex(FUNCTION_RVA),
        scope='Native function instructions with a host cosf import bridge; no original-game experiment',
        tested_cases=len(cases),
        mismatches=0,
        cases=[c for c in cases
               if c['center'][0] in (0.0, 11.0, 15.0)
               and c['center'][1] in (0.0, 5.5)
               and c['center'][2] in (0.0, -1.0)],
    )
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print('Native portal/sphere probe:', len(cases), 'cases, no mismatches; retained', len(result['cases']), 'boundary cases')
