#!/usr/bin/env python3
"""Run the scoped Mutagen P0 pilot from a guarded temporary source copy.

All source copies, NuGet restore state, build outputs, generated plugins, and
observations stay under the artifact directory, which must be outside the repo.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import uuid
from pathlib import Path

import p0_fixtures
from p0_tools import (
    QualificationError, ProcessTimeout, ProcessOutputDecodeError, _read_verified_bytes,
    validate_artifact_destination, _run_supervised,
    RE_PROJECT_ROOT, MCRAB_STORE, REPO_ROOT,
    runner_provenance,
)


PINNED_PROGRAM_SHA256 = "8e61a44f4f953c0491aab3c0519021fb67ca1b7528c607797cd4a2d948c06a06"
PINNED_CSPROJ_SHA256 = "de51c55aa0b0d56e722cefc3b8b0b4505c28114537468ba3d7ae5c0ce69129be"
PINNED_DOTNET = "9.0.318"
PINNED_MUTAGEN = "0.54.4"
ORACLE_ENTRY = 'if (args.Length >= 3 && args[0] == "placed-lo")'
INSPECT_ENTRY = b'if (args.Length == 2 && args[0] == "inspect")\n    return MutagenP0Inspect.Run(args[1]);\n'
RESTORE_TIMEOUT_SECONDS = 240
BUILD_TIMEOUT_SECONDS = 180
ORACLE_TIMEOUT_SECONDS = 45
CLI_TIMEOUT_SECONDS = 15


def _sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _save_decode_failure(exc: ProcessOutputDecodeError, base: Path) -> Path:
    """Save unavailable text and its exact raw evidence at the report owner."""
    stdout_path = base.with_suffix(".decode.stdout.bin")
    stderr_path = base.with_suffix(".decode.stderr.bin")
    stdout_path.write_bytes(exc.stdout_bytes)
    stderr_path.write_bytes(exc.stderr_bytes)
    report_path = base.with_suffix(".decode-error.json")
    _save_json(report_path, {
        "status": "incomplete", "completed_verdict_saved": False,
        "decoded_output": "unavailable", "label": exc.label,
        "timeout_seconds": exc.timeout,
        "raw_stdout_file": stdout_path.name, "raw_stderr_file": stderr_path.name,
        **exc.failure_record(),
    })
    return report_path


def default_artifact_destination(env: dict[str, str] | None = None) -> Path:
    """Choose a temp base without tempfile.gettempdir's write/unlink probe."""
    environment = os.environ if env is None else env
    base = next(
        (Path(environment[name]).expanduser() for name in ("TMPDIR", "TMP", "TEMP") if environment.get(name)),
        Path("/tmp"),
    )
    return base / f"mudcrab-p0-mutagen-{uuid.uuid4().hex}"


def apply_inspect_extension(program: bytes, extension: bytes) -> bytes:
    marker = ORACLE_ENTRY.encode("utf-8")
    if program.count(marker) != 1:
        raise QualificationError("pinned oracle source no longer has the unique placed-lo insertion point")
    if b"MutagenP0Inspect.Run(args[1])" in program:
        raise QualificationError("oracle source already contains the inspect extension")
    patched = program.replace(marker, INSPECT_ENTRY + marker, 1)
    return patched + b"\n" + extension.rstrip(b"\n") + b"\n"


def _resolve_dotnet(explicit: str | None) -> Path:
    candidate = explicit or os.environ.get("DOTNET") or shutil.which("dotnet")
    if not candidate:
        raise QualificationError(
            "dotnet SDK 9.0.318 is required for this separate local qualification command"
        )
    located = shutil.which(candidate) if "/" not in candidate else candidate
    if not located:
        raise QualificationError(f"dotnet executable was not found: {candidate}")
    path = Path(located).expanduser().resolve()
    if not path.is_file():
        raise QualificationError(f"dotnet executable is not a regular file: {path}")
    return path


def _run_checked(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    label: str,
    log_dir: Path,
    timeout: float,
):
    _save_json(log_dir / f"{label}.command.json", command)
    try:
        result = _run_supervised(command, cwd=cwd, env=env, label=label, timeout=timeout)
    except ProcessOutputDecodeError as exc:
        report_path = _save_decode_failure(exc, log_dir / label)
        _save_json(log_dir / f"{label}.command.json", command)
        raise QualificationError(f"{exc}; see {report_path}") from exc
    except ProcessTimeout as exc:
        (log_dir / f"{label}.stdout.txt").write_text(exc.stdout, encoding="utf-8")
        (log_dir / f"{label}.stderr.txt").write_text(exc.stderr, encoding="utf-8")
        _save_json(log_dir / f"{label}.timeout.json", {
            "timeout_seconds": timeout, "completed_verdict_saved": False,
            "cleanup_errors": exc.cleanup_errors,
        })
        _save_json(log_dir / f"{label}.command.json", command)
        raise QualificationError(f"{exc}; see {log_dir / (label + '.timeout.json')}") from exc
    (log_dir / f"{label}.stdout.txt").write_text(result.stdout, encoding="utf-8")
    (log_dir / f"{label}.stderr.txt").write_text(result.stderr, encoding="utf-8")
    (log_dir / f"{label}.command.json").write_text(
        json.dumps(command, indent=2) + "\n", encoding="utf-8"
    )
    if result.returncode != 0:
        raise QualificationError(f"{label} exited {result.returncode}; see {log_dir / (label + '.stderr.txt')}")
    return result


def _record_map(report: dict) -> dict[tuple[str, str], dict]:
    records = {}
    raw_records = report.get("records")
    if not isinstance(raw_records, list):
        raise QualificationError("typed major record observations are not an array")
    for record in raw_records:
        if not isinstance(record, dict):
            raise QualificationError("typed record observation is not an object")
        signature_data = record.get("signature")
        if not isinstance(signature_data, dict):
            raise QualificationError("typed record signature observation is not an object")
        signature = signature_data.get("value")
        key = record.get("form_key")
        if not isinstance(signature, str) or not signature or not isinstance(key, str) or not key:
            raise QualificationError("typed record is missing an available signature or FormKey")
        marker = (signature, key)
        if marker in records:
            raise QualificationError(f"typed traversal collapsed into a duplicate observation: {marker}")
        records[marker] = record
    return records


def _expect_record(
    records: dict[tuple[str, str], dict],
    *,
    signature: str,
    form_key: str,
    editor_id: str | None,
    raw_flags: int,
    deleted: bool = False,
    printed_contains: str | None = None,
    link_form_keys: tuple[str, ...] = (),
) -> dict:
    marker = (signature, form_key)
    if marker not in records:
        raise QualificationError(f"expected typed record was not observed: {marker}")
    record = records[marker]
    expected_edid_state = "absent" if editor_id is None else "present"
    if record["editor_id"]["state"] != expected_edid_state:
        raise QualificationError(f"EditorID state mismatch for {marker}")
    if editor_id is not None and record["editor_id"]["value"] != editor_id:
        raise QualificationError(f"EditorID mismatch for {marker}")
    if record["major_record_flags_raw"] != raw_flags:
        raise QualificationError(f"raw record flags mismatch for {marker}")
    if record["is_deleted"] is not deleted:
        raise QualificationError(f"deletion state mismatch for {marker}")
    if record["typed_data_print"]["state"] != "available":
        raise QualificationError(f"typed data print is unavailable for {marker}")
    if printed_contains is not None:
        expected_print = printed_contains.replace("\\", "/")
        observed_print = record["typed_data_print"]["text"].replace("\\", "/")
        if expected_print not in observed_print:
            raise QualificationError(f"typed data did not contain {printed_contains!r} for {marker}")
    if record["typed_links"]["state"] != "available":
        raise QualificationError(f"typed link enumeration is unavailable for {marker}")
    observed_links = tuple(item["target_form_key"] for item in record["typed_links"]["items"])
    for target in link_form_keys:
        if target not in observed_links:
            raise QualificationError(f"typed link {target} was not observed for {marker}")
    for field in ("physical_offset", "physical_subrecord_order", "unknown_payloads"):
        if record[field]["state"] != "unavailable":
            raise QualificationError(f"physical field {field} was represented as available for {marker}")
    return record


def _localized_name_observation(record: dict) -> dict:
    """Separate a localized wire ID from unavailable translated text."""
    text = record["typed_data_print"]["text"]
    name_values = [
        line.split("=>", 1)[1].strip()
        for line in text.splitlines()
        if line.strip().startswith("Name =>")
    ]
    if len(name_values) != 1:
        raise QualificationError("localized ARMO typed print did not expose one Name field")
    expected_id = 0x12345678
    visible_id = name_values[0]
    if visible_id in {str(expected_id), f"{expected_id:08X}", f"0x{expected_id:08X}"}:
        printable_identifier = {"state": "available", "value": visible_id}
    elif not visible_id:
        printable_identifier = {
            "state": "unavailable",
            "reason": "Mutagen IPrintable rendered the localized Name as blank",
        }
    else:
        raise QualificationError(f"localized ARMO Name had an unexpected typed value: {visible_id!r}")
    strings_key = record.get("localized_name_strings_key")
    if not isinstance(strings_key, dict):
        strings_key = {
            "state": "unavailable",
            "reason": "the inspect adapter did not emit a Mutagen StringsKey observation",
        }
    elif strings_key.get("state") == "available":
        key_value = strings_key.get("value")
        if not isinstance(key_value, str) or key_value not in {
            str(expected_id),
            f"{expected_id:08X}",
            f"0x{expected_id:08X}",
        }:
            raise QualificationError(
                f"Mutagen StringsKey did not match the hand-encoded localized ID: {key_value!r}"
            )
    elif strings_key.get("state") != "unavailable":
        raise QualificationError("Mutagen StringsKey observation has an invalid state")
    return {
        "fixture_wire_identifier": {
            "state": "available",
            "value": f"0x{expected_id:08X}",
            "source": "hand-encoded fixture provenance",
        },
        "printable_identifier": printable_identifier,
        "mutagen_strings_key": strings_key,
        "translated_text": {
            "state": "unavailable",
            "reason": "the synthetic localized plugin has no string table",
        },
    }


def validate_report(report: dict, filename: str, input_bytes: bytes) -> dict:
    if not isinstance(report, dict):
        raise QualificationError("Mutagen stdout JSON root must be an object")
    if report.get("execution_status") != "completed":
        raise QualificationError(f"Mutagen did not complete typed observation for {filename}")
    acceptance = report.get("acceptance_verdict")
    if not isinstance(acceptance, dict) or acceptance.get("state") != "unavailable":
        raise QualificationError("P0 report must keep acceptance verdict unavailable")
    adapter = report.get("adapter")
    if not isinstance(adapter, dict) or adapter.get("package_version") != PINNED_MUTAGEN:
        raise QualificationError("Mutagen package version in report does not match the pinned package")
    diagnostics = report.get("diagnostics")
    if not isinstance(diagnostics, dict):
        raise QualificationError("Mutagen diagnostics status is unavailable")
    captured_stdout = diagnostics.get("captured_stdout")
    if not isinstance(captured_stdout, str):
        raise QualificationError("captured Mutagen stdout diagnostic status is unavailable")
    if captured_stdout:
        raise QualificationError(
            "Mutagen emitted incidental stdout during typed import; the full diagnostic is preserved in the report"
        )
    source = report.get("source_plugin", {})
    if not isinstance(source, dict):
        raise QualificationError("source plugin identity observation is unavailable")
    if source.get("file_name") != filename:
        raise QualificationError(f"source plugin identity mismatch for {filename}")
    if source.get("mod_key") != filename:
        raise QualificationError(f"Mutagen ModKey identity mismatch for {filename}")
    if source.get("sha256") != _sha256(input_bytes) or source.get("size_bytes") != len(input_bytes):
        raise QualificationError(f"source plugin hash/size mismatch for {filename}")
    source_coverage = report.get("source_occurrence_coverage")
    if not isinstance(source_coverage, dict) or source_coverage.get("state") != "unavailable":
        raise QualificationError("typed traversal must not claim complete source-occurrence coverage")
    records = _record_map(report)
    if report.get("typed_major_record_count") != len(records):
        raise QualificationError(f"typed record count mismatch for {filename}")

    full_plugin = "p0-hand-full.esp"
    base_plugin = "p0-base.esm"
    localized_name_observation = None
    if filename in {full_plugin, "p0-unknown-repeated.esp"}:
        if len(records) != 5:
            raise QualificationError(f"{filename} expected exactly five typed records, saw {len(records)}")
        for expected in p0_fixtures.HAND_ENCODED_EXPECTED["records"]:
            key = f"{expected['form_id']:06X}:{filename}"
            expected_links = (
                (expected["typed_link"].split(":", 1)[0] + ":" + filename,)
                if "typed_link" in expected
                else ()
            )
            _expect_record(
                records,
                signature=expected["signature"],
                form_key=key,
                editor_id=expected["editor_id"],
                raw_flags=expected["raw_flags"],
                printed_contains=expected["typed_data"],
                link_form_keys=expected_links,
        )
        if filename == "p0-unknown-repeated.esp":
            framing = report.get("framing_observations")
            if not isinstance(framing, dict) or framing.get("unknown_payloads") != "unavailable":
                raise QualificationError("unknown-payload visibility must remain unavailable")
    elif filename == "p0-hand-light.esl":
        if len(records) != 1:
            raise QualificationError("light-plugin fixture expected exactly one typed record")
        record = _expect_record(
            records,
            signature="GLOB",
            form_key="000800:p0-hand-light.esl",
            editor_id="P0LightGlobal",
            raw_flags=0,
            printed_contains="2.5",
        )
        identity = report["source_plugin"]
        if identity.get("is_small_master") is not True:
            raise QualificationError(".esl fixture was not identified as a light plugin")
        if record["signature"]["value"] != "GLOB":
            raise QualificationError("light-plugin signature mismatch")
    elif filename == base_plugin:
        if len(records) != 4:
            raise QualificationError("base fixture expected exactly four typed records")
        base_expected = (
            ("GLOB", 0x800, "P0OverrideGlobal", 0, "1"),
            ("STAT", 0x801, "P0OverrideStatic", 0, "meshes\\p0_override.nif"),
            ("CELL", 0x802, "P0OverrideCell", 0, None),
            ("REFR", 0x803, None, 0x400, "1, 2, 3"),
        )
        for signature, form_id, editor_id, flags, printed in base_expected:
            _expect_record(
                records,
                signature=signature,
                form_key=f"{form_id:06X}:{base_plugin}",
                editor_id=editor_id,
                raw_flags=flags,
                printed_contains=printed,
                link_form_keys=("000801:p0-base.esm",) if signature == "REFR" else (),
            )
    elif filename == "p0-override.esp":
        if len(records) != 4:
            raise QualificationError("override fixture expected exactly four typed records")
        _expect_record(
            records,
            signature="GLOB",
            form_key="000800:p0-base.esm",
            editor_id="P0OverrideGlobal",
            raw_flags=0,
            printed_contains="9.5",
        )
        _expect_record(
            records,
            signature="STAT",
            form_key="000801:p0-base.esm",
            editor_id=None,
            raw_flags=0x20,
            deleted=True,
        )
        _expect_record(
            records,
            signature="CELL",
            form_key="000802:p0-base.esm",
            editor_id="P0OverrideCell",
            raw_flags=0,
        )
        _expect_record(
            records,
            signature="REFR",
            form_key="000803:p0-base.esm",
            editor_id=None,
            raw_flags=0xC00,
            printed_contains="50",
            link_form_keys=("000801:p0-base.esm",),
        )
    elif filename == "p0-localized.esp":
        if len(records) != 1:
            raise QualificationError("localized fixture expected exactly one typed record")
        armor = _expect_record(
            records,
            signature="ARMO",
            form_key="000800:p0-localized.esp",
            editor_id="P0LocalizedArmor",
            raw_flags=0,
        )
        if report["source_plugin"].get("using_localization") is not True:
            raise QualificationError("localized plugin header was not reflected by Mutagen")
        localized_name_observation = _localized_name_observation(armor)
    else:
        raise QualificationError(f"unexpected positive fixture name {filename}")

    summary = {
        "status": "passed",
        "file_name": filename,
        "sha256": source["sha256"],
        "size_bytes": source["size_bytes"],
        "typed_major_record_count": len(records),
        "expected_values_source": (
            "independently hand-encoded wire values"
            if filename != "p0-unknown-repeated.esp"
            else "same hand-encoded fixture with raw repeated-unknown injection"
        ),
    }
    if localized_name_observation is not None:
        summary["localized_name_observation"] = localized_name_observation
    return summary


def classify_negative_run(
    filename: str, input_bytes: bytes, return_code: int, stdout: str, stderr: str
) -> dict:
    """Accept only the extension's managed nonzero parse failure, without stdout."""
    if return_code == 0:
        return {
            "status": "failed",
            "file_name": filename,
            "failure": "Mutagen returned zero for malformed/truncated input",
            "unexpected_stdout_sha256": _sha256(stdout.encode("utf-8")),
            "unexpected_stdout_size_bytes": len(stdout.encode("utf-8")),
            "completed_verdict_saved": False,
        }
    if return_code != 2 or "Mutagen inspect failed:" not in stderr:
        return {
            "status": "failed",
            "file_name": filename,
            "failure": "negative input did not reach the managed Mutagen inspect error path",
            "exit_code": return_code,
            "stderr_sha256": _sha256(stderr.encode("utf-8")),
            "completed_verdict_saved": False,
        }
    if stdout.strip():
        return {
            "status": "failed",
            "file_name": filename,
            "failure": "failed import emitted stdout instead of withholding a completed verdict",
            "stdout_sha256": _sha256(stdout.encode("utf-8")),
            "completed_verdict_saved": False,
        }
    return {
        "status": "rejected_as_expected",
        "file_name": filename,
        "sha256": _sha256(input_bytes),
        "size_bytes": len(input_bytes),
        "exit_code": return_code,
        "completed_verdict_saved": False,
    }


def _save_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _case_run(
    *,
    dotnet: Path,
    assembly: Path,
    filename: str,
    input_bytes: bytes,
    fixtures_dir: Path,
    observations_dir: Path,
    env: dict[str, str],
    expect_failure: bool = False,
) -> tuple[dict, dict | None]:
    plugin_path = fixtures_dir / filename
    plugin_path.write_bytes(input_bytes)
    command = [str(dotnet), str(assembly), "inspect", str(plugin_path)]
    _save_json((observations_dir / Path(filename).stem).with_suffix(".command.json"), command)
    try:
        result = _run_supervised(
            command,
            cwd=fixtures_dir,
            env=env,
            label=f"inspect-{Path(filename).stem}",
            timeout=ORACLE_TIMEOUT_SECONDS,
        )
    except ProcessOutputDecodeError as exc:
        case_base = observations_dir / Path(filename).stem
        report_path = _save_decode_failure(exc, case_base)
        _save_json(case_base.with_suffix(".command.json"), command)
        return {
            "status": "failed", "file_name": filename,
            "failure": f"{exc}; see {report_path}",
            "completed_verdict_saved": False,
        }, None
    except ProcessTimeout as exc:
        case_base = observations_dir / Path(filename).stem
        (case_base.with_suffix(".raw.stdout.txt")).write_text(exc.stdout, encoding="utf-8")
        (case_base.with_suffix(".stderr.txt")).write_text(exc.stderr, encoding="utf-8")
        _save_json(
            case_base.with_suffix(".timeout.json"),
            {
                "status": "incomplete",
                "timeout_seconds": ORACLE_TIMEOUT_SECONDS,
                "cleanup_errors": exc.cleanup_errors,
                "stdout_sha256": _sha256(exc.stdout.encode("utf-8")),
                "stdout_size_bytes": len(exc.stdout.encode("utf-8")),
                "stderr_sha256": _sha256(exc.stderr.encode("utf-8")),
                "stderr_size_bytes": len(exc.stderr.encode("utf-8")),
                "completed_verdict_saved": False,
            },
        )
        return {
            "status": "failed",
            "file_name": filename,
            "failure": str(exc),
            "completed_verdict_saved": False,
        }, None
    case_base = observations_dir / Path(filename).stem
    _save_json(case_base.with_suffix(".command.json"), command)
    case_base.with_suffix(".raw.stdout.txt").write_text(result.stdout, encoding="utf-8")
    (case_base.with_suffix(".stderr.txt")).write_text(result.stderr, encoding="utf-8")

    if expect_failure:
        summary = classify_negative_run(
            filename, input_bytes, result.returncode, result.stdout, result.stderr
        )
        return summary, None

    if result.returncode != 0:
        return {
            "status": "failed",
            "file_name": filename,
            "failure": f"Mutagen exited {result.returncode}",
            "completed_verdict_saved": False,
        }, None
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        return {
            "status": "failed",
            "file_name": filename,
            "failure": f"Mutagen stdout is not one JSON report: {exc}",
            "stdout_sha256": _sha256(result.stdout.encode("utf-8")),
            "completed_verdict_saved": False,
        }, None
    if result.stderr.strip():
        return {
            "status": "failed",
            "file_name": filename,
            "failure": "successful Mutagen invocation emitted stderr diagnostics; review the preserved log",
            "stderr_sha256": _sha256(result.stderr.encode("utf-8")),
            "completed_verdict_saved": False,
        }, None
    try:
        case_summary = validate_report(report, filename, input_bytes)
    except QualificationError as exc:
        return {
            "status": "failed",
            "file_name": filename,
            "failure": str(exc),
            "completed_verdict_saved": False,
        }, None
    _save_json(case_base.with_suffix(".json"), report)
    return case_summary, report


def _run_legacy_command(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    label: str,
    observations_dir: Path,
) -> subprocess.CompletedProcess:
    _save_json((observations_dir / label).with_suffix(".command.json"), command)
    try:
        result = _run_supervised(
            command,
            cwd=cwd,
            env=env,
            label=label,
            timeout=ORACLE_TIMEOUT_SECONDS,
        )
    except ProcessOutputDecodeError as exc:
        base = observations_dir / label
        report_path = _save_decode_failure(exc, base)
        _save_json(base.with_suffix(".command.json"), command)
        raise QualificationError(f"{exc}; see {report_path}") from exc
    except ProcessTimeout as exc:
        base = observations_dir / label
        base.with_suffix(".timeout.stdout.txt").write_text(exc.stdout, encoding="utf-8")
        base.with_suffix(".timeout.stderr.txt").write_text(exc.stderr, encoding="utf-8")
        timeout_path = base.with_suffix(".timeout.json")
        _save_json(
            timeout_path,
            {
                "status": "incomplete",
                "timeout_seconds": ORACLE_TIMEOUT_SECONDS,
                "cleanup_errors": exc.cleanup_errors,
                "stdout_sha256": _sha256(exc.stdout.encode("utf-8")),
                "stdout_size_bytes": len(exc.stdout.encode("utf-8")),
                "stderr_sha256": _sha256(exc.stderr.encode("utf-8")),
                "stderr_size_bytes": len(exc.stderr.encode("utf-8")),
                "completed_verdict_saved": False,
            },
        )
        raise QualificationError(f"{label} timed out; incomplete evidence saved to {timeout_path}") from exc
    base = observations_dir / label
    base.with_suffix(".stdout.txt").write_text(result.stdout, encoding="utf-8")
    base.with_suffix(".stderr.txt").write_text(result.stderr, encoding="utf-8")
    return result


def _run_legacy_smokes(dotnet: Path, assembly: Path, fixtures_dir: Path, env: dict[str, str], observations_dir: Path) -> dict:
    hand = fixtures_dir / "p0-hand-full.esp"
    placed = _run_legacy_command(
        [str(dotnet), str(assembly), "placed", str(hand)],
        cwd=fixtures_dir,
        env=env,
        label="legacy-placed",
        observations_dir=observations_dir,
    )
    if placed.returncode != 0:
        raise QualificationError(f"legacy placed command exited {placed.returncode}")
    if placed.stderr.strip() != "1 placed records":
        raise QualificationError("legacy placed diagnostics differ from the expected single-record count")
    try:
        rows = [json.loads(line) for line in placed.stdout.splitlines() if line.strip()]
    except json.JSONDecodeError as exc:
        raise QualificationError(f"legacy placed output is not JSONL: {exc}") from exc
    if len(rows) != 1:
        raise QualificationError(f"legacy placed expected one REFR row, saw {len(rows)}")
    row = rows[0]
    if row.get("form") != "000804:p0-hand-full.esp" or row.get("type") != "REFR":
        raise QualificationError("legacy placed row identity mismatch")
    if row.get("flags") != 0xC00 or row.get("initially_disabled") is not True:
        raise QualificationError("legacy placed raw flags or disabled projection mismatch")

    load_order = _run_legacy_command(
        [str(dotnet), str(assembly), "placed-lo", str(fixtures_dir), "p0-base.esm", "p0-override.esp"],
        cwd=fixtures_dir,
        env=env,
        label="legacy-placed-lo",
        observations_dir=observations_dir,
    )
    if load_order.returncode != 0:
        raise QualificationError(f"legacy placed-lo command exited {load_order.returncode}")
    if load_order.stderr.strip() != "1 winning placed REFR/ACHR from 2 plugins":
        raise QualificationError("legacy placed-lo diagnostics differ from the expected winner count")
    try:
        winners = [json.loads(line) for line in load_order.stdout.splitlines() if line.strip()]
    except json.JSONDecodeError as exc:
        raise QualificationError(f"legacy placed-lo output is not JSONL: {exc}") from exc
    if len(winners) != 1:
        raise QualificationError(f"legacy placed-lo expected one winning REFR, saw {len(winners)}")
    winner = winners[0]
    if winner.get("form") != "000803:p0-base.esm" or winner.get("winner") != "p0-override.esp":
        raise QualificationError("legacy placed-lo winner identity mismatch")
    if winner.get("base") != "000801:p0-base.esm" or winner.get("flags") != 0xC00:
        raise QualificationError("legacy placed-lo link or raw flags mismatch")
    if winner.get("pos") != [50, 60, 70]:
        raise QualificationError("legacy placed-lo override placement mismatch")
    return {
        "placed": {"status": "passed", "row_count": len(rows), "form": row["form"]},
        "placed_lo": {
            "status": "passed",
            "winner_count": len(winners),
            "form": winner["form"],
            "winner": winner["winner"],
        },
    }


def run_suite(args: argparse.Namespace) -> tuple[dict, Path]:
    artifact_dir = (
        Path(args.artifact_dir).expanduser().resolve()
        if args.artifact_dir
        else default_artifact_destination()
    )
    oracle_dir = Path(args.oracle_source).expanduser().resolve()
    validate_artifact_destination(artifact_dir, oracle_dir)
    dotnet = _resolve_dotnet(args.dotnet)
    validate_artifact_destination(artifact_dir, oracle_dir, additional_protected=(dotnet.parent,))
    runner_source = runner_provenance(Path(__file__))
    if artifact_dir.exists() and any(artifact_dir.iterdir()):
        raise QualificationError(f"artifact directory must be empty: {artifact_dir}")
    artifact_dir.mkdir(parents=True, exist_ok=True)
    source_copy = artifact_dir / "oracle-source"
    build_dir = artifact_dir / "build"
    fixtures_dir = artifact_dir / "fixtures"
    observations_dir = artifact_dir / "observations"
    logs_dir = artifact_dir / "logs"
    nuget_dir = artifact_dir / "nuget"
    cli_home = artifact_dir / "dotnet-home"
    task_tmp_dir = artifact_dir / "tmp"
    for path in (source_copy, build_dir, fixtures_dir, observations_dir, logs_dir, nuget_dir, cli_home, task_tmp_dir):
        path.mkdir(parents=True, exist_ok=True)

    program_sha, _, program_raw = _read_verified_bytes(oracle_dir / "Program.cs")
    project_sha, _, project_raw = _read_verified_bytes(oracle_dir / "records.csproj")
    if program_sha != PINNED_PROGRAM_SHA256:
        raise QualificationError(
            f"Program.cs source pin drift: expected {PINNED_PROGRAM_SHA256}, observed {program_sha}"
        )
    if project_sha != PINNED_CSPROJ_SHA256:
        raise QualificationError(
            f"records.csproj source pin drift: expected {PINNED_CSPROJ_SHA256}, observed {project_sha}"
        )
    ext_path = Path(__file__).with_name("mutagen_inspect_extension.cs")
    extension_sha, _, extension_raw = _read_verified_bytes(ext_path)
    patched = apply_inspect_extension(program_raw, extension_raw)
    patched_sha = _sha256(patched)
    (source_copy / "Program.cs").write_bytes(patched)
    (source_copy / "records.csproj").write_bytes(project_raw)
    provenance_dir = artifact_dir / "provenance"
    provenance_dir.mkdir()
    (provenance_dir / "Program.base.txt").write_bytes(program_raw)
    (provenance_dir / "mutagen_inspect_extension.txt").write_bytes(extension_raw)

    env = os.environ.copy()
    env.update(
        {
            "DOTNET_ROOT": str(dotnet.parent),
            "DOTNET_CLI_HOME": str(cli_home),
            "DOTNET_CLI_TELEMETRY_OPTOUT": "1",
            "DOTNET_SKIP_FIRST_TIME_EXPERIENCE": "1",
            "DOTNET_NOLOGO": "1",
            "DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE": "1",
            "NUGET_PACKAGES": str(nuget_dir),
            "NUGET_HTTP_CACHE_PATH": str(artifact_dir / "nuget-http-cache"),
            "TMPDIR": str(task_tmp_dir),
            "TMP": str(task_tmp_dir),
            "TEMP": str(task_tmp_dir),
            "MSBUILDDISABLENODEREUSE": "1",
            "DOTNET_CLI_USE_MSBUILD_SERVER": "0",
        }
    )
    _save_json(logs_dir / "dotnet-version.command.json", [str(dotnet), "--version"])
    try:
        version = _run_supervised(
            [str(dotnet), "--version"],
            cwd=source_copy,
            env=env,
            label="dotnet-version",
            timeout=CLI_TIMEOUT_SECONDS,
        )
    except ProcessOutputDecodeError as exc:
        report_path = _save_decode_failure(exc, logs_dir / "dotnet-version")
        _save_json(logs_dir / "dotnet-version.command.json", [str(dotnet), "--version"])
        raise QualificationError(f"{exc}; see {report_path}") from exc
    except ProcessTimeout as exc:
        (logs_dir / "dotnet-version.stdout.txt").write_text(exc.stdout, encoding="utf-8")
        (logs_dir / "dotnet-version.stderr.txt").write_text(exc.stderr, encoding="utf-8")
        _save_json(
            logs_dir / "dotnet-version.timeout.json",
            {
                "timeout_seconds": CLI_TIMEOUT_SECONDS, "completed_verdict_saved": False,
                "cleanup_errors": exc.cleanup_errors,
            },
        )
        raise QualificationError(str(exc)) from exc
    if version.returncode != 0 or version.stdout.strip() != PINNED_DOTNET:
        raise QualificationError(
            f"expected .NET SDK {PINNED_DOTNET}, got exit={version.returncode}, output={version.stdout.strip()!r}"
        )
    (logs_dir / "dotnet-version.stdout.txt").write_text(version.stdout, encoding="utf-8")
    (logs_dir / "dotnet-version.stderr.txt").write_text(version.stderr, encoding="utf-8")

    nuget_config = artifact_dir / "NuGet.Config"
    nuget_config.write_text(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n"
        "<configuration><packageSources><clear/><add key=\"nuget.org\" "
        "value=\"https://api.nuget.org/v3/index.json\" /></packageSources></configuration>\n",
        encoding="utf-8",
    )
    project = source_copy / "records.csproj"
    obj_dir = build_dir / "obj"
    output_dir = build_dir / "output"
    obj_dir.mkdir(parents=True, exist_ok=True)
    output_dir.mkdir(parents=True, exist_ok=True)
    restore = [
        str(dotnet),
        "restore",
        str(project),
        "--configfile",
        str(nuget_config),
        "--use-lock-file",
        "--verbosity",
        "minimal",
        f"-p:BaseIntermediateOutputPath={obj_dir}/",
        f"-p:RestorePackagesPath={nuget_dir}",
        "-p:NuGetAudit=false",
    ]
    restore_result = _run_checked(
        restore,
        cwd=source_copy,
        env=env,
        label="restore",
        log_dir=logs_dir,
        timeout=RESTORE_TIMEOUT_SECONDS,
    )
    build = [
        str(dotnet),
        "build",
        str(project),
        "--configuration",
        "Release",
        "--output",
        str(output_dir),
        "--no-restore",
        "--disable-build-servers",
        "--nologo",
        "--verbosity",
        "minimal",
        f"-p:BaseIntermediateOutputPath={obj_dir}/",
        f"-p:RestorePackagesPath={nuget_dir}",
    ]
    build_result = _run_checked(
        build,
        cwd=source_copy,
        env=env,
        label="build",
        log_dir=logs_dir,
        timeout=BUILD_TIMEOUT_SECONDS,
    )
    package_lock = source_copy / "packages.lock.json"
    if not package_lock.is_file():
        raise QualificationError("dotnet restore did not emit the expected temporary packages.lock.json")
    lock_sha, _, lock_raw = _read_verified_bytes(package_lock)
    lock = json.loads(lock_raw)
    target = lock.get("dependencies", {}).get("net9.0", {})
    mutagen_version = target.get("Mutagen.Bethesda.Skyrim", {}).get("resolved")
    if mutagen_version != PINNED_MUTAGEN:
        raise QualificationError(
            f"resolved Mutagen version drift: expected {PINNED_MUTAGEN}, observed {mutagen_version!r}"
        )

    cases = p0_fixtures.hand_encoded_cases()
    case_metadata = {}
    for filename, (raw, provenance) in cases.items():
        case_metadata[filename] = {"bytes": raw, "provenance": provenance, "sha256": _sha256(raw), "size": len(raw)}
        (fixtures_dir / filename).write_bytes(raw)
    positives = [
        "p0-hand-full.esp",
        "p0-hand-light.esl",
        "p0-base.esm",
        "p0-override.esp",
        "p0-localized.esp",
        "p0-unknown-repeated.esp",
    ]
    positive_summaries = []
    for filename in positives:
        metadata = case_metadata[filename]
        summary, report = _case_run(
            dotnet=dotnet,
            assembly=output_dir / "mcrab-records.dll",
            filename=filename,
            input_bytes=metadata["bytes"],
            fixtures_dir=fixtures_dir,
            observations_dir=observations_dir,
            env=env,
        )
        if summary.get("status") != "passed" or report is None:
            raise QualificationError(f"synthetic typed observation failed for {filename}: {summary}")
        summary["provenance"] = metadata["provenance"]
        positive_summaries.append(summary)

    negative_names = [
        "p0-truncated-tail.esp",
        "p0-truncated-header.esp",
        "p0-bad-subrecord-length.esp",
        "p0-bad-tes4-length.esp",
    ]
    negative_summaries = []
    for filename in negative_names:
        metadata = case_metadata[filename]
        summary, report = _case_run(
            dotnet=dotnet,
            assembly=output_dir / "mcrab-records.dll",
            filename=filename,
            input_bytes=metadata["bytes"],
            fixtures_dir=fixtures_dir,
            observations_dir=observations_dir,
            env=env,
            expect_failure=True,
        )
        if summary.get("status") != "rejected_as_expected" or report is not None:
            raise QualificationError(f"negative input did not fail closed for {filename}: {summary}")
        summary["provenance"] = metadata["provenance"]
        negative_summaries.append(summary)

    legacy = _run_legacy_smokes(
        dotnet,
        output_dir / "mcrab-records.dll",
        fixtures_dir,
        env,
        observations_dir,
    )
    summary = {
        "pilot_status": "passed_bounded_synthetic_qualification",
        "runner": runner_source,
        "acceptance_verdict": {
            "state": "unavailable",
            "reason": "P0 typed traversal does not establish full structural, catalog, corpus, or native-runtime acceptance",
        },
        "tool": {
            "name": "Mutagen.Bethesda.Skyrim",
            "version": mutagen_version,
            "release": "SkyrimSE",
            "dotnet_sdk": version.stdout.strip(),
        },
        "runner_provenance": {
            "oracle_directory": str(oracle_dir),
            "baseline_program_sha256": program_sha,
            "baseline_project_sha256": project_sha,
            "extended_program_sha256": patched_sha,
            "extension_sha256": extension_sha,
            "nuget_lock_sha256": lock_sha,
            "restore_timeout_seconds": RESTORE_TIMEOUT_SECONDS,
            "build_timeout_seconds": BUILD_TIMEOUT_SECONDS,
            "oracle_timeout_seconds": ORACLE_TIMEOUT_SECONDS,
            "cli_timeout_seconds": CLI_TIMEOUT_SECONDS,
            "restore_stderr": restore_result.stderr,
            "build_stderr": build_result.stderr,
            "build_warning_lines": [
                line
                for line in build_result.stdout.splitlines()
                if "warning" in line.casefold()
            ],
            "nuget_packages_directory": str(nuget_dir),
            "build_output_directory": str(output_dir),
            "artifact_directory": str(artifact_dir),
        },
        "synthetic_positive_cases": positive_summaries,
        "synthetic_negative_cases": negative_summaries,
        "legacy_command_smokes": legacy,
        "xedit": {
            "status": "not_run",
            "reason": "This suite executes Mutagen only; run_xedit_p0.py separately records xEdit capabilities and qualification gaps.",
        },
        "limits": [
            "Mutagen typed groups may collapse or skip source record occurrences.",
            "Physical offsets, physical subrecord order, and unknown-payload observations remain unavailable.",
            "The localized ARMO probe records fixture wire-ID provenance and separate typed-ID availability; no string table is supplied, so translated text remains unavailable.",
            "All plugin inputs are synthetic; no retail corpus or private user plugin was opened.",
        ],
    }
    _save_json(artifact_dir / "qualification.json", summary)
    return summary, artifact_dir


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dotnet", help="path to the pinned .NET SDK executable")
    parser.add_argument(
        "--oracle-source", required=True,
        help="explicit path to the separately supplied, hash-pinned qualification oracle",
    )
    parser.add_argument("--artifact-dir", help="new or empty path outside the repository")
    args = parser.parse_args(argv)
    candidate = (
        Path(args.artifact_dir).expanduser().resolve()
        if args.artifact_dir
        else default_artifact_destination()
    )
    artifact_owned = False
    try:
        validate_artifact_destination(candidate, Path(args.oracle_source))
        dotnet = _resolve_dotnet(args.dotnet)
        validate_artifact_destination(candidate, Path(args.oracle_source), additional_protected=(dotnet.parent,))
        if candidate.exists() and (not candidate.is_dir() or any(candidate.iterdir())):
            raise QualificationError(f"artifact directory must be empty: {candidate}")
        candidate.mkdir(parents=True, exist_ok=True)
        artifact_owned = True
        args.artifact_dir = str(candidate)
        summary, artifact_dir = run_suite(args)
        print(json.dumps({"pilot_status": summary["pilot_status"], "artifact_dir": str(artifact_dir)}, indent=2))
        return 0
    except (QualificationError, OSError, ValueError, KeyError, TypeError) as exc:
        if artifact_owned and candidate.exists() and not (candidate / "qualification.json").exists():
            _save_json(
                candidate / "qualification.json",
                {
                    "pilot_status": "failed",
                    "acceptance_verdict": {"state": "unavailable", "reason": str(exc)},
                    "failure": str(exc),
                },
            )
        print(f"P0 Mutagen qualification failed: {exc}", file=sys.stderr)
        print(f"Artifacts: {candidate}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
