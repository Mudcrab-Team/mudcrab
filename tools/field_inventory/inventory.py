#!/usr/bin/env python3
"""Skyrim record field inventory and disposition ratchet guard.

Checks that every known ESM record field and NIF block field in catalog.json
has a valid disposition (either individually in dispositions or grouped by
record signature in system_not_implemented_by_signature), valid repo-relative
evidence anchors, an issue number, spec reference, or written reason when
dropped or partial, and a dropped+partial count at or below baseline.json.
"""

from collections import Counter
import importlib.util
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent


def _load_common():
    path = Path(__file__).resolve().parents[1] / "inventory_common.py"
    spec = importlib.util.spec_from_file_location("inventory_common", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load shared inventory module from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_common = _load_common()

ALLOWED_DISPOSITIONS = _common.BASE_DISPOSITIONS + ("covered-by-flag-audit",)
RATCHET_DISPOSITIONS = _common.RATCHET_DISPOSITIONS
CATALOG_REQUIRED_FIELDS = (
    "id",
    "signature_or_block",
    "subrecord",
    "field",
    "offset",
    "type",
    "meaning",
    "source",
    "kind",
    "occurrences",
)
CATALOG_OPTIONAL_FIELDS = ("size_bytes", "conditional")
CATALOG_ALL_FIELDS = set(CATALOG_REQUIRED_FIELDS) | set(CATALOG_OPTIONAL_FIELDS)
ENTRY_FIELDS = {"disposition", "owner", "reason", "evidence", "issue", "spec"}
GROUP_FIELDS = {"disposition", "missing_system", "owner", "reason", "evidence", "field_ids"}
LOCAL_PATH_RE = _common.LOCAL_PATH_RE

load_json = _common.load_json
write_json = _common.write_json
validate_evidence_refs = _common.validate_evidence_refs
validate_baseline = _common.validate_baseline


def validate_catalog(catalog):
    """Validate catalog.json structure and return {id: entry}."""
    if not isinstance(catalog, list) or not catalog:
        raise ValueError("catalog.json must be a non-empty JSON array")
    by_id = {}
    for index, item in enumerate(catalog):
        if not isinstance(item, dict):
            raise ValueError(f"catalog entry {index} must be an object")
        missing = [field for field in CATALOG_REQUIRED_FIELDS if field not in item]
        extra = sorted(set(item) - CATALOG_ALL_FIELDS)
        if missing:
            raise ValueError(f"catalog entry {index} missing fields: {', '.join(missing)}")
        if extra:
            raise ValueError(f"catalog entry {index} has unknown fields: {', '.join(extra)}")
        field_id = item["id"]
        if not isinstance(field_id, str) or not field_id.strip():
            raise ValueError(f"catalog entry {index} has empty or non-string id")
        if field_id in by_id:
            raise ValueError(f"duplicate catalog id: {field_id}")
        for req_str in ("signature_or_block", "field", "type", "source", "kind"):
            if not isinstance(item[req_str], str) or not item[req_str].strip():
                raise ValueError(f"catalog {field_id} has empty or non-string {req_str}")
        if item["kind"] not in ("esm", "nif"):
            raise ValueError(f"catalog {field_id} has invalid kind: {item['kind']!r}")
        if item["subrecord"] is not None and not isinstance(item["subrecord"], str):
            raise ValueError(f"catalog {field_id} field subrecord must be a string or null")
        if not isinstance(item["meaning"], str):
            raise ValueError(f"catalog {field_id} field meaning must be a string")
        offset = item["offset"]
        if isinstance(offset, bool) or (
            offset is not None and not isinstance(offset, (int, str))
        ):
            raise ValueError(f"catalog {field_id} has invalid offset: {offset!r}")
        for int_or_none in ("occurrences", "size_bytes"):
            val = item.get(int_or_none)
            if isinstance(val, bool) or (val is not None and (not isinstance(val, int) or val < 0)):
                raise ValueError(f"catalog {field_id} has invalid {int_or_none}: {val!r}")
        cond = item.get("conditional")
        if cond is not None and not isinstance(cond, str):
            raise ValueError(f"catalog {field_id} conditional must be string or null")
        if LOCAL_PATH_RE.search(item["source"]):
            raise ValueError(f"catalog {field_id} source must not contain local paths: {item['source']}")
        by_id[field_id] = item
    return by_id


def validate_dispositions(root, catalog_by_id, doc):
    """Validate dispositions.json against catalog_by_id and repository files."""
    if not isinstance(doc, dict):
        raise ValueError("dispositions.json must be a JSON object")
    fmt = doc.get("format_version")
    if isinstance(fmt, bool) or fmt != 1:
        raise ValueError(f"unsupported dispositions format_version: {fmt!r}")
    allowed_top = {"format_version", "dispositions", "system_not_implemented_by_signature"}
    extra_top = sorted(set(doc) - allowed_top)
    if extra_top:
        raise ValueError(f"unexpected top-level keys in dispositions.json: {', '.join(extra_top)}")
    entries = doc.get("dispositions")
    if not isinstance(entries, dict):
        raise ValueError("dispositions.json must contain a 'dispositions' object")

    groups = doc.get("system_not_implemented_by_signature", {})
    if not isinstance(groups, dict):
        raise ValueError("system_not_implemented_by_signature must be an object")

    file_cache = {}
    counts = Counter()

    for field_id in sorted(entries):
        disposition = _common.validate_entry(
            root,
            field_id,
            entries[field_id],
            allowed_dispositions=ALLOWED_DISPOSITIONS,
            entry_fields=ENTRY_FIELDS,
            file_cache=file_cache,
            ratchet_dispositions=RATCHET_DISPOSITIONS,
            allow_spec=True,
            require_issue_field_on_ratchet=False,
        )
        counts[disposition] += 1

    grouped_ids = set()
    for sig in sorted(groups):
        group = groups[sig]
        if not isinstance(group, dict):
            raise ValueError(f"signature group {sig}: must be an object")
        missing = sorted({"disposition", "missing_system", "owner", "reason", "evidence", "field_ids"} - set(group))
        extra = sorted(set(group) - GROUP_FIELDS)
        if missing:
            raise ValueError(f"signature group {sig}: missing fields: {', '.join(missing)}")
        if extra:
            raise ValueError(f"signature group {sig}: unknown fields: {', '.join(extra)}")
        if group["disposition"] != "system-not-implemented":
            raise ValueError(
                f"signature group {sig}: disposition must be 'system-not-implemented', got {group['disposition']!r}"
            )
        for req_str in ("missing_system", "owner", "reason"):
            if not isinstance(group[req_str], str) or not group[req_str].strip():
                raise ValueError(f"signature group {sig}: {req_str} must be a non-empty string")
        validate_evidence_refs(root, f"signature group {sig}", group["evidence"], file_cache)
        if not group["evidence"]:
            raise ValueError(f"signature group {sig}: must cite at least one evidence anchor")
        field_ids = group["field_ids"]
        if not isinstance(field_ids, list) or not field_ids:
            raise ValueError(f"signature group {sig}: field_ids must be a non-empty list")
        for fid in field_ids:
            if not isinstance(fid, str) or not fid.strip():
                raise ValueError(f"signature group {sig}: field_ids must contain non-empty strings")
            if fid in entries:
                raise ValueError(f"signature group {sig}: field {fid} is also in 'dispositions'")
            if fid in grouped_ids:
                raise ValueError(f"signature group {sig}: duplicate field_id {fid}")
            if fid in catalog_by_id and catalog_by_id[fid]["signature_or_block"] != sig:
                raise ValueError(
                    f"signature group {sig}: field {fid} belongs to signature_or_block "
                    f"{catalog_by_id[fid]['signature_or_block']!r}"
                )
            grouped_ids.add(fid)
        counts["system-not-implemented"] += len(field_ids)

    _common.validate_id_coverage(set(catalog_by_id), set(entries) | grouped_ids, item_label="field")
    return {disp: counts.get(disp, 0) for disp in ALLOWED_DISPOSITIONS}


def check_inventory(root, catalog_path, dispositions_path, baseline_path, *, base_baseline=None, base_ref=None):
    """Run all field-inventory checks and return a summary dictionary."""
    catalog = load_json(catalog_path, "catalog")
    catalog_by_id = validate_catalog(catalog)
    dispositions_doc = load_json(dispositions_path, "dispositions")
    counts = validate_dispositions(root, catalog_by_id, dispositions_doc)
    baseline_doc = load_json(baseline_path, "baseline")
    dropped_plus_partial, baseline = validate_baseline(baseline_doc, counts)
    base_baseline_limit, base_note = _common.validate_base_ceiling(
        root,
        baseline_path,
        baseline_doc,
        base_baseline_path=base_baseline,
        base_ref=base_ref,
    )
    return {
        "total": len(catalog_by_id),
        "counts": counts,
        "dropped_plus_partial": dropped_plus_partial,
        "baseline": baseline,
        "base_baseline": base_baseline_limit,
        "base_note": base_note,
    }


def update_baseline(root, catalog_path, dispositions_path, baseline_path):
    """Validate catalog and dispositions, then write dropped+partial to baseline.json."""
    catalog = load_json(catalog_path, "catalog")
    catalog_by_id = validate_catalog(catalog)
    dispositions_doc = load_json(dispositions_path, "dispositions")
    counts = validate_dispositions(root, catalog_by_id, dispositions_doc)
    current = _common.update_baseline_file(baseline_path, counts, RATCHET_DISPOSITIONS)
    return current, counts


def run(argv, root=ROOT, here=HERE):
    return _common.run_cli(
        argv,
        tool_name="field_inventory",
        item_plural="fields",
        default_root=ROOT,
        root=root,
        here=here,
        description=__doc__,
        check_fn=check_inventory,
        update_baseline_fn=update_baseline,
    )


def main():
    try:
        run(sys.argv[1:])
    except ValueError as error:
        sys.exit(f"error: {error}")


if __name__ == "__main__":
    main()
