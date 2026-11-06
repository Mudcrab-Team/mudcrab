#!/usr/bin/env python3
"""Validate supplied research-note receipts and refresh their derived claim index.

This checks provenance and references only. It does not validate claim semantics.
"""
import argparse
import hashlib
from collections import Counter
import json
import sys
from pathlib import Path

SCHEMA = "mudcrab-research-note-claims/v1"
INDEX_SCHEMA = "mudcrab-research-note-claim-index/v1"
RELATIONSHIPS = {"strengthens", "challenges", "duplicates", "related"}
ASSESSMENTS = {
    "supported_static", "partially_supported", "contradicted", "unverified"
}
WORKSPACE_PATH_ROOTS = {
    "docs", "crates", "scripts", "assets", "tests", "data", "config"
}
AUDITS = {
    "claims-imagespace.json": "imagespace-audit.md",
    "claims-mod-interventions.json": "mod-interventions-audit.md",
    "claims-native-lighting.json": "native-lighting-audit.md",
}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def read_json(path, errors):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:
        errors.append("cannot read JSON {}: {}".format(path, exc))
        return None


def repo_relative(path, root):
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except Exception:
        return None


def source_artifacts(root, receipt_paths, errors):
    artifacts = {}
    seen_notes = {}
    seen_artifacts = {}
    receipts = {}
    corpus_root = root / "docs/research/skyrim-render-map/research-notes"
    for receipt_path in receipt_paths:
        receipt = read_json(receipt_path, errors)
        if not isinstance(receipt, dict):
            continue
        if receipt.get("schema") != "mudcrab-research-note-input/v1":
            errors.append("{} has unexpected input receipt schema".format(repo_relative(receipt_path, root)))
        notes = receipt.get("notes")
        if notes is None and isinstance(receipt.get("note"), dict):
            notes = [receipt["note"]]
        if notes is None and isinstance(receipt.get("id"), str):
            notes = [receipt]
        if not isinstance(notes, list):
            errors.append("{} receipt notes must be an array".format(repo_relative(receipt_path, root)))
            continue
        for note in notes:
            if not isinstance(note, dict) or not isinstance(note.get("id"), str):
                errors.append("{} contains a note without a string id".format(receipt_path))
                continue
            note_id = note["id"]
            receipt_rel = repo_relative(receipt_path, root)
            receipt_hash = sha256(receipt_path.read_bytes())
            if note_id in seen_notes:
                errors.append("duplicate note receipt ID {} in {} and {}".format(
                    note_id, seen_notes[note_id], receipt_rel))
            else:
                seen_notes[note_id] = receipt_rel
                receipts[note_id] = {"path": receipt_rel, "sha256": receipt_hash}
            rows = note.get("artifacts")
            if not isinstance(rows, list) or not rows:
                errors.append("{} has no input artifacts".format(note_id))
                continue
            loaded = []
            for row in rows:
                if not isinstance(row, dict) or not isinstance(row.get("path"), str):
                    errors.append("{} contains an invalid artifact receipt".format(note_id))
                    continue
                rel = row["path"]
                rel_path = Path(rel)
                if rel_path.is_absolute() or ".." in rel_path.parts:
                    errors.append("{} artifact path must be relative to corpus root: {}".format(note_id, rel))
                    continue
                if rel in seen_artifacts:
                    errors.append("duplicate artifact receipt {} in {} and {}".format(
                        rel, seen_artifacts[rel], repo_relative(receipt_path, root)))
                else:
                    seen_artifacts[rel] = repo_relative(receipt_path, root)
                path = (corpus_root / rel_path).resolve()
                try:
                    path.relative_to(corpus_root.resolve())
                except ValueError:
                    errors.append("{} artifact escapes corpus root: {}".format(note_id, rel))
                    continue
                if not path.is_file():
                    errors.append("{} source artifact is missing: {}".format(note_id, rel))
                    continue
                raw = path.read_bytes()
                actual_hash = sha256(raw)
                if actual_hash != row.get("sha256"):
                    errors.append("{} source hash mismatch: {} expected {} got {}".format(
                        note_id, rel, row.get("sha256"), actual_hash))
                if len(raw) != row.get("bytes"):
                    errors.append("{} source byte count mismatch: {} expected {} got {}".format(
                        note_id, rel, row.get("bytes"), len(raw)))
                loaded.append({
                    "path": repo_relative(path, root),
                    "receipt_path": rel_path.as_posix(),
                    "receipt_file": receipt_rel,
                    "receipt_sha256": receipt_hash,
                    "sha256": actual_hash,
                    "bytes": len(raw),
                    "raw": raw,
                    "text": raw.decode("utf-8", errors="replace"),
                })
            artifacts[note_id] = loaded
    return artifacts, receipts


def is_workspace_local_path(value, root):
    if not isinstance(value, str) or not value.strip():
        return None
    text = value.strip()
    candidate = Path(text)
    if candidate.is_absolute():
        try:
            candidate.resolve().relative_to(root.resolve())
            return candidate.resolve()
        except Exception:
            return None
    parts = candidate.parts
    if parts and parts[0].startswith("note-"):
        corpus = root / "docs/research/skyrim-render-map/research-notes"
        resolved = (corpus / candidate).resolve()
        try:
            resolved.relative_to(corpus.resolve())
            return resolved
        except Exception:
            return None
    if not parts or parts[0] not in WORKSPACE_PATH_ROOTS:
        return None
    resolved = (root / candidate).resolve()
    try:
        resolved.relative_to(root.resolve())
    except Exception:
        return None
    return resolved


def collect_anchors(claim):
    rows = []
    keys = ("evidence", "evidence_anchors", "current_implementation",
            "current_mudcrab_implementation", "implementation", "implementation_anchors")
    for key in keys:
        value = claim.get(key)
        if isinstance(value, dict):
            rows.append((key, value))
        elif isinstance(value, str) and value.strip():
            rows.append((key, {"locator": value}))
        elif isinstance(value, list):
            for row in value:
                if isinstance(row, dict):
                    rows.append((key, row))
                elif isinstance(row, str) and row.strip():
                    rows.append((key, {"locator": row}))
    return rows


def check_anchor(anchor, root, note_id, claim_id, drift, skipped, errors):
    url = anchor.get("url") or anchor.get("uri") or anchor.get("external_url")
    path_value = anchor.get("file") or anchor.get("path")
    hash_value = anchor.get("sha256") or anchor.get("hash")
    hash_basis = anchor.get("hash_basis")
    if url:
        skipped.append({"note_id": note_id, "claim_id": claim_id,
                        "anchor": str(path_value or url), "reason": "external_url"})
        return
    if hash_basis:
        skipped.append({"note_id": note_id, "claim_id": claim_id,
                        "anchor": str(path_value or anchor.get("locator", "label")),
                        "reason": "hash_basis: " + str(hash_basis)})
        return
    local = is_workspace_local_path(path_value, root)
    if local is None:
        skipped.append({"note_id": note_id, "claim_id": claim_id,
                        "anchor": str(path_value or anchor.get("contract_id") or anchor.get("locator", "label")),
                        "reason": "external_repo_relative_or_label_anchor"})
        return
    if not local.is_file():
        errors.append("{} {} local evidence path is missing: {}".format(note_id, claim_id, path_value))
        return
    if not hash_value:
        skipped.append({"note_id": note_id, "claim_id": claim_id,
                        "anchor": str(path_value), "reason": "local_anchor_without_hash"})
        return
    actual = sha256(local.read_bytes())
    if actual != hash_value:
        drift.append({"note_id": note_id, "claim_id": claim_id,
                      "path": repo_relative(local, root), "expected_sha256": hash_value,
                      "current_sha256": actual})


def find_source_span(statement, sources):
    try:
        needle = statement.encode("utf-8")
    except Exception:
        return None
    for source in sources:
        start = source["raw"].find(needle)
        if start >= 0:
            return source, start, start + len(needle)
    return None


def choose_source(statement, sources):
    # Prefer the exact extracted article where present; retain the complete message as fallback.
    ordered = sorted(sources, key=lambda row: ("supplied-notes" not in Path(row["receipt_path"]).name.lower(), row["receipt_path"]))
    exact = find_source_span(statement, ordered)
    if exact:
        return exact[0], exact[1], exact[2], "verbatim"
    if not sources:
        return None, None, None, "audit_paraphrase"
    source = ordered[0]
    return source, None, None, "audit_paraphrase"


def audit_for(claim_path, payload, notes_root, root):
    explicit = payload.get("source_audit")
    if explicit:
        candidate = Path(explicit)
        return candidate if candidate.is_absolute() else root / candidate
    audit_name = AUDITS.get(claim_path.name)
    if audit_name:
        return claim_path.parent / audit_name
    candidate = claim_path.parent / "audit.md"
    return candidate


def validate_world_model(root, claim_ids, errors, drift, skipped):
    path = root / "docs/research/skyrim-render-map/research-notes/world-model.json"
    if not path.exists():
        return {"present": False}
    graph = read_json(path, errors)
    if graph is None:
        return {"present": True}
    contracts_file = root / "docs/research/skyrim-render-map/input-contracts.json"
    contract_ids = set()
    if contracts_file.exists():
        catalog = read_json(contracts_file, errors)
        if isinstance(catalog, dict):
            contract_ids = set(row.get("id") for row in catalog.get("contracts", [])
                               if isinstance(row, dict) and row.get("id"))
    nodes = graph.get("nodes", []) if isinstance(graph, dict) else []
    edges = graph.get("edges", []) if isinstance(graph, dict) else []
    if not isinstance(nodes, list) or not nodes:
        errors.append("world-model.json must declare a nonempty nodes array")
        nodes = []
    if not isinstance(edges, list) or not edges:
        errors.append("world-model.json must declare a nonempty edges array")
        edges = []
    node_ids = set(row.get("id") for row in nodes if isinstance(row, dict) and row.get("id"))
    edge_ids = set(row.get("id") for row in edges if isinstance(row, dict) and row.get("id"))
    if len(node_ids) != len([row for row in nodes if isinstance(row, dict) and row.get("id")]):
        errors.append("world-model.json has duplicate node IDs")
    if len(edge_ids) != len([row for row in edges if isinstance(row, dict) and row.get("id")]):
        errors.append("world-model.json has duplicate edge IDs")
    if any(not isinstance(row, dict) or not isinstance(row.get("id"), str) or not row["id"] for row in nodes):
        errors.append("every world-model node must have a nonempty id")
    if any(not isinstance(row, dict) or not isinstance(row.get("id"), str) or not row["id"] for row in edges):
        errors.append("every world-model edge must have a nonempty id")
    if isinstance(graph, dict):
        scope = graph.get("scope", {})
        if not isinstance(scope, dict):
            errors.append("world-model scope must be an object")
        else:
            for flag in ("conceptual_dependencies_only", "frame_order_recovered",
                         "runtime_observed", "complete_selected_retail_route"):
                expected = flag == "conceptual_dependencies_only"
                if scope.get(flag) is not expected:
                    errors.append("world-model scope.{} must be {}".format(flag, str(expected).lower()))

    def verify_path(value, owner, field, expected_hash=None):
        local = is_workspace_local_path(value, root)
        if local is None:
            skipped.append({"anchor": value, "reason": "world_model_nonworkspace_path"})
        elif not local.exists():
            errors.append("world-model {} {} path is missing: {}".format(owner, field, value))
        elif expected_hash and local.is_file():
            actual_hash = sha256(local.read_bytes())
            if actual_hash != expected_hash:
                drift.append({"path": repo_relative(local, root), "expected_sha256": expected_hash,
                              "current_sha256": actual_hash, "source": "world-model reference"})

    for row in nodes:
        if not isinstance(row, dict):
            continue
        if row.get("complete_selected_retail_route") is not False:
            errors.append("world-model node {} must declare complete_selected_retail_route=false".format(row.get("id")))
        for claim_id in row.get("claim_ids", []):
            if claim_id not in claim_ids:
                errors.append("world-model node {} references unknown claim ID {}".format(row.get("id"), claim_id))
        for contract_id in row.get("contract_backlinks", row.get("contract_ids", [])):
            if contract_id not in contract_ids:
                errors.append("world-model node {} references unknown contract ID {}".format(row.get("id"), contract_id))
        paths = row.get("implementation_paths", [])
        if not isinstance(paths, list):
            errors.append("world-model node {} implementation_paths must be an array".format(row.get("id")))
        else:
            for value in paths:
                verify_path(value, "node {}".format(row.get("id")), "implementation_paths")
    for row in edges:
        if not isinstance(row, dict):
            continue
        for endpoint in ("from", "to"):
            if row.get(endpoint) not in node_ids:
                errors.append("world-model edge {} has unknown {} node {!r}".format(row.get("id"), endpoint, row.get(endpoint)))
        for flag in ("proven_frame_order", "complete_selected_retail_route"):
            if row.get(flag) is not False:
                errors.append("world-model edge {} must declare {}=false".format(row.get("id"), flag))
    def check_proof_flag(key, value):
        normalized = key.lower()
        route_or_order = (
            "complete" in normalized and "retail" in normalized and "route" in normalized
        ) or normalized in ("proven_frame_order", "frame_order_recovered", "runtime_observed")
        if route_or_order and value is not False:
            errors.append("world-model proof flag {} must be false".format(key))

    def walk(value, key="", in_edge=False):
        if isinstance(value, dict):
            path_value = value.get("path") or value.get("file")
            hash_value = value.get("sha256")
            if isinstance(path_value, str) and isinstance(hash_value, str):
                if is_workspace_local_path(path_value, root) is not None:
                    verify_path(path_value, "world-model", "path", hash_value)
            for k, v in value.items():
                check_proof_flag(k, v)
                walk(v, k, in_edge or key == "edges")
        elif isinstance(value, list):
            for item in value:
                walk(item, key, in_edge)
        elif isinstance(value, str):
            if key in ("claim_id", "claim_ref"):
                if value not in claim_ids:
                    errors.append("world-model.json references unknown claim ID {}".format(value))
            elif key in ("contract_id", "contract_ref"):
                if value not in contract_ids:
                    errors.append("world-model.json references unknown contract ID {}".format(value))
            elif key in ("node_id", "source_node_id", "target_node_id"):
                if value not in node_ids:
                    errors.append("world-model.json references unknown node ID {}".format(value))
            elif key in ("edge_id", "edge_ref"):
                if value not in edge_ids:
                    errors.append("world-model.json references unknown edge ID {}".format(value))
            elif in_edge and key in ("from", "to", "source", "target") and node_ids and value not in node_ids:
                errors.append("world-model.json edge references unknown node {}".format(value))
            elif key in ("path", "file", "artifact", "workspace_path", "implementation_path"):
                if value.startswith(("http://", "https://")):
                    skipped.append({"anchor": value, "reason": "world_model_external_url"})
                else:
                    verify_path(value, "world-model", key)
    walk(graph)
    return {"present": True, "node_count": len(node_ids), "edge_count": len(edge_ids),
            "node_ids": sorted(node_ids)}


def validate_model_associations(root, claim_ids, node_ids, errors):
    path = root / "docs/research/skyrim-render-map/research-notes/model-associations.json"
    if not path.is_file():
        errors.append("model-associations.json is missing")
        return {"present": False}
    payload = read_json(path, errors)
    if not isinstance(payload, dict):
        return {"present": True}
    if payload.get("schema") != "mudcrab-research-note-model-associations/v1":
        errors.append("model-associations.json has unexpected schema")
    rows = payload.get("associations")
    if not isinstance(rows, list):
        errors.append("model-associations.json associations must be an array")
        return {"present": True}
    seen = set()
    for row in rows:
        if not isinstance(row, dict):
            errors.append("model-associations.json contains a non-object association")
            continue
        cid = row.get("claim_id")
        if not isinstance(cid, str) or cid not in claim_ids:
            errors.append("model association references unknown claim ID {!r}".format(cid))
        if cid in seen:
            errors.append("duplicate model association for claim ID {}".format(cid))
        seen.add(cid)
        linked = row.get("node_ids")
        if not isinstance(linked, list) or not linked:
            errors.append("model association {} must have a nonempty node_ids array".format(cid))
            continue
        for node_id in linked:
            if node_id not in node_ids:
                errors.append("model association {} references unknown node ID {!r}".format(cid, node_id))
    return {"present": True, "association_count": len(seen),
            "unassociated_claim_ids": sorted(claim_ids - seen)}


def validate_source_sections(root, claim_ids, sources, errors):
    notes_root = root / "docs/research/skyrim-render-map/research-notes"
    paths = sorted(notes_root.glob("note-*/source-sections.json"))
    total_sections = 0
    checked = []
    for path in paths:
        sections = read_json(path, errors)
        if not isinstance(sections, dict):
            continue
        rel_path = repo_relative(path, root)
        if sections.get("schema") != "mudcrab-research-note-sections/v1":
            errors.append("{} has unexpected schema".format(rel_path))
        note_id = sections.get("note_id")
        source = sections.get("source", {})
        source_path = source.get("path") if isinstance(source, dict) else None
        note_sources = sources.get(note_id, [])
        source_row = next((row for row in note_sources if row.get("receipt_path") == source_path), None)
        if source_row is None:
            errors.append("{} source path is absent from its input receipt".format(rel_path))
            continue
        if source.get("sha256") != source_row.get("sha256"):
            errors.append("{} source hash does not match its input receipt".format(rel_path))
        rows = sections.get("sections")
        if not isinstance(rows, list) or not rows:
            errors.append("{} sections must be a nonempty array".format(rel_path))
            continue
        seen = set()
        for row in rows:
            if not isinstance(row, dict) or not isinstance(row.get("id"), str):
                errors.append("{} contains a section without an ID".format(rel_path))
                continue
            section_id = row["id"]
            if section_id in seen:
                errors.append("{} duplicate section ID {}".format(rel_path, section_id))
            seen.add(section_id)
            span = row.get("source_span_bytes")
            if not isinstance(span, dict) or not isinstance(span.get("start"), int) or not isinstance(span.get("end_exclusive"), int):
                errors.append("source section {} has invalid byte span".format(section_id))
                continue
            start, end = span["start"], span["end_exclusive"]
            if start < 0 or end < start or end > len(source_row["raw"]):
                errors.append("source section {} byte span is outside source".format(section_id))
                continue
            heading = row.get("heading_verbatim")
            if not isinstance(heading, str) or heading.encode("utf-8") not in source_row["raw"][start:end]:
                errors.append("source section {} heading is not verbatim within its byte span".format(section_id))
            refs = row.get("claim_ids")
            if not isinstance(refs, list):
                errors.append("source section {} claim_ids must be an array".format(section_id))
            else:
                for cid in refs:
                    if cid not in claim_ids:
                        errors.append("source section {} references unknown claim ID {}".format(section_id, cid))
        total_sections += len(seen)
        checked.append({"path": rel_path, "note_id": note_id, "section_count": len(seen)})
    return {"present": bool(paths), "files": checked, "section_count": total_sections}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh", action="store_true", help="regenerate claims-index.json")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    notes_root = root / "docs/research/skyrim-render-map/research-notes"
    receipt_path = notes_root / "input-receipt.json"
    index_path = notes_root / "claims-index.json"
    errors = []
    drift = []
    skipped = []
    receipt_paths = [receipt_path] + sorted(notes_root.glob("note-*/input-receipt*.json"))
    if not receipt_path.is_file():
        errors.append("root input-receipt.json is missing")
    sources, receipts = source_artifacts(root, receipt_paths, errors)
    claim_files = sorted(notes_root.glob("note-*/claims-*.json"))
    if not claim_files:
        errors.append("no note-*/claims-*.json ledgers found")
    receipt_ids = set(sources.keys())
    all_ids = set()
    ledgers = []
    index_claims = []
    for claim_path in claim_files:
        payload = read_json(claim_path, errors)
        if not isinstance(payload, dict):
            continue
        note_id = payload.get("note_id")
        if payload.get("schema") != SCHEMA:
            errors.append("{} has unexpected claims schema".format(repo_relative(claim_path, root)))
        if note_id not in receipt_ids:
            errors.append("{} note_id {} has no matching input receipt".format(claim_path, note_id))
        claims = payload.get("claims")
        if not isinstance(claims, list):
            errors.append("{} claims must be an array".format(claim_path))
            continue
        audit_path = audit_for(claim_path, payload, notes_root, root)
        if not audit_path.is_file():
            errors.append("{} audit file is missing: {}".format(note_id, audit_path))
            audit_hash = None
            audit_rel = str(audit_path)
        else:
            audit_hash = sha256(audit_path.read_bytes())
            audit_rel = repo_relative(audit_path, root)
        ledger_hash = sha256(claim_path.read_bytes())
        for claim in claims:
            if not isinstance(claim, dict):
                errors.append("{} contains a non-object claim".format(claim_path))
                continue
            cid = claim.get("id")
            if not isinstance(cid, str) or not cid.startswith(str(note_id) + "-"):
                errors.append("{} has invalid stable claim ID {!r}".format(note_id, cid))
                continue
            if cid in all_ids:
                errors.append("duplicate claim ID {}".format(cid))
            all_ids.add(cid)
            scope = claim.get("subject_scope") or claim.get("topic")
            evidence_rows = collect_anchors(claim)
            unresolved = claim.get("unresolved")
            next_query = claim.get("next_query")
            if not isinstance(claim.get("supplied_statement"), str) or not claim["supplied_statement"]:
                errors.append("{} {} missing supplied_statement".format(note_id, cid))
            scope_valid = ((isinstance(scope, str) and bool(scope.strip())) or
                           (isinstance(scope, list) and bool(scope)))
            if not scope_valid:
                errors.append("{} {} missing scope/topic".format(note_id, cid))
            raw_evidence = claim.get("evidence", claim.get("evidence_anchors"))
            evidence_valid = ((isinstance(raw_evidence, str) and bool(raw_evidence.strip())) or
                              (isinstance(raw_evidence, list) and bool(raw_evidence)) or
                              isinstance(raw_evidence, dict))
            if not evidence_valid:
                errors.append("{} {} missing evidence".format(note_id, cid))
            if not isinstance(unresolved, (list, str)):
                errors.append("{} {} unresolved must be list or string".format(note_id, cid))
            if not isinstance(next_query, (list, str)) or not next_query:
                errors.append("{} {} next_query must be nonempty list or string".format(note_id, cid))
            if claim.get("assessment") not in ASSESSMENTS:
                errors.append("{} {} has invalid assessment {!r}".format(note_id, cid, claim.get("assessment")))
            for _, anchor in evidence_rows:
                check_anchor(anchor, root, note_id, cid, drift, skipped, errors)
            src, start, end, quote_status = choose_source(claim.get("supplied_statement", ""), sources.get(note_id, []))
            if src is None:
                errors.append("{} has no source artifact in receipt".format(note_id))
                source_path = None
                source_hash = None
            else:
                source_path = src["receipt_path"]
                source_hash = src["sha256"]
            source_row = {
                "path": source_path,
                "sha256": source_hash,
                "quotation_status": quote_status,
                "receipt_path": receipts.get(note_id, {}).get("path"),
                "receipt_sha256": receipts.get(note_id, {}).get("sha256"),
            }
            if quote_status == "verbatim":
                source_row["source_span_bytes"] = {"start": start, "end_exclusive": end}
            verbatim_fields = []
            for field, value in claim.items():
                if not field.endswith("_verbatim"):
                    continue
                if not isinstance(value, str) or not value:
                    errors.append("{} {} {} must be a nonempty verbatim string".format(note_id, cid, field))
                    continue
                field_sources = sorted(
                    sources.get(note_id, []),
                    key=lambda row: ("supplied-notes" not in Path(row["receipt_path"]).name.lower(),
                                    row["receipt_path"]))
                found = find_source_span(value, field_sources)
                if found is None:
                    errors.append("{} {} {} is not an exact source substring".format(note_id, cid, field))
                    continue
                field_source, field_start, field_end = found
                verbatim_fields.append({
                    "field": field,
                    "source": {
                        "path": field_source["receipt_path"],
                        "sha256": field_source["sha256"],
                        "source_span_bytes": {"start": field_start, "end_exclusive": field_end},
                    },
                })
            index_claims.append({
                "id": cid,
                "note_id": note_id,
                "domain": payload.get("domain"),
                "assessment": claim.get("assessment"),
                "scope": scope,
                "audit": {"path": audit_rel, "sha256": audit_hash},
                "claim_record": {"path": repo_relative(claim_path, root), "sha256": ledger_hash},
                "source": source_row,
                "statement": claim.get("supplied_statement"),
                "verbatim_fields": verbatim_fields,
                "relates_to": claim.get("relates_to", []),
            })
        ledgers.append({"path": repo_relative(claim_path, root), "note_id": note_id,
                        "claim_count": len(claims)})
    for row in index_claims:
        relations = row.get("relates_to")
        if not isinstance(relations, list):
            errors.append("{} {} relates_to must be an array".format(row["note_id"], row["id"]))
            continue
        for relation in relations:
            if not isinstance(relation, dict):
                errors.append("{} {} relates_to entries must be objects".format(row["note_id"], row["id"]))
                continue
            target = relation.get("claim_id")
            kind = relation.get("relation")
            if not isinstance(target, str) or target not in all_ids:
                errors.append("{} {} relates_to references unknown claim ID {!r}".format(row["note_id"], row["id"], target))
            if kind not in RELATIONSHIPS:
                errors.append("{} {} has invalid relates_to relation {!r}".format(row["note_id"], row["id"], kind))
    # Receipt note entries without a corresponding claim ledger are permitted for staged inputs,
    # but every source artifact is still byte-checked above.
    world_model = validate_world_model(root, all_ids, errors, drift, skipped)
    model_associations = validate_model_associations(
        root, all_ids, set(world_model.get("node_ids", [])), errors)
    source_sections = validate_source_sections(root, all_ids, sources, errors)
    index = {"schema": INDEX_SCHEMA, "claims": sorted(index_claims, key=lambda row: (row["note_id"], row["id"]))}
    encoded = (json.dumps(index, ensure_ascii=False, indent=2, sort_keys=False) + "\n").encode("utf-8")
    index_consistent = index_path.is_file() and index_path.read_bytes() == encoded
    if args.refresh and not errors:
        index_path.write_bytes(encoded)
        index_consistent = True
    elif not args.refresh and not index_consistent:
        errors.append("claims-index.json is missing or stale; run with --refresh")
    summary = {
        "ok": not errors,
        "mode": "refresh" if args.refresh else "check",
        "notes": len(sources),
        "ledgers": len(ledgers),
        "claims": len(index_claims),
        "index_consistent": index_consistent,
        "workspace_anchor_drift_count": len(drift),
        "workspace_anchor_drift_examples": drift[:5],
        "skipped_anchor_count": len(skipped),
        "skipped_anchor_reasons": dict(Counter(row["reason"].split(":", 1)[0] for row in skipped)),
        "skipped_anchor_examples": skipped[:5],
        "world_model": world_model,
        "model_associations": model_associations,
        "source_sections": source_sections,
        "errors": errors,
    }
    print(json.dumps(summary, ensure_ascii=False, separators=(",", ":")))
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
