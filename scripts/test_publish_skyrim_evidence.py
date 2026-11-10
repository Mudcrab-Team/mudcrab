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
    def test_nested_archives_remove_paths_and_signatures_and_keep_trace_bytes(self):
        original = zipped({'inner.zip': zipped({
            'runtime.json': b'{"camera_signature": "1122334455", "path": "/home/person/game"}',
            'build.log.gz': gzip.compress(b'/Users/person/build\n'),
            'trace.bin.gz': gzip.compress(bytes(range(256))),
        })})
        self.assertTrue(publication.findings('outer.zip', original))
        changes = {}
        published = publication.transform('outer.zip', original, changes)
        self.assertFalse(publication.findings('outer.zip', published))
        self.assertEqual(changes['outer.zip']['original_sha256'], publication.sha(original))
        with zipfile.ZipFile(io.BytesIO(published)) as outer:
            with zipfile.ZipFile(io.BytesIO(outer.read('inner.zip'))) as inner:
                profile = json.loads(inner.read('runtime.json'))
                self.assertEqual(profile['camera_signature'], publication.range_identity(bytes(range(0x11, 0x56, 0x11))))
                self.assertEqual(gzip.decompress(inner.read('trace.bin.gz')), bytes(range(256)))
        second_changes = {}
        self.assertEqual(publication.transform('outer.zip', published, second_changes), published)
        self.assertEqual(second_changes, {})

    def test_utf8_numeric_trace_is_never_rewritten_even_when_audit_reports_path(self):
        original = gzip.compress(b'UTF8-valid numeric trace payload /home/operator/observation')
        changes = {}
        self.assertEqual(publication.transform('trace.bin.gz', original, changes), original)
        self.assertEqual(changes, {})
        self.assertTrue(publication.findings('trace.bin.gz', original))
        archive = zipped({'trace.bin.gz': original})
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
            self.assertFalse(publication.findings(name, evolved))
            self.assertTrue(publication.findings(name, original))

    def test_static_extractors_audit_all_recorded_signature_lengths(self):
        for name in publication.STATIC:
            original = b"CHECKS = [('short', 16, '11223344'), ('long', 32, '" + b'11' * 27 + b"')]\n"
            self.assertTrue(publication.findings(name, original))
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
