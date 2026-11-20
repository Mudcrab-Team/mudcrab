#!/usr/bin/env python3
"""Check publication derivatives without commercial executable content."""
import gzip
import importlib.util
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import publish_skyrim_evidence as publication


def zipped(members):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w') as archive:
        for name, data in members.items():
            archive.writestr(name, data)
    return output.getvalue()


class PublicationTests(unittest.TestCase):
    def test_publication_manifest_is_scanned_for_operator_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps(dict(changes={}, scope='/home/alice/capture')))
            with patch('sys.argv', ['audit', '--root', str(root)]):
                with self.assertRaisesRegex(SystemExit, 'operator home path'):
                    publication.main()

    def test_nested_zip_member_names_are_audited_and_rejected_before_transform(self):
        raw = zipped({'nested.zip': zipped({'/home/alice/capture.txt': b'1'})})
        self.assertTrue(publication.findings('capture.zip', raw))
        with self.assertRaisesRegex(ValueError, 'member name'):
            publication.transform('capture.zip', raw, {})

    def test_nested_zip_comment_only_changes_are_audited_and_recorded(self):
        for location in ('archive', 'entry'):
            with self.subTest(location=location):
                buf = io.BytesIO()
                with zipfile.ZipFile(buf, 'w') as archive:
                    entry = zipfile.ZipInfo('capture.log')
                    archive.comment = b'/home/alice/private' if location == 'archive' else b'public'
                    entry.comment = b'/Users/alice/private' if location == 'entry' else b'public'
                    archive.writestr(entry, b'unchanged')
                original = zipped({'inner.zip': buf.getvalue()})
                self.assertTrue(publication.findings('outer.zip', original))
                changes = {}
                public = publication.transform('outer.zip', original, changes)
                self.assertEqual(changes['outer.zip']['published_sha256'], publication.sha(public))
                self.assertFalse(publication.findings('outer.zip', public))
                with zipfile.ZipFile(io.BytesIO(public)) as outer:
                    with zipfile.ZipFile(io.BytesIO(outer.read('inner.zip'))) as inner:
                        self.assertEqual(inner.read('capture.log'), b'unchanged')
                        self.assertEqual(inner.comment, b'' if location == 'archive' else b'public')
                        self.assertEqual(inner.getinfo('capture.log').comment, b'' if location == 'entry' else b'public')
                self.assertEqual(publication.transform('outer.zip', public, {}), public)

    def test_local_and_central_zip_metadata_are_checked_independently(self):
        private = b'/home/alice/capture'
        clean = b'X' * len(private)
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, 'w') as archive:
            entry = zipfile.ZipInfo('log')
            entry.extra = struct.pack('<HH', 0xcafe, len(clean)) + clean
            archive.writestr(entry, b'public')
        for offset, location in ((buf.getvalue().index(clean), 'local'),
                                 (buf.getvalue().rindex(clean), 'central')):
            with self.subTest(location=location):
                raw = bytearray(buf.getvalue())
                raw[offset:offset + len(clean)] = private
                outer = zipped({'inner.zip': bytes(raw)})
                self.assertIn(f'outer.zip!inner.zip!log!{location} ZIP extra: operator home path',
                              publication.findings('outer.zip', outer))
                changes = {}
                derivative = publication.transform('outer.zip', outer, changes)
                self.assertFalse(publication.findings('outer.zip', derivative))
                self.assertEqual(changes['outer.zip']['published_sha256'], publication.sha(derivative))

    def test_present_evidence_trees_must_be_completely_readable_directories(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree = root / publication.TREES[0]
            tree.parent.mkdir(parents=True)
            tree.write_text('wrong shape')
            with self.assertRaises(NotADirectoryError):
                publication.candidates(root)
            tree.unlink()
            hidden = tree / '__pycache__'
            hidden.mkdir(parents=True)
            iterdir = Path.iterdir

            def read_dir(path):
                if path == hidden:
                    raise PermissionError(str(path))
                return iterdir(path)

            with patch.object(Path, 'iterdir', read_dir), self.assertRaises(PermissionError):
                publication.candidates(root)

    def test_unlisted_payload_under_pycache_is_audited(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({'changes': {}}))
            cache = manifest.parent / '__pycache__'
            cache.mkdir()
            (cache / 'capture.log').write_text('/home/alice/capture')
            with patch('sys.argv', ['audit', '--root', str(root)]):
                with self.assertRaisesRegex(SystemExit, 'operator home path'):
                    publication.main()

    def test_both_modes_reject_private_names_even_with_clean_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            manifest.write_text('{"changes": {}}')
            private = manifest.parent / 'home/operator/run.log'
            private.parent.mkdir(parents=True)
            private.write_text('public')
            for argv in (['audit', '--root', str(root)], ['audit', '--root', str(root), '--sanitize']):
                with self.subTest(argv=argv), patch('sys.argv', argv):
                    with self.assertRaisesRegex(SystemExit, 'Rename the evidence file'):
                        publication.main()

    def test_clean_payload_with_private_link_text_is_materialized(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'checkout'
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            manifest.write_text('{"changes": {}}')
            external = Path(directory) / 'home/operator/original.log'
            external.parent.mkdir(parents=True)
            external.write_bytes(b'public data')
            link = manifest.parent / 'capture.log'
            link.symlink_to(external)
            argv = ['audit', '--root', str(root)]
            with patch('sys.argv', argv), self.assertRaisesRegex(SystemExit, 'link target: operator home path'):
                publication.main()
            with patch('sys.argv', argv + ['--sanitize']):
                publication.main()
            self.assertFalse(link.is_symlink())
            self.assertEqual(link.read_bytes(), b'public data')
            self.assertEqual(external.read_bytes(), b'public data')

    def test_wine_home_prefixes_in_logs_and_serialized_paths(self):
        name = 'failure.json'
        wine = r'Z:\home\operator\trace.bin'
        self.assertEqual(publication.public_value(wine), r'<operator-home>\trace.bin')
        raw = json.dumps({'trace': wine}).encode()
        self.assertTrue(publication.findings(name, raw))
        redacted = publication.public_text(name, raw)
        self.assertEqual(json.loads(redacted), {'trace': r'<operator-home>\trace.bin'})
        self.assertFalse(publication.findings(name, redacted))
        clean = rb'Z:\games\public\trace.bin'
        self.assertEqual(publication.public_text('recorder.log', clean), clean)
        prefix = b'compatdata/pfx/drive_c/users/steamuser/Documents'
        self.assertFalse(publication.findings('recorder.py', prefix))

    def test_rejected_payload_or_proposed_mapping_preserves_all_originals(self):
        for finding in ('frozen trace', 'manifest path'):
            with self.subTest(finding=finding), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                manifest = root / publication.MANIFEST
                manifest.parent.mkdir(parents=True)
                log = manifest.parent / '0.log'
                log.write_bytes(b'/home/user/log')
                if finding == 'frozen trace':
                    private = manifest.parent / 'trace.bin.gz'
                    private.write_bytes(gzip.compress(b'/home/user/frozen'))
                else:
                    private = manifest.parent / 'home/user/run.log'
                    private.parent.mkdir(parents=True)
                    private.write_bytes(b'/home/user/record')
                before = private.read_bytes()
                with patch('sys.argv', ['audit', '--root', str(root), '--sanitize']):
                    with self.assertRaisesRegex(SystemExit, 'operator home path'):
                        publication.main()
                self.assertEqual(log.read_bytes(), b'/home/user/log')
                self.assertEqual(private.read_bytes(), before)
                self.assertFalse(manifest.exists())

    def test_sanitizing_a_link_publishes_an_independent_local_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'checkout'
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            external = Path(directory) / 'original.log'
            external.write_bytes(b'/home/operator/data')
            published = manifest.parent / 'capture.log'
            published.symlink_to(external)
            with patch('sys.argv', ['audit', '--root', str(root), '--sanitize']):
                publication.main()
            self.assertFalse(published.is_symlink())
            self.assertEqual(published.read_bytes(), b'<operator-home>/data')
            self.assertEqual(external.read_bytes(), b'/home/operator/data')
            self.assertEqual(publication.artifact_digest(published, root), publication.sha(external.read_bytes()))

    def test_initial_sanitization_and_retry_preserve_acquisition_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            originals = {manifest.parent / f'{index}.txt': f'/home/user/{index}'.encode() for index in range(2)}
            for path, data in originals.items():
                path.write_bytes(data)
            argv = ['audit', '--root', str(root)]
            with patch('sys.argv', argv), self.assertRaises(FileNotFoundError):
                publication.main()
            write_file = publication.write_publication_file

            def interrupted(path, data, **kwargs):
                if path.name == '1.txt':
                    raise KeyboardInterrupt()
                write_file(path, data, **kwargs)

            with patch('sys.argv', argv + ['--sanitize']), patch.object(publication, 'write_publication_file', interrupted):
                with self.assertRaises(KeyboardInterrupt):
                    publication.main()
            self.assertEqual((manifest.parent / '0.txt').read_bytes(), b'<operator-home>/0')
            self.assertEqual((manifest.parent / '1.txt').read_bytes(), b'/home/user/1')
            first_mapping = manifest.read_bytes()
            with patch('sys.argv', argv), self.assertRaisesRegex(ValueError, 'artifact changed'):
                publication.main()
            with patch('sys.argv', argv + ['--sanitize']):
                publication.main()
            self.assertEqual(manifest.read_bytes(), first_mapping)
            with patch('sys.argv', argv):
                publication.main()
            for path, original in originals.items():
                self.assertEqual(publication.artifact_digest(path, root), publication.sha(original))
                self.assertNotEqual(path.read_bytes(), original)

    def test_duplicate_zip_entries_cannot_hide_the_earlier_payload(self):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, 'w') as archive:
            archive.writestr('capture.log', b'/home/alice/private')
            with self.assertWarns(UserWarning):
                archive.writestr('capture.log', b'public')
        issues = publication.findings('capture.zip', buf.getvalue())
        self.assertIn('capture.zip!capture.log: operator home path', issues)
        with self.assertRaisesRegex(ValueError, 'duplicate ZIP member'):
            publication.transform('capture.zip', buf.getvalue(), {})

    def test_mapped_symlink_uses_its_declared_publication_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'public'
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            archive = manifest.parent / 'capture.zip'
            external = Path(directory) / 'outside.zip'
            external.write_bytes(zipped({'log': b'public'}))
            archive.symlink_to(external)
            name = archive.relative_to(root).as_posix()
            changes = {name: dict(original_sha256='a' * 64, published_sha256=publication.sha(external.read_bytes()))}
            manifest.write_text(json.dumps(dict(changes=changes)))
            self.assertEqual(publication.artifact_digest(external, root), publication.sha(external.read_bytes()))
            with patch('sys.argv', ['audit', '--root', str(root)]):
                publication.main()
            self.assertEqual(publication.artifact_digest(archive, root), 'a' * 64)
            external.write_bytes(zipped({'log': b'other public data'}))
            with patch('sys.argv', ['audit', '--root', str(root)]), self.assertRaisesRegex(ValueError, 'artifact changed'):
                publication.main()

    def test_gzip_metadata_in_later_members_is_audited_inside_zip(self):
        for flag in (4, 8, 16):
            with self.subTest(flag=flag):
                member = bytearray(gzip.compress(b'payload', mtime=0))
                member[3] |= flag
                field = b'/home/alice/private'
                member[10:10] = (len(field).to_bytes(2, 'little') + field if flag == 4 else field + b'\0')
                raw = gzip.compress(b'first', mtime=0) + member
                archive = zipped({'build.log.gz': raw})
                self.assertTrue(publication.findings('outer.zip', archive))
                changes = {}
                public = publication.transform('outer.zip', archive, changes)
                self.assertFalse(publication.findings('outer.zip', public))
                self.assertEqual(changes['outer.zip']['published_sha256'], publication.sha(public))
                with zipfile.ZipFile(io.BytesIO(public)) as result:
                    self.assertEqual(gzip.decompress(result.read('build.log.gz')), b'firstpayload')
                with self.assertRaisesRegex(ValueError, 'gzip header'):
                    publication.transform('trace.bin.gz', raw, {})

    def test_missing_recorded_immutable_evidence_fails_the_audit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            changes = {'tools/skyrim-recorder/reference/missing.zip':
                       dict(original_sha256='a' * 64, published_sha256='b' * 64)}
            manifest.write_text(json.dumps(dict(changes=changes)))
            with patch('sys.argv', ['audit', '--root', str(root)]):
                with self.assertRaisesRegex(ValueError, 'missing'):
                    publication.main()

    def test_nested_archives_remove_paths_and_signatures_and_keep_trace_bytes(self):
        trace = gzip.compress(bytes(range(256)), mtime=12345)
        original = zipped({'inner.ZIP': zipped({
            'runtime.json': b'{"camera_signature": "1122334455", "path": "/home/person/game"}',
            'build.log.GZ': gzip.compress(b'/Users/person/build\n'),
            'trace.BIN.GZ': trace,
        })})
        self.assertTrue(publication.findings('outer.ZiP', original))
        changes = {}
        published = publication.transform('outer.ZiP', original, changes)
        self.assertFalse(publication.findings('outer.ZiP', published))
        self.assertEqual(changes['outer.ZiP']['original_sha256'], publication.sha(original))
        with zipfile.ZipFile(io.BytesIO(published)) as outer:
            with zipfile.ZipFile(io.BytesIO(outer.read('inner.ZIP'))) as inner:
                profile = json.loads(inner.read('runtime.json'))
                self.assertEqual(profile['camera_signature'], publication.range_identity(bytes(range(0x11, 0x56, 0x11))))
                self.assertEqual(inner.read('trace.BIN.GZ'), trace)
        second_changes = {}
        self.assertEqual(publication.transform('outer.ZiP', published, second_changes), published)
        self.assertEqual(second_changes, {})

    def test_utf8_numeric_trace_is_never_rewritten_even_when_audit_reports_path(self):
        original = gzip.compress(b'UTF8-valid numeric trace payload /home/operator/observation')
        changes = {}
        self.assertEqual(publication.transform('trace.BIN.GZ', original, changes), original)
        self.assertEqual(changes, {})
        self.assertTrue(publication.findings('trace.BIN.GZ', original))
        archive = zipped({'trace.BIN.GZ': original})
        self.assertEqual(publication.transform('capture.zip', archive, changes), archive)
        self.assertEqual(changes, {})

    def test_historical_identity_requires_exact_published_archive_and_member(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            name = 'tools/skyrim-recorder/runtime.json'
            original = zipped({name: b'{"camera_signature":"1122334455"}'})
            archive = root / 'evidence.zip'
            changes = {}
            archive.write_bytes(publication.transform('evidence.zip', original, changes))
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({'changes': changes}))
            with zipfile.ZipFile(archive) as published:
                member = published.read(name)
            self.assertEqual(publication.source_digest(archive, name, member, root),
                             publication.sha(b'{"camera_signature":"1122334455"}'))
            with self.assertRaisesRegex(ValueError, 'Published source changed'):
                publication.source_digest(archive, name, member + b' ', root)
            archive.write_bytes(archive.read_bytes() + b' ')
            with self.assertRaisesRegex(ValueError, 'Published artifact changed'):
                publication.source_digest(archive, name, member, root)

    def test_cli_checks_nested_member_hashes_in_verified_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            path = manifest.parent / 'outer.zip'
            name = path.relative_to(root).as_posix()
            original = zipped({'inner.zip': zipped({'capture.log': b'/home/operator/data'})})
            changes = {}
            path.write_bytes(publication.transform(name, original, changes))
            nested = changes[name + '!members']['inner.zip!members']['capture.log']
            nested['original_sha256'] = '0' * 64
            manifest.write_text(json.dumps(publication.publication_record(changes)))
            with patch('sys.argv', ['audit', '--root', str(root)]):
                publication.main()
                nested['published_sha256'] = '0' * 64
                manifest.write_text(json.dumps(publication.publication_record(changes)))
                with self.assertRaisesRegex(ValueError, r'Published artifact changed: .*outer\.zip!inner\.zip!capture\.log'):
                    publication.main()

    def test_live_sources_can_evolve_but_remain_leak_audited(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            name = 'tools/skyrim-recorder/runtime.json'
            original = b'{"camera_signature":"1122334455"}'
            changes = {}
            transformed = publication.transform(name, original, changes)
            record = publication.publication_record(changes)
            self.assertNotIn(name, record['changes'])
            self.assertIn(name, record['source_transforms'])
            manifest = root / publication.MANIFEST
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps(record))
            live = root / name
            live.parent.mkdir(parents=True)
            evolved = transformed + b'\n'
            live.write_bytes(evolved)
            self.assertEqual(publication.artifact_digest(live, root), publication.sha(evolved))
            with patch('sys.argv', ['audit', '--root', str(root)]):
                publication.main()
                live.write_bytes(original)
                with self.assertRaisesRegex(SystemExit, 'literal executable signature'):
                    publication.main()

    def test_runtime_signature_spellings_describe_the_same_bytes(self):
        name = 'tools/skyrim-recorder/runtime.json'
        identity = publication.range_identity(bytes.fromhex('abcdefff'))
        for spelling in ('abcdefff', 'ABCDEFFF', 'aBCdEfFf'):
            with self.subTest(spelling=spelling):
                original = json.dumps({'camera_signature': spelling}).encode()
                self.assertIn(name + ': literal executable signature', publication.findings(name, original))
                published = publication.public_text(name, original)
                self.assertEqual(json.loads(published)['camera_signature'], identity)
                self.assertFalse(publication.findings(name, published))

    def test_static_extractors_audit_all_recorded_signature_lengths(self):
        for name in publication.STATIC:
            for size in (4, 27):
                with self.subTest(name=name, size=size):
                    original = b"CHECK = ('fixture', 16, '" + b'aB' * size + b"')"
                    self.assertIn(name + ': literal static executable range', publication.findings(name, original))
                    self.assertFalse(publication.findings(name, publication.public_text(name, original)))

    def test_external_capture_archive_has_no_historical_publication_mapping(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / 'capture.zip'
            raw = zipped({'source.py': b'capture source'})
            archive.write_bytes(raw)
            self.assertEqual(publication.artifact_digest(archive), publication.sha(raw))
            self.assertEqual(publication.source_digest(archive, 'source.py', b'capture source'),
                             publication.sha(b'capture source'))

    def test_public_json_redaction_preserves_numeric_values_and_private_input(self):
        private = {'path': '/home/operator/capture', 'nested': [2.5, {'error': '/Users/operator/failure'}]}
        self.assertEqual(publication.public_value(private),
                         {'path': '<operator-home>/capture', 'nested': [2.5, {'error': '<operator-home>/failure'}]})
        self.assertEqual(private['path'], '/home/operator/capture')

    def test_local_build_uses_hashes_and_keeps_runtime_guard_bytes_local(self):
        spec = importlib.util.spec_from_file_location('recorder_build', publication.ROOT / 'tools/skyrim-recorder/build.py')
        build = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(build)
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            output = source / 'output'
            raw = bytearray(1024)
            struct.pack_into('<I', raw, 0x3c, 0x80)
            struct.pack_into('<H', raw, 0x80 + 6, 1)
            struct.pack_into('<H', raw, 0x80 + 20, 0)
            struct.pack_into('<IIII', raw, 0x80 + 24 + 8, 512, 0x1000, 512, 512)
            profile = {'preferred_base': 0x140000000}
            for index, stem in enumerate(('movement', 'jump', 'camera', 'controller_getter', 'controller_gate', 'air', 'launch')):
                rva = 0x1000 + index * 16
                local = bytes([index + 1]) * 4
                raw[512 + rva - 0x1000:512 + rva - 0x1000 + 4] = local
                profile[stem + '_function'] = rva
                profile[stem + '_signature'] = publication.range_identity(local)
            for index, stem in enumerate(('movement', 'jump', 'air', 'launch')):
                rva = 0x1100 + index * 8
                struct.pack_into('<Q', raw, 512 + rva - 0x1000, profile['preferred_base'] + profile[stem + '_function'])
                profile[stem + '_vtable_slot'] = rva
            profile['camera_call'] = 0x1080
            raw[640:645] = b'local'
            profile['camera_call_signature'] = publication.range_identity(b'local')
            profile['sha256'] = publication.sha(raw)
            executable = source / 'synthetic.exe'
            executable.write_bytes(raw)
            (source / 'runtime.json').write_text(json.dumps(profile))
            (source / 'probe.cpp').write_text('Synthetic build fixture.\n')

            def compile_stub(command, check):
                if command[0] == 'lld-link':
                    (output / 'dinput8.dll').write_bytes(b'synthetic output')

            with patch.object(build, '__file__', str(source / 'build.py')), patch.object(build.subprocess, 'run', compile_stub):
                build.build(executable, output)
                header = (output / 'runtime.generated.h').read_text()
                self.assertIn('movement_signature[] = {1,1,1,1}', header)
                receipt = json.loads((output / 'build.json').read_text())
                self.assertIsInstance(receipt['profile']['movement_signature'], dict)
                profile['movement_signature']['sha256'] = '0' * 64
                (source / 'runtime.json').write_text(json.dumps(profile))
                with self.assertRaisesRegex(ValueError, 'local executable range differs'):
                    build.build(executable, output)


if __name__ == '__main__':
    unittest.main()
