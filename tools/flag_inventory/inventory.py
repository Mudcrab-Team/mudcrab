#!/usr/bin/env python3
"""Skyrim flag inventory and disposition ratchet guard.

Checks that every known flag in catalog.json has a valid disposition, valid
repo-relative evidence anchors, an issue number or written reason when dropped
or partial, and a dropped+partial count at or below baseline.json.
"""

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent

ALLOWED_DISPOSITIONS = (
    "respected",
    "partial",
    "dropped",
    "system-not-implemented",
    "format-not-read",
    "record-not-read",
    "not-applicable",
)
RATCHET_DISPOSITIONS = ("dropped", "partial")
CATALOG_FIELDS = ("id", "container", "bit", "name", "meaning", "source", "group")
ENTRY_FIELDS = {"disposition", "owner", "reason", "evidence", "issue"}
LOCAL_PATH_RE = re.compile(r"(?:^/|/Users/|/home/|^[A-Za-z]:[/\\]|\\\\)")


def load_json(path, label):
    """Read and parse a UTF-8 JSON file, raising ValueError on failure."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise ValueError(f"cannot read {label} ({path}): {error}") from None
    try:
        return json.loads(text)
    except json.JSONDecodeError as error:
        raise ValueError(f"invalid JSON in {label} ({path}): {error}") from None


def write_json(path, payload):
    """Write canonical UTF-8 JSON with LF newlines."""
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(json.dumps(payload, indent=2, ensure_ascii=False) + "\n")


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


def validate_evidence_refs(root, flag_id, refs, file_cache):
    """Validate that every {path, anchor} pair resolves in the repository tree."""
    if not isinstance(refs, list):
        raise ValueError(f"{flag_id}: evidence must be a list")
    root_resolved = Path(root).resolve()
    for ref in refs:
        if not isinstance(ref, dict):
            raise ValueError(f"{flag_id}: evidence item must be an object: {ref!r}")
        if set(ref) != {"path", "anchor"}:
            raise ValueError(f"{flag_id}: evidence item must have exactly path and anchor: {ref!r}")
        rel = ref["path"]
        anchor = ref["anchor"]
        if not isinstance(rel, str) or not rel.strip():
            raise ValueError(f"{flag_id}: evidence path must be a non-empty string")
        if not isinstance(anchor, str) or not anchor.strip():
            raise ValueError(f"{flag_id}: evidence anchor must be a non-empty string")
        rel_path = Path(rel)
        if rel_path.is_absolute() or ".." in rel_path.parts or LOCAL_PATH_RE.search(rel):
            raise ValueError(f"{flag_id}: evidence path must be repo-relative: {rel}")
        if rel not in file_cache:
            target = (root_resolved / rel_path).resolve()
            try:
                target.relative_to(root_resolved)
            except ValueError:
                raise ValueError(f"{flag_id}: evidence path escapes repo root: {rel}") from None
            if not target.is_file():
                raise ValueError(f"{flag_id}: broken evidence anchor; missing file: {rel}")
            try:
                file_cache[rel] = target.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError) as error:
                raise ValueError(f"{flag_id}: cannot read evidence file {rel}: {error}") from None
        if anchor not in file_cache[rel]:
            raise ValueError(f"{flag_id}: broken evidence anchor in {rel}: {anchor!r}")


def validate_dispositions(root, catalog_by_id, doc):
    """Validate dispositions.json against catalog_by_id and repository files."""
    if not isinstance(doc, dict):
        raise ValueError("dispositions.json must be a JSON object")
    if doc.get("format_version") != 1:
        raise ValueError(f"unsupported dispositions format_version: {doc.get('format_version')!r}")
    extra_top = sorted(set(doc) - {"format_version", "dispositions"})
    if extra_top:
        raise ValueError(f"unexpected top-level keys in dispositions.json: {', '.join(extra_top)}")
    entries = doc.get("dispositions")
    if not isinstance(entries, dict):
        raise ValueError("dispositions.json must contain a 'dispositions' object")

    missing_ids = sorted(set(catalog_by_id) - set(entries))
    if missing_ids:
        preview = ", ".join(missing_ids[:10])
        suffix = f" (+{len(missing_ids) - 10} more)" if len(missing_ids) > 10 else ""
        raise ValueError(f"catalog flag(s) without disposition ({len(missing_ids)}): {preview}{suffix}")

    unknown_ids = sorted(set(entries) - set(catalog_by_id))
    if unknown_ids:
        preview = ", ".join(unknown_ids[:10])
        suffix = f" (+{len(unknown_ids) - 10} more)" if len(unknown_ids) > 10 else ""
        raise ValueError(f"disposition id(s) absent from catalog ({len(unknown_ids)}): {preview}{suffix}")

    allowed = set(ALLOWED_DISPOSITIONS)
    file_cache = {}
    counts = Counter()

    for flag_id in sorted(entries):
        entry = entries[flag_id]
        if not isinstance(entry, dict):
            raise ValueError(f"{flag_id}: disposition entry must be an object")
        unknown_fields = sorted(set(entry) - ENTRY_FIELDS)
        if unknown_fields:
            raise ValueError(f"{flag_id}: unknown fields: {', '.join(unknown_fields)}")
        for req in ("disposition", "owner", "reason", "evidence"):
            if req not in entry:
                raise ValueError(f"{flag_id}: missing required field '{req}'")

        disposition = entry["disposition"]
        if not isinstance(disposition, str) or disposition not in allowed:
            raise ValueError(
                f"{flag_id}: invalid disposition {disposition!r}; "
                f"expected one of {', '.join(ALLOWED_DISPOSITIONS)}"
            )

        owner = entry["owner"]
        if not isinstance(owner, str) or not owner.strip():
            raise ValueError(f"{flag_id}: owner must be a non-empty string")

        reason = entry["reason"]
        if not isinstance(reason, str):
            raise ValueError(f"{flag_id}: reason must be a string")

        if disposition in RATCHET_DISPOSITIONS:
            if "issue" not in entry or not isinstance(entry["issue"], str):
                raise ValueError(f"{flag_id}: {disposition} entry must include an 'issue' string field")
            if not entry["issue"].strip() and not reason.strip():
                raise ValueError(
                    f"{flag_id}: {disposition} entry must have an issue number or a written reason"
                )
        else:
            if "issue" in entry and not isinstance(entry["issue"], str):
                raise ValueError(f"{flag_id}: 'issue' must be a string")
            if not reason.strip():
                raise ValueError(f"{flag_id}: reason must be non-empty")

        validate_evidence_refs(root, flag_id, entry["evidence"], file_cache)
        if disposition == "respected" and not entry["evidence"]:
            raise ValueError(f"{flag_id}: respected flag must cite at least one evidence anchor")

        counts[disposition] += 1

    ordered_counts = {disp: counts.get(disp, 0) for disp in ALLOWED_DISPOSITIONS}
    return ordered_counts


def validate_baseline(baseline_doc, counts):
    """Validate baseline.json and enforce the dropped+partial ratchet."""
    if not isinstance(baseline_doc, dict):
        raise ValueError("baseline.json must be a JSON object")
    if baseline_doc.get("format_version") != 1:
        raise ValueError(f"unsupported baseline format_version: {baseline_doc.get('format_version')!r}")
    limit = baseline_doc.get("dropped_plus_partial")
    if isinstance(limit, bool) or not isinstance(limit, int) or limit < 0:
        raise ValueError(f"invalid baseline dropped_plus_partial: {limit!r}")
    current = counts["dropped"] + counts["partial"]
    if current > limit:
        raise ValueError(
            f"dropped+partial count rose from baseline {limit} to {current} "
            f"(dropped={counts['dropped']}, partial={counts['partial']})"
        )
    return current, limit


def check_inventory(root, catalog_path, dispositions_path, baseline_path):
    """Run all flag-inventory checks and return a summary dictionary."""
    catalog = load_json(catalog_path, "catalog")
    catalog_by_id = validate_catalog(catalog)
    dispositions_doc = load_json(dispositions_path, "dispositions")
    counts = validate_dispositions(root, catalog_by_id, dispositions_doc)
    baseline_doc = load_json(baseline_path, "baseline")
    dropped_plus_partial, baseline = validate_baseline(baseline_doc, counts)
    return {
        "total": len(catalog_by_id),
        "counts": counts,
        "dropped_plus_partial": dropped_plus_partial,
        "baseline": baseline,
    }


def update_baseline(root, catalog_path, dispositions_path, baseline_path):
    """Validate catalog and dispositions, then write dropped+partial to baseline.json."""
    catalog = load_json(catalog_path, "catalog")
    catalog_by_id = validate_catalog(catalog)
    dispositions_doc = load_json(dispositions_path, "dispositions")
    counts = validate_dispositions(root, catalog_by_id, dispositions_doc)
    current = counts["dropped"] + counts["partial"]
    write_json(baseline_path, {"format_version": 1, "dropped_plus_partial": current})
    return current, counts


def build_parser(here):
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command")

    def add_common(sub):
        sub.add_argument("--root", type=Path, default=ROOT, help="repository root for evidence paths")
        sub.add_argument("--catalog", type=Path, default=here / "catalog.json")
        sub.add_argument("--dispositions", type=Path, default=here / "dispositions.json")
        sub.add_argument("--baseline", type=Path, default=here / "baseline.json")

    add_common(subparsers.add_parser("check", help="validate catalog, dispositions, anchors, and ratchet"))
    add_common(subparsers.add_parser("summary", help="print disposition counts and baseline"))
    add_common(subparsers.add_parser("update-baseline", help="write current dropped+partial count to baseline.json"))
    return parser


def run(argv, root=ROOT, here=HERE):
    if not argv or argv[0] == "--check":
        argv = ["check", *argv[1:]] if argv else ["check"]
    parser = build_parser(here)
    args = parser.parse_args(argv)
    effective_root = args.root if args.root != ROOT or root == ROOT else root

    if args.command in ("check", "summary"):
        result = check_inventory(effective_root, args.catalog, args.dispositions, args.baseline)
        counts_str = ", ".join(f"{k}={v}" for k, v in result["counts"].items())
        print(
            f"flag_inventory: {result['total']} flags; "
            f"dropped+partial={result['dropped_plus_partial']} (baseline={result['baseline']}); "
            f"{counts_str}"
        )
        if result["dropped_plus_partial"] < result["baseline"]:
            print(
                f"note: dropped+partial ({result['dropped_plus_partial']}) is below baseline "
                f"({result['baseline']}); run `inventory.py update-baseline` to tighten the ratchet."
            )
        return result

    if args.command == "update-baseline":
        current, counts = update_baseline(effective_root, args.catalog, args.dispositions, args.baseline)
        print(f"updated baseline to dropped+partial={current} (dropped={counts['dropped']}, partial={counts['partial']})")
        return {"dropped_plus_partial": current, "counts": counts}

    parser.error(f"unknown command: {args.command}")


def main():
    try:
        run(sys.argv[1:])
    except ValueError as error:
        sys.exit(f"error: {error}")


if __name__ == "__main__":
    main()
