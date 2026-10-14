#!/usr/bin/env python3
"""Validate rendering-map identities, references and coverage bounds.

This checks research artifacts, not shader semantics or retail visual parity.
With --target, also check identities/embedded bytes and curated output-transfer
instruction bytes against the original PE. --native-output-evidence additionally
checks that extension's private exports and saved LLVM receipt hashes; it does
not add its overlapping evidence to the central native union.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import struct


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def check(condition, message):
    if not condition:
        raise ValueError(message)


def objects(value):
    """Visit nested interpreted receipts and control/equation nodes."""
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from objects(child)
    elif isinstance(value, list):
        for child in value:
            yield from objects(child)


def original_span(identity, pe_bytes, address, size):
    """Resolve only fully file-backed spans; mapped zero-fill is not original bytes."""
    rva = int(address, 16) - int(identity['image_base'], 16)
    section = next((s for s in identity['sections']
                    if s['rva'] <= rva and rva + size <= s['rva'] + s['raw_size']), None)
    if section is None:
        return None
    offset = section['raw_offset'] + rva - section['rva']
    return pe_bytes[offset:offset + size]


def indirect_call_displacement(code):
    """Decode only the FF /2 base+disp8/disp32 forms used by the five receipts."""
    if code and 0x40 <= code[0] <= 0x4f:
        code = code[1:]
    check(len(code) >= 3 and code[0] == 0xff, 'Output API call has no supported FF encoding')
    modrm = code[1]
    mode = modrm >> 6
    check((modrm >> 3) & 7 == 2 and modrm & 7 != 4,
          'Output API call is not the supported indirect CALL form')
    check((mode == 1 and len(code) == 3) or (mode == 2 and len(code) == 6),
          'Output API call has an unsupported displacement form')
    return struct.unpack('<b' if mode == 1 else '<i', code[2:])[0]


def check_output_transfer(report, identity, connections, abi, pe_bytes=None,
                          evidence=None, native_evidence=None):
    """Check scoped receipt coherence/bytes, not reconstruct full register data flow.

    SDK call offsets and rel32 edges are independently decoded here. Object,
    guard and argument meanings remain interpreted static receipts with explicit
    prerequisites. Saved LLVM hashes are checked; LLVM is not rerun by this tool.
    """
    check(report['schema'] == 'mudcrab-native-output-transfer/v1', 'Unknown output-transfer schema')
    target = report['target']
    check(target['sha256'] == identity['sha256'] and target['version'] == '1.7.104.0'
          and target['image_base'] == identity['image_base'], 'Output-transfer target mismatch')
    for record in objects(report):
        for field, value in record.items():
            if field.endswith('_observed') or field in ['full_selected_dispatch_and_pixel_output_proven', 'se_ae_build_generalization']:
                check(value is False, 'Output-transfer static scope promoted: ' + field)
    check(report['preconditions'] and report['unresolved'] and report['rejected_inferences'],
          'Output-transfer evidence boundaries missing')
    method = report['query_method']
    check(method['read_only'] and method['noanalysis'] and not method['cold_import']
          and not method['binary_executed'] and method['per_query_timeout_seconds'] == 60,
          'Output-transfer query boundary mismatch')
    verification = report['verification']
    check(verification['target_sha256'] == identity['sha256'] and verification['all_pass']
          and not verification['original_span_mismatches']
          and not verification['original_instruction_mismatches']
          and verification['llvm_mismatch_count'] == 0, 'Output-transfer recorded comparison failure')
    check(len(report['new_queries']) == 5 and len(report['cached_sources']) == 6,
          'Output-transfer source scope changed')
    source_pins = {pin['path']: pin for pin in report['new_queries'] + report['cached_sources']}
    check(len(source_pins) == 11, 'Duplicate output-transfer source pin')
    for pin in source_pins.values():
        check(re.fullmatch(r'[0-9a-f]{64}', pin['sha256']), 'Invalid output-transfer source hash')

    receipts = report['instruction_receipts']
    check(len({r['id'] for r in receipts}) == len(receipts) == 33, 'Output-transfer receipt scope changed')
    starts = {}
    for receipt in receipts:
        pin = source_pins.get(receipt['source'])
        check(pin and pin['sha256'] == receipt['source_sha256'], 'Output-transfer receipt source mismatch')
        check(receipt['instructions'], 'Empty output-transfer receipt')
        for instruction in receipt['instructions']:
            address = instruction['address'].lower()
            code = bytes.fromhex(instruction['loaded_bytes_hex'])
            check(0 < len(code) <= 15, 'Output-transfer instruction length invalid')
            if address in starts:
                check(starts[address] == instruction, 'Conflicting output-transfer instruction receipts')
            starts[address] = instruction
            if pe_bytes is not None:
                check(original_span(identity, pe_bytes, address, len(code)) == code,
                      'Output-transfer receipt differs from original PE: ' + address)
    curated = report['curated_verification']
    check(curated['receipt_count'] == len(receipts)
          and curated['unique_original_instruction_count'] == len(starts) == 1569
          and curated['original_mismatch_count'] == curated['llvm_mismatch_count'] == curated['direct_edge_mismatch_count'] == 0
          and curated['counts_overlap_prior_evidence'], 'Output-transfer curated comparison/count mismatch')
    check(curated['llvm_compared_selected_instructions'] == len(starts)
          and curated['independently_decoded_function_ranges'] == len(curated['llvm']) == 30,
          'Output-transfer selected LLVM scope mismatch')

    edges = report['direct_edges']
    check(len({e['id'] for e in edges}) == len(edges) == curated['signed_rel32_edge_count'] == 24,
          'Output-transfer direct-edge scope changed')
    for edge in edges:
        instruction = starts.get(edge['site'].lower())
        check(instruction and edge['signed_rel32_verified'], 'Output-transfer edge lacks a receipt')
        code = bytes.fromhex(edge['original_bytes'])
        check(instruction['loaded_bytes_hex'] == edge['original_bytes'] and len(code) == 5 and code[0] in [0xe8, 0xe9],
              'Output-transfer edge encoding mismatch')
        check(int(edge['site'], 16) + 5 + struct.unpack_from('<i', code, 1)[0] == int(edge['target'], 16),
              'Output-transfer edge differs from signed rel32')

    interfaces = {i['name']: i for i in abi['interfaces']}
    check(len(report['api_calls']) == curated['abi_call_count'] == 5, 'Output-transfer API scope changed')
    for call in report['api_calls']:
        interface = interfaces[call['interface']]
        sdk = next(m for m in interface['methods'] if m['name'] == call['method'])
        check(sdk['x64_offset'] == call['vtable_offset'] and sdk['source_line'] == call['header_source_line']
              and interface['source_sha256'] == call['header_source_sha256'], 'Output-transfer SDK receipt mismatch')
        instruction = starts.get(call['native_site'].lower())
        code = bytes.fromhex(call['original_bytes'])
        check(instruction and instruction['loaded_bytes_hex'] == call['original_bytes'], 'Output-transfer API byte receipt mismatch')
        check(indirect_call_displacement(code) == int(call['vtable_offset'], 16), 'Output-transfer API encoded offset mismatch')

    owner = report['owner_alias']
    backbuffer = report['backbuffer']
    check(int(owner['static_data_address'], 16) - int(owner['frame_receiver_address'], 16) == owner['offset_bytes'] == 16,
          'Output-transfer renderer/data alias mismatch')
    check(backbuffer['window_base_from_renderer'] + backbuffer['window_rtv_offset'] - owner['offset_bytes'] == backbuffer['data_window_rtv_offset']
          and backbuffer['window_base_from_renderer'] + backbuffer['window_srv_offset'] - owner['offset_bytes'] == backbuffer['data_window_srv_offset'],
          'Output-transfer view offsets use inconsistent receivers')
    check(next(f['value'] for f in abi['dxgi_formats'] if f['name'] == backbuffer['swapchain_format_name']) == backbuffer['swapchain_format_value'] == 28,
          'Output-transfer backbuffer format mismatch')
    check(report['selected_output_flow']['requested_output_index'] == 0
          and not report['selected_output_flow']['full_selected_dispatch_and_pixel_output_proven']
          and not report['target_binding']['descriptor_copy_is_target_allocation']
          and not report['target_binding']['viewport_helper_is_om_bind'], 'Output-transfer role/scope mismatch')
    slot_links = {link['index']: link for link in connections['effect_group_links']}
    registrations = report['registration_source']['entries']
    check(len(registrations) == 4 and {r['index'] for r in registrations} == {151, 152, 153, 154},
          'Output-transfer post-tone shader scope changed')
    check(all(r == slot_links[r['index']] for r in registrations), 'Output-transfer shader registration differs from connection catalog')
    check(len(report['post_tone_helpers']) == 2, 'Output-transfer helper count changed')
    for helper in report['post_tone_helpers']:
        check(helper['stored_shader_slots'] and len(helper['stored_shader_slots']) == len(helper['stored_shader_fields']) == len(helper['names']),
              'Output-transfer helper shader fields differ')
        check(helper['names'] == [slot_links[slot]['label'] for slot in helper['stored_shader_slots']],
              'Output-transfer helper shader identity differs')

    summary = dict(metadata_checked=True, curated_instruction_receipts=len(receipts),
                   unique_curated_instruction_starts=len(starts), signed_rel32_transfers_checked=len(edges),
                   sdk_call_offsets_checked=len(report['api_calls']), original_curated_instruction_bytes_checked=pe_bytes is not None,
                   private_exports_checked=False, saved_llvm_receipt_hashes_checked=False,
                   runtime_observed=False, complete_tone_to_present_pixel_transfer_proven=False,
                   numeric_domains_proven=False, added_to_central_native_union=False,
                   scope='Static receipt coherence, encoded calls/offsets and optional original bytes; receiver/control/argument interpretation is not independently reconstructed.')
    if evidence is None:
        return summary
    check(pe_bytes is not None, '--native-output-evidence requires --target')
    private_verification = evidence / 'verification.json'
    check(json.loads(private_verification.read_text()) == verification, 'Output-transfer private verification differs from report')
    check(digest(evidence / 'scripts/FogEvidence.java') == method['query_script_sha256']
          and digest(evidence / 'query.py') == method['driver_sha256']
          and digest(evidence / 'verify.py') == method['verification_script_sha256'], 'Output-transfer query/verification script changed')
    private_starts = {}
    private_functions = {}
    private_spans = []
    for pin in source_pins.values():
        path = Path(pin['path'])
        if pin in report['new_queries']:
            path = evidence / 'native' / path.name
            manifest_path = evidence / 'native' / Path(pin['manifest']).name
            check(digest(manifest_path) == pin['manifest_sha256'], 'Output-transfer query manifest changed')
            manifest = json.loads(manifest_path.read_text())
            check(manifest['exit_code'] == 0 and manifest['evidence_written'] and manifest['program_read_only']
                  and manifest['query_noanalysis'] and not manifest['cold_import'] and not manifest['binary_executed']
                  and manifest['timeout_seconds'] == 60 and manifest['elapsed_seconds'] <= 60,
                  'Output-transfer query manifest boundary differs')
        elif native_evidence is not None:
            # Cached pins retain their area/native/name suffix when the private
            # central evidence tree has been relocated.
            path = native_evidence / Path(*path.parts[-3:])
        check(digest(path) == pin['sha256'], 'Output-transfer private source changed: ' + str(path))
        export = json.loads(path.read_text())
        check(export['executable_sha256'] == identity['sha256'] and not export['analysis_enabled_this_run'],
              'Output-transfer private target/analysis mismatch')
        source_starts = {}
        new_query = pin in report['new_queries']
        for seed in export['seeds']:
            function = seed.get('function')
            if function:
                check(not function['instructions_truncated'], 'Output-transfer private function is truncated')
                if new_query:
                    private_functions[function['entry'].lower()] = function
            instructions = list(seed.get('requested_span', {}).get('existing_instructions', []))
            if function:
                instructions.extend(function['instructions'])
            for instruction in instructions:
                address = instruction['address'].lower()
                code = bytes.fromhex(instruction['loaded_bytes_hex'])
                if new_query:
                    check(original_span(identity, pe_bytes, address, len(code)) == code,
                          'Output-transfer private instruction differs from original PE')
                    if address in private_starts:
                        check(private_starts[address]['loaded_bytes_hex'] == instruction['loaded_bytes_hex'], 'Conflicting output-transfer private exports')
                    private_starts[address] = instruction
                source_starts[address] = instruction
            if new_query:
                span = seed['requested_span']
                code = bytes.fromhex(span['loaded_bytes_hex'])
                actual = original_span(identity, pe_bytes, seed['address'], len(code))
                check(actual is None or actual == code, 'Output-transfer private span differs from original PE')
                private_spans.append((seed['label'], seed['address'], len(code), actual is not None))
        for receipt in receipts:
            if receipt['source'] != pin['path']:
                continue
            for instruction in receipt['instructions']:
                source_instruction = source_starts.get(instruction['address'].lower())
                check(source_instruction and all(source_instruction[k] == instruction[k] for k in ['address', 'loaded_bytes_hex', 'disassembly']),
                      'Output-transfer public instruction differs from source export')
    check(len(private_spans) == verification['original_span_count'] == 24
          and len(private_starts) == verification['original_unique_instructions'] == 3633
          and len(private_functions) == verification['independently_decoded_functions'] == 23,
          'Output-transfer private export counts differ')
    check({q['artifact']: q['sha256'] for q in verification['queries']} == {Path(p['path']).name:p['sha256'] for p in report['new_queries']},
          'Output-transfer verification query provenance differs')
    check(sum(f['instruction_count'] for f in verification['functions']) == verification['llvm_compared_instructions'] == 1776,
          'Output-transfer full-function LLVM scope differs')
    for function in verification['functions']:
        check(not function['instruction_truncated'] and not function['llvm_mismatches']
              and digest(evidence / 'llvm' / (function['entry'].lower() + '.asm')) == function['llvm_artifact_sha256'],
              'Output-transfer saved full-function LLVM receipt changed')
    for function in curated['llvm']:
        check(function['mismatch_count'] == 0
              and digest(evidence / 'llvm' / Path(function['llvm_artifact']).name) == function['llvm_sha256'],
              'Output-transfer saved selected LLVM receipt changed')
    summary.update(private_exports_checked=True, saved_llvm_receipt_hashes_checked=True,
                   new_query_count=len(report['new_queries']), new_export_unique_instruction_starts=len(private_starts),
                   new_export_functions=len(private_functions), requested_spans=len(private_spans),
                   file_backed_requested_spans=sum(p[3] for p in private_spans),
                   non_file_backed_data_spans=sum(not p[3] for p in private_spans))
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', type=Path, help='Original SkyrimSE.exe; read only')
    parser.add_argument('--shader-archive', type=Path, help='Original Skyrim - Shaders.bsa; read only')
    parser.add_argument('--native-evidence', type=Path, help='Private render-map root; verify native exports against --target')
    parser.add_argument('--native-output-evidence', type=Path, help='Private native-output extension root; check exports/LLVM receipt hashes separately, requires --target')
    parser.add_argument('--project-source-evidence', type=Path,
                        help='Frozen checkpoint workspace for historical project_source pins; record current source differences, requires parent receipt.json')
    parser.add_argument('--write-coverage', action='store_true', help='Refresh coverage.json after validation')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    folder = root / 'docs/research/skyrim-render-map'
    required = ['target-identity', 'native-types', 'native-render-imports', 'd3d11-abi',
                'shader-inventory', 'native-shader-loader', 'shader-selection-backlog',
                'embedded-shaders', 'input-contracts', 'frame-topology',
                'native-image-space-connections', 'image-space-arithmetic',
                'mod-content-rendering', 'mod-rendering-hooks', 'native-output-transfer']
    docs = {name: json.loads((folder / (name + '.json')).read_text()) for name in required}
    identity = docs['target-identity']
    target_hash = identity['sha256']
    check(re.fullmatch(r'[0-9a-f]{64}', target_hash), 'Invalid target identity')
    check(identity['version_resources'][0]['file_version'] == [1, 7, 104, 0], 'Unexpected target version')
    for name in ['native-types', 'native-render-imports']:
        check(docs[name]['target_sha256'] == target_hash, name + ' target mismatch')
    for name, document in docs.items():
        for match in re.findall(r'\b0x14[0-9a-fA-F]{7,}\b', json.dumps(document)):
            rva = int(match, 16) - int(identity['image_base'], 16)
            check(rva == 0 or any(s['rva'] <= rva < s['rva'] + max(s['virtual_size'], s['raw_size']) for s in identity['sections']),
                  name + ' contains an address outside the selected module: ' + match)

    shaders = docs['shader-inventory']
    programs = shaders['programs']
    profiles = shaders['profiles_by_bytecode_sha256']
    groups = {g['id']: g for g in shaders['groups']}
    check(len(groups) == 135, 'Package group count mismatch')
    check(len(programs) == 16044, 'Package program count mismatch')
    check(len(profiles) == 8057, 'Package unique bytecode count mismatch')
    check(len({p['id'] for p in programs}) == len(programs), 'Duplicate package program IDs')
    check(dict(Counter(p['stage'] for p in programs)) == {'vs': 3390, 'ps': 12635, 'cs': 19}, 'Stage counts mismatch')
    referenced_hashes = set()
    for program in programs:
        check(program['group_id'] in groups, 'Unresolved group ' + program['id'])
        key = program['bytecode_sha256']
        check(key in profiles, 'Unresolved bytecode ' + program['id'])
        profile = profiles[key]
        check(profile['stage'] == program['stage'], 'Program/profile stage mismatch')
        check(profile['byte_length'] == program['byte_length'], 'Program/profile length mismatch')
        check(0 <= program['package_bytecode_offset'] <= shaders['source']['embedded_byte_length'] - program['byte_length'], 'Package span out of bounds')
        check(not profile['reflection_names_available'] and 'RDEF' not in profile['chunks'], 'Unexpected package reflection')
        for field, value in profile.items():
            if field.endswith('_id'):
                registry = field[:-3]
                check(value in shaders['registries'][registry], 'Unresolved registry ' + field)
        referenced_hashes.add(key)
    check(referenced_hashes == set(profiles), 'Unused or missing bytecode profiles')
    check(all(shaders['validation'][k] for k in shaders['validation'] if k.startswith('all_')), 'Package validation failure')
    check(shaders['validation']['consumed_package_bytes'] == shaders['source']['embedded_byte_length'], 'Package not fully consumed')
    records_by_group = {key: [] for key in groups}
    for program in programs:
        records_by_group[program['group_id']].append(program)
    metadata_bytes = {'vs': 32, 'ps': 68, 'cs': 36}
    package_cursor = 0
    for group in sorted(groups.values(), key=lambda g: g['ordinal']):
        check(group['package_start'] == package_cursor, 'Gap or overlap between package groups')
        stages = ['cs'] if 'cs' in group['program_counts'] else ['vs', 'ps']
        cursor = group['package_start'] + (4 if stages == ['cs'] else 8)
        group_records = records_by_group[group['id']]
        check(Counter(p['stage'] for p in group_records) == group['program_counts'], 'Group record counts mismatch')
        for stage in stages:
            records = sorted((p for p in group_records if p['stage'] == stage), key=lambda p: p['ordinal_in_stage'])
            for ordinal, program in enumerate(records):
                check(program['ordinal_in_stage'] == ordinal and program['package_entry_offset'] == cursor, 'Noncontiguous package records')
                check(program['package_metadata_length'] == metadata_bytes[stage]
                      and program['package_bytecode_offset'] == cursor + 12 + metadata_bytes[stage], 'Package record layout mismatch')
                cursor = program['package_bytecode_offset'] + program['byte_length']
        check(cursor == group['package_end'], 'Package group cursor mismatch')
        package_cursor = cursor
    check(package_cursor == shaders['source']['embedded_byte_length'], 'Package group regions do not cover full payload')

    loader = docs['native-shader-loader']
    check(loader['target_sha256'] == target_hash, 'Shader loader target mismatch')
    check(loader['source_package_sha256'] == shaders['source']['package_sha256'], 'Shader loader package mismatch')
    check(loader['validation']['all_pass'] and not loader['validation']['runtime_observed'], 'Shader loader verification/scope mismatch')
    for field in ['llvm_mismatch_count', 'original_instruction_mismatch_count', 'original_span_mismatch_count']:
        check(loader['validation'][field] == 0, 'Shader loader comparison failure')
    links = loader['core_section_links'] + loader['image_section_links'] + loader['tail_compute_section_links']
    check(len(links) == 135 and {link['ordinal'] for link in links} == set(range(135)), 'Incomplete native group ordinals')
    check(len({link['group_id'] for link in links}) == 135, 'Duplicate native group association')
    check(loader['counts']['groups_with_native_static_ordinal_and_loader_links'] == 135, 'Native group count mismatch')
    check(shaders['counts']['unlabeled_groups'] == 0, 'Stale unnamed shader group count')
    loader_hash = digest(folder / 'native-shader-loader.json')
    tables = docs['native-types']['vtables']
    for link in links:
        group = groups[link['group_id']]
        check(group['ordinal'] == link['ordinal'] and group['package_start'] == link['package_start']
              and group['package_end'] == link['package_end'], 'Native group region mismatch')
        check(group['program_counts'] == link['program_counts'], 'Native group stage counts mismatch')
        check(group['label'] == link['native_loader_label'] and not link['draw_observed'], 'Native group label/scope mismatch')
        check(link['validated_program_records'] == sum(group['program_counts'].values()), 'Native group record count mismatch')
        proof = group['native_ordinal_proof']
        check(proof['sha256'] == loader_hash and not proof['runtime_observed'], 'Stale native group proof')
        check(proof['primary_vtable_va'] == link['primary_vtable_va'] and proof['native_type_name'] == link['native_type_name'], 'Native group type mismatch')
        check(any(table['vtable']['va'].lower() == link['primary_vtable_va'].lower()
                  and table['type_name'] == link['native_type_name'] for table in tables), 'Native group vtable not in original-byte catalog')
    by_program_id = {program['id']: program for program in programs}
    for interface in loader['selected_lighting_interfaces']:
        program = by_program_id[interface['id']]
        check(all(interface[field] == value for field, value in program.items()), 'Lighting interface differs from packaged record')
        check(not interface['draw_observed'], 'Lighting static interface marked observed')
    backlog = docs['shader-selection-backlog']
    check(backlog['counts']['groups_with_native_static_ordinal_links'] == 135
          and backlog['counts']['groups_with_observed_retail_execution'] == 0, 'Shader selection backlog count/scope mismatch')

    connections = docs['native-image-space-connections']
    check(connections['target_sha256'] == target_hash and connections['source_package_sha256'] == shaders['source']['package_sha256'], 'Image-space native identity mismatch')
    effect_links = connections['effect_group_links']
    check(len(effect_links) == 124 and {r['ordinal'] for r in effect_links} == set(range(9, 133)), 'Image-space slot coverage mismatch')
    check(len({r['index'] for r in effect_links}) == 124, 'Duplicate image-space effect slot')
    slot_links = {r['index']: r for r in effect_links}
    for link in effect_links:
        group = groups[link['group_id']]
        check(link['ordinal'] == group['ordinal'] and link['label'] == group['label'] and not link['runtime_observed'], 'Image-space slot identity/scope mismatch')
    for helper in connections['helper_objects']:
        check(not helper['runtime_observed'] and helper['constructor_argument_proof'], 'Unqualified image-space object')
        check(len({b['field_offset'] for b in helper['bindings']}) == len(helper['bindings']), 'Duplicate helper field')
        for binding in helper['bindings']:
            link = slot_links[binding['effect_slot']]
            check(binding['group_id'] == link['group_id'] and binding['ordinal'] == link['ordinal'] and binding['native_loader_label'] == link['label'], 'Helper field/shader slot mismatch')
    for guard in connections['guards_and_state']:
        check(guard['runtime_value'] is None, 'Unobserved guard given a live value')
    dispatch = connections['compute_dispatch']
    check(not dispatch['runtime_observed'], 'Uncaptured compute dispatch marked observed')
    for link in loader['image_section_links']:
        table = next(t for t in tables if t['vtable']['va'].lower() == link['primary_vtable_va'].lower())
        check(table['methods'][12]['va'].lower() == dispatch['effect_wrapper_va'].lower(), 'Image-space primary compute wrapper mismatch')
    owner_table = next(t for t in tables if t['vtable']['va'].lower() == dispatch['compute_owner_vtable'].lower())
    check(owner_table['methods'][2]['va'].lower() == dispatch['owner_method_va'].lower(), 'Compute owner callback mismatch')
    for api in dispatch['api_calls']:
        check(next(m['x64_offset'] for i in docs['d3d11-abi']['interfaces'] if i['name'] == api['interface'] for m in i['methods'] if m['name'] == api['method']) == api['vtable_offset'], 'Compute API slot mismatch')
    hdr = connections['hdr_parameter_connection']
    check(slot_links[hdr['selected_effect_slot']]['group_id'] == hdr['group_id'] and not hdr['runtime_observed'], 'HDR child shader/scope mismatch')

    arithmetic = docs['image-space-arithmetic']
    check(arithmetic['source']['catalog_sha256'] == digest(folder / 'shader-inventory.json')
          and arithmetic['source']['package_sha256'] == shaders['source']['package_sha256'], 'Shipped arithmetic identity mismatch')
    scoped_ordinals = set(arithmetic['scope']['group_ordinals'])
    scoped_records = arithmetic['records']
    equations = arithmetic['programs_by_bytecode_sha256']
    check(len(scoped_ordinals) == 54 and len(scoped_records) == 108 and len(equations) == 54, 'Shipped equation scope count mismatch')
    check({r['id'] for r in scoped_records} == {p['id'] for p in programs if groups[p['group_id']]['ordinal'] in scoped_ordinals}, 'Arithmetic omits a scoped serialized key/stage')
    for record in scoped_records:
        original = by_program_id[record['id']]
        check(all(record[k] == v for k, v in original.items()), 'Arithmetic record differs from original package catalog')
        check(record['native_label_evidence_sha256'] == loader_hash and record['label'] == groups[record['group_id']]['label'], 'Arithmetic native group identity mismatch')
        check(record['bytecode_sha256'] in equations, 'Arithmetic record lacks an equation program')
    check(sum(p['instruction_count'] for p in equations.values()) == 1821, 'Equation instruction count mismatch')
    for key, program in equations.items():
        count = program['instruction_count']
        ranges = program['source_instruction_byte_ranges']
        profile = profiles[key]
        for field in ['stage', 'shader_model', 'instruction_count', 'global_flags']:
            check(program[field] == profile[field], 'Equation program/profile mismatch: ' + field)
        for field, interface in program['interface'].items():
            check(interface == shaders['registries'][field][profile[field + '_id']], 'Equation interface/profile mismatch: ' + field)
        check(set(ranges) == {str(i) for i in range(1, count + 1)}, 'Equation program lacks exact instruction byte-range keys')
        covered = []
        for node in objects(program['equations']):
            for field in ['source_instruction', 'else_instruction', 'end_instruction']:
                if field in node:
                    check(type(node[field]) is int and 1 <= node[field] <= count, 'Equation control index has wrong type or bounds')
            if 'op' in node:
                check(node['op'] in arithmetic['operation_semantics'], 'Equation expression has no declared operator semantics')
            if 'literal_bits32' in node:
                bits = node['literal_bits32']
                check(re.fullmatch(r'[0-9a-f]{8}', bits), 'Equation literal has invalid authoritative bits')
                packed = struct.pack('<I', int(bits, 16))
                check(node.get('as_int32') == struct.unpack('<i', packed)[0], 'Equation literal integer view differs from bits')
                if isinstance(node.get('as_float32'), (int, float)):
                    check(struct.pack('<f', node['as_float32']) == packed, 'Equation literal float view differs from bits')
            covered.extend(node[k] for k in ['source_instruction', 'else_instruction', 'end_instruction'] if k in node)
        check(Counter(covered) == Counter(range(1, count + 1)), 'Equation control tree omits or duplicates an instruction')
        for start, end in ranges.values():
            check(0 <= start < end <= profiles[key]['byte_length'] and start % 4 == end % 4 == 0, 'Equation instruction span out of bytecode')

    content_mods = docs['mod-content-rendering']
    check(content_mods['target_native_executable_sha256'] == target_hash and content_mods['counts']['native_proof_promotions'] == 0, 'Content mod identity/proof boundary mismatch')
    source_ids = {s['id'] for s in content_mods['source_pins']}
    anchor_ids = {a['id'] for a in content_mods['native_anchors']}
    query_ids = {q['id'] for q in content_mods['query_queue']}
    check(len(source_ids) == 12 and len(content_mods['findings']) == 32, 'Content mod audit counts mismatch')
    for finding in content_mods['findings']:
        check(set(finding['primary_source_ids']) <= source_ids and set(finding['native_anchor_ids']) <= anchor_ids
              and set(finding['query_ids']) <= query_ids, 'Unresolved content mod finding reference')
        check(not finding['target_native_behavior_proved_by_this_finding'] and not finding['record_or_asset_diff_verified']
              and not finding['runtime_observed'], 'Author evidence promoted to native/archive/runtime proof')
    for pin in content_mods['input_anchor_evidence']:
        check(digest(root / pin['path']) == pin['sha256'], 'Content mod native anchor changed')
    if args.native_evidence:
        for pin in content_mods['source_pins']:
            check(digest(Path(pin['artifact_path'])) == pin['artifact_sha256'], 'Content mod source snapshot changed')
        for pin in content_mods['web_snapshot_manifest']:
            check(digest(Path(pin['path'])) == pin['sha256'], 'Content mod raw web snapshot changed')

    hook_mods = docs['mod-rendering-hooks']
    check(hook_mods['target_native_executable']['sha256'] == target_hash
          and hook_mods['method']['native_queries_executed'] == hook_mods['method']['retail_captures'] == 0, 'Hook mod target/proof boundary mismatch')
    repositories = {r['id']: r for r in hook_mods['repository_pins']}
    source_anchors = {a['id']: a for a in hook_mods['source_anchors']}
    hook_query_ids = {q['id'] for q in hook_mods['next_native_queries']}
    check(len(repositories) == 6 and len(source_anchors) == 101 and len(hook_mods['interventions']) == 22, 'Hook mod audit counts mismatch')
    for intervention in hook_mods['interventions']:
        check(not intervention['vanilla_implementation_proved_by_this_record']
              and set(intervention['source_anchors']) <= set(source_anchors)
              and set(intervention['next_native_query_ids']) <= hook_query_ids, 'Hook intervention proof/reference mismatch')
    for reference in hook_mods['literal_hook_and_relocation_references']:
        check(not reference['original_pe_verified'] and not reference['target_address_resolved'], 'Unresolved source hook promoted to original target proof')
    if args.native_evidence:
        for repository in repositories.values():
            check(re.fullmatch(r'[0-9a-f]{40}', repository['commit']) and digest(Path(repository['file_manifest_private'])) == repository['file_manifest_sha256'], 'Hook repo pin/manifest mismatch')
        for anchor in source_anchors.values():
            path = Path(repositories[anchor['repository']]['local_path']) / anchor['path']
            check(digest(path) == anchor['file_sha256'], 'Hook source file changed')
            lines = path.read_text(encoding='utf-8-sig').splitlines()
            start, end = anchor['line_start'], anchor['line_end']
            check(1 <= start <= end <= len(lines), 'Hook anchor line range invalid')
            selected = ('\n'.join(lines[start-1:end]) + '\n').encode()
            check(hashlib.sha256(selected).hexdigest() == anchor['selected_lines_sha256'], 'Hook selected source lines changed')

    embedded = docs['embedded-shaders']
    occurrences = embedded['occurrences']
    embedded_profiles = embedded['profiles_by_bytecode_sha256']
    check(len(occurrences) == 57 and len(embedded_profiles) == 53, 'Embedded count mismatch')
    check(len({p['id'] for p in occurrences}) == 57, 'Duplicate embedded IDs')
    check(dict(Counter(p['stage'] for p in occurrences)) == {'vs': 12, 'ps': 45}, 'Embedded stage mismatch')
    for occurrence in occurrences:
        check(occurrence['bytecode_sha256'] in embedded_profiles, 'Unresolved embedded profile')
        offset = int(occurrence['file_offset'], 16)
        check(0 <= offset <= identity['bytes'] - occurrence['byte_length'], 'Embedded span out of bounds')
        check('RDEF' in embedded_profiles[occurrence['bytecode_sha256']]['chunks'], 'Missing embedded reflection')

    native = docs['native-types']
    type_names = {t['name'] for t in native['types']}
    check(len(type_names) == native['coverage']['rendering_candidate_types'], 'RTTI count mismatch')
    check(len(native['vtables']) == native['coverage']['rendering_candidate_vtables'], 'Vtable count mismatch')
    for table in native['vtables']:
        check(table['type_name'] in type_names, 'Unresolved vtable type')
        check(table['hierarchy_va'] in native['hierarchies_by_va'], 'Unresolved hierarchy')
        for index, method in enumerate(table['methods']):
            check(method['slot'] == index, 'Vtable slot sequence mismatch')
            rva = int(method['va'], 16) - int(identity['image_base'], 16)
            check(any(s['characteristics'] & 0x20000000 and s['rva'] <= rva < s['rva'] + s['virtual_size'] for s in identity['sections']), 'Vtable pointer outside executable sections')

    frame = docs['frame-topology']
    check(frame['target']['sha256'] == target_hash, 'Frame target mismatch')
    verification = frame['verification']
    check(verification['all_file_backed_checks_pass'] and verification['instruction_mismatches'] == 0
          and verification['llvm_mismatches'] == 0, 'Frame byte verification failure')
    node_ids = {n['id'] for n in frame['graph']['nodes']}
    check(len(node_ids) == len(frame['graph']['nodes']), 'Duplicate frame node')
    for edge in frame['graph']['edges']:
        check(edge['source'] in node_ids and edge['target'] in node_ids, 'Unresolved frame graph edge')
        check(edge['runtime_observed'] is False and edge['execution_condition'], 'Unexpected or unqualified runtime edge')
    check(frame['runtime_route']['gpu_capture_verified'] is False, 'Update checker for captured runtime evidence')
    abi = docs['d3d11-abi']
    interfaces = {i['name']: i for i in abi['interfaces']}
    for interface, method, offset in [('IDXGISwapChain', 'Present', '0x40'),
                                      ('ID3D11Device', 'CreateInputLayout', '0x58'),
                                      ('ID3D11DeviceContext', 'IASetInputLayout', '0x88')]:
        check(next(m['x64_offset'] for m in interfaces[interface]['methods'] if m['name'] == method) == offset, 'ABI slot mismatch')
    check(next(f['value'] for f in abi['dxgi_formats'] if f['name'] == 'DXGI_FORMAT_R8G8B8A8_UNORM') == 28, 'ABI format mismatch')

    inputs = docs['input-contracts']
    contracts = inputs['contracts']
    check(len(contracts) == 45, 'Input domain count mismatch')
    evidence_ids = {e['id'] for e in inputs['evidence']}
    query_ids = {q['id'] for q in inputs['query_queue']}
    edge_ids = []
    statuses = Counter()
    for contract in contracts:
        check(not contract['effective_retail_state_observed'] and not contract['full_input_to_pixel_trace_closed'], 'Unobserved input marked closed')
        check(set(contract['query_ids']) <= query_ids, 'Unresolved input query')
        for edge in contract['connections']:
            check(set(edge['evidence_ids']) <= evidence_ids, 'Unresolved input evidence')
            if edge['status'] != 'open':
                check(edge['evidence_ids'], 'Non-open edge lacks evidence')
            edge_ids.append(edge['id'])
            statuses[edge['status']] += 1
    check(len(set(edge_ids)) == len(edge_ids), 'Duplicate input connection')
    check(inputs['scope_counts']['connections'] == len(edge_ids), 'Input connection count mismatch')
    check(inputs['scope_counts']['connections_by_status'] == dict(statuses), 'Input status counts mismatch')
    project_source_rows = []
    project_source_receipt = None
    if args.project_source_evidence:
        project_source_root = args.project_source_evidence.resolve()
        receipt_path = project_source_root.parent / 'receipt.json'
        receipt = json.loads(receipt_path.read_text())
        check(receipt['schema'] == 'mudcrab-render-source-checkpoint/v1',
              'Unknown historical project-source checkpoint schema')
        checkpoint_files = {entry['path']: entry for entry in receipt['files']}
        check(len(checkpoint_files) == len(receipt['files']), 'Duplicate historical checkpoint file')
        project_source_receipt = dict(path=str(receipt_path), sha256=digest(receipt_path),
                                      scope='Receipt identity and selected source-file pins only; no complete checkpoint inventory validation here.')
    for evidence in inputs['evidence']:
        path = Path(evidence['path'])
        if not path.is_absolute():
            path = root / path
        live_path = path
        if args.project_source_evidence and evidence['level'] == 'project_source':
            relative = Path(evidence['path'])
            check(not relative.is_absolute() and '..' not in relative.parts,
                  'Historical project-source pin must be a repository-relative file')
            entry = checkpoint_files.get(str(relative))
            check(entry and entry['kind'] == 'file' and entry['sha256'] == evidence['sha256'],
                  'Historical project-source receipt does not match input-map pin: ' + evidence['id'])
            path = project_source_root / relative
        check(path.exists(), 'Missing input evidence: ' + str(path))
        check(digest(path) == evidence['sha256'], 'Stale input evidence: ' + str(path))
        if evidence['level'] == 'project_source':
            live_hash = digest(live_path) if live_path.is_file() else None
            project_source_rows.append(dict(evidence_id=evidence['id'], path=evidence['path'],
                                            checked_path=str(path), pinned_sha256=evidence['sha256'],
                                            current_path=str(live_path), current_sha256=live_hash,
                                            current_source_matches_pin=live_hash == evidence['sha256']))
    project_source_scope = dict(historical_snapshot_used=args.project_source_evidence is not None,
                                receipt=project_source_receipt, rows=project_source_rows,
                                all_current_source_files_match_pins=all(row['current_source_matches_pin'] for row in project_source_rows),
                                scope='Pinned project-source receipts describe their checked source bytes; historical validation does not reinterpret the current implementation or prove native behavior.')

    original_checked = False
    original_package_checked = False
    native_aggregate = None
    pe_bytes = None
    if args.target:
        check(digest(args.target) == target_hash, 'Original PE mismatch')
        pe_bytes = args.target.read_bytes()
        for occurrence in occurrences:
            offset = int(occurrence['file_offset'], 16)
            code = pe_bytes[offset:offset + occurrence['byte_length']]
            check(hashlib.sha256(code).hexdigest() == occurrence['bytecode_sha256'], 'Original embedded identity mismatch')
        original_checked = True
    if args.shader_archive:
        check(digest(args.shader_archive) == shaders['source']['archive_sha256'], 'Original shader archive mismatch')
        archive = args.shader_archive.read_bytes()
        offset = shaders['source']['embedded_offset']
        package = archive[offset:offset + shaders['source']['embedded_byte_length']]
        check(hashlib.sha256(package).hexdigest() == loader['source_package_sha256'], 'Original shader package mismatch')
        for group in groups.values():
            stage_counts = group['program_counts']
            expected = [stage_counts['cs']] if 'cs' in stage_counts else [stage_counts['vs'], stage_counts['ps']]
            actual = list(struct.unpack_from('<' + 'I' * len(expected), package, group['package_start']))
            check(actual == expected, 'Original shader group counts differ')
        for program in programs:
            entry = program['package_entry_offset']
            check(struct.unpack_from('<I', package, entry)[0] == 0x11223344, 'Original shader record marker differs')
            length, key = struct.unpack_from('<II', package, entry + 4)
            check(length == program['byte_length'] and key == int(program['technique_id'], 16), 'Original shader record header differs')
            metadata = package[entry + 12:program['package_bytecode_offset']]
            check(hashlib.sha256(metadata).hexdigest() == program['package_metadata_sha256'], 'Original shader metadata differs')
            code = package[program['package_bytecode_offset']:program['package_bytecode_offset'] + length]
            check(hashlib.sha256(code).hexdigest() == program['bytecode_sha256'], 'Original shader bytecode differs')
        for record in scoped_records:
            base = record['package_bytecode_offset']
            code = package[base:base + record['byte_length']]
            chunk_offsets = struct.unpack_from('<' + 'I' * struct.unpack_from('<I', code, 28)[0], code, 32)
            shex = next(o for o in chunk_offsets if code[o:o+4] == b'SHEX')
            shex_end = shex + 8 + struct.unpack_from('<I', code, shex + 4)[0]
            for start, end in equations[record['bytecode_sha256']]['source_instruction_byte_ranges'].values():
                check(shex + 16 <= start < end <= shex_end, 'Equation executable range is outside original SHEX instruction payload')
                token = struct.unpack_from('<I', package, base + start)[0]
                check(token & 0x7ff != 0x35, 'Equation executable range points to excluded custom-data table')
                check(((token >> 24) & 0x7f) * 4 == end - start, 'Equation instruction length differs from original token')
        original_package_checked = True
    if args.native_evidence:
        check(args.target is not None, '--native-evidence requires --target')
        semantic_receipt = args.native_evidence / 'shaders/image-space-arithmetic/semantic-validation.json'
        semantic_validation = json.loads(semantic_receipt.read_text())
        check(semantic_validation['all_pass'], 'Private equation semantic receipt did not pass')
        for name, expected in semantic_validation['public_hashes'].items():
            check(digest(folder / name) == expected, 'Equation public artifact differs from semantic receipt')
        starts = {}
        functions = set()
        complete_function_exports = set()
        superseded_partial_exports = []
        llvm_functions = set()
        queries = []
        for verification_file in sorted(args.native_evidence.glob('*/verification.json')):
            area = verification_file.parent
            verify = json.loads(verification_file.read_text())
            check(verify['target_sha256'] == target_hash and verify['all_pass'], 'Private verification mismatch')
            check(not verify['runtime_observed'], 'Unexpected native runtime claim')
            for function in verify['functions']:
                check(not function['instruction_truncated'] and not function['llvm_mismatches'], 'Incomplete independent native decode')
                llvm_file = area / 'llvm' / (function['entry'].lower() + '.asm')
                check(digest(llvm_file) == function['llvm_artifact_sha256'], 'Changed independent native decode')
            if area.name == 'shaders':
                check(digest(verification_file) == loader['validation']['native_verification_artifact_sha256'], 'Stale native loader verification')
                check({q['artifact']: q['sha256'] for q in verify['queries']} == loader['native_queries'], 'Shader loader query provenance mismatch')
            llvm_functions.update(f['entry'].lower() for f in verify['functions'])
            for query in verify['queries']:
                query_file = area / 'native' / query['artifact']
                check(digest(query_file) == query['sha256'], 'Changed native export')
                export = json.loads(query_file.read_text())
                check(export['executable_sha256'] == target_hash and not export['analysis_enabled_this_run'], 'Unexpected native target/analysis')
                for seed in export['seeds']:
                    function = seed.get('function')
                    if function:
                        functions.add(function['entry'].lower())
                        if function['instructions_truncated']:
                            superseded_partial_exports.append(dict(area=area.name, artifact=query['artifact'], entry=function['entry']))
                        else:
                            complete_function_exports.add(function['entry'].lower())
                    instructions = list(seed.get('requested_span', {}).get('existing_instructions', []))
                    if function:
                        instructions.extend(function['instructions'])
                    for instruction in instructions:
                        address = instruction['address'].lower()
                        code = bytes.fromhex(instruction['loaded_bytes_hex'])
                        rva = int(address, 16) - int(identity['image_base'], 16)
                        section = next((s for s in identity['sections'] if s['rva'] <= rva and rva + len(code) <= s['rva'] + s['raw_size']), None)
                        check(section is not None, 'Native instruction not file-backed')
                        offset = section['raw_offset'] + rva - section['rva']
                        check(pe_bytes[offset:offset + len(code)] == code, 'Native instruction differs from original PE')
                        if address in starts:
                            check(starts[address]['loaded_bytes_hex'] == instruction['loaded_bytes_hex'], 'Conflicting native instruction exports')
                        starts[address] = instruction
                queries.append(dict(area=area.name, artifact=query['artifact'], sha256=query['sha256']))
        check(functions <= complete_function_exports, 'Native function has no complete replacement export')
        for pin in connections['evidence'] + [connections['verification']]:
            check(digest(Path(pin['path'])) == pin['sha256'], 'Image-space native evidence changed')
        for receipt in objects(connections):
            if 'instruction_va' in receipt:
                instruction = starts.get(receipt['instruction_va'].lower())
                check(instruction is not None, 'Image-space receipt has no original checked instruction')
                check(hashlib.sha256(bytes.fromhex(instruction['loaded_bytes_hex'])).hexdigest() == receipt['original_instruction_sha256'], 'Image-space instruction receipt differs')
        for helper in connections['helper_objects']:
            for binding in helper['bindings']:
                read = starts[binding['table_read']['instruction_va'].lower()]['disassembly']
                check('[RCX + ' + hex(binding['effect_slot'] * 8) + ']' in read, 'Helper table read differs from stated effect slot')
                store = starts[binding['constructor_field_store']['instruction_va'].lower()]['disassembly']
                destination = '[RCX]' if binding['field_offset'] == '0x0' else '[RCX + ' + binding['field_offset'] + ']'
                check(destination in store, 'Helper constructor store differs from stated field')
        expansion_calls = 0
        expansion_tails = 0
        for edge in connections['curated_transfers']:
            check(edge['condition'] and not edge['runtime_observed'], 'Unqualified image-space direct transfer')
            instruction = starts[edge['callsite'].lower()]
            code = bytes.fromhex(instruction['loaded_bytes_hex'])
            opcode = 0xe8 if edge['encoding'] == 'E8' else 0xe9
            check(len(code) == 5 and code[0] == opcode, 'Image-space transfer encoding mismatch')
            check(int(instruction['address'], 16) + 5 + struct.unpack_from('<i', code, 1)[0] == int(edge['target'], 16), 'Image-space transfer target differs from original rel32')
            expansion_calls += opcode == 0xe8
            expansion_tails += opcode == 0xe9
        node_by_id = {n['id']: n for n in frame['graph']['nodes']}
        direct_edges_checked = 0
        direct_calls_checked = 0
        tail_jumps_checked = 0
        for edge in frame['graph']['edges']:
            if edge['proof_status'] in ['native_direct_call_bytes_verified', 'native_direct_tail_jump_bytes_verified']:
                instruction = starts.get(edge['callsite'].lower())
                target = node_by_id[edge['target']].get('va')
                check(instruction is not None and instruction['call'], 'Direct edge has no native CALL')
                check(target and target.lower() in [a.lower() for a in instruction['flow_targets']], 'Direct edge target differs from native operand')
                code = bytes.fromhex(instruction['loaded_bytes_hex'])
                opcode = 0xe8 if edge['proof_status'] == 'native_direct_call_bytes_verified' else 0xe9
                check(len(code) == 5 and code[0] == opcode, 'Direct edge has no supported original E8/E9 rel32 encoding')
                decoded_target = int(instruction['address'], 16) + 5 + struct.unpack_from('<i', code, 1)[0]
                check(decoded_target == int(target, 16), 'Direct edge target differs from original encoded displacement')
                direct_edges_checked += 1
                direct_calls_checked += opcode == 0xe8
                tail_jumps_checked += opcode == 0xe9
        check(direct_edges_checked == len(frame['graph']['edges']), 'Unverified curated frame transfer')
        native_aggregate = dict(original_instruction_starts_verified=len(starts), recovered_functions_with_verified_bytes=len(functions),
                                independently_decoded_unique_functions=len(llvm_functions), curated_direct_frame_edges_checked=direct_edges_checked,
                                original_rel32_calls_checked=direct_calls_checked, original_rel32_tail_jumps_checked=tail_jumps_checked,
                                query_count=len(queries), queries=queries, native_byte_mismatches=0,
                                curated_image_space_transfers_checked=expansion_calls + expansion_tails,
                                unique_curated_transfer_sites=len({e['callsite'].lower() for e in frame['graph']['edges']} | {e['callsite'].lower() for e in connections['curated_transfers']}),
                                superseded_partial_function_exports=superseded_partial_exports,
                                note='Union of current verified exports; no runtime execution or exhaustive call closure claim.')

    output_report = docs['native-output-transfer']
    check(output_report['curated_verification']['abi_catalog_sha256'] == digest(folder / 'd3d11-abi.json')
          and output_report['registration_source']['sha256'] == digest(folder / 'native-image-space-connections.json'),
          'Output-transfer public dependency changed')
    output_transfer = check_output_transfer(output_report, identity, connections, abi, pe_bytes,
                                            args.native_output_evidence, args.native_evidence)

    for page in folder.glob('*.md'):
        for link in re.findall(r'\[[^\]]*\]\(([^)]+)\)', page.read_text()):
            if '://' in link or link.startswith('#'):
                continue
            target = (page.parent / link.split('#')[0]).resolve()
            if target.name != 'coverage.json' or not args.write_coverage:
                check(target.exists(), 'Broken map link: ' + page.name + ' -> ' + link)
    artifacts = [dict(path=str(path.relative_to(root)), bytes=path.stat().st_size, sha256=digest(path))
                 for path in sorted(folder.iterdir()) if path.is_file() and path.name not in ['coverage.json', 'map.html']]
    summary = dict(schema='mudcrab-render-coverage/v1', date='2026-10-10', status='partial native trace; complete selected package inventory; no retail draw capture',
                   target_sha256=target_hash, package=shaders['counts'], embedded=embedded['counts'], native_identity=native['coverage'],
                   frame_verification=verification, frame_nodes=len(node_ids), frame_edges=len(frame['graph']['edges']),
                   frame_stage_domains=len(frame['coverage_matrix']), input_coverage=inputs['scope_counts'],
                   project_source_evidence=project_source_scope,
                   native_shader_loader=loader['counts'],
                   image_space_connections=connections['validation'],
                   native_output_transfer=output_transfer,
                   image_space_arithmetic=dict(groups=len(scoped_ordinals), records=len(scoped_records), unique_programs=len(equations),
                                               executable_instructions=sum(p['instruction_count'] for p in equations.values()),
                                               validation=arithmetic['validation'], runtime_observed=False),
                   mod_evidence=dict(content=content_mods['counts'], source_hooks=hook_mods['method']),
                   equation_semantic_receipt=(dict(path=str(semantic_receipt), sha256=digest(semantic_receipt), all_pass=True,
                                                  scope='static expression/control validation; no GPU output parity') if args.native_evidence else None),
                   native_aggregate=native_aggregate,
                   original_pe_and_embedded_bytes_checked=original_checked, artifacts=artifacts,
                   original_shader_package_records_checked=original_package_checked,
                   validation_tools=[dict(path=str(path.relative_to(root)), sha256=digest(path))
                                     for path in [Path(__file__).resolve(), root / 'scripts/build-skyrim-render-map.py']],
                   remaining_gates=['Complete symbolic shader equations and alternate permutations',
                                    'Remaining technique selection, producers, callbacks and complete conditional call closure',
                                    'Active session/record/override/settings identities and per-draw committed resource/state receipts',
                                    'Matched retail intermediate outputs and visual acceptance'])
    if args.write_coverage:
        (folder / 'coverage.json').write_text(json.dumps(summary, indent=2) + '\n')
    printed = {k: v for k, v in summary.items() if k not in ['artifacts', 'remaining_gates', 'frame_verification']}
    if native_aggregate:
        printed['native_aggregate'] = {k: v for k, v in native_aggregate.items() if k != 'queries'}
    print(json.dumps(printed, indent=2))


if __name__ == '__main__':
    main()
