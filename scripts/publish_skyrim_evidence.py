#!/usr/bin/env python3
"""Publish explicit derivatives of acquisition evidence and audit nested containers.

The original acquisition hashes remain in receipts. publication.json identifies
both the original bytes and the published derivative. It is an assertion of the
transformation, not a replacement acquisition receipt or proof of original bytes.
Live-source source_transforms describe the one-time historical transformation;
only immutable evidence changes are checked against publication hashes. Live
sources continue to be leak-audited and may evolve.
Reproduce with --sanitize on a private checkout of original acquisition artifacts.
"""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import zipfile

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = 'docs/research/evidence/skyrim-physics-221/publication.json'
HOME = re.compile(rb'/(?:home|Users)/[^/\s"\'`<>]+')
SIGNATURE = re.compile(rb'("[a-z_]+_signature"\s*:\s*)"([0-9a-f]+)"')
STATIC = {'scripts/inspect_skyrim_physics.py', 'scripts/inspect_skyrim_jump_inputs.py'}
DIRECT = (*STATIC, 'tools/skyrim-recorder/build.py', 'tools/skyrim-recorder/runtime.json',
          'tools/skyrim-recorder/runtime-inputs-v4.json')
TREES = ('docs/research/evidence/skyrim-physics-221', 'tools/skyrim-recorder/reference')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def range_identity(raw):
    return {'length': len(raw), 'sha256': sha(raw)}


def public_text(name, data):
    data = HOME.sub(b'<operator-home>', data)
    data = SIGNATURE.sub(lambda m: m[1] + json.dumps(range_identity(bytes.fromhex(m[2].decode()))).encode(), data)
    text = data.decode('utf-8')
    if name.endswith('/light-radius-construction.txt') and 'Disassembly of section' in text:
        text = """Executable SHA256: 846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f
Publication derivative. Copied instruction listings and executable byte strings were removed.
The original artifact SHA256 and derivative SHA256 are recorded in publication.json.
Scope: light-construction radius selection and stores only. The reference helper and downstream photometry remain unresolved.

Recorded structural observations (RVAs and numeric facts, not an instruction listing):
- The bounded radius-selection region starts at RVA 0x27468e.
- The selected scalar is stored in the three construction fields at offsets 0x128, 0x12c, and 0x130.
- The optional helper target is RVA 0x2ffd70. Its semantics remain unresolved.
- Type descriptor RVA 0x20dc1b8 identifies TESObjectLIGH.
- Complete-object locator RVA 0x1b741d0 and vtable RVA 0x18140f0 identify this path.
- Vtable slot 0x4a targets RVA 0x274090.
- The constructor call at RVA 0x274263 targets RVA 0x274390.
- PE function bounds for the vtable entry are [0x274090, 0x2742c9), with unwind RVA 0x1cd4120.

Operators can inspect these bounded regions locally in their own pinned executable:
llvm-objdump -d --start-address=0x14027468e --stop-address=0x1402746fd /path/to/SkyrimSE.exe
llvm-objdump -d --start-address=0x140274250 --stop-address=0x140274268 /path/to/SkyrimSE.exe
Do not publish the resulting instruction listings or executable bytes.
"""
    if name == 'tools/skyrim-recorder/build.py' and 'def checked_signature' not in text:
        marker = "    for stem in ('movement', 'jump', 'camera', 'controller_getter', 'controller_gate', 'air', 'launch'):"
        helper = '''    def checked_signature(name):
        identity = profile[name]
        stem = name.removesuffix('_signature')
        rva = profile[stem if stem.endswith('_call') else stem + '_function']
        local = at(rva, identity['length'])
        if hashlib.sha256(local).hexdigest() != identity['sha256']:
            raise ValueError(f'{stem} local executable range differs')
        return local

'''
        if marker not in text:
            raise ValueError('Unknown archived build shape')
        text = text.replace(marker, helper + marker)
        text = re.sub(r'bytes\.fromhex\(profile\[(.*?)\]\)', r'checked_signature(\1)', text)
        text = text.replace('bytes.fromhex(value)', 'checked_signature(name)')
    if name in STATIC:
        text = re.sub(r"'([0-9a-f]{8,62})'", lambda m: repr(range_identity(bytes.fromhex(m[1]))), text)
        text = text.replace('len(bytes.fromhex(signature))', "signature['length']")
        text = text.replace('len(bytes.fromhex(expected))', "expected['length']")
        text = re.sub(r'at\(rva, (signature\[\x27length\x27\]|expected\[\x27length\x27\])\)\.hex\(\) != (signature|expected)',
                      r"hashlib.sha256(at(rva, \1)).hexdigest() != \2['sha256']", text)
        text = text.replace('actual.hex() != expected', "hashlib.sha256(actual).hexdigest() != expected['sha256']")
    return text.encode()


def transform(name, data, changes):
    if name.endswith('.bin.gz'):
        return data
    original = data
    if name.endswith('.zip'):
        output = io.BytesIO()
        with zipfile.ZipFile(io.BytesIO(data)) as source, zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED) as dest:
            for entry in source.infolist():
                member = transform(entry.filename, source.read(entry), changes.setdefault(name + '!members', {}))
                info = zipfile.ZipInfo(entry.filename, (1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                dest.writestr(info, member)
        candidate = output.getvalue()
        if changes.get(name + '!members'):
            data = candidate
        else:
            changes.pop(name + '!members', None)
    elif name.endswith('.gz'):
        raw = gzip.decompress(data)
        try:
            published = public_text(name[:-3], raw)
        except UnicodeDecodeError:
            published = raw
        if published != raw:
            data = gzip.compress(published, mtime=0)
    else:
        try:
            data = public_text(name, data)
        except UnicodeDecodeError:
            pass
    if data != original:
        changes[name] = {'original_sha256': sha(original), 'published_sha256': sha(data)}
    return data


def artifact_digest(path, root=ROOT):
    """Check the publication identity and return its declared original identity."""
    actual = sha(path.read_bytes())
    try:
        relative = str(path.resolve().relative_to(root.resolve()))
    except ValueError:
        return actual
    manifest_path = root / MANIFEST
    if not manifest_path.exists():
        return actual
    record = json.loads(manifest_path.read_bytes())['changes'].get(relative)
    if record:
        if actual != record['published_sha256']:
            raise ValueError(f'Published artifact changed: {relative}')
        return record['original_sha256']
    return actual


def source_digest(archive_path, member, data, root=ROOT):
    """Validate a derivative before resolving its historical acquisition identity."""
    artifact_digest(archive_path, root)
    try:
        relative = str(archive_path.resolve().relative_to(root.resolve()))
    except ValueError:
        return sha(data)
    manifest_path = root / MANIFEST
    if not manifest_path.exists():
        return sha(data)
    manifest = json.loads(manifest_path.read_bytes())
    record = manifest['changes'].get(relative + '!members', {}).get(member)
    if record:
        if sha(data) != record['published_sha256']:
            raise ValueError(f'Published source changed: {member}')
        return record['original_sha256']
    return sha(data)


def public_value(value):
    """Redact home prefixes in a JSON value without changing numeric observations."""
    if isinstance(value, str):
        return HOME.sub(b'<operator-home>', value.encode()).decode()
    if isinstance(value, list):
        return [public_value(item) for item in value]
    if isinstance(value, dict):
        return {key: public_value(item) for key, item in value.items()}
    return value


def candidates(root):
    files = {root / name for name in DIRECT}
    for name in TREES:
        files.update(p for p in (root / name).rglob('*') if p.is_file())
    return sorted(p for p in files if p.exists() and p != root / MANIFEST and '__pycache__' not in p.parts)


def findings(name, data):
    if name.endswith('.zip'):
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            return [issue for member in archive.namelist()
                    for issue in findings(name + '!' + member, archive.read(member))]
    if name.endswith('.gz'):
        return findings(name[:-3], gzip.decompress(data))
    issues = []
    if HOME.search(data):
        issues.append(name + ': operator home path')
    if SIGNATURE.search(data):
        issues.append(name + ': literal executable signature')
    if re.search(rb'^\s*[0-9a-f]{8,16}:\s+(?:[0-9a-f]{2} ){2,}', data, re.M):
        issues.append(name + ': copied disassembly')
    if name.split('!')[-1] in STATIC and re.search(rb"'[0-9a-f]{8,62}'", data):
        issues.append(name + ': literal static executable range')
    return issues


def publication_record(changes):
    """Separate historical source-edit assertions from immutable evidence hashes."""
    return {'schema': 1, 'transform': 'scripts/publish_skyrim_evidence.py',
            'scope': 'Publication derivatives. Acquisition receipts retain original hashes; raw numeric traces and frozen evaluator/model are unchanged. source_transforms records the historical live-source edit, not a permanent working-tree pin. changes identifies immutable published evidence derivatives.',
            'changes': {name: value for name, value in changes.items() if name not in DIRECT},
            'source_transforms': {name: value for name, value in changes.items() if name in DIRECT}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--sanitize', action='store_true')
    args = parser.parse_args()
    root = args.root.resolve()
    changes = {}
    pending = []
    manifest_path = root / MANIFEST
    prior = json.loads(manifest_path.read_bytes()) if manifest_path.exists() else None
    for path in candidates(root):
        name = str(path.relative_to(root))
        raw = path.read_bytes()
        if args.sanitize:
            published = transform(name, raw, changes)
            if published != raw:
                pending.append((path, published))
    if args.sanitize and changes:
        if prior:
            raise ValueError('Existing publication mapping must not be overwritten; reproduce from original artifacts')
        for path, published in pending:
            path.write_bytes(published)
        manifest_path.write_text(json.dumps(publication_record(changes), indent=2, sort_keys=True) + '\n')
    issues = [issue for path in candidates(root) for issue in findings(str(path.relative_to(root)), path.read_bytes())]
    for path in candidates(root):
        artifact_digest(path, root)
    if issues:
        raise SystemExit('\n'.join(issues))
    print('Publication audit passed, including nested ZIP and gzip payloads.')


if __name__ == '__main__':
    main()
