"""Shared validation, evidence anchor checking, and CLI helpers for inventory guards."""

import argparse
import json
from pathlib import Path
import re
import subprocess

BASE_DISPOSITIONS = (
    "respected",
    "partial",
    "dropped",
    "system-not-implemented",
    "format-not-read",
    "record-not-read",
    "not-applicable",
)
RATCHET_DISPOSITIONS = ("dropped", "partial")
LOCAL_PATH_RE = re.compile(r"(?:(?:^|\s)(?:/|[A-Za-z]:[/\\])|/Users/|/home/|\\\\)")
ISSUE_REF_RE = re.compile(r"#\d+")
BASELINE_FIELDS = {"format_version", "dropped_plus_partial", "raise_reason"}


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


def validate_evidence_refs(root, item_id, refs, file_cache):
    """Validate that every {path, anchor} pair resolves in the repository tree."""
    if not isinstance(refs, list):
        raise ValueError(f"{item_id}: evidence must be a list")
    root_resolved = Path(root).resolve()
    for ref in refs:
        if not isinstance(ref, dict):
            raise ValueError(f"{item_id}: evidence item must be an object: {ref!r}")
        if set(ref) != {"path", "anchor"}:
            raise ValueError(f"{item_id}: evidence item must have exactly path and anchor: {ref!r}")
        rel = ref["path"]
        anchor = ref["anchor"]
        if not isinstance(rel, str) or not rel.strip():
            raise ValueError(f"{item_id}: evidence path must be a non-empty string")
        if not isinstance(anchor, str) or not anchor.strip():
            raise ValueError(f"{item_id}: evidence anchor must be a non-empty string")
        rel_path = Path(rel)
        if rel_path.is_absolute() or ".." in rel_path.parts or LOCAL_PATH_RE.search(rel):
            raise ValueError(f"{item_id}: evidence path must be repo-relative: {rel}")
        if rel not in file_cache:
            target = (root_resolved / rel_path).resolve()
            try:
                target.relative_to(root_resolved)
            except ValueError:
                raise ValueError(f"{item_id}: evidence path escapes repo root: {rel}") from None
            if not target.is_file():
                raise ValueError(f"{item_id}: broken evidence anchor; missing file: {rel}")
            try:
                file_cache[rel] = target.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError) as error:
                raise ValueError(f"{item_id}: cannot read evidence file {rel}: {error}") from None
        if anchor not in file_cache[rel]:
            raise ValueError(f"{item_id}: broken evidence anchor in {rel}: {anchor!r}")


def validate_entry(
    root,
    item_id,
    entry,
    *,
    allowed_dispositions,
    entry_fields,
    file_cache,
    ratchet_dispositions=RATCHET_DISPOSITIONS,
    allow_spec=False,
    require_issue_field_on_ratchet=False,
):
    """Validate one disposition entry and return its disposition string."""
    if not isinstance(entry, dict):
        raise ValueError(f"{item_id}: disposition entry must be an object")
    unknown_fields = sorted(set(entry) - entry_fields)
    if unknown_fields:
        raise ValueError(f"{item_id}: unknown fields: {', '.join(unknown_fields)}")
    for req in ("disposition", "owner", "reason", "evidence"):
        if req not in entry:
            raise ValueError(f"{item_id}: missing required field '{req}'")

    disposition = entry["disposition"]
    if not isinstance(disposition, str) or disposition not in allowed_dispositions:
        raise ValueError(
            f"{item_id}: invalid disposition {disposition!r}; "
            f"expected one of {', '.join(allowed_dispositions)}"
        )

    owner = entry["owner"]
    if not isinstance(owner, str) or not owner.strip():
        raise ValueError(f"{item_id}: owner must be a non-empty string")

    reason = entry["reason"]
    if not isinstance(reason, str):
        raise ValueError(f"{item_id}: reason must be a string")

    if "issue" in entry and not isinstance(entry["issue"], str):
        raise ValueError(f"{item_id}: 'issue' must be a string")
    if allow_spec and "spec" in entry and not isinstance(entry["spec"], str):
        raise ValueError(f"{item_id}: 'spec' must be a string")

    if disposition in ratchet_dispositions:
        if require_issue_field_on_ratchet and "issue" not in entry:
            raise ValueError(f"{item_id}: {disposition} entry must include an 'issue' string field")
        has_issue = bool(entry.get("issue", "").strip())
        has_spec = bool(entry.get("spec", "").strip()) if allow_spec else False
        if not has_issue and not has_spec and not reason.strip():
            if allow_spec:
                raise ValueError(
                    f"{item_id}: {disposition} entry must have an issue number, spec reference, or a written reason"
                )
            raise ValueError(
                f"{item_id}: {disposition} entry must have an issue number or a written reason"
            )
    else:
        if not reason.strip():
            raise ValueError(f"{item_id}: reason must be non-empty")

    validate_evidence_refs(root, item_id, entry["evidence"], file_cache)
    if disposition == "respected" and not entry["evidence"]:
        raise ValueError(f"{item_id}: respected entry must cite at least one evidence anchor")

    return disposition


def validate_id_coverage(catalog_ids, covered_ids, *, item_label):
    """Verify exact 1-to-1 coverage between catalog IDs and disposition IDs."""
    missing_ids = sorted(catalog_ids - covered_ids)
    if missing_ids:
        preview = ", ".join(missing_ids[:10])
        suffix = f" (+{len(missing_ids) - 10} more)" if len(missing_ids) > 10 else ""
        raise ValueError(f"catalog {item_label}(s) without disposition ({len(missing_ids)}): {preview}{suffix}")

    unknown_ids = sorted(covered_ids - catalog_ids)
    if unknown_ids:
        preview = ", ".join(unknown_ids[:10])
        suffix = f" (+{len(unknown_ids) - 10} more)" if len(unknown_ids) > 10 else ""
        raise ValueError(f"disposition id(s) absent from catalog ({len(unknown_ids)}): {preview}{suffix}")


def parse_baseline_doc(baseline_doc, label="baseline.json"):
    """Validate baseline.json schema and return (limit, raise_reason)."""
    if not isinstance(baseline_doc, dict):
        raise ValueError(f"{label} must be a JSON object")
    extra = sorted(set(baseline_doc) - BASELINE_FIELDS)
    if extra:
        raise ValueError(f"unexpected keys in {label}: {', '.join(extra)}")
    fmt = baseline_doc.get("format_version")
    if isinstance(fmt, bool) or fmt != 1:
        raise ValueError(f"unsupported baseline format_version: {fmt!r}")
    limit = baseline_doc.get("dropped_plus_partial")
    if isinstance(limit, bool) or not isinstance(limit, int) or limit < 0:
        raise ValueError(f"invalid baseline dropped_plus_partial: {limit!r}")
    raise_reason = baseline_doc.get("raise_reason", "")
    if not isinstance(raise_reason, str):
        raise ValueError(f"invalid baseline raise_reason in {label}: must be a string")
    return limit, raise_reason.strip()


def validate_baseline(baseline_doc, counts, ratchet_dispositions=RATCHET_DISPOSITIONS):
    """Validate baseline.json and enforce the dropped+partial ratchet."""
    limit, _ = parse_baseline_doc(baseline_doc)
    current = sum(counts[disp] for disp in ratchet_dispositions)
    if current > limit:
        detail = ", ".join(f"{disp}={counts[disp]}" for disp in ratchet_dispositions)
        raise ValueError(
            f"dropped+partial count rose from baseline {limit} to {current} ({detail})"
        )
    return current, limit


def load_base_baseline_from_git(root, baseline_path, base_ref):
    """Read baseline.json at base_ref via git show, returning None if absent at base."""
    root_resolved = Path(root).resolve()
    baseline_resolved = Path(baseline_path).resolve()
    try:
        rel = baseline_resolved.relative_to(root_resolved).as_posix()
    except ValueError:
        raise ValueError(
            f"baseline path ({baseline_path}) must be inside repo root ({root}) when using --base-ref"
        ) from None

    verify = subprocess.run(
        ["git", "-C", str(root_resolved), "rev-parse", "--verify", f"{base_ref}^{{commit}}"],
        capture_output=True,
        text=True,
        check=False,
    )
    if verify.returncode != 0:
        raise ValueError(f"invalid --base-ref {base_ref!r}: {verify.stderr.strip()}")

    exists = subprocess.run(
        ["git", "-C", str(root_resolved), "cat-file", "-e", f"{base_ref}:{rel}"],
        capture_output=True,
        text=True,
        check=False,
    )
    if exists.returncode != 0:
        return None

    show = subprocess.run(
        ["git", "-C", str(root_resolved), "show", f"{base_ref}:{rel}"],
        capture_output=True,
        text=True,
        check=False,
    )
    if show.returncode != 0:
        raise ValueError(f"git show {base_ref}:{rel} failed: {show.stderr.strip()}")
    try:
        return json.loads(show.stdout)
    except json.JSONDecodeError as error:
        raise ValueError(f"invalid JSON in {base_ref}:{rel}: {error}") from None


def validate_base_ceiling(root, baseline_path, baseline_doc, *, base_baseline_path=None, base_ref=None):
    """Ensure baseline dropped_plus_partial did not rise vs base without an issue-linked raise_reason."""
    if base_baseline_path is not None and base_ref is not None:
        raise ValueError("pass at most one of --base-baseline and --base-ref")
    limit, raise_reason = parse_baseline_doc(baseline_doc)

    if base_baseline_path is not None:
        base_doc = load_json(base_baseline_path, "base baseline")
    elif base_ref is not None:
        base_doc = load_base_baseline_from_git(root, baseline_path, base_ref)
        if base_doc is None:
            return None, f"initial baseline ({limit}); absent at {base_ref}"
    else:
        return None, None

    base_limit, _ = parse_baseline_doc(base_doc, label="base baseline.json")
    if limit > base_limit:
        if not raise_reason or not ISSUE_REF_RE.search(raise_reason):
            raise ValueError(
                f"baseline dropped_plus_partial rose from base {base_limit} to {limit} "
                "without a non-empty raise_reason naming an issue (e.g. #1061)"
            )
        return base_limit, f"baseline raised from {base_limit} to {limit}: {raise_reason}"
    return base_limit, None


def update_baseline_file(baseline_path, counts, ratchet_dispositions=RATCHET_DISPOSITIONS):
    """Lower (or keep equal) baseline.json without ever raising an existing ceiling."""
    current = sum(counts[disp] for disp in ratchet_dispositions)
    path = Path(baseline_path)
    doc = {"format_version": 1, "dropped_plus_partial": current}
    if path.exists():
        existing_doc = load_json(path, "baseline")
        existing_limit, _ = parse_baseline_doc(existing_doc)
        if current > existing_limit:
            raise ValueError(
                f"refusing to raise baseline from {existing_limit} to {current}; "
                "update-baseline may only keep or lower the ceiling"
            )
        # Keep an approved raise reason: a lowered ceiling can still sit above the base
        # revision's ceiling, and the base check needs the reason until it does not.
        reason = existing_doc.get("raise_reason") if isinstance(existing_doc, dict) else None
        if isinstance(reason, str) and reason.strip():
            doc["raise_reason"] = reason
    write_json(path, doc)
    return current


def build_parser(here, default_root, description):
    """Build standard check / summary / update-baseline CLI parser."""
    parser = argparse.ArgumentParser(description=description)
    subparsers = parser.add_subparsers(dest="command")

    def add_common(sub, *, include_base=False):
        sub.add_argument("--root", type=Path, default=default_root, help="repository root for evidence paths")
        sub.add_argument("--catalog", type=Path, default=here / "catalog.json")
        sub.add_argument("--dispositions", type=Path, default=here / "dispositions.json")
        sub.add_argument("--baseline", type=Path, default=here / "baseline.json")
        if include_base:
            sub.add_argument(
                "--base-baseline",
                type=Path,
                default=None,
                help="path to base baseline.json for ceiling comparison",
            )
            sub.add_argument(
                "--base-ref",
                default=None,
                help="git revision whose baseline.json is read with git show for ceiling comparison",
            )

    add_common(subparsers.add_parser("check", help="validate catalog, dispositions, anchors, and ratchet"), include_base=True)
    add_common(subparsers.add_parser("summary", help="print disposition counts and baseline"), include_base=True)
    add_common(subparsers.add_parser("update-baseline", help="write current dropped+partial count to baseline.json"))
    return parser


def run_cli(argv, *, tool_name, item_plural, default_root, root, here, description, check_fn, update_baseline_fn):
    """Execute standard inventory CLI subcommands."""
    if not argv or argv[0] == "--check":
        argv = ["check", *argv[1:]] if argv else ["check"]
    parser = build_parser(here, root, description)
    args = parser.parse_args(argv)

    if args.command in ("check", "summary"):
        result = check_fn(
            args.root,
            args.catalog,
            args.dispositions,
            args.baseline,
            base_baseline=args.base_baseline,
            base_ref=args.base_ref,
        )
        counts_str = ", ".join(f"{k}={v}" for k, v in result["counts"].items())
        print(
            f"{tool_name}: {result['total']} {item_plural}; "
            f"dropped+partial={result['dropped_plus_partial']} (baseline={result['baseline']}); "
            f"{counts_str}"
        )
        if result.get("base_note"):
            print(f"note: {result['base_note']}")
        if result["dropped_plus_partial"] < result["baseline"]:
            print(
                f"note: dropped+partial ({result['dropped_plus_partial']}) is below baseline "
                f"({result['baseline']}); run `inventory.py update-baseline` to tighten the ratchet."
            )
        return result

    if args.command == "update-baseline":
        current, counts = update_baseline_fn(args.root, args.catalog, args.dispositions, args.baseline)
        print(f"updated baseline to dropped+partial={current} (dropped={counts['dropped']}, partial={counts['partial']})")
        return {"dropped_plus_partial": current, "counts": counts}

    parser.error(f"unknown command: {args.command}")
