#!/usr/bin/env python3
"""Skyrim flag inventory and disposition ratchet guard.

Checks that every known flag in catalog.json has a valid disposition, valid
repo-relative evidence anchors, an issue number or written reason when dropped
or partial, and a dropped+partial count at or below baseline.json.
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

ALLOWED_DISPOSITIONS = _common.BASE_DISPOSITIONS
RATCHET_DISPOSITIONS = _common.RATCHET_DISPOSITIONS
CATALOG_FIELDS = ("id", "container", "bit", "name", "meaning", "source", "group")
ENTRY_FIELDS = {"disposition", "owner", "reason", "evidence", "issue"}
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
        missing = [field for field in CATALOG_FIELDS if field not in item]
        extra = sorted(set(item) - set(CATALOG_FIELDS))
        if missing:
            raise ValueError(f"catalog entry {index} missing fields: {', '.join(missing)}")
        if extra:
            raise ValueError(f"catalog entry {index} has unknown fields: {', '.join(extra)}")
        flag_id = item["id"]
        if not isinstance(flag_id, str) or not flag_id.strip():
            raise ValueError(f"catalog entry {index} has empty or non-string id")
        if flag_id in by_id:
            raise ValueError(f"duplicate catalog id: {flag_id}")
        for field in ("container", "source", "group"):
            if not isinstance(item[field], str) or not item[field].strip():
                raise ValueError(f"catalog {flag_id} has empty or non-string {field}")
        for field in ("name", "meaning"):
            if not isinstance(item[field], str):
                raise ValueError(f"catalog {flag_id} field {field} must be a string")
        bit = item["bit"]
        if isinstance(bit, bool) or not (isinstance(bit, int) or (isinstance(bit, str) and bit.strip())):
            raise ValueError(f"catalog {flag_id} has invalid bit value: {bit!r}")
        if LOCAL_PATH_RE.search(item["source"]):
            raise ValueError(f"catalog {flag_id} source must not contain local paths: {item['source']}")
        by_id[flag_id] = item
    return by_id


def validate_dispositions(root, catalog_by_id, doc):
    """Validate dispositions.json against catalog_by_id and repository files."""
    if not isinstance(doc, dict):
        raise ValueError("dispositions.json must be a JSON object")
    fmt = doc.get("format_version")
    if isinstance(fmt, bool) or fmt != 1:
        raise ValueError(f"unsupported dispositions format_version: {fmt!r}")
    extra_top = sorted(set(doc) - {"format_version", "dispositions"})
    if extra_top:
        raise ValueError(f"unexpected top-level keys in dispositions.json: {', '.join(extra_top)}")
    entries = doc.get("dispositions")
    if not isinstance(entries, dict):
        raise ValueError("dispositions.json must contain a 'dispositions' object")

    _common.validate_id_coverage(set(catalog_by_id), set(entries), item_label="flag")

    file_cache = {}
    counts = Counter()

    for flag_id in sorted(entries):
        disposition = _common.validate_entry(
            root,
            flag_id,
            entries[flag_id],
            allowed_dispositions=ALLOWED_DISPOSITIONS,
            entry_fields=ENTRY_FIELDS,
            file_cache=file_cache,
            ratchet_dispositions=RATCHET_DISPOSITIONS,
            allow_spec=False,
            require_issue_field_on_ratchet=True,
        )
        counts[disposition] += 1

    return {disp: counts.get(disp, 0) for disp in ALLOWED_DISPOSITIONS}


def check_inventory(root, catalog_path, dispositions_path, baseline_path, *, base_baseline=None, base_ref=None):
    """Run all flag-inventory checks and return a summary dictionary."""
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
        tool_name="flag_inventory",
        item_plural="flags",
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
