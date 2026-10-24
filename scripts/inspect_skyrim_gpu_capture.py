#!/usr/bin/env python3
"""Inspect an existing RenderDoc capture without launching or modifying Skyrim.

Run with RenderDoc 1.44 or 1.45's Python module and a compatible replay GPU/driver.
The default inventories actions and the final 64 draw/dispatch pipelines.
Repeat --event to export selected events' bound textures, buffers and shaders.
Use a fresh, ignored artifacts directory: captures and raw game resources are
private evidence, not source assets. Output permissions are owner-only.

Resource bindings have no inferred Skyrim semantics. Vulkan descriptor numbers
are not automatically the original D3D shader's tN/sN/cbN registers. Raw bytes
are resource storage, not post-view/sampler values. PNG previews are explicitly
clamped RenderDoc exports and must not be used as numeric shader inputs.

API references, pinned to the version used by this collector:
https://github.com/baldurk/renderdoc/blob/v1.44/renderdoc/api/replay/pipestate.h
https://github.com/baldurk/renderdoc/blob/v1.44/renderdoc/api/replay/common_pipestate.h
https://github.com/baldurk/renderdoc/blob/v1.44/renderdoc/api/replay/renderdoc_replay.h
https://github.com/baldurk/renderdoc/blob/v1.44/docs/python_api/examples/renderdoc_intro.py
https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/api/replay/vk_pipestate.h
https://github.com/baldurk/renderdoc/blob/v1.45/renderdoc/api/replay/data_types.h
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import struct
import sys


LIMITATIONS = [
    'Bound or declared resources do not prove a particular GPU invocation accessed them.',
    'Resource identity and shader semantics require separate shader correlation evidence.',
    'Raw texture bytes precede resource-view casts, swizzles, sampling and filtering.',
    'Resources are read after the selected event; read/write snapshots are not pre-invocation inputs.',
    'Only the first visible mip and array slice, sample zero, are exported per binding.',
    'For 3D textures GetTextureData returns the entire selected mip depth.',
    'PNG previews clamp/map values to [0,1] and are not native numeric shader inputs.',
    'Replayed resources are captured-frame evidence, not evidence of an earlier screenshot.',
]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(4 * 1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def resource_id(value):
    return str(value)


def finite_list(values):
    return [value if not isinstance(value, float) or math.isfinite(value) else str(value)
            for value in values]


def shader_source_metadata(reflection, raw_shader):
    """Record debug filenames and bounded SPIR-V shader-name strings verbatim.

    A name resembling a DXVK shader key is correlation evidence, not proof of
    the original native shader bytes or their role. OpString and OpSource use
    literal UTF-8 strings encoded in little-endian SPIR-V words.
    """
    debug_info = getattr(reflection, 'debugInfo', None)
    debug_files = list(getattr(debug_info, 'files', ()))
    require(len(debug_files) <= 1024, 'Shader debug filenames exceed 1024-entry bound')
    filenames = [str(item.filename) for item in debug_files]
    require(all(len(name) <= 4096 for name in filenames), 'Shader debug filename exceeds bound')
    candidates = []
    pattern = re.compile(r'(?<![A-Za-z0-9])(?:vs|hs|ds|gs|ps|fs|cs|ms|ts)\.[0-9a-fA-F]{32,40}(?![0-9a-fA-F])')
    for filename in filenames:
        for match in pattern.finditer(filename):
            candidates.append({'name': match.group(), 'origin': 'reflection.debugInfo.files.filename'})
    spirv_status = 'not-SPIR-V'
    if len(raw_shader) >= 20 and raw_shader[:4] == b'\x03\x02\x23\x07':
        spirv_status = 'parsed-source-strings'
        require(len(raw_shader) <= 16 * 1024 ** 2, 'SPIR-V metadata parse exceeds 16-MiB bound')
        if len(raw_shader) % 4:
            spirv_status = 'malformed-word-alignment'
        else:
            offset = 5
            while offset < len(raw_shader) // 4:
                header = struct.unpack_from('<I', raw_shader, offset * 4)[0]
                count, opcode = header >> 16, header & 0xFFFF
                if count == 0 or offset + count > len(raw_shader) // 4:
                    spirv_status = 'malformed-instruction-length'
                    break
                string_start = {7: 2, 3: 4}.get(opcode)  # OpString, OpSource
                if string_start is not None and count > string_start:
                    literal = raw_shader[(offset + string_start) * 4:(offset + count) * 4]
                    # Scan only the string prefix; source bodies can be large.
                    literal = literal[:16384].split(b'\0', 1)[0].decode('utf-8', 'replace')
                    for match in pattern.finditer(literal):
                        candidates.append({'name': match.group(), 'origin': 'SPIR-V source string',
                                           'opcode': opcode, 'instruction_word_offset': offset})
                offset += count
    return {'debug_source_files': filenames, 'shader_name_candidates': candidates,
            'spirv_source_scan': spirv_status}


def fields(value, names):
    """Serialize a deliberately bounded field list, never arbitrary SWIG graphs."""
    output = {}
    for name in names:
        item = getattr(value, name, None)
        if item is None:
            continue
        if isinstance(item, (str, int, bool)):
            output[name] = item
        elif isinstance(item, float):
            output[name] = item if math.isfinite(item) else str(item)
        else:
            output[name] = str(item)
    return output


def format_info(value):
    return {'name': value.Name(),
            **fields(value, ('type', 'compCount', 'compByteWidth', 'compType')),
            'bgra_order': bool(value.BGRAOrder()),
            'srgb_corrected': bool(value.SRGBCorrected())}


def describe_texture(value):
    return {**fields(value, ('width', 'height', 'depth', 'dimension', 'mips',
                            'arraysize', 'msSamp', 'byteSize', 'type', 'creationFlags')),
            'resource_id': resource_id(value.resourceId), 'format': format_info(value.format)}


def describe_descriptor(value, textures, buffers):
    rid = resource_id(value.resource)
    output = {**fields(value, ('type', 'flags', 'byteOffset', 'byteSize', 'elementByteSize',
                              'firstMip', 'numMips', 'firstSlice', 'numSlices', 'minLODClamp',
                              'textureType')),
              'resource_id': rid, 'view_id': resource_id(value.view),
              'secondary_resource_id': resource_id(value.secondary),
              'view_format': format_info(value.format),
              'swizzle': fields(value.swizzle, ('red', 'green', 'blue', 'alpha'))}
    if rid in textures:
        output['texture'] = describe_texture(textures[rid])
    elif rid in buffers:
        output['buffer'] = fields(buffers[rid], ('gpuAddress', 'length', 'creationFlags'))
    return output


def describe_used(value, reflected, textures, buffers):
    access = value.access
    index = int(access.index)
    output = {'access': {**fields(access, ('stage', 'type', 'index', 'arrayElement',
                                         'byteOffset', 'byteSize', 'staticallyUnused')),
                         'descriptor_store': resource_id(access.descriptorStore)},
              'descriptor': describe_descriptor(value.descriptor, textures, buffers)}
    if 0 <= index < len(reflected):
        output['reflection'] = fields(reflected[index],
                                     ('name', 'fixedBindNumber', 'fixedBindSetOrSpace',
                                      'bindArraySize', 'byteSize', 'bufferBacked',
                                      'inlineDataBytes', 'compileConstants', 'isTexture'))
    return output


def flatten_actions(actions, structured_file, max_actions=200000):
    """Avoid recursive traversal and reject malformed/cyclic event trees."""
    result, seen = [], set()
    stack = [(action, []) for action in reversed(list(actions))]
    while stack:
        require(len(result) < max_actions, 'Action tree exceeds --max-actions')
        action, parents = stack.pop()
        event_id = int(action.eventId)
        require(event_id > 0 and event_id not in seen,
                'Action tree has nonpositive or duplicate event ID')
        seen.add(event_id)
        result.append((action, {'event_id': event_id, 'name': action.GetName(structured_file),
                               'flags': str(action.flags), 'parent_event_ids': parents,
                               **fields(action, ('numIndices', 'numInstances')),
                               'dispatch_dimensions': list(action.dispatchDimension)}))
        stack.extend((child, parents + [event_id]) for child in reversed(list(action.children)))
    return result


def float4_storage(data):
    """Interpret complete 16-byte rows as little-endian binary32, without semantics."""
    rows = []
    for offset in range(0, len(data) - 15, 16):
        rows.append([number if math.isfinite(number) else str(number)
                     for number in struct.unpack_from('<4f', data, offset)])
    return {'byte_order': 'little-endian', 'binary32_float4': rows,
            'trailing_bytes': len(data) % 16,
            'meaning': 'Storage interpretation only; not proof of declared numeric types.'}


class ArtifactWriter:
    def __init__(self, root, max_resource_bytes, max_total_bytes, max_resources):
        self.root = root
        self.max_resource_bytes = max_resource_bytes
        self.max_total_bytes = max_total_bytes
        self.max_resources = max_resources
        self.total_bytes = 0
        self.resource_count = 0
        self.files = []

    def reserve(self, size):
        require(isinstance(size, int) and size > 0, 'Invalid resource size')
        require(size <= self.max_resource_bytes, 'Resource exceeds --max-resource-mib')
        require(self.total_bytes + size <= self.max_total_bytes,
                'Resources exceed --max-total-mib')
        require(self.resource_count < self.max_resources, 'Resources exceed --max-resources')

    def record(self, path):
        size = path.stat().st_size
        require(size > 0, 'Export produced an empty file')
        require(size <= self.max_resource_bytes, 'Export exceeds --max-resource-mib')
        require(self.total_bytes + size <= self.max_total_bytes, 'Exports exceed --max-total-mib')
        self.total_bytes += size
        self.resource_count += 1
        value = {'path': str(path.relative_to(self.root)), 'size_bytes': size,
                 'sha256': sha256_file(path)}
        self.files.append(value)
        return value

    def write(self, name, data):
        self.reserve(len(data))
        path = self.root / name
        with path.open('xb') as stream:
            stream.write(data)
        return self.record(path)


def buffer_range(descriptor, buffer, limit):
    offset, size, length = int(descriptor.byteOffset), int(descriptor.byteSize), int(buffer.length)
    require(0 <= offset < length, 'Buffer view offset outside buffer')
    if size in (0, 0xFFFFFFFFFFFFFFFF):
        size = length - offset
    require(0 < size <= length - offset, 'Buffer view range outside buffer')
    require(size <= limit, 'Buffer view exceeds --max-resource-mib')
    return offset, size


def texture_selection(descriptor, texture, limit):
    mip, array_slice = int(descriptor.firstMip), int(descriptor.firstSlice)
    require(0 <= mip < int(texture.mips), 'Texture view mip outside texture')
    require(0 <= array_slice < (int(texture.arraysize) if texture.dimension != 3 else texture.depth),
            'Texture view slice outside texture')
    width, height, depth = [max(1, int(getattr(texture, name)) >> mip)
                            for name in ('width', 'height', 'depth')]
    # A conservative pre-read bound also covers packed/depth formats. Unknown
    # unusually large formats are rejected by the returned-size guard afterward.
    regular_bytes = max(16, int(texture.format.compCount) * int(texture.format.compByteWidth))
    estimate = width * height * depth * regular_bytes
    require(estimate <= limit, 'Texture subresource exceeds --max-resource-mib estimate')
    return {'mip': mip, 'slice': 0 if texture.dimension == 3 else array_slice, 'sample': 0,
            'width': width, 'height': height, 'depth': depth,
            'estimated_max_bytes': estimate,
            'whole_depth_mip': texture.dimension == 3}


def export_descriptor(rd, controller, descriptor, textures, buffers, writer, prefix, previews):
    rid = resource_id(descriptor.resource)
    if rid in buffers:
        offset, length = buffer_range(descriptor, buffers[rid], writer.max_resource_bytes)
        writer.reserve(length)
        data = bytes(controller.GetBufferData(descriptor.resource, offset, length))
        require(len(data) == length, 'RenderDoc returned a truncated buffer range')
        return {'kind': 'buffer', 'byte_offset': offset, 'byte_length': length,
                'raw': writer.write(prefix + '.bin', data),
                'storage_decode': float4_storage(data) if length <= 65536 else None}
    if rid not in textures:
        return {'status': 'not-exported', 'reason': 'No matching texture/buffer description'}
    texture = textures[rid]
    selection = texture_selection(descriptor, texture, writer.max_resource_bytes)
    writer.reserve(selection['estimated_max_bytes'])
    subresource = rd.Subresource()
    subresource.mip, subresource.slice, subresource.sample = (
        selection['mip'], selection['slice'], selection['sample'])
    data = bytes(controller.GetTextureData(descriptor.resource, subresource))
    require(len(data) <= selection['estimated_max_bytes'],
            'RenderDoc texture data exceeds conservative pre-read size estimate')
    result = {'kind': 'texture', 'selection': selection,
              'raw': writer.write(prefix + '.bin', data)}
    if previews:
        # SaveTexture may allocate a conversion buffer. Bound dimensions before
        # calling it; it writes only this chosen mip, array/depth slice and sample.
        writer.reserve(selection['width'] * selection['height'] * 16 + 65536)
        settings = rd.TextureSave()
        settings.resourceId, settings.destType = descriptor.resource, rd.FileType.PNG
        settings.mip, settings.slice.sliceIndex = selection['mip'], selection['slice']
        settings.sample.sampleIndex = 0
        settings.comp.blackPoint, settings.comp.whitePoint = 0.0, 1.0
        settings.typeCast = descriptor.format.compType
        path = writer.root / (prefix + '.png')
        status = controller.SaveTexture(settings, str(path))
        require(status == rd.ResultCode.Succeeded, 'RenderDoc SaveTexture failed: ' + str(status))
        result['preview'] = {**writer.record(path), 'format': 'PNG',
                             'black_point': 0.0, 'white_point': 1.0,
                             'type_cast': str(settings.typeCast),
                             'purpose': 'Visual inspection only; no native numeric preservation.'}
    return result


def inspect_event(rd, controller, event_id, textures, buffers, writer=None, previews=False,
                  action_kind=None):
    require(action_kind in (None, 'draw', 'dispatch'), 'Unknown action kind')
    controller.SetFrameEvent(event_id, True)
    pipeline = controller.GetPipelineState()
    result = {'event_id': event_id, 'resource_snapshot': 'after-selected-event',
              'pipeline_scope': {'draw': 'graphics', 'dispatch': 'compute'}.get(action_kind, 'unspecified'),
              'stages': [], 'outputs': []}
    if action_kind != 'dispatch':
        result['viewport_0'] = fields(pipeline.GetViewport(0),
                                      ('x', 'y', 'width', 'height', 'minDepth', 'maxDepth', 'enabled'))
        result['scissor_0'] = fields(pipeline.GetScissor(0), ('x', 'y', 'width', 'height', 'enabled'))
    if writer:
        vulkan = controller.GetVulkanPipelineState()
        if vulkan is not None:
            push_constants = bytes(vulkan.pushconsts)
            require(len(push_constants) <= 65536, 'Vulkan push constants exceed 65536-byte bound')
            result['vulkan_push_constants'] = {
                'size_bytes': len(push_constants), 'hex': push_constants.hex(),
                'interpretation': 'Raw bound Vulkan push-constant storage; no numeric or address semantics.'}
            if push_constants:
                result['vulkan_push_constants']['raw'] = writer.write(
                    f'event-{event_id}-vulkan-push-constants.bin', push_constants)
    stage_names = {'draw': ('Vertex', 'Pixel'), 'dispatch': ('Compute',)}.get(
        action_kind, ('Vertex', 'Pixel', 'Compute'))
    for stage_name in stage_names:
        stage = getattr(rd.ShaderStage, stage_name)
        reflection = pipeline.GetShaderReflection(stage)
        if reflection is None:
            continue
        raw_shader = bytes(reflection.rawBytes)
        record = {'stage': stage_name, 'shader_id': resource_id(pipeline.GetShader(stage)),
                  'entry_point': pipeline.GetShaderEntryPoint(stage),
                  'encoding': str(reflection.encoding), 'shader_size_bytes': len(raw_shader),
                  'shader_sha256': hashlib.sha256(raw_shader).hexdigest(),
                  'constant_blocks': [], 'read_only': [], 'read_write': [], 'samplers': []}
        record.update(shader_source_metadata(reflection, raw_shader))
        for signature_name in ('inputSignature', 'outputSignature'):
            record[signature_name] = [
                fields(parameter, ('varName', 'semanticName', 'semanticIndex', 'regIndex',
                                   'systemValue', 'varType', 'regChannelMask', 'channelUsedMask',
                                   'compCount', 'stream'))
                for parameter in getattr(reflection, signature_name)]
        if writer and raw_shader:
            record['shader_raw'] = writer.write(f'event-{event_id}-{stage_name}-shader.bin', raw_shader)
            pipeline_object = (pipeline.GetComputePipelineObject() if stage_name == 'Compute'
                               else pipeline.GetGraphicsPipelineObject())
            disassembly = str(controller.DisassembleShader(pipeline_object, reflection, ''))
            record['disassembly'] = {
                **writer.write(f'event-{event_id}-{stage_name}-shader.txt',
                               disassembly.encode('utf-8')),
                'target': 'RenderDoc default', 'pipeline_id': resource_id(pipeline_object),
                'interpretation': 'RenderDoc disassembly response; may contain an API error message.'}
        for category, method, reflected in (
                ('constant_blocks', 'GetConstantBlocks', reflection.constantBlocks),
                ('read_only', 'GetReadOnlyResources', reflection.readOnlyResources),
                ('read_write', 'GetReadWriteResources', reflection.readWriteResources)):
            for index, used in enumerate(getattr(pipeline, method)(stage, False)):
                item = describe_used(used, reflected, textures, buffers)
                if writer:
                    try:
                        item['export'] = export_descriptor(
                            rd, controller, used.descriptor, textures, buffers, writer,
                            f'event-{event_id}-{stage_name}-{category}-{index}', previews)
                    except ValueError as error:
                        item['export'] = {'status': 'not-exported', 'reason': str(error)}
                record[category].append(item)
        for used in pipeline.GetSamplers(stage, False):
            item = {'access': fields(used.access, ('stage', 'type', 'index', 'arrayElement',
                                                  'staticallyUnused')),
                    'sampler': fields(used.sampler, ('type', 'object', 'addressU', 'addressV',
                                                     'addressW', 'compareFunction', 'maxAnisotropy',
                                                     'minLOD', 'maxLOD', 'mipBias', 'borderColorType',
                                                     'unnormalized', 'srgbBorder')),
                    'filter': fields(used.sampler.filter, ('minify', 'magnify', 'mip', 'filter')),
                    'border_color_float': finite_list(used.sampler.borderColorValue.floatValue),
                    'border_color_uint': list(used.sampler.borderColorValue.uintValue)}
            index = int(used.access.index)
            if 0 <= index < len(reflection.samplers):
                item['reflection'] = fields(reflection.samplers[index],
                                            ('name', 'fixedBindNumber', 'fixedBindSetOrSpace'))
            record['samplers'].append(item)
        result['stages'].append(record)
    output_targets = pipeline.GetOutputTargets() if action_kind != 'dispatch' else ()
    for index, descriptor in enumerate(output_targets):
        item = describe_descriptor(descriptor, textures, buffers)
        if writer:
            try:
                item['export'] = export_descriptor(rd, controller, descriptor, textures, buffers,
                                                   writer, f'event-{event_id}-output-{index}', previews)
            except ValueError as error:
                item['export'] = {'status': 'not-exported', 'reason': str(error)}
        result['outputs'].append(item)
    if action_kind != 'dispatch':
        result['depth_target'] = describe_descriptor(pipeline.GetDepthTarget(), textures, buffers)
        if hasattr(pipeline, 'GetBlendFactor'):
            result['blend_factor'] = finite_list(pipeline.GetBlendFactor())
        result['color_blends'] = []
    for blend in (pipeline.GetColorBlends() if action_kind != 'dispatch' else ()):
        result['color_blends'].append({**fields(blend, ('enabled', 'logicOperationEnabled',
                                                       'logicOperation', 'writeMask')),
                                      'color': fields(blend.colorBlend,
                                                      ('source', 'destination', 'operation')),
                                      'alpha': fields(blend.alphaBlend,
                                                      ('source', 'destination', 'operation'))})
    return result


def collect(rd, capture_path, output, events=(), tail=64, previews=False,
            max_resource_bytes=64 * 1024 * 1024, max_total_bytes=512 * 1024 * 1024,
            max_resources=256, max_actions=200000):
    capture_path = capture_path.resolve(strict=True)
    require(capture_path.is_file(), 'Capture must be a regular file')
    require(not output.exists(), 'Output directory already exists')
    require(0 <= tail <= 10000, '--tail must be between 0 and 10000')
    require(all(event > 0 for event in events), '--event IDs must be positive')
    require(len(events) == len(set(events)), 'Duplicate --event IDs')
    require(0 < max_resource_bytes <= max_total_bytes <= 4 * 1024 ** 3,
            'Export byte limits must be positive, ordered, and at most 4096 MiB total')
    require(0 < max_resources <= 10000 and 0 < max_actions <= 1000000,
            'Invalid max-resources or max-actions limit')
    output.mkdir(parents=True, mode=0o700)
    writer = ArtifactWriter(output, max_resource_bytes, max_total_bytes, max_resources)
    receipt = {'schema_version': 1, 'status': 'incomplete',
               'created_utc': datetime.now(timezone.utc).isoformat(),
               'capture': {'name': capture_path.name, 'size_bytes': capture_path.stat().st_size,
                           'sha256': sha256_file(capture_path)},
               'collector_sha256': sha256_file(Path(__file__).resolve()),
               'renderdoc_version': rd.GetVersionString(), 'limitations': LIMITATIONS,
               'requested_events': list(events), 'pipeline_tail_count': tail,
               'limits': {'max_resource_bytes': max_resource_bytes,
                          'max_total_bytes': max_total_bytes,
                          'max_resources': max_resources, 'max_actions': max_actions}}
    cap = controller = None
    replay_started = False
    try:
        rd.InitialiseReplay(rd.GlobalEnvironment(), [])
        replay_started = True
        cap = rd.OpenCaptureFile()
        status = cap.OpenFile(str(capture_path), '', None)
        require(status == rd.ResultCode.Succeeded, 'Capture open failed: ' + str(status))
        require(bool(cap.LocalReplaySupport()), 'Capture has no local replay support')
        status, controller = cap.OpenCapture(rd.ReplayOptions(), None)
        require(status == rd.ResultCode.Succeeded and controller is not None,
                'Capture replay initialization failed: ' + str(status))
        receipt['api_properties'] = fields(controller.GetAPIProperties(),
                                            ('pipelineType', 'localRenderer', 'degraded'))
        receipt['resources'] = [
            {'resource_id': resource_id(item.resourceId),
             **fields(item, ('name', 'autogeneratedName', 'type'))}
            for item in controller.GetResources()]
        resource_names = {item['resource_id']: item.get('name') for item in receipt['resources']}
        textures = {resource_id(item.resourceId): item for item in controller.GetTextures()}
        buffers = {resource_id(item.resourceId): item for item in controller.GetBuffers()}
        receipt['textures'] = [describe_texture(item) for item in textures.values()]
        receipt['buffers'] = [{**fields(item, ('gpuAddress', 'length', 'creationFlags')),
                               'gpu_address_hex': hex(int(item.gpuAddress)),
                               'resource_id': rid, 'resource_name': resource_names.get(rid)}
                              for rid, item in buffers.items()]
        actions = flatten_actions(controller.GetRootActions(), controller.GetStructuredFile(),
                                  max_actions)
        receipt['actions'] = [item for _, item in actions]
        work = [item['event_id'] for action, item in actions
                if action.flags & (rd.ActionFlags.Drawcall | rd.ActionFlags.Dispatch)]
        valid_events = set(work)
        action_kinds = {item['event_id']: ('dispatch' if action.flags & rd.ActionFlags.Dispatch else 'draw')
                        for action, item in actions if item['event_id'] in valid_events}
        require(set(events) <= valid_events, '--event must select a draw or dispatch action')
        selected = sorted(set(events) | (set(work[-tail:]) if tail else set()))
        receipt['pipelines'] = []
        for event in selected:
            receipt['pipelines'].append(inspect_event(rd, controller, event, textures, buffers,
                                                      writer if event in events else None,
                                                      previews, action_kinds[event]))
        receipt['capture']['sha256_after_replay'] = sha256_file(capture_path)
        require(receipt['capture']['sha256'] == receipt['capture']['sha256_after_replay'],
                'Capture changed during replay')
        receipt['status'] = 'complete'
    except Exception as error:
        receipt['status'] = 'failed'
        receipt['error'] = {'type': type(error).__name__, 'message': str(error)}
        raise
    finally:
        if controller is not None:
            controller.Shutdown()
        if cap is not None:
            cap.Shutdown()
        if replay_started:
            rd.ShutdownReplay()
        receipt['artifacts'] = writer.files
        receipt['exported_bytes'] = writer.total_bytes
        with (output / 'receipt.json').open('x') as stream:
            json.dump(receipt, stream, indent=2, allow_nan=False)
            stream.write('\n')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--event', type=int, action='append', default=[])
    parser.add_argument('--tail', type=int, default=64,
                        help='Inspect last N draw/dispatch pipelines without exporting (default64)')
    parser.add_argument('--previews', action='store_true', help='Also export clamped PNG previews')
    parser.add_argument('--max-resource-mib', type=int, default=64)
    parser.add_argument('--max-total-mib', type=int, default=512)
    parser.add_argument('--max-resources', type=int, default=256)
    parser.add_argument('--max-actions', type=int, default=200000)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        import renderdoc as rd
        result = collect(rd, args.capture, args.output, args.event, args.tail, args.previews,
                         args.max_resource_mib * 1024 ** 2, args.max_total_mib * 1024 ** 2,
                         args.max_resources, args.max_actions)
    except Exception as error:
        print(f'{type(error).__name__}: {error}', file=sys.stderr)
        return 1
    print(json.dumps({'status': result['status'], 'capture_sha256': result['capture']['sha256'],
                      'actions': len(result['actions']), 'pipelines': len(result['pipelines']),
                      'exported_bytes': result['exported_bytes'],
                      'receipt': str(args.output / 'receipt.json')}))
    return 0


if __name__ == '__main__':
    sys.exit(main())
