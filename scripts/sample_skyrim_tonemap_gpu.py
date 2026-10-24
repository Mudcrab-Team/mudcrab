#!/usr/bin/env python3
"""Measure captured tone-map samples with guarded, replay-only SPIR-V probes.

This never launches or attaches to Skyrim. It opens an immutable RenderDoc
capture, checks the exact captured SPIR-V module, and temporarily substitutes
three shaders in the replay controller. A probe replaces only the final output
stores with an existing sampled vector, or the selected bloom RGB plus alpha 1.
Original coordinates, samplers, descriptors and sample instructions remain in
the module. RenderDoc PixelHistory.shaderOut measures the instrumented replay
using an RGBA32 float attachment with blending disabled. It is not an original
frame register read, and does not measure original intermediate GPU rounding.

The pinned module is captured DXVK group-26 source fs.716590ec34046095c58617804cdff8cc,
SPIR-V SHA-256 6c72aaa001228aa37ddf79dc31669a7087b9981f75f180b2fa36354dc59802be.
No other shader is accepted. All resource/event IDs and sample pixels must be
supplied explicitly and are checked against the replay pipeline. Replacement
resources are removed/freed in finally blocks; capture hashes are checked again.

Run using RenderDoc 1.45's Python module and a compatible Vulkan replay GPU.
Output is private evidence and belongs in a fresh ignored directory.

API and shader-output behavior:
https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/api/replay/renderdoc_replay.h
https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/driver/vulkan/vk_pixelhistory.cpp#L3233
https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/driver/vulkan/vk_pixelhistory.cpp#L3711
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import struct
import sys


EXPECTED_SHADER_SHA256 = '6c72aaa001228aa37ddf79dc31669a7087b9981f75f180b2fa36354dc59802be'
EXPECTED_DXVK_NAME = 'fs.716590ec34046095c58617804cdff8cc'
EXPECTED_SAMPLE_IDS = {'t1': 117, 't2': 156, 't0': None}
HISTORY_FAILURE_FLAGS = ('sampleMasked', 'backfaceCulled', 'depthClipped', 'depthBoundsFailed',
                         'viewClipped', 'scissorClipped', 'shaderDiscarded',
                         'depthTestFailed', 'stencilTestFailed', 'predicationSkipped',
                         'unboundPS', 'directShaderWrite')
LIMITATIONS = [
    'These are instrumented GPU replay measurements, not original-frame intermediate register reads.',
    'Original PixelHistory shaderOut is a separate GPU replay with an RGBA32 float attachment and blending disabled.',
    'Replacing shader output can change driver optimization; original arithmetic execution is not measured by sample probes.',
    'Captured sampler and view state is reused, but probe agreement does not prove an earlier screenshot used this state.',
    'Raw source SPIR-V and patched modules remain private evidence; do not publish proprietary shader artifacts.',
]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(4 * 1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def encode(value):
    if isinstance(value, float) and not math.isfinite(value):
        return 'NaN' if math.isnan(value) else ('Infinity' if value > 0 else '-Infinity')
    if isinstance(value, dict):
        return {key: encode(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [encode(item) for item in value]
    return value


def literal_string(words):
    require(words, 'SPIR-V literal string is missing')
    data = struct.pack('<' + 'I' * len(words), *words)
    require(b'\0' in data, 'Unterminated SPIR-V literal string')
    end = data.index(0)
    try:
        return data[:end].decode('utf-8'), end // 4 + 1
    except UnicodeDecodeError as error:
        raise ValueError('SPIR-V literal string is not UTF-8') from error


def instruction(opcode, *operands):
    return [((len(operands) + 1) << 16) | opcode, *operands]


def parse_spirv(data):
    require(20 <= len(data) <= 1024 * 1024 and len(data) % 4 == 0,
            'SPIR-V byte length is invalid or exceeds one MiB')
    words = list(struct.unpack('<' + 'I' * (len(data) // 4), data))
    require(words[0] == 0x07230203 and words[1] == 0x00010600 and words[4] == 0,
            'Expected little-endian SPIR-V 1.6 header')
    require(0 < words[3] <= 100000, 'SPIR-V ID bound is invalid')
    instructions, definitions, names, strings = [], {}, {}, []
    function = None
    offset = 5
    result_first = {7, 11, 19, 20, 21, 22, 23, 25, 26, 27, 29, 30, 32, 33, 248}
    result_second = {12, 41, 42, 43, 44, 54, 55, 57, 59, 61, 65, 66, 80, 81, 83,
                     86, 87, 113, 127, 131, 133, 136, 169, 186, 245}
    while offset < len(words):
        count, opcode = words[offset] >> 16, words[offset] & 0xFFFF
        require(count > 0 and offset + count <= len(words), 'Malformed SPIR-V instruction length')
        operands = words[offset + 1:offset + count]
        if opcode == 54:
            require(function is None and len(operands) == 4, 'Malformed SPIR-V function nesting')
            function = operands[1]
        item = {'word_offset': offset, 'opcode': opcode, 'operands': operands, 'function': function}
        instructions.append(item)
        result_index = 0 if opcode in result_first else 1 if opcode in result_second else None
        if result_index is not None:
            require(len(operands) > result_index, 'Missing SPIR-V result ID')
            result_id = operands[result_index]
            require(0 < result_id < words[3] and result_id not in definitions,
                    'SPIR-V result ID is duplicate or outside bound')
            definitions[result_id] = item
        if opcode == 5:
            require(len(operands) >= 2 and operands[0] not in names, 'Malformed or duplicate OpName')
            names[operands[0]] = literal_string(operands[1:])[0]
        elif opcode == 7:
            strings.append(literal_string(operands[1:])[0])
        elif opcode == 56:
            require(function is not None and not operands, 'Malformed SPIR-V function end')
            function = None
        offset += count
    require(function is None, 'Unclosed SPIR-V function')
    return words, instructions, definitions, names, strings


def validate_pinned_module(data):
    require(sha256(data) == EXPECTED_SHADER_SHA256, 'Source shader SHA-256 differs from pinned capture')
    words, instructions, definitions, names, strings = parse_spirv(data)
    require(words[3] == 261, 'Pinned module ID bound changed')
    require(EXPECTED_DXVK_NAME in strings, 'Native DXVK source checksum name is absent')
    expected_names = {11: 'cb12', 14: 'cb2', 17: 't0', 18: 't1', 19: 't2',
                      20: 'TEXCOORD', 22: 'SV_Target', 61: 'sampler_heap', 107: 'push_data'}
    require(all(names.get(key) == value for key, value in expected_names.items()),
            'Pinned input, resource or output OpName mapping changed')
    entries = [item for item in instructions if item['opcode'] == 15]
    require(len(entries) == 1 and entries[0]['operands'][:2] == [4, 1],
            'Expected one Fragment entry point with function ID 1')
    entry_name, used = literal_string(entries[0]['operands'][2:])
    require(entry_name == 'main' and 22 in entries[0]['operands'][2 + used:],
            'Entry point name or output interface changed')

    def exact(result_id, opcode, operands):
        item = definitions.get(result_id)
        require(item is not None and item['opcode'] == opcode and item['operands'] == operands,
                f'Pinned SPIR-V definition %{result_id} changed')

    exact(7, 22, [7, 32])
    exact(6, 23, [6, 7, 4])
    exact(23, 32, [23, 3, 6])
    exact(22, 59, [23, 22, 3])
    exact(256, 32, [256, 3, 7])
    exact(38, 43, [7, 38, 0x3F800000])
    for result_id, operands in {117: [6, 117, 116, 114], 137: [6, 137, 136, 114],
                                142: [6, 142, 141, 132], 156: [6, 156, 155, 132]}.items():
        exact(result_id, 87, operands)
        require(definitions[result_id]['function'] == 1, 'Sample moved outside main function')
    for result_id, operands in {116: [115, 116, 102, 111], 136: [115, 136, 126, 130],
                                141: [115, 141, 126, 130], 155: [115, 155, 149, 153]}.items():
        exact(result_id, 86, operands)
    for result_id, pointer in ((102, 18), (126, 17), (149, 19)):
        exact(result_id, 61, [15, result_id, pointer])
    for result_id, operands in {146: [7, 146, 140, 135, 145, 134],
                                147: [7, 147, 139, 135, 144, 134],
                                148: [7, 148, 138, 135, 143, 134]}.items():
        exact(result_id, 245, operands)
    stores = [item for item in instructions if item['opcode'] == 62]
    require([item['operands'] for item in stores] == [[257, 253], [258, 254], [259, 255], [260, 241]]
            and all(item['function'] == 1 for item in stores),
            'Expected only the four pinned final component output stores')
    for pointer, index in ((257, 24), (258, 25), (259, 27), (260, 39)):
        exact(pointer, 65, [256, pointer, 22, index])
    returns = [item for item in instructions if item['opcode'] in (252, 253, 254) and item['function'] == 1]
    require(len(returns) == 1 and returns[0]['opcode'] == 253
            and returns[0]['word_offset'] > stores[-1]['word_offset'],
            'Expected one main return after the four output stores')
    return words, instructions, definitions, stores, returns[0]


def patch_sample_output(data, probe):
    """Change final output only; every existing non-store instruction stays exact."""
    require(probe in ('t0', 't1', 't2'), 'Unknown sample probe')
    words, instructions, definitions, stores, final_return = validate_pinned_module(data)
    header = words[:5]
    removed_offsets = {item['word_offset'] for item in stores}
    source_result = EXPECTED_SAMPLE_IDS[probe]
    injection = []
    if probe == 't0':
        source_result = words[3]
        require(source_result not in definitions, 'New composite ID collides with original module')
        injection.extend(instruction(80, 6, source_result, 148, 147, 146, 38))
        header[3] += 1
    injection.extend(instruction(62, 22, source_result))
    patched_words = list(header)
    for item in instructions:
        if item['word_offset'] in removed_offsets:
            continue
        if item['word_offset'] == final_return['word_offset']:
            patched_words.extend(injection)
        offset = item['word_offset']
        patched_words.extend(words[offset:offset + (words[offset] >> 16)])
    patched = struct.pack('<' + 'I' * len(patched_words), *patched_words)
    parse_spirv(patched)
    return patched, {'probe': probe, 'source_shader_sha256': sha256(data),
                     'patched_shader_sha256': sha256(patched), 'output_variable_id': 22,
                     'sampled_vector_id': source_result,
                     'rgb_phi_ids': [148, 147, 146] if probe == 't0' else None,
                     'alpha_one_id': 38 if probe == 't0' else None,
                     'removed_stores': stores,
                     'insertion_before_original_word_offset': final_return['word_offset'],
                     'new_instructions_words': injection,
                     'original_id_bound': words[3], 'patched_id_bound': header[3],
                     'invariant': 'All original non-OpStore instruction words remain in original order.'}


def pixel_argument(value):
    try:
        components = value.split(',')
        require(len(components) == 2, 'Expected x,y')
        x, y = (int(component) for component in components)
        require(x >= 0 and y >= 0, 'Pixel coordinates must be nonnegative')
        return x, y
    except (ValueError, TypeError) as error:
        raise argparse.ArgumentTypeError(f'Invalid pixel {value!r}; expected nonnegative x,y') from error


def modification_value(value):
    return {'valid': bool(value.IsValid()),
            'rgba_float32': encode(list(value.col.floatValue)),
            'rgba_bits32': [f'{int(word):08x}' for word in value.col.uintValue],
            'depth': encode(float(value.depth)), 'stencil': int(value.stencil)}


def measure_pixels(rd, controller, event_id, target, pixels):
    results = []
    for x, y in pixels:
        controller.SetFrameEvent(event_id, True)
        history = controller.PixelHistory(target, x, y, rd.Subresource(0, 0, 0), rd.CompType.Typeless)
        require(len(history) <= 200000, 'Pixel history exceeds bounded event count')
        matching = [item for item in history if int(item.eventId) == event_id]
        records = []
        for item in matching:
            flags = {name: bool(getattr(item, name)) for name in HISTORY_FAILURE_FLAGS}
            records.append({'event_id': int(item.eventId), 'primitive_id': int(item.primitiveID),
                            'fragment_index': int(item.fragIndex), 'flags': flags,
                            'pre_mod': modification_value(item.preMod),
                            'shader_output': modification_value(item.shaderOut),
                            'post_mod': modification_value(item.postMod)})
        require(len(records) == 1, f'Pixel {x},{y} has {len(records)} matching fragments; expected one')
        require(not any(records[0]['flags'].values()), f'Pixel {x},{y} fragment has a failure or unsupported write flag')
        require(records[0]['shader_output']['valid'], f'Pixel {x},{y} shader output is invalid')
        results.append({'xy': [x, y], 'matching_fragment': records[0]})
    return results


def artifact(output, name, data):
    path = output / name
    with path.open('xb') as stream:
        stream.write(data)
    return {'path': name, 'size_bytes': len(data), 'sha256': sha256(data)}


def collect(rd, capture_path, source_path, output, capture_sha256, event_id,
            shader_id, output_id, pixels):
    require(isinstance(capture_sha256, str) and len(capture_sha256) == 64
            and all(c in '0123456789abcdef' for c in capture_sha256), 'Expected explicit lowercase capture SHA-256')
    require(type(event_id) is int and event_id > 0, 'Expected positive event ID')
    require(1 <= len(pixels) <= 16 and len(set(pixels)) == len(pixels), 'Provide one through 16 distinct sample pixels')
    require(all(len(point) == 2 and all(type(c) is int and c >= 0 for c in point) for point in pixels),
            'Pixel coordinates must be nonnegative integer pairs')
    require(not output.exists(), 'Output directory already exists')
    output.mkdir(parents=True, mode=0o700)
    receipt = {'schema_version': 1, 'status': 'incomplete',
               'created_utc': datetime.now(timezone.utc).isoformat(),
               'measurement_kind': 'offline_instrumented_shader_replay',
               'collector_sha256': sha256_file(Path(__file__).resolve()),
               'capture': {'path': str(capture_path), 'expected_sha256': capture_sha256},
               'requested': {'event_id': event_id, 'pixel_shader_id': shader_id,
                             'output_resource_id': output_id, 'pixels': [list(point) for point in pixels]},
               'limitations': LIMITATIONS, 'artifacts': [], 'probes': [],
               'cleanup': {'replacement_remove_attempts': [], 'target_free_attempts': [],
                           'target_free_deferred_to_shutdown': []}}
    cap = controller = None
    replay_started = False
    original_shader = None
    capture_path = Path(capture_path)
    try:
        source = Path(source_path).read_bytes()
        validate_pinned_module(source)
        receipt['source'] = artifact(output, 'original-pixel-shader.spv', source)
        capture_path = capture_path.resolve(strict=True)
        require(capture_path.is_file(), 'Capture must be a regular file')
        before_hash = sha256_file(capture_path)
        receipt['capture'].update(path=str(capture_path), size_bytes=capture_path.stat().st_size,
                                  sha256_before_replay=before_hash)
        require(before_hash == capture_sha256, 'Capture SHA-256 differs from explicit expected hash')
        receipt['renderdoc_version'] = rd.GetVersionString()
        require(receipt['renderdoc_version'].startswith('1.45'), 'This collector requires RenderDoc 1.45')
        rd.InitialiseReplay(rd.GlobalEnvironment(), [])
        replay_started = True
        cap = rd.OpenCaptureFile()
        result = cap.OpenFile(str(capture_path), '', None)
        require(result == rd.ResultCode.Succeeded, 'Capture open failed: ' + str(result))
        require(bool(cap.LocalReplaySupport()), 'Capture has no local replay support')
        result, controller = cap.OpenCapture(rd.ReplayOptions(), None)
        require(result == rd.ResultCode.Succeeded and controller is not None,
                'Capture replay initialization failed: ' + str(result))
        properties = controller.GetAPIProperties()
        receipt['api_properties'] = {'pipelineType': str(properties.pipelineType),
                                     'localRenderer': str(properties.localRenderer),
                                     'degraded': bool(properties.degraded),
                                     'pixelHistory': bool(properties.pixelHistory)}
        require(properties.pipelineType == rd.GraphicsAPI.Vulkan and not properties.degraded,
                'Expected a non-degraded Vulkan replay')
        require(properties.pixelHistory, 'Replay API does not support PixelHistory')
        require(rd.ShaderEncoding.SPIRV in controller.GetTargetShaderEncodings(),
                'Replay API cannot build SPIR-V replacement shaders')
        controller.SetFrameEvent(event_id, True)
        pipeline = controller.GetPipelineState()
        original_shader = pipeline.GetShader(rd.ShaderStage.Pixel)
        require(str(original_shader) == shader_id, 'Selected pixel shader resource ID differs')
        reflection = pipeline.GetShaderReflection(rd.ShaderStage.Pixel)
        require(reflection is not None and bytes(reflection.rawBytes) == source,
                'Selected pixel shader bytes differ from pinned source')
        require(pipeline.GetShaderEntryPoint(rd.ShaderStage.Pixel) == 'main', 'Selected entry point differs')
        targets = list(pipeline.GetOutputTargets())
        require(len(targets) == 1 and str(targets[0].resource) == output_id,
                'Selected output resource differs or multiple targets are bound')
        target = targets[0].resource
        textures = {str(texture.resourceId): texture for texture in controller.GetTextures()}
        require(output_id in textures, 'Output texture is not described')
        description = textures[output_id]
        require(description.msSamp == 1 and description.dimension == 2
                and description.arraysize == 1 and description.mips == 1,
                'Expected one non-multisampled 2D output subresource')
        require(all(x < description.width and y < description.height for x, y in pixels),
                'Requested pixel lies outside output texture')
        receipt['output_texture'] = {'width': int(description.width), 'height': int(description.height),
                                     'format': description.format.Name(),
                                     'srgb_corrected': bool(description.format.SRGBCorrected())}
        receipt['baseline_original_shader'] = measure_pixels(rd, controller, event_id, target, pixels)
        for probe in ('t1', 't0', 't2'):
            patched, patch_receipt = patch_sample_output(source, probe)
            record = {'patch': patch_receipt, 'source': artifact(output, f'probe-{probe}.spv', patched)}
            receipt['probes'].append(record)
            replacement = None
            replacement_attempted = False
            try:
                replacement, diagnostics = controller.BuildTargetShader(
                    'main', rd.ShaderEncoding.SPIRV, patched, rd.ShaderCompileFlags(), rd.ShaderStage.Pixel)
                record['build_diagnostics'] = str(diagnostics)
                record['replacement_resource_id'] = str(replacement)
                require(replacement is not None and replacement != rd.ResourceId.Null(),
                        f'Probe {probe} replacement build failed: {diagnostics}')
                replacement_attempted = True
                controller.ReplaceResource(original_shader, replacement)
                controller.ClearReplayCache()
                record['pixels'] = measure_pixels(rd, controller, event_id, target, pixels)
                require(all(all(isinstance(value, (int, float)) and math.isfinite(value)
                                for value in point['matching_fragment']['shader_output']['rgba_float32'])
                            for point in record['pixels']), f'Probe {probe} returned a nonfinite sample')
                record['status'] = 'complete'
            finally:
                if replacement_attempted:
                    cleanup = {'probe': probe, 'resource_id': str(original_shader), 'succeeded': False}
                    receipt['cleanup']['replacement_remove_attempts'].append(cleanup)
                    try:
                        controller.RemoveReplacement(original_shader)
                        cleanup['succeeded'] = True
                    except Exception as error:
                        cleanup['error'] = str(error)
                        receipt['cleanup']['target_free_deferred_to_shutdown'].append(
                            {'probe': probe, 'resource_id': str(replacement),
                             'reason': 'Replacement removal failed; preserve possibly active resource until controller shutdown.'})
                        raise
                    finally:
                        if cleanup['succeeded'] and replacement is not None and replacement != rd.ResourceId.Null():
                            freed = {'probe': probe, 'resource_id': str(replacement), 'succeeded': False}
                            receipt['cleanup']['target_free_attempts'].append(freed)
                            controller.FreeTargetResource(replacement)
                            freed['succeeded'] = True
                elif replacement is not None and replacement != rd.ResourceId.Null():
                    freed = {'probe': probe, 'resource_id': str(replacement), 'succeeded': False}
                    receipt['cleanup']['target_free_attempts'].append(freed)
                    controller.FreeTargetResource(replacement)
                    freed['succeeded'] = True
        controller.ClearReplayCache()
        controller.SetFrameEvent(event_id, True)
        restored = controller.GetPipelineState()
        require(str(restored.GetShader(rd.ShaderStage.Pixel)) == shader_id
                and bytes(restored.GetShaderReflection(rd.ShaderStage.Pixel).rawBytes) == source,
                'Original shader identity was not restored')
        receipt['baseline_after_restoration'] = measure_pixels(rd, controller, event_id, target, pixels)
        receipt['baseline_outputs_restored'] = (
            receipt['baseline_original_shader'] == receipt['baseline_after_restoration'])
        require(receipt['baseline_outputs_restored'], 'Original shader PixelHistory changed after restoring probes')
        receipt['status'] = 'complete'
    except Exception as error:
        receipt['status'] = 'failed'
        receipt['error'] = {'type': type(error).__name__, 'message': str(error)}
        raise
    finally:
        try:
            if controller is not None:
                controller.Shutdown()
        finally:
            try:
                if cap is not None:
                    cap.Shutdown()
            finally:
                try:
                    if replay_started:
                        rd.ShutdownReplay()
                finally:
                    if 'sha256_before_replay' in receipt['capture']:
                        try:
                            receipt['capture']['sha256_after_replay'] = sha256_file(capture_path)
                            receipt['capture']['unchanged'] = (
                                receipt['capture']['sha256_after_replay'] == receipt['capture']['sha256_before_replay'])
                            if not receipt['capture']['unchanged']:
                                receipt['status'] = 'failed'
                                receipt['error'] = {'type': 'ValueError', 'message': 'Capture changed during replay'}
                        except OSError as error:
                            receipt['status'] = 'failed'
                            receipt['capture']['hash_after_error'] = str(error)
                    receipt['artifacts'] = ([receipt['source']] if 'source' in receipt else []) + [
                        item['source'] for item in receipt['probes']]
                    with (output / 'receipt.json').open('x') as stream:
                        json.dump(encode(receipt), stream, indent=2, allow_nan=False)
                        stream.write('\n')
    require(receipt['status'] == 'complete', receipt.get('error', {}).get('message', 'Probe collection failed'))
    return receipt


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--source-shader', type=Path, required=True)
    parser.add_argument('--capture-sha256', required=True)
    parser.add_argument('--event', type=int, required=True)
    parser.add_argument('--shader-id', required=True)
    parser.add_argument('--output-id', required=True)
    parser.add_argument('--pixel', action='append', type=pixel_argument, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    os.umask(0o077)
    try:
        import renderdoc as rd
        receipt = collect(rd, args.capture, args.source_shader, args.output, args.capture_sha256,
                          args.event, args.shader_id, args.output_id, args.pixel)
        print(json.dumps({'status': receipt['status'], 'receipt': str(args.output / 'receipt.json'),
                          'capture_unchanged': receipt['capture']['unchanged'],
                          'baseline_outputs_restored': receipt['baseline_outputs_restored']}))
        return 0
    except Exception as error:
        print(f'{type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
