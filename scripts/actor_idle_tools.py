"""Pinned reuse of the shared diagnostic controls. Importing this starts no process."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(os.environ.get('MUDCRAB_WORKSPACE_ROOT', '/Users/taylor/Projects/mudcrab/.mudcrab-local'))
SHARED = ROOT / 'evidence/geometry-user-test-20261031/watchdog-prep-20261101/tools'
PINS = {
    'geometry_watchdog': '8374e271100cb8843dbda19f7305b448068a693714934a9b669c9c301e50f959',
    'geometry_oracle': 'f8cbf6a4f90b063b68f1698d8ebe568ab405269e40531bb0ce2dbde3d72a9790',
}
QUALIFIED = {
    'materials_process': (
        ROOT / 'evidence/issue471-python-adapter-20261101/shared/object_materials_oracle.py',
        '053dfdce5c4bc9692e0b507e22f0e9fa0861c5d4b001194d6ee6c772a8c35a2a'),
    'lifecycle': (
        ROOT / 'evidence/interior-scene-lighting-20261031-t3-aac3c0de/control-successor-20261101/lifecycle.py',
        'c9a4940aa6a80e5c3b8f92fef5f84e188d18d4be89c71d3a5233981b278fdcbd'),
    'lease_admission': (
        ROOT / 'evidence/geometry-user-test-20261031/admission-successor-20261101/lease_admission.py',
        'fa5eaad301f377bf5e8186eb4f2e3ba7779f72fe95d7999f06212da51231c9ac'),
}


def sha(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def shared(name):
    path, digest = (QUALIFIED[name] if name in QUALIFIED
                    else (SHARED / (name + '.py'), PINS[name]))
    if path.is_symlink() or sha(path) != digest:
        raise ValueError('shared diagnostic source changed: ' + str(path))
    spec = importlib.util.spec_from_file_location('issue471_' + name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def need(ok, message):
    if not ok:
        raise ValueError(message)


def load_json(path, limit=8 * 1024 * 1024):
    path = Path(path)
    need(not path.is_symlink() and path.stat().st_size <= limit, 'JSON path/size cap')

    def pairs(items):
        result = {}
        for key, value in items:
            need(key not in result, 'duplicate JSON key: ' + key)
            result[key] = value
        return result

    def nonfinite(value):
        raise ValueError('nonfinite JSON: ' + value)

    return json.loads(path.read_text(), object_pairs_hook=pairs, parse_constant=nonfinite)
