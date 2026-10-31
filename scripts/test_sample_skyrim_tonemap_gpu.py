"""Synthetic modules and fake replay API verify guards without accessing a GPU."""

import copy
import importlib.util
import json
import os
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('sample_skyrim_tonemap_gpu.py')
SPEC = importlib.util.spec_from_file_location('sample_skyrim_tonemap_gpu', SCRIPT)
GPU = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GPU)


def string_words(value):
    data = value.encode() + b'\0'
    data += b'\0' * (-len(data) % 4)
    return list(struct.unpack('<' + 'I' * (len(data) // 4), data))


def synthetic_module():
    """A small stand-in with the pinned IDs; no proprietary shader bytes."""
    words = [0x07230203, 0x00010600, 0, 261, 0]

    def add(opcode, *operands):
        words.extend(GPU.instruction(opcode, *operands))

    add(17, 1)
    add(14, 0, 1)
    add(15, 4, 1, *string_words('main'), 11, 14, 17, 18, 19, 20, 22, 61, 107)
    add(16, 1, 7)
    add(7, 2, *string_words(GPU.EXPECTED_DXVK_NAME))
    for result, name in {11: 'cb12', 14: 'cb2', 17: 't0', 18: 't1', 19: 't2',
                         20: 'TEXCOORD', 22: 'SV_Target', 61: 'sampler_heap', 107: 'push_data'}.items():
        add(5, result, *string_words(name))
    for args in ((22, 30, 0), (20, 30, 1), (17, 34, 1), (17, 33, 0),
                 (18, 34, 1), (18, 33, 1), (19, 34, 1), (19, 33, 2)):
        add(71, *args)
    for opcode, args in ((19, (77,)), (33, (78, 77)), (22, (7, 32)), (23, (6, 7, 4)),
                         (23, (8, 7, 2)), (21, (5, 32, 0)), (20, (125,)),
                         (32, (23, 3, 6)), (32, (256, 3, 7)), (32, (21, 1, 8)),
                         (25, (15, 7, 1, 0, 0, 0, 1, 0)), (27, (115, 15)),
                         (26, (58,)), (32, (60, 0, 58)), (32, (16, 0, 15)),
                         (32, (10, 2, 6)), (32, (13, 2, 6))):
        add(opcode, *args)
    for value, result in ((0, 24), (1, 25), (2, 27), (3, 39)):
        add(43, 5, result, value)
    add(43, 7, 38, 0x3F800000)
    add(41, 125, 124)
    for args in ((10, 11, 2), (13, 14, 2), (16, 17, 0), (16, 18, 0), (16, 19, 0),
                 (21, 20, 1), (23, 22, 3), (60, 61, 0), (60, 107, 0)):
        add(59, *args)
    add(54, 77, 1, 0, 78)
    add(248, 79)
    for args in ((58, 111, 61), (15, 102, 18), (15, 126, 17), (15, 149, 19), (8, 132, 20)):
        add(61, *args)
    add(83, 8, 114, 132)
    add(83, 58, 130, 111)
    add(83, 58, 153, 111)
    add(86, 115, 116, 102, 111)
    add(87, 6, 117, 116, 114)
    add(247, 133, 0)
    add(250, 124, 135, 134)
    for label, combined, sampled, extracted, coord in ((135, 136, 137, (138, 139, 140), 114),
                                                     (134, 141, 142, (143, 144, 145), 132)):
        add(248, label)
        add(86, 115, combined, 126, 130)
        add(87, 6, sampled, combined, coord)
        for index, result in enumerate(extracted):
            add(81, 7, result, sampled, index)
        add(249, 133)
    add(248, 133)
    for args in ((7, 146, 140, 135, 145, 134), (7, 147, 139, 135, 144, 134),
                 (7, 148, 138, 135, 143, 134)):
        add(245, *args)
    add(86, 115, 155, 149, 153)
    add(87, 6, 156, 155, 132)
    for result, value in ((253, 148), (254, 147), (255, 146), (241, 38)):
        add(83, 7, result, value)
    for pointer, index, value in ((257, 24, 253), (258, 25, 254), (259, 27, 255), (260, 39, 241)):
        add(65, 256, pointer, 22, index)
        add(62, pointer, value)
    add(253)
    add(56)
    return struct.pack('<' + 'I' * len(words), *words)


class Resource:
    def __init__(self, number):
        self.number = number

    def __str__(self):
        return f'ResourceId::{self.number}'

    def __eq__(self, other):
        return isinstance(other, Resource) and self.number == other.number

    @staticmethod
    def Null():
        return Resource(0)


def modification(values):
    values = [struct.unpack('<f', struct.pack('<f', value))[0] for value in values]
    words = [struct.unpack('<I', struct.pack('<f', value))[0] for value in values]
    return SimpleNamespace(IsValid=lambda: True, col=SimpleNamespace(floatValue=values, uintValue=words),
                           depth=-1, stencil=-1)


class FakeReplay:
    def __init__(self, source, fault=None):
        self.source, self.fault = source, fault
        self.operations, self.built, self.active = [], {}, None
        self.api = SimpleNamespace(pipelineType=3, localRenderer=3, degraded=False, pixelHistory=True)
        self.texture = SimpleNamespace(resourceId=Resource(363), width=1920, height=1080, dimension=2,
                                       arraysize=1, mips=1, msSamp=1,
                                       format=SimpleNamespace(Name=lambda: 'R8G8B8A8_UNORM', SRGBCorrected=lambda: False))

    def GetAPIProperties(self): return self.api
    def GetTargetShaderEncodings(self): return [3]
    def GetTextures(self): return [self.texture]
    def SetFrameEvent(self, event, force): self.operations.append(('event', event, force))
    def ClearReplayCache(self): self.operations.append(('clear',))
    def GetPipelineState(self): return self
    def GetShader(self, stage): return self.active or Resource(27852)
    def GetShaderReflection(self, stage):
        return SimpleNamespace(rawBytes=self.built[self.active.number][0] if self.active else self.source)
    def GetShaderEntryPoint(self, stage): return 'main'
    def GetOutputTargets(self): return [SimpleNamespace(resource=Resource(363))]

    def BuildTargetShader(self, entry, encoding, source, flags, stage):
        _, instructions, _, _, _ = GPU.parse_spirv(source)
        store = [item for item in instructions if item['opcode'] == 62][-1]['operands'][1]
        kind = {117: 't1', 156: 't2', 261: 't0'}[store]
        resource = Resource(100 + len(self.built))
        self.built[resource.number] = source, kind
        self.operations.append(('build', kind))
        return resource, ''

    def ReplaceResource(self, original, replacement):
        self.active = replacement
        self.operations.append(('replace', replacement.number))
        if self.fault == 'replace':
            raise RuntimeError('injected partial replacement error')

    def RemoveReplacement(self, original):
        self.operations.append(('remove', self.active.number))
        if self.fault == 'remove':
            raise RuntimeError('injected replacement removal error')
        self.active = None

    def FreeTargetResource(self, replacement):
        self.operations.append(('free', replacement.number))

    def PixelHistory(self, target, x, y, subresource, cast):
        kind = self.built[self.active.number][1] if self.active else 'original'
        self.operations.append(('history', kind, x, y))
        if self.fault == 'history' and kind == 't0':
            raise RuntimeError('injected history error')
        values = {'original': [0.1, 0.2, 0.3, 0.7], 't1': [2, 3, 4, 1],
                  't0': [0.01, 0.02, 0.03, 1], 't2': [0.36, 0.36, 0.36, 1]}[kind]
        if self.fault == 'restoration' and not self.active and len(self.built) == 3:
            values = [0.2, 0.2, 0.3, 0.7]
        if self.fault == 'capture' and not self.active and len(self.built) == 3:
            self.capture.write_bytes(b'changed capture')
        item = SimpleNamespace(eventId=11359, primitiveID=0, fragIndex=0,
                               preMod=modification([0, 0, 0, 0]), shaderOut=modification(values),
                               postMod=modification([min(1, max(0, value)) for value in values]),
                               **{name: False for name in GPU.HISTORY_FAILURE_FLAGS})
        if self.fault == 'discard': item.shaderDiscarded = True
        if self.fault == 'predication': item.predicationSkipped = True
        return [item, copy.deepcopy(item)] if self.fault == 'ambiguous' else [item]

    def Shutdown(self): self.operations.append(('controller_shutdown',))


class FakeRenderDoc:
    ResultCode = SimpleNamespace(Succeeded=0)
    GraphicsAPI = SimpleNamespace(Vulkan=3)
    ShaderEncoding = SimpleNamespace(SPIRV=3)
    ShaderStage = SimpleNamespace(Pixel=4)
    CompType = SimpleNamespace(Typeless=0)
    ResourceId = Resource
    GlobalEnvironment = staticmethod(lambda: None)
    ReplayOptions = staticmethod(lambda: None)
    ShaderCompileFlags = staticmethod(lambda: None)
    Subresource = staticmethod(lambda *args: args)
    GetVersionString = staticmethod(lambda: '1.45')

    def __init__(self, controller): self.controller = controller
    def InitialiseReplay(self, environment, args): self.controller.operations.append(('init',))
    def OpenCaptureFile(self): return self
    def OpenFile(self, path, filetype, callback): return 0
    def LocalReplaySupport(self): return True
    def OpenCapture(self, options, callback): return 0, self.controller
    def Shutdown(self): self.controller.operations.append(('capture_shutdown',))
    def ShutdownReplay(self): self.controller.operations.append(('replay_shutdown',))


class PatchGuardTests(unittest.TestCase):
    def setUp(self):
        self.source = synthetic_module()
        self.pin = patch.object(GPU, 'EXPECTED_SHADER_SHA256', GPU.sha256(self.source))
        self.pin.start()
        self.addCleanup(self.pin.stop)

    def test_each_probe_changes_only_output_stores_and_bloom_id_bound(self):
        original_words, original, _, _, _ = GPU.parse_spirv(self.source)
        retained = [GPU.instruction(item['opcode'], *item['operands']) for item in original if item['opcode'] != 62]
        for kind in ('t0', 't1', 't2'):
            with self.subTest(kind=kind):
                data, receipt = GPU.patch_sample_output(self.source, kind)
                words, instructions, definitions, _, _ = GPU.parse_spirv(data)
                new_stores = [item for item in instructions if item['opcode'] == 62]
                self.assertEqual(len(new_stores), 1)
                self.assertEqual(new_stores[0]['operands'], [22, {'t0': 261, 't1': 117, 't2': 156}[kind]])
                unmodified = [GPU.instruction(item['opcode'], *item['operands']) for item in instructions
                              if item['opcode'] != 62 and not (kind == 't0' and item['opcode'] == 80)]
                self.assertEqual(unmodified, retained)
                self.assertEqual(words[3], original_words[3] + (kind == 't0'))
                self.assertEqual(receipt['patched_shader_sha256'], GPU.sha256(data))
                if kind == 't0':
                    self.assertEqual(definitions[261]['operands'], [6, 261, 148, 147, 146, 38])
                self.assertEqual(instructions[-2]['opcode'], 253)
                self.assertEqual(instructions[-3]['opcode'], 62)

    def test_unpinned_or_truncated_bytes_fail_before_structural_patch(self):
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            GPU.patch_sample_output(self.source[:-4], 't1')
        with self.assertRaisesRegex(ValueError, 'Unknown sample'):
            GPU.patch_sample_output(self.source, 'guess')

    def mutate(self, predicate, replacement):
        words, instructions, _, _, _ = GPU.parse_spirv(self.source)
        item = next(item for item in instructions if predicate(item))
        offset = item['word_offset']
        old_count = words[offset] >> 16
        words[offset:offset + old_count] = replacement
        return struct.pack('<' + 'I' * len(words), *words)

    def assert_structural_failure(self, data, pattern):
        with patch.object(GPU, 'EXPECTED_SHADER_SHA256', GPU.sha256(data)):
            with self.assertRaisesRegex(ValueError, pattern):
                GPU.patch_sample_output(data, 't1')

    def test_source_name_entry_point_sample_and_output_guards(self):
        cases = [
            (lambda i: i['opcode'] == 7, GPU.instruction(7, 2, *string_words('fs.other')), 'checksum'),
            (lambda i: i['opcode'] == 5 and i['operands'][0] == 18,
             GPU.instruction(5, 18, *string_words('other')), 'OpName'),
            (lambda i: i['opcode'] == 15, GPU.instruction(15, 5, 1, *string_words('main'), 22), 'Fragment'),
            (lambda i: i['opcode'] == 87 and i['operands'][1] == 117,
             GPU.instruction(87, 6, 117, 116, 132), '%117'),
            (lambda i: i['opcode'] == 61 and i['operands'][1] == 102,
             GPU.instruction(61, 15, 102, 17), '%102'),
            (lambda i: i['opcode'] == 245 and i['operands'][1] == 148,
             GPU.instruction(245, 7, 148, 139, 135, 143, 134), '%148'),
            (lambda i: i['opcode'] == 62 and i['operands'][0] == 257,
             GPU.instruction(62, 257, 117), 'four pinned'),
            (lambda i: i['opcode'] == 65 and i['operands'][1] == 258,
             GPU.instruction(65, 256, 258, 22, 24), '%258'),
            (lambda i: i['opcode'] == 253, GPU.instruction(253) + GPU.instruction(253), 'one main return'),
        ]
        for predicate, replacement, pattern in cases:
            with self.subTest(pattern=pattern):
                self.assert_structural_failure(self.mutate(predicate, replacement), pattern)

    def test_parser_rejects_zero_length_duplicate_ids_and_bad_header(self):
        data = bytearray(self.source)
        struct.pack_into('<I', data, 20, 17)
        self.assert_structural_failure(bytes(data), 'instruction length')
        data = self.source + struct.pack('<III', *GPU.instruction(22, 7, 32))
        self.assert_structural_failure(data, 'duplicate')
        data = bytearray(self.source)
        struct.pack_into('<I', data, 4, 0x00010500)
        self.assert_structural_failure(bytes(data), 'header')


class ReplayLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = synthetic_module()
        self.source_path, self.capture = self.root / 'source.spv', self.root / 'source.rdc'
        self.source_path.write_bytes(self.source)
        self.capture.write_bytes(b'synthetic capture bytes')
        self.capture_hash = GPU.sha256_file(self.capture)
        self.output = self.root / 'evidence'
        self.pin = patch.object(GPU, 'EXPECTED_SHADER_SHA256', GPU.sha256(self.source))
        self.pin.start()
        self.addCleanup(self.pin.stop)

    def collect(self, fault=None):
        self.controller = FakeReplay(self.source, fault)
        self.controller.capture = self.capture
        return GPU.collect(FakeRenderDoc(self.controller), self.capture, self.source_path, self.output,
                           self.capture_hash, 11359, 'ResourceId::27852', 'ResourceId::363',
                           [(960, 540), (400, 650), (1400, 300)])

    def test_success_preserves_originals_and_reads_unquantized_sample_output(self):
        receipt = self.collect()
        self.assertEqual(receipt['status'], 'complete')
        self.assertTrue(receipt['capture']['unchanged'])
        self.assertTrue(receipt['baseline_outputs_restored'])
        scene = receipt['probes'][0]['pixels'][0]['matching_fragment']
        self.assertEqual(scene['shader_output']['rgba_float32'], [2, 3, 4, 1])
        self.assertEqual(scene['post_mod']['rgba_float32'], [1, 1, 1, 1])
        self.assertEqual(len(receipt['cleanup']['replacement_remove_attempts']), 3)
        self.assertTrue(all(item['succeeded'] for item in receipt['cleanup']['target_free_attempts']))
        self.assertIsNone(self.controller.active)
        self.assertEqual(self.controller.operations[-3:], [('controller_shutdown',), ('capture_shutdown',),
                                                         ('replay_shutdown',)])
        self.assertEqual(self.source_path.read_bytes(), self.source)
        self.assertEqual(GPU.sha256_file(self.capture), self.capture_hash)
        self.assertEqual(json.loads((self.output / 'receipt.json').read_text()), receipt)
        # Windows ignores mkdir mode bits and reports every directory as 0o777;
        # the private-directory guarantee is a POSIX permission bit.
        if os.name == 'posix':
            self.assertEqual(self.output.stat().st_mode & 0o777, 0o700)

    def test_partial_replace_and_pixel_history_errors_restore_and_free(self):
        for fault in ('replace', 'history'):
            with self.subTest(fault=fault):
                self.output = self.root / fault
                with self.assertRaisesRegex(RuntimeError, 'injected'):
                    self.collect(fault)
                receipt = json.loads((self.output / 'receipt.json').read_text())
                self.assertEqual(receipt['status'], 'failed')
                self.assertIsNone(self.controller.active)
                self.assertTrue(all(item['succeeded'] for item in receipt['cleanup']['replacement_remove_attempts']))
                self.assertEqual(len(receipt['cleanup']['target_free_attempts']),
                                 len(receipt['cleanup']['replacement_remove_attempts']))
                self.assertTrue(receipt['capture']['unchanged'])
                self.assertEqual(self.controller.operations[-1], ('replay_shutdown',))

    def test_restoration_output_mismatch_and_capture_mutation_fail(self):
        for fault, pattern in (('restoration', 'changed after restoring'), ('capture', 'Capture changed')):
            with self.subTest(fault=fault):
                self.capture.write_bytes(b'synthetic capture bytes')
                self.output = self.root / fault
                with self.assertRaisesRegex(ValueError, pattern):
                    self.collect(fault)
                receipt = json.loads((self.output / 'receipt.json').read_text())
                self.assertEqual(receipt['status'], 'failed')
                self.assertIsNone(self.controller.active)

    def test_failed_removal_never_frees_a_possibly_active_replacement(self):
        with self.assertRaisesRegex(RuntimeError, 'replacement removal error'):
            self.collect('remove')
        receipt = json.loads((self.output / 'receipt.json').read_text())
        self.assertEqual(receipt['status'], 'failed')
        self.assertFalse(receipt['cleanup']['replacement_remove_attempts'][0]['succeeded'])
        self.assertEqual(receipt['cleanup']['target_free_attempts'], [])
        deferred = receipt['cleanup']['target_free_deferred_to_shutdown']
        self.assertEqual(len(deferred), 1)
        self.assertEqual(deferred[0]['resource_id'], 'ResourceId::100')
        self.assertFalse(any(item[0] == 'free' for item in self.controller.operations))
        self.assertEqual(self.controller.operations[-3:], [('controller_shutdown',), ('capture_shutdown',),
                                                         ('replay_shutdown',)])
        self.assertTrue(receipt['capture']['unchanged'])

    def test_ambiguous_or_discarded_fragments_are_not_selected_arbitrarily(self):
        for fault, pattern in (('ambiguous', 'matching fragments'), ('discard', 'failure'),
                               ('predication', 'failure')):
            with self.subTest(fault=fault):
                self.output = self.root / fault
                with self.assertRaisesRegex(ValueError, pattern):
                    self.collect(fault)

    def test_bad_source_hash_fails_before_replay_and_retains_receipt(self):
        self.source_path.write_bytes(self.source[:-4])
        with self.assertRaisesRegex(ValueError, 'Source shader SHA-256'):
            self.collect()
        self.assertEqual(self.controller.operations, [])
        self.assertEqual(json.loads((self.output / 'receipt.json').read_text())['status'], 'failed')

    def test_existing_output_directory_never_overwritten(self):
        self.output.mkdir()
        marker = self.output / 'keep'
        marker.write_text('preserve')
        with self.assertRaisesRegex(ValueError, 'already exists'):
            self.collect()
        self.assertEqual(marker.read_text(), 'preserve')


if __name__ == '__main__':
    unittest.main()
