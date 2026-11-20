#!/usr/bin/env python3
"""Publish explicit derivatives of acquisition evidence and audit nested containers.

The original acquisition hashes remain in receipts. publication.json identifies
both the original bytes and the published derivative. It is an assertion of the
transformation, not a replacement acquisition receipt or proof of original bytes.
Live-source source_transforms describe the one-time historical transformation;
only immutable evidence changes are checked against publication hashes. Live
sources continue to be leak-audited and may evolve.
The audit scans DIRECT, TREES, and the publication manifest below. Other Skyrim
tools and unrelated repository files are outside this acquisition-evidence audit.
Reproduce with --sanitize on a private checkout of original acquisition artifacts.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import struct
import tempfile
import zipfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = 'docs/research/evidence/skyrim-physics-221/publication.json'
HOME = re.compile(rb'(?:(?:[A-Za-z]:)?[/\\]+(?:home|Users)|(?:[A-Za-z]:)?\\+users|[A-Za-z]:/+users)[/\\]+[^/\\\s"\'`<>]+')
SIGNATURE = re.compile(rb'("[a-z_]+_signature"\s*:\s*)"([0-9a-fA-F]+)"')
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
        text = re.sub(r"'([0-9a-fA-F]{8,62})'", lambda m: repr(range_identity(bytes.fromhex(m[1]))), text)
        text = text.replace('len(bytes.fromhex(signature))', "signature['length']")
        text = text.replace('len(bytes.fromhex(expected))', "expected['length']")
        text = re.sub(r'at\(rva, (signature\[\x27length\x27\]|expected\[\x27length\x27\])\)\.hex\(\) != (signature|expected)',
                      r"hashlib.sha256(at(rva, \1)).hexdigest() != \2['sha256']", text)
        text = text.replace('actual.hex() != expected', "hashlib.sha256(actual).hexdigest() != expected['sha256']")
    return text.encode()


def read_gzip(name, data):
    """Return the combined payload and findings in each member's optional header fields."""
    payloads = []
    issues = []
    while data:
        stream = zlib.decompressobj(wbits=31)
        payloads.append(stream.decompress(data))
        if not stream.eof:
            raise ValueError(f'{name}: incomplete gzip member')
        # zlib validated the member. The optional fields follow its ten-byte fixed header.
        flags = data[3]
        offset = 10
        if flags & 4:
            size = int.from_bytes(data[offset:offset + 2], 'little')
            offset += 2
            issues.extend(content_findings(name + '!gzip header extra', data[offset:offset + size]))
            offset += size
        for flag, label in ((8, 'filename'), (16, 'comment')):
            if flags & flag:
                end = data.index(b'\0', offset)
                issues.extend(content_findings(name + '!gzip header ' + label, data[offset:end]))
                offset = end + 1
        data = stream.unused_data.lstrip(b'\0')
    return b''.join(payloads), issues


def zip_extra_findings(name, data, entry):
    # ZipInfo.extra describes only the central directory. The local copy can differ.
    filename_len, extra_len = struct.unpack_from('<HH', data, entry.header_offset + 26)
    start = entry.header_offset + 30 + filename_len
    return (content_findings(name + '!central ZIP extra', entry.extra) +
            content_findings(name + '!local ZIP extra', data[start:start + extra_len]))


def transform(name, data, changes):
    original = data
    if name.lower().endswith('.zip'):
        output = io.BytesIO()
        with zipfile.ZipFile(io.BytesIO(data)) as source, zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED) as dest:
            if len(set(source.namelist())) != len(source.infolist()):
                raise ValueError(f'{name}: duplicate ZIP member name')
            dest.comment = b'' if content_findings(name + '!archive comment', source.comment) else source.comment
            metadata_changed = dest.comment != source.comment
            for entry in source.infolist():
                if unsafe_member(entry.filename):
                    raise ValueError(f'Unsafe ZIP member name: {entry.filename}')
                member = transform(entry.filename, source.read(entry), changes.setdefault(name + '!members', {}))
                info = zipfile.ZipInfo(entry.filename, (1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                if content_findings(entry.filename + '!member comment', entry.comment):
                    metadata_changed = True
                else:
                    info.comment = entry.comment
                if zip_extra_findings(entry.filename, data, entry):
                    metadata_changed = True
                dest.writestr(info, member)
        candidate = output.getvalue()
        if metadata_changed or changes.get(name + '!members'):
            data = candidate
        if not changes.get(name + '!members'):
            changes.pop(name + '!members', None)
    elif name.lower().endswith('.gz'):
        raw, header_issues = read_gzip(name, data)
        if name.lower().endswith('.bin.gz'):
            if header_issues:
                raise ValueError('\n'.join(header_issues))
            return data
        published = transform(name[:-3], raw, {})
        if published != raw or header_issues:
            data = gzip.compress(published, mtime=0)
    else:
        try:
            data = public_text(name, data)
        except UnicodeDecodeError:
            pass
    if data != original:
        changes[name] = {'original_sha256': sha(original), 'published_sha256': sha(data)}
    return data


def publication_identity(path, root):
    actual = sha(path.read_bytes())
    manifest_path = root / MANIFEST
    if manifest_path.is_file():
        changes = json.loads(manifest_path.read_bytes())['changes']
        # Prefer the declared name. External aliases of retained evidence can still use its receipt.
        for candidate, base in ((path.absolute(), root.absolute()), (path.resolve(), root.resolve())):
            if candidate.is_relative_to(base):
                name = candidate.relative_to(base).as_posix()
                if name in changes:
                    record = changes[name]
                    if actual != record['published_sha256']:
                        raise ValueError(f'Published artifact changed: {name}')
                    return record['original_sha256'], changes.get(name + '!members', {})
    return actual, {}


def artifact_digest(path, root=ROOT):
    """Check the publication identity and return its declared original identity."""
    digest, _ = publication_identity(path, root)
    return digest


def source_digest(archive_path, member, data, root=ROOT):
    """Validate a derivative before resolving its historical acquisition identity."""
    _, members = publication_identity(archive_path, root)
    record = members.get(member)
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


def evidence_files(directory):
    for path in directory.iterdir():
        mode = path.stat().st_mode
        if stat.S_ISREG(mode):
            yield path
        elif stat.S_ISDIR(mode) and not path.is_symlink():
            yield from evidence_files(path)
        else:
            raise ValueError(f'Cannot audit evidence entry: {path}')


def candidates(root):
    files = set()
    for name in DIRECT + TREES:
        path = root / name
        try:
            path.lstat()
        except FileNotFoundError:
            continue
        if name in TREES:
            files.update(evidence_files(path))
        else:
            files.add(path)
    paths = sorted(files - {root / MANIFEST})
    for path in paths:
        name = path.relative_to(root).as_posix()
        issues = content_findings(name, os.fsencode('/' + name))
        if issues:
            raise SystemExit('Rename the evidence file before publication: ' + ', '.join(issues))
    return paths


def unsafe_member(name):
    normalized = name.replace('\\', '/')
    return (HOME.search(os.fsencode('/' + name)) or PurePosixPath(normalized).is_absolute() or
            '..' in PurePosixPath(normalized).parts or re.match(r'^[A-Za-z]:', normalized))


def findings(name, data):
    if name.lower().endswith('.zip'):
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            issues = content_findings(name + '!archive comment', archive.comment)
            for entry in archive.infolist():
                member = name + '!' + entry.filename
                if unsafe_member(entry.filename):
                    issues.append(f'{name}: unsafe ZIP member name {entry.filename}')
                issues.extend(content_findings(member + '!member comment', entry.comment))
                issues.extend(zip_extra_findings(member, data, entry))
                issues.extend(findings(member, archive.read(entry)))
            if len(set(archive.namelist())) != len(archive.infolist()):
                issues.append(f'{name}: duplicate ZIP member name')
            return issues
    if name.lower().endswith('.gz'):
        raw, header_issues = read_gzip(name, data)
        return header_issues + findings(name[:-3], raw)
    return content_findings(name, data)


def content_findings(name, data):
    issues = []
    if HOME.search(data):
        issues.append(name + ': operator home path')
    if SIGNATURE.search(data):
        issues.append(name + ': literal executable signature')
    if re.search(rb'^\s*[0-9a-fA-F]{8,16}:\s+(?:[0-9a-fA-F]{2} ){2,}', data, re.M):
        issues.append(name + ': copied disassembly')
    if name.split('!')[-1] in STATIC and re.search(rb"'[0-9a-fA-F]{8,62}'", data):
        issues.append(name + ': literal static executable range')
    return issues


def publication_record(changes):
    """Separate historical source-edit assertions from immutable evidence hashes."""
    return {'schema': 1, 'transform': 'scripts/publish_skyrim_evidence.py',
            'scope': 'Publication derivatives. Acquisition receipts retain original hashes; raw numeric traces and frozen evaluator/model are unchanged. source_transforms records the historical live-source edit, not a permanent working-tree pin. changes identifies immutable published evidence derivatives.',
            'changes': {name: value for name, value in changes.items() if name not in DIRECT},
            'source_transforms': {name: value for name, value in changes.items() if name in DIRECT}}


def write_publication_file(path, data, *, create=False):
    # Install complete files. The first publisher owns the immutable acquisition mapping.
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(data)
            if not create:
                os.fchmod(output.fileno(), stat.S_IMODE(path.stat().st_mode))
            output.flush()
            os.fsync(output.fileno())
            if create:
                os.link(temporary, path)
            else:
                os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def checked_record(data):
    issues = findings(MANIFEST, data)
    if issues:
        raise SystemExit('\n'.join(issues))
    return json.loads(data)


def check_recorded_hashes(records, read, prefix=''):
    for name, identity in records.items():
        if name.endswith('!members'):
            if name.removesuffix('!members') not in records:
                raise ValueError(f'Member hashes lack an archive identity: {prefix}{name}')
            continue
        try:
            data = read(name)
        except (FileNotFoundError, KeyError) as error:
            raise ValueError(f'Recorded publication artifact missing: {prefix}{name}') from error
        if sha(data) != identity['published_sha256']:
            raise ValueError(f'Published artifact changed: {prefix}{name}')
        if name + '!members' in records:
            with zipfile.ZipFile(io.BytesIO(data)) as archive:
                check_recorded_hashes(records[name + '!members'], archive.read, prefix + name + '!')


def check_recorded_files(root, recorded, pending):
    def read(name):
        return pending[name] if name in pending else (root / name).read_bytes()

    check_recorded_hashes(recorded['changes'], read)


def link_findings(path):
    if path.is_symlink():
        return content_findings(f'{path.name} link target', os.fsencode('/' + os.readlink(path)))
    return []


def sanitize(root, paths, manifest):
    changes = {}
    pending = {}
    for path in paths:
        name = path.relative_to(root).as_posix()
        raw = path.read_bytes()
        published = transform(name, raw, changes)
        issues = findings(name, published)
        if issues:
            raise SystemExit('\n'.join(issues))
        if published != raw or link_findings(path):
            pending[name] = published
    proposed = publication_record(changes)
    for path in [manifest, *(root / name for name in pending)]:
        if not path.parent.resolve().is_relative_to(root):
            raise ValueError(f'Publication destination outside root: {path}')
    data = (json.dumps(proposed, indent=2, sort_keys=True) + '\n').encode()
    checked_record(data)
    if not manifest.exists():
        manifest.parent.mkdir(parents=True, exist_ok=True)
        try:
            write_publication_file(manifest, data, create=True)
        except FileExistsError:
            pass
    # On retry only the still-original files need transforming. Their identities
    # (including nested archive members) must match the original transaction.
    recorded = checked_record(manifest.read_bytes())
    check_recorded_files(root, recorded, pending)
    for section in ('changes', 'source_transforms'):
        for name, identity in proposed[section].items():
            if recorded.get(section, {}).get(name) != identity:
                raise ValueError(f'Existing publication mapping differs for {name}')
    for name, data in pending.items():
        path = root / name
        actual = sha(path.read_bytes())
        if actual == sha(data):
            if link_findings(path):
                write_publication_file(path, data)
            continue
        if name not in changes or actual != changes[name]['original_sha256']:
            raise ValueError(f'Acquisition changed during publication: {name}')
        write_publication_file(path, data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--sanitize', action='store_true')
    args = parser.parse_args()
    root = args.root.resolve()
    paths = candidates(root)
    manifest_path = root / MANIFEST
    manifest_issues = link_findings(manifest_path)
    if manifest_issues:
        raise SystemExit('\n'.join(manifest_issues))
    if args.sanitize:
        sanitize(root, paths, manifest_path)
    recorded = checked_record(manifest_path.read_bytes())
    check_recorded_files(root, recorded, {})
    issues = []
    for path in paths:
        issues.extend(link_findings(path))
        issues.extend(findings(path.relative_to(root).as_posix(), path.read_bytes()))
        artifact_digest(path, root)
    if issues:
        raise SystemExit('\n'.join(issues))
    print('Publication audit passed, including nested ZIP and gzip payloads.')


if __name__ == '__main__':
    main()
