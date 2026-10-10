#!/usr/bin/env python3
"""Build the static Skyrim lighting model JSON and self-contained research viewer."""
import hashlib
import html
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
NOTES = ROOT / "docs/research/skyrim-render-map/research-notes"
TARGET = ROOT / "docs/research/skyrim-render-map/target-identity.json"
INPUT_RECEIPT = NOTES / "input-receipt.json"
ASSOCIATIONS = NOTES / "model-associations.json"


def discover_corpus():
    """Load every receipt-backed note source and authored claim ledger."""
    receipt_paths = [INPUT_RECEIPT] + sorted(NOTES.glob("note-*/input-receipt*.json"))
    receipt_rows = []
    note_receipts = {}
    source_paths = {}
    seen_artifacts = set()
    for receipt_path in receipt_paths:
        if not receipt_path.is_file():
            raise SystemExit("input receipt is missing: " + str(receipt_path))
        raw_receipt = receipt_path.read_bytes()
        receipt_hash = hashlib.sha256(raw_receipt).hexdigest()
        receipt = json.loads(raw_receipt.decode("utf-8"))
        receipt_note_rows = receipt.get("notes")
        if receipt_note_rows is None and isinstance(receipt.get("note"), dict):
            receipt_note_rows = [receipt["note"]]
        if receipt_note_rows is None and isinstance(receipt.get("id"), str):
            receipt_note_rows = [receipt]
        if not isinstance(receipt_note_rows, list):
            raise SystemExit("receipt notes must be an array: " + str(receipt_path))
        receipt_rel = receipt_path.relative_to(ROOT).as_posix()
        receipt_rows.append({"path": receipt_rel, "sha256": receipt_hash})
        for note in receipt_note_rows:
            note_id = note.get("id") if isinstance(note, dict) else None
            if not isinstance(note_id, str) or not note_id:
                raise SystemExit("receipt contains a note without a string id: " + str(receipt_path))
            if note_id in note_receipts:
                raise SystemExit("duplicate note receipt ID {} in {} and {}".format(
                    note_id, note_receipts[note_id]["path"], receipt_rel))
            note_receipts[note_id] = {"path": receipt_rel, "sha256": receipt_hash, "artifacts": []}
            artifacts = []
            for artifact in note.get("artifacts", []):
                rel = Path(artifact["path"])
                if rel.is_absolute() or ".." in rel.parts:
                    raise SystemExit("receipt artifact path must be corpus-relative: " + str(rel))
                path = (NOTES / rel).resolve()
                try:
                    path.relative_to(NOTES.resolve())
                except ValueError:
                    raise SystemExit("receipt artifact escapes the research-notes corpus: " + str(rel))
                if not path.is_file():
                    raise SystemExit("receipt artifact is missing: " + str(path))
                raw = path.read_bytes()
                digest = hashlib.sha256(raw).hexdigest()
                if digest != artifact.get("sha256"):
                    raise SystemExit("receipt hash mismatch for " + str(path))
                if artifact.get("bytes") != len(raw):
                    raise SystemExit("receipt byte-count mismatch for " + str(path))
                if rel.as_posix() in seen_artifacts:
                    raise SystemExit("duplicate receipt artifact path: " + rel.as_posix())
                seen_artifacts.add(rel.as_posix())
                note_receipts[note_id]["artifacts"].append({
                    "path": rel.as_posix(), "sha256": digest, "bytes": len(raw)
                })
                name = path.name.lower()
                if name.startswith("supplied-notes") or name.startswith("supplied-message"):
                    artifacts.append((path, digest))
            if artifacts:
                artifacts.sort(key=lambda item: (not item[0].name.lower().startswith("supplied-notes"), item[0].as_posix()))
                source_paths[note_id] = artifacts

    audit_paths = []
    for path in sorted(NOTES.glob("note-*/claims-*.json")):
        try:
            audit = json.loads(path.read_text(encoding="utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            continue
        if isinstance(audit.get("claims"), list) and audit.get("note_id"):
            audit_paths.append((path, audit))
    return receipt_rows, note_receipts, source_paths, audit_paths

# Edges are conceptual input dependencies. They do not encode a frame schedule.
NODE_ROWS = [
    ("resource-winners", "Resource winners", "source", "Winning plugin records, meshes, textures and shader package resources. Active overrides and loose/archive winner identity remain open.", ["I-C01", "I-C15", "I-C16", "I-C17", "I-C19", "I-C20", "I-C21"], ["N001-C01", "N001-C02", "N001-C06", "N001-C07", "N001-C08", "N001-C10", "N001-C11", "N001-C13", "N001-C14"], ["crates/engine/src/world/database.rs", "crates/engine/src/lighting_catalog.rs"], "Winning source identity is serialized/catalogued; current retail load order and resource precedence are not observed.", "Pin exact winning record/resource identities for one selected geometry and light."),
    ("world-weather-cell", "World, weather and cell selection", "source → CPU", "WTHR, CLMT, REGN, WRLD, CELL and LGTM values, parent links, weather weights and cell/template inheritance.", ["I-C02", "I-C03", "I-C04", "I-C07", "I-C08", "I-C09", "I-C10"], ["N001-C02", "N001-C03", "N001-C04", "N001-C06", "N001-C08", "N001-C10", "N002-P01", "N002-P02", "N002-P03"], ["crates/engine/src/lighting_catalog.rs", "crates/engine/src/lighting_runtime.rs", "crates/engine/src/lighting_settings.rs"], "Selected static consumers and serialized fields exist; live automatic selection and interior precedence remain incomplete.", "Capture source winner, effective inheritance mask and active weather/cell owner together."),
    ("view-time", "View, time and scene context", "source → CPU → runtime", "Camera/view, clock, worldspace, room, menu/map state and frame-specific ownership predicates.", ["I-C04", "I-C06", "I-C08", "I-C11", "I-C26"], ["N001-C04", "N001-C08", "N001-C12", "N002-P01", "N002-P03", "N002-L01", "N002-L02", "N002-L03"], ["crates/engine/src/environment_preview.rs", "crates/engine/src/streaming.rs", "crates/engine/src/lighting_runtime.rs"], "Mudcrab preview/runtime context is project-specific; selected retail view state was not observed.", "Record one retail view with camera, time, weather, cell/room and visibility state."),
    ("material-geometry", "Material and geometry", "source → CPU → shader", "NIF property flags and constants, mesh streams, transforms, vertex colors and authored tangent frame.", ["I-C17", "I-C18", "I-C23", "I-C24", "I-C25", "I-C26", "I-C31", "I-C32", "I-C33", "I-C34"], ["N002-L08", "N002-L12", "N002-L13", "N002-L24"], ["crates/engine/src/nif_material.rs", "crates/engine/src/nif_material/native.rs", "crates/engine/src/nif_material/native_common.wgsl"], "Selected native layouts/uploads are partial; live mesh, material and specialized permutation selection remain open.", "Match a pinned NIF/property and draw to the native input layout and selected shader key."),
    ("texture-view", "Texture resource and view", "source → CPU → shader → runtime", "Texture identity, DDS encoding, SRV format, sampler, slot mapping and sampled values.", ["I-C19", "I-C20", "I-C21", "I-C22", "I-C23", "I-C24", "I-C31", "I-C32"], ["N002-L08", "N002-L13", "N002-L24"], ["crates/converter/src/texture.rs", "crates/engine/src/nif_material/native.rs", "crates/engine/src/render.rs"], "Legacy diffuse view creation has static evidence; actual per-draw SRV and transfer interpretation remain open.", "Capture selected source texture, created view format, bound slot and shader sample operation."),
    ("shader-selection", "Shader key and program selection", "CPU → shader → runtime", "Native key translation, package lookup, stage interfaces, bytecode identity and selected resource binding.", ["I-C17", "I-C18", "I-C20", "I-C23", "I-C31", "I-C34", "I-C35"], ["N002-L05", "N002-L08", "N002-L09", "N002-L10", "N002-L11", "N002-L12", "N002-L13", "N002-L14", "N002-L24"], ["crates/engine/src/nif_material/native.rs", "crates/engine/src/nif_material/native_common.wgsl"], "Pinned native key and package mappings do not establish the actual selected retail draw. Mudcrab shader choice is separate.", "Resolve one raw native key through package lookup, binding and observed draw."),
    ("sun-direction", "Directional sunlight and moonlight", "source → CPU → shader → runtime", "Weather/celestial inputs, light object direction, dimmer and sunlight-scale packing, selected shader consumer.", ["I-C02", "I-C04", "I-C05", "I-C06", "I-C33"], ["N002-L16", "N002-L17", "N002-L18", "N002-L19", "N002-P01", "N002-P03", "N002-P25"], ["crates/engine/src/lighting_runtime.rs", "crates/engine/src/nif_material/native_common.wgsl", "crates/engine/src/lights.rs"], "Selected static direction and color segments exist; source identity, active values and exact selected shader arithmetic are not closed.", "Connect active light identity and RGB/dimmer writers to the selected pixel program and draw."),
    ("ambient-environment", "Directional ambient and environment", "source → CPU → shader → runtime", "DALC/cell ambient source, affine rows, interior cube selection and ambient/specular contributions.", ["I-C03", "I-C08", "I-C10", "I-C33"], ["N001-C05", "N002-L20", "N002-L18"], ["crates/engine/src/lighting_runtime.rs", "crates/engine/src/environment_preview.rs", "crates/engine/src/nif_material/native_common.wgsl"], "DALC row preparation is statically recovered; live extra RGB, interior precedence and effective source are open.", "Trace each ambient face/value from winning source through native row packing into a selected shader."),
    ("local-light-candidates", "Local-light candidates and packing", "source → CPU → shader → runtime", "LIGH/REFR source values, candidate list, radius/fade, ordering, per-geometry packing, count and attenuation.", ["I-C15", "I-C16", "I-C11", "I-C02"], ["N001-C07", "N001-C09", "N002-L02", "N002-L13", "N002-L14", "N002-L21", "N002-L22", "N002-L23"], ["crates/engine/src/lights.rs", "crates/engine/src/lighting_runtime.rs", "crates/engine/src/world/database.rs"], "Mudcrab budgets camera-near lights. Native candidate enumeration, selection, overflow and local equations remain open.", "Trace native source light and placement to one draw's list, packed constants and shader result."),
    ("shadow-visibility", "Shadow allocation and visibility", "source → CPU → shader → runtime", "Directional/local shadow allocation, depth resources, channel masks, selection, filtering and material visibility.", ["I-C11", "I-C15", "I-C16", "I-C33", "I-C37", "I-C43"], ["N001-C09", "N002-L03", "N002-L04", "N002-L15", "N002-M13", "N002-M14", "N002-M15", "N002-M16"], ["crates/engine/src/render.rs", "crates/engine/src/lights.rs", "crates/engine/src/nif_material/native_common.wgsl"], "Mudcrab shadow path is not Skyrim evidence; native local allocation, channel producer/consumer and active draw remain open.", "Trace exact target depth resource creation, channel writes, shader reads and selected light count."),
    ("surface-response", "Surface response", "shader → runtime", "Selected program arithmetic for diffuse, specular, ambient, emission and specialized material families.", ["I-C17", "I-C18", "I-C22", "I-C23", "I-C24", "I-C27", "I-C28", "I-C29", "I-C30", "I-C31", "I-C32", "I-C33", "I-C34"], ["N002-L08", "N002-L18", "N002-L19", "N002-L21", "N002-L24", "N002-M17", "N002-M18", "N002-M19", "N002-M20"], ["crates/engine/src/nif_material.rs", "crates/engine/src/nif_material/native.rs", "crates/engine/src/nif_material/native_common.wgsl"], "Mudcrab implements an approximation for ordinary materials; Skyrim specialized families and selected formulas remain incompletely mapped.", "Associate bytecode arithmetic and constants with the exact material/permutation and target draw."),
    ("fog-effects", "Fog, effects and volumetrics", "source → CPU → shader → runtime", "CELL/LGTM/WTHR fog, effect geometry, particle materials and VOLI/volumetric inputs.", ["I-C09", "I-C14", "I-C35", "I-C36", "I-C38"], ["N001-C11", "N002-L01", "N002-L07", "N002-P01", "N002-P22"], ["crates/engine/src/shaders/nif_fog.wgsl", "crates/engine/src/environment_preview.rs", "crates/engine/src/lighting_runtime.rs"], "Selected fog arithmetic and some conditional call paths exist; geometry, volume resources and pass composition remain open.", "Separate distance fog, effect fog and volumetrics in a target draw/resource trace."),
    ("scene-accumulation", "Scene accumulation and composition", "CPU → shader → runtime", "Opaque, alpha/effects, water/reflections, volumetric passes and the scene target/resource chain.", ["I-C35", "I-C36", "I-C37", "I-C38", "I-C39", "I-C40", "I-C41", "I-C42", "I-C43", "I-C44", "I-C45"], ["N002-L01", "N002-L05", "N002-L06", "N002-L07", "N002-P22"], ["crates/engine/src/render.rs", "crates/engine/src/image_space.rs"], "The current call map is partial; no complete native resource timeline or selected retail frame is captured.", "Capture resource producers/consumers and ordering for one chosen retail frame."),
    ("image-space-records", "Image-space records and settings", "source → CPU → runtime", "IMGS/base data, weather/time association, room ownership and setting fields.", ["I-C08", "I-C09", "I-C12", "I-C13"], ["N001-C12", "N002-P01", "N002-P02", "N002-P04", "N002-P05", "N002-P06", "N002-P07", "N002-P08", "N002-P18"], ["crates/engine/src/lighting_runtime.rs", "crates/engine/src/lighting_settings.rs", "crates/engine/src/environment_preview.rs"], "Stable fields and selected packing are static; active record, view ownership and effective values remain open.", "Identify the active IMGS and its winning source for a captured view."),
    ("modifier-stack", "Image-space modifier evaluation", "source → CPU → runtime", "IMAD interpolators, ordered composition, strength, crossfades, manager channels and active list.", ["I-C12", "I-C13"], ["N001-C12", "N002-P19", "N002-P20", "N002-P21"], ["crates/engine/src/lighting_runtime.rs", "crates/engine/src/environment_preview.rs"], "Selected static interpolation/stack behavior is mapped; source curves, full lifecycle and active stack values remain open.", "Capture active modifiers, order, curve values/strength and final channel values together."),
    ("luminance-adaptation", "Luminance and exposure adaptation", "shader → runtime", "Luminance reduction, history resources, adaptation rate, minimum step, time and active image-space controls.", ["I-C12", "I-C13", "I-C39", "I-C40", "I-C41", "I-C42", "I-C43"], ["N002-P04", "N002-P05", "N002-P06", "N002-P07", "N002-P09", "N002-P10", "N002-P11", "N002-P22", "N002-P23"], ["crates/engine/src/render.rs", "crates/engine/src/image_space.rs"], "Pinned shader arithmetic differs from generic N002 formulas; runtime history/resource selection and active settings remain open.", "Trace selected adaptation program, input samples, history textures, constants and enabled branch."),
    ("bloom-tone-grade", "Bloom, tone mapping and grading", "source → CPU → shader → runtime", "Bloom threshold/filter/composition, tone curve, saturation, brightness, contrast and tint operations.", ["I-C12", "I-C13", "I-C39", "I-C40", "I-C41", "I-C42", "I-C43"], ["N002-P08", "N002-P12", "N002-P13", "N002-P14", "N002-P15", "N002-P16", "N002-P17", "N002-P18", "N002-P23", "N002-P24", "N002-M05", "N002-M06", "N002-M07", "N002-M08", "N002-M09", "N002-M10", "N002-M11", "N002-M12"], ["crates/engine/src/render.rs", "crates/engine/src/image_space.rs"], "Shipped scoped arithmetic conflicts with some generic article equations; effective branches and settings remain unobserved.", "Match one selected output program and its constants to the active image-space and final target."),
    ("output-display", "Output transfer and display", "shader → runtime", "Final render target format, gamma/transfer operations, swapchain encoding and display mode.", ["I-C20", "I-C43", "I-C45"], ["N002-P15", "N002-P16", "N002-P17", "N002-P23", "N002-P24", "N002-M02", "N002-M05", "N002-M06", "N002-M07", "N002-M08"], ["crates/engine/src/image_space.rs", "crates/engine/src/render.rs", "crates/engine/src/shaders/image_space_hdr.wgsl"], "A texture/view format does not establish physical light domain; final production output and display transfer are unobserved.", "Trace output target through final shader, swapchain format/space and display mode."),
    ("mod-overlays", "Mod interventions", "external source → hooks/overrides → runtime", "ENB proxy and post effects; CS hooks/features/finite buffers; authored lighting-mod records and assets.", ["I-C01", "I-C15", "I-C16", "I-C17", "I-C18", "I-C35", "I-C37", "I-C43", "I-C45"], ["N002-M01", "N002-M02", "N002-M03", "N002-M04", "N002-M05", "N002-M06", "N002-M07", "N002-M08", "N002-M09", "N002-M10", "N002-M11", "N002-M12", "N002-M13", "N002-M14", "N002-M15", "N002-M16", "N002-M17", "N002-M18", "N002-M19", "N002-M20", "N002-M21", "N002-M22", "N002-M23", "N002-M24", "N002-M25", "N002-M26", "N002-M27", "N002-M28", "N002-M29"], ["crates/engine/src/lights.rs", "crates/engine/src/nif_material/native_common.wgsl"], "Mudcrab has no ENB or CS runtime integration. Mod evidence is separate from vanilla behavior; source capability is not active state.", "Pin an exact mod release, configuration and authored winners before comparing any intervention with native behavior."),
]

EDGES = [
    ("resource-winners", "world-weather-cell", "record winners and parent links"),
    ("resource-winners", "material-geometry", "mesh and material inputs"),
    ("resource-winners", "texture-view", "texture/package identity"),
    ("world-weather-cell", "sun-direction", "weather and celestial inputs"),
    ("world-weather-cell", "ambient-environment", "CELL/LGTM/DALC inputs"),
    ("world-weather-cell", "local-light-candidates", "cell and placement ownership"),
    ("view-time", "world-weather-cell", "time and context affect selection"),
    ("material-geometry", "shader-selection", "flags, pass and key inputs"),
    ("texture-view", "shader-selection", "bound resource and permutation inputs"),
    ("shader-selection", "surface-response", "selected bytecode interface"),
    ("sun-direction", "surface-response", "directional light contribution"),
    ("ambient-environment", "surface-response", "ambient contribution"),
    ("local-light-candidates", "surface-response", "local light contribution"),
    ("local-light-candidates", "shadow-visibility", "candidate and shadow selection"),
    ("shadow-visibility", "surface-response", "visibility contribution"),
    ("world-weather-cell", "fog-effects", "fog and volume records"),
    ("material-geometry", "fog-effects", "effect geometry and materials"),
    ("surface-response", "scene-accumulation", "surface color inputs"),
    ("fog-effects", "scene-accumulation", "fog/effect/volume inputs"),
    ("scene-accumulation", "image-space-records", "scene image-space association"),
    ("image-space-records", "modifier-stack", "base data with modifiers"),
    ("image-space-records", "luminance-adaptation", "exposure settings"),
    ("modifier-stack", "bloom-tone-grade", "effective image-space channels"),
    ("luminance-adaptation", "bloom-tone-grade", "exposure result"),
    ("bloom-tone-grade", "output-display", "graded scene output"),
    ("mod-overlays", "local-light-candidates", "optional replacement light assignment"),
    ("mod-overlays", "shadow-visibility", "optional shadow feature paths"),
    ("mod-overlays", "bloom-tone-grade", "optional postprocessing path"),
    ("mod-overlays", "surface-response", "optional replacement shader/material features"),
]


def normalized(text):
    return re.sub(r"\s+", " ", text).strip()


def small_evidence(claim):
    rows = claim.get("evidence_anchors", claim.get("evidence", []))
    out = []
    for row in rows[:5]:
        if isinstance(row, str):
            out.append({"reference": row})
        elif isinstance(row, dict):
            out.append({k: row[k] for k in ("file", "path", "url", "anchors", "line_or_locus", "sha256", "basis", "target_identity", "native_anchor_ids", "query_ids") if k in row})
    return out


def implementation(claim):
    rows = claim.get("current_implementation", claim.get("implementation_anchors", claim.get("implementation", [])))
    if isinstance(rows, dict):
        rows = [rows]
    out = []
    for row in rows[:4]:
        if isinstance(row, str):
            out.append({"path": row})
        elif isinstance(row, dict):
            out.append({k: row[k] for k in ("path", "file", "symbol", "lines", "sha256", "status", "note", "basis") if k in row})
    return out


def make_claim_index(source_paths=None, audit_paths=None, note_receipts=None):
    if source_paths is None or audit_paths is None:
        _, note_receipts, source_paths, audit_paths = discover_corpus()
    claims = []
    for path, audit in audit_paths:
        note = audit["note_id"]
        sources = [(source_path, digest, source_path.read_text(encoding="utf-8"))
                   for source_path, digest in source_paths.get(note, [])]
        if not sources:
            raise SystemExit("no receipt-backed supplied source found for " + note + "; add its source artifact to input-receipt.json")
        for raw in audit.get("claims", []):
            cid = raw["id"]
            statement = raw.get("supplied_statement", raw.get("claim", ""))
            match = next(((source_path, digest) for source_path, digest, source in sources
                          if statement and statement in source), None)
            source_path, source_sha = match or sources[0][:2]
            basis_text = raw.get("supplied_excerpt_verbatim")
            source_basis = None
            if basis_text:
                basis_match = next(((source_path, digest, source) for source_path, digest, source in sources
                                    if basis_text in source), None)
                if basis_match is None:
                    raise SystemExit("supplied_excerpt_verbatim is not present in receipt-backed source for " + cid)
                basis_path, basis_sha, basis_source = basis_match
                basis_start = basis_source.encode("utf-8").find(basis_text.encode("utf-8"))
                source_basis = {
                    "text": basis_text,
                    "source_file": str(basis_path.relative_to(NOTES)),
                    "source_sha256": basis_sha,
                    "source_span_bytes": {"start": basis_start, "end_exclusive": basis_start + len(basis_text.encode("utf-8"))},
                }
            claims.append({
                "id": cid, "note": note, "domain": audit.get("domain", "unclassified"),
                "assessment": raw.get("assessment", "unverified"),
                "source_kind": "verbatim" if match else "paraphrase",
                "statement": statement,
                "source_basis": source_basis,
                "source_file": str(source_path.relative_to(NOTES)),
                "source_sha256": source_sha,
                "source_receipt_path": (note_receipts or {}).get(note, {}).get("path"),
                "source_receipt_sha256": (note_receipts or {}).get(note, {}).get("sha256"),
                "audit_file": str(path.relative_to(ROOT)),
                "audit_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "scope": raw.get("subject_scope", raw.get("topic", "")),
                "evidence": small_evidence(raw),
                "current_consumer": implementation(raw) or raw.get("current_mudcrab_implementation", raw.get("current_implementation", raw.get("implementation", []))),
                "unresolved": raw.get("unresolved", ""),
                "next_query": raw.get("next_query", ""),
                "relates_to": raw.get("relates_to", []),
            })
    ids = {claim["id"] for claim in claims}
    relation_kinds = {"strengthens", "challenges", "duplicates", "related"}
    for claim in claims:
        relations = claim["relates_to"]
        if not isinstance(relations, list):
            raise SystemExit("relates_to must be an array for " + claim["id"])
        for relation in relations:
            if not isinstance(relation, dict) or relation.get("claim_id") not in ids:
                raise SystemExit("relates_to references unknown claim ID for " + claim["id"])
            if relation.get("relation") not in relation_kinds:
                raise SystemExit("invalid relates_to relation for " + claim["id"])
    priority = {"contradicted": 0, "unverified": 1, "partially_supported": 2, "supported_static": 3}
    return sorted(claims, key=lambda x: (priority.get(x["assessment"], 9), x["note"], x["id"]))


def build_model(receipt_rows, note_receipts, source_paths, audit_paths, claims):
    target = json.loads(TARGET.read_text(encoding="utf-8"))
    target_sha = target.get("executable_sha256", target.get("sha256", "846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f"))
    target_version = target.get("version", "1.7.104.0")
    nodes = []
    for row in NODE_ROWS:
        id_, label, layer, description, contracts, claim_ids, paths, boundary, query = row
        nodes.append({"id": id_, "label": label, "domain_layer": layer, "description": description,
                      "contract_backlinks": contracts, "claim_ids": claim_ids,
                      "implementation_paths": paths, "current_boundary": boundary,
                      "gaps": {"code_or_cpu": boundary, "data_domain": boundary, "selection_or_runtime": query},
                      "next_query": query, "complete_selected_retail_route": False})
    claim_ids = {claim["id"] for claim in claims}
    associations = json.loads(ASSOCIATIONS.read_text(encoding="utf-8"))
    if associations.get("schema") != "mudcrab-research-note-model-associations/v1":
        raise SystemExit("model-associations.json has unexpected schema")
    node_ids = {node["id"] for node in nodes}
    association_map = {}
    for row in associations.get("associations", []):
        cid = row.get("claim_id")
        if cid not in claim_ids:
            raise SystemExit("model association references unknown claim ID: " + str(cid))
        if cid in association_map:
            raise SystemExit("duplicate model association for claim ID: " + str(cid))
        linked_nodes = row.get("node_ids")
        if not isinstance(linked_nodes, list) or any(node_id not in node_ids for node_id in linked_nodes):
            raise SystemExit("model association references unknown node for claim ID: " + str(cid))
        association_map[cid] = linked_nodes
    node_claims = {node_id: [] for node_id in node_ids}
    for cid, linked_nodes in association_map.items():
        for node_id in linked_nodes:
            node_claims[node_id].append(cid)
    for node in nodes:
        node["claim_ids"] = sorted(node_claims[node["id"]])
    unmapped = sorted(claim_ids - set(association_map))
    for claim in claims:
        claim["model_nodes"] = association_map.get(claim["id"], [])
    prefixes = {}
    for claim in claims:
        found = re.match(r"^(N\d{3}-[A-Z]+)(\d+)$", claim["id"])
        if found:
            prefixes.setdefault(found.group(1), []).append(int(found.group(2)))
    note_receipt_rows = []
    for note_id, receipt_ref in sorted(note_receipts.items()):
        note_receipt_rows.append({"id": note_id, "receipt": receipt_ref,
                                  "artifacts": receipt_ref["artifacts"]})
    association_hash = hashlib.sha256(ASSOCIATIONS.read_bytes()).hexdigest()
    audit_pins = [{"path": str(path.relative_to(ROOT)), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                   "note_id": audit["note_id"], "claim_count": len(audit.get("claims", []))}
                  for path, audit in audit_paths]
    model = {
        "schema": "mudcrab-skyrim-lighting-world-model/v1",
        "title": "Lighting and color ownership model",
        "scope": {"target": f"SkyrimSE.exe {target_version}", "target_sha256": target_sha,
                  "conceptual_dependencies_only": True, "frame_order_recovered": False,
                  "runtime_observed": False, "complete_selected_retail_route": False,
                  "static_research_not_visual_acceptance": True,
                  "note": "Edges describe conceptual dependencies, not proven frame order. Mod interventions are a separate overlay and never fill vanilla evidence gaps."},
        "evidence_domains": [
            {"id":"source","label":"Source and authored data","includes":["plugin/record winners","NIF and texture identities","settings and configuration"]},
            {"id":"cpu","label":"Native CPU preparation","includes":["record selection","transforms and runtime state","permutation keys","constant packing and resource binding"]},
            {"id":"shader","label":"Shipped shader consumption","includes":["exact program identity","register/vector offsets","sampled resources","static arithmetic"]},
            {"id":"runtime","label":"Selected retail execution","includes":["active source winners","view and feature state","selected draw and resource values","display output"]}
        ],
        "nodes": nodes,
        "edges": [{"id":f"E{i:02d}","from":a,"to":b,"relation":label,"kind":"conceptual_dependency","proven_frame_order":False,"complete_selected_retail_route":False} for i,(a,b,label) in enumerate(EDGES,1)],
        "separation_rules": [
            "Static source identity does not prove a winning retail resource.",
            "A parameter metadata ID is not a GPU cbuffer slot; offsets are program-specific.",
            "Shader-declared capacity and CPU allocation/overflow are separate questions.",
            "Mudcrab implementation evidence describes Mudcrab only.",
            "Mod capability and mod activation are separate; mod paths do not prove vanilla behavior."
        ],
        "input_receipts": receipt_rows,
        "note_receipts": note_receipt_rows,
        "model_associations": {"path": str(ASSOCIATIONS.relative_to(ROOT)),
                               "sha256": association_hash,
                               "association_count": len(association_map)},
        "source_pins": [
            {"id":"native-map","path":"docs/research/skyrim-render-map","revision":"target-pinned original binary and package maps","target_sha256":target_sha,"complete_selected_retail_route":False},
            {"id":"model-associations","path":str(ASSOCIATIONS.relative_to(ROOT)),"sha256":association_hash,"scope":"reviewed claim-to-model links; unassociated claims remain unmapped"},
            *audit_pins,
            {"id":"community-shaders","revision":"d001d4de3cde4feeec539dbaca5481cdda2b1b15","path":"/Users/taylor/.local/share/mudcrab-research/render-map-20261010/mod-hooks/community-shaders","scope":"mod source only; not vanilla evidence"}
        ],
        "claim_count": len(claims),
        "unmapped_claim_ids": unmapped,
        "claim_id_ranges": {prefix: f"01–{max(numbers):02d}" for prefix, numbers in sorted(prefixes.items())},
    }
    return model


def render_html(model, claims):
    payload = json.dumps({"model": model, "claims": claims}, ensure_ascii=False, separators=(",", ":"))
    payload = payload.replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    return '''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Skyrim lighting and color model</title><style>
:root{font-family:var(--font-sans,system-ui,sans-serif);color:var(--foreground,#d9e1e9);background:transparent}*{box-sizing:border-box}html,body{background:transparent}body{margin:0;padding:0;line-height:1.5;max-width:100%;overflow-x:hidden}h2{font-size:1.05rem;margin:20px 0 8px}p{margin:0 0 12px}.muted,small{color:var(--muted-foreground,#9ba6b3)}code{font-family:var(--font-mono,ui-monospace,monospace);font-size:.88em;overflow-wrap:anywhere}.controls{display:grid;grid-template-columns:minmax(0,2fr) repeat(3,minmax(0,1fr));gap:8px;margin:12px 0}.controls>*{min-width:0}input,select,button{font:inherit;color:inherit;background:var(--secondary,#272e35);border:1px solid var(--border,#3b4551);border-radius:6px;padding:8px 10px}button{cursor:pointer}button:hover{border-color:var(--accent,#59c9da)}.model{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1.5fr);gap:18px}.model>*{min-width:0}.nodes{display:flex;flex-direction:column;gap:3px;max-height:430px;overflow:auto}.node{width:100%;text-align:left;border:0;border-radius:4px;background:transparent;padding:7px 9px}.node.active{background:var(--accent-surface,#183c49);color:var(--accent-surface-foreground,#a7e0ed)}.node-detail{padding:2px 8px;min-width:0;overflow-wrap:anywhere}.pill{display:inline-block;border:1px solid var(--border,#3b4551);border-radius:12px;font-size:.72rem;padding:1px 7px;margin:0 4px 4px 0;color:var(--muted-foreground,#9ba6b3)}.warning{color:var(--warning,#edbb73)}.edge-list{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:6px}.edge-link{font:inherit;text-align:left;border:1px solid var(--border,#3b4551);background:transparent;border-radius:4px;padding:6px 8px;min-width:0;overflow-wrap:anywhere}.claim-list{border-top:1px solid var(--border,#3b4551)}.claim{padding:10px 2px;border-bottom:1px solid var(--border,#3b4551);min-width:0;overflow-wrap:anywhere}summary{cursor:pointer}.claim summary{font-size:.94rem}.claim details{margin-top:7px}.claim p{font-size:.87rem;margin:7px 0}.claim .statement{margin-top:8px}.quote{color:var(--info,#84bfd8)}.paraphrase{color:var(--warning,#edbb73)}.meta{font-size:.78rem;color:var(--muted-foreground,#9ba6b3);overflow-wrap:anywhere}.list{padding-left:18px;margin:5px 0}.list li{margin:5px 0;font-size:.85rem;overflow-wrap:anywhere}.more{margin-top:10px}.empty{padding:16px 2px;color:var(--muted-foreground,#9ba6b3)}.status{font-size:.83rem;color:var(--muted-foreground,#9ba6b3)}.model-toggle{margin:5px 0 12px}.model-toggle summary{color:var(--accent-surface-foreground,#a7e0ed)}@media(max-width:720px){.controls{grid-template-columns:minmax(0,1fr) minmax(0,1fr)}.controls input{grid-column:1/-1}.model{grid-template-columns:minmax(0,1fr)}.nodes{max-height:260px}.edge-list{grid-template-columns:minmax(0,1fr)}}
</style></head><body>
<p class="status">Static research, not visual acceptance. Dependencies are conceptual; no selected retail route or frame order is proved. Mod overlays remain separate from vanilla evidence.</p>
<details class="model-toggle"><summary>Inspect the lighting/color model · MODEL_NODES stages, MODEL_EDGES dependencies</summary><section aria-label="Conceptual model"><div class="model"><div id="nodes" class="nodes"></div><div id="nodeDetail" class="node-detail"></div></div></section></details>
<section aria-label="Claim corpus"><h2>Claim corpus</h2><p class="muted">Statements are labeled verbatim only when they match a receipt-backed source exactly. Expand a claim for evidence, current implementation context and the next query.</p>
<p id="unmapped" class="muted"></p><div class="controls"><input id="search" type="search" placeholder="Search claims, evidence, paths or IDs" aria-label="Search claims"><select id="note"><option value="">All notes</option></select><select id="domain"><option value="">All domains</option></select><select id="assessment"><option value="">All assessments</option><option>supported_static</option><option>partially_supported</option><option>contradicted</option><option>unverified</option></select></div>
<p id="count" class="muted"></p><div id="claims" class="claim-list"></div><button id="more" class="more" hidden>Show more</button></section>
<script id="corpus" type="application/json">PAYLOAD</script>
<script>
(()=>{const D=JSON.parse(document.getElementById('corpus').textContent),M=D.model,C=D.claims;let active=M.nodes[0].id,limit=7;const $=id=>document.getElementById(id),esc=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])),tag=(s,cl='')=>`<span class="pill ${cl}">${esc(s)}</span>`;
function drawNodes(){ $('nodes').innerHTML=M.nodes.map(n=>`<button class="node ${n.id===active?'active':''}" data-node="${esc(n.id)}">${esc(n.label)} ${tag(n.domain_layer)}</button>`).join('');$('nodes').querySelectorAll('[data-node]').forEach(b=>b.onclick=()=>{active=b.dataset.node;drawNodes();drawDetail()});drawDetail() }
function drawDetail(){const n=M.nodes.find(x=>x.id===active);if(!n)return;const edges=M.edges.filter(e=>e.from===n.id||e.to===n.id);$('nodeDetail').innerHTML=`<h3>${esc(n.label)}</h3><p>${esc(n.description)}</p><p>${tag(n.domain_layer)} ${tag('retail route open','warning')}</p><p><b>Contract backlinks</b><br>${n.contract_backlinks.map(x=>`<code>${esc(x)}</code>`).join(' · ')}</p><p><b>Current implementation</b><br>${n.implementation_paths.map(x=>`<code>${esc(x)}</code>`).join('<br>')}</p><p><b>Boundary</b><br>${esc(n.current_boundary)}</p><p><b>Next query</b><br>${esc(n.next_query)}</p><h3>Conceptual dependencies</h3><div class="edge-list">${edges.map(e=>`<button class="edge-link" data-target="${esc(e.from===n.id?e.to:e.from)}">${e.from===n.id?'→':'←'} ${esc(M.nodes.find(x=>x.id===(e.from===n.id?e.to:e.from))?.label||'')}<br><small>${esc(e.relation)}</small></button>`).join('')}</div><h3>Linked claims</h3><p>${n.claim_ids.map(id=>`<code>${esc(id)}</code>`).join(' · ')||'No direct claims'}</p>`;$('nodeDetail').querySelectorAll('[data-target]').forEach(b=>b.onclick=()=>{active=b.dataset.target;drawNodes()})}
function initFilters(){const ns=[...new Set(C.map(c=>c.note))].sort(),ds=[...new Set(C.map(c=>c.domain))].sort();$('note').innerHTML+='<option>'+ns.map(esc).join('</option><option>')+'</option>';$('domain').innerHTML+='<option>'+ds.map(esc).join('</option><option>')+'</option>';const missing=M.unmapped_claim_ids||[];$('unmapped').textContent=missing.length?'Claims without a curated model stage: '+missing.join(', '):'All claims have curated model links.';['search','note','domain','assessment'].forEach(id=>$(id).addEventListener('input',()=>{limit=7;drawClaims()}))}
function evidence(c){let x=c.evidence||[];if(!Array.isArray(x))x=[x];return x.length?`<ul class="list">${x.map(e=>`<li>${esc(e.file||e.path||e.url||e.reference||'Evidence')} ${e.anchors?`<small>${esc(e.anchors)}</small>`:''} ${e.line_or_locus?`<small>${esc(e.line_or_locus)}</small>`:''} ${e.sha256?`<code>${esc(e.sha256)}</code>`:''}<br><small>${esc(e.basis||'')}</small></li>`).join('')}</ul>`:'<p class="muted">No compact evidence anchor was recorded.</p>'}
function relations(c){let r=c.relates_to||[];let nodes=c.model_nodes||[];let links=r.map(x=>`<li>${tag(x.relation)} <a href="#${esc(x.claim_id)}" data-related-claim="${esc(x.claim_id)}">${esc(x.claim_id)}</a></li>`).join('');let stages=nodes.map(x=>tag(x)).join(' ');return `<p><b>Cross-note relationships</b></p>${links?`<ul class="list">${links}</ul>`:'<p class="muted">No related claims recorded.</p>'}<p><b>Curated model nodes</b><br>${stages||'<span class="muted">Unmapped</span>'}</p>`}
function curr(c){let x=c.current_consumer;if(Array.isArray(x)){return x.length?`<ul class="list">${x.map(e=>`<li>${esc(e.path||e.file||JSON.stringify(e))} ${e.lines?`<code>${esc(e.lines)}</code>`:''} ${e.symbol?`<small>${esc(e.symbol)}</small>`:''}</li>`).join('')}</ul>`:'<p class="muted">No direct Mudcrab consumer anchor.</p>'}if(typeof x==='object'&&x)return `<p>${esc(x.status||x.note||x.basis||JSON.stringify(x))}</p>`;return `<p>${esc(x||'No direct Mudcrab consumer anchor.')}</p>`}
function filtered(){let q=$('search').value.toLowerCase(),note=$('note').value,domain=$('domain').value,assessment=$('assessment').value;return C.filter(c=>(!note||c.note===note)&&(!domain||c.domain===domain)&&(!assessment||c.assessment===assessment)&&(!q||[c.id,c.statement,c.scope,c.domain,c.assessment,JSON.stringify(c.evidence),JSON.stringify(c.current_consumer),JSON.stringify(c.relates_to),JSON.stringify(c.model_nodes),JSON.stringify(c.source_basis),c.next_query].join(' ').toLowerCase().includes(q)))}
function drawClaims(){const F=filtered(),shown=F.slice(0,limit);$('count').textContent=`Showing ${shown.length} of ${F.length} matching claims (${C.length} total).`;$('claims').innerHTML=shown.length?shown.map(c=>`<article class="claim" id="${esc(c.id)}"><div class="meta"><code>${esc(c.id)}</code> ${tag(c.note)} ${tag(c.domain)} ${tag(c.assessment,c.assessment==='unverified'?'warning':'')} ${tag(c.source_kind,c.source_kind==='verbatim'?'quote':'paraphrase')}</div><details><summary>${esc(c.scope||c.statement)}</summary><p class="statement ${c.source_kind==='verbatim'?'quote':'paraphrase'}"><b>${c.source_kind==='verbatim'?'Source quote':'Auditor paraphrase'}:</b> ${esc(c.statement)}</p>${c.source_basis?`<p class="statement quote"><b>Literal source basis:</b> ${esc(c.source_basis.text)}<br><small>${esc(c.source_basis.source_file)} bytes ${esc(c.source_basis.source_span_bytes?.start)}–${esc(c.source_basis.source_span_bytes?.end_exclusive)} · SHA-256 <code>${esc(c.source_basis.source_sha256)}</code></small></p>`:''}<p class="meta">Matched source: <code>${esc(c.source_file)}</code><br>SHA-256: <code>${esc(c.source_sha256)}</code><br>Input receipt: <code>${esc(c.source_receipt_path)}</code><br>Receipt SHA-256: <code>${esc(c.source_receipt_sha256)}</code></p>${relations(c)}<p><b>Evidence</b></p>${evidence(c)}<p><b>Current consumer</b></p>${curr(c)}<p><b>Open point</b><br>${esc(Array.isArray(c.unresolved)?c.unresolved.join(' '):c.unresolved||'')}</p><p><b>Next query</b><br>${esc(c.next_query||'')}</p><p class="meta">Audit artifact path: <code>${esc(c.audit_file)}</code><br>SHA-256: <code>${esc(c.audit_sha256)}</code></p></details></article>`).join(''):'<p class="empty">No claims match these filters.</p>';$('more').hidden=F.length<=limit;$('more').textContent=`Show more (${F.length-limit} remaining)`}
$('more').onclick=()=>{limit+=7;drawClaims()};document.addEventListener('click',ev=>{const link=ev.target.closest('[data-related-claim]');if(!link)return;ev.preventDefault();const target=link.dataset.relatedClaim;$('note').value='';$('domain').value='';$('assessment').value='';$('search').value=target;limit=7;drawClaims();const article=document.getElementById(target);if(article){const details=article.querySelector('details');if(details)details.open=true;article.scrollIntoView({behavior:'smooth',block:'start'})}});initFilters();drawNodes();drawClaims()})();
</script></body></html>'''.replace("MODEL_NODES", str(len(model["nodes"]))).replace("MODEL_EDGES", str(len(model["edges"]))).replace("PAYLOAD", payload)


def main():
    missing = [(row[0], path) for row in NODE_ROWS for path in row[6] if not (ROOT / path).exists()]
    if missing:
        raise SystemExit("implementation paths missing from workspace: " + repr(missing))
    receipt_rows, note_receipts, source_paths, audit_paths = discover_corpus()
    claims = make_claim_index(source_paths, audit_paths, note_receipts)
    model = build_model(receipt_rows, note_receipts, source_paths, audit_paths, claims)
    (NOTES / "world-model.json").write_text(json.dumps(model, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (NOTES / "model.html").write_text(render_html(model, claims), encoding="utf-8")
    print("nodes", len(model["nodes"]), "conceptual edges", len(model["edges"]), "claims", len(claims))

if __name__ == "__main__":
    main()
