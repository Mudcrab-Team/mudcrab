#!/usr/bin/env python3
"""Probe the pinned xEdit console release on synthetic P0 inputs only.

This records capabilities and failures; completed dumps with schema diagnostics
are observations, not valid-fixture or whole-catalog acceptance.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import uuid
from pathlib import Path

import p0_fixtures
from p0_tools import (
    ProcessOutputDecodeError, ProcessTimeout, QualificationError, _read_verified_bytes,
    _run_supervised, validate_artifact_destination, runner_provenance,
)

XDUMP_SHA256 = "30c085b8a20dc02bf5abae2cb6610870c9bb9eea50330e0fe5ade98e3f89efe6"
XDUMP_SIZE = 20_510_720
RELEASE = "xedit-4.1.5f"
RELEASE_COMMIT = "f5c00f3fa3ee39511185515802647246c807f759"
COMMAND_TIMEOUT = 60


def windows_path(path: Path) -> str:
    return "Z:" + str(path.absolute()).replace("/", "\\")


def classify_dump(filename: str, returncode: int, stdout: str, stderr: str) -> dict:
    """Require completion and exact pilot identities; preserve diagnostics."""
    combined = stdout + "\n" + stderr
    result = {"file_name": filename, "exit_code": returncode,
              "completed_verdict_saved": False}
    crash = any(marker in combined.lower() for marker in (
        "unhandled exception", "unhandled page fault", "winedbg", "access violation",
    ))
    if returncode != 0 or crash or not re.search(r"(?m)^<[^\n>]+> All Done\.$", stderr):
        return {**result, "status": "incomplete", "failure": "nonzero exit, crash, or missing dump completion"}

    diagnostics = [line.strip() for line in combined.splitlines()
                   if any(marker in line.lower() for marker in ("error", "warning", "unexpected"))]
    signatures = re.findall(r"(?m)^\s+Signature: ([A-Z0-9_]{4})\s*$", stdout)
    identities = re.findall(r"(?m)^\s+FormID: .*?\[([0-9A-F]{8})\]", stdout)
    result.update(diagnostics=diagnostics, signatures=signatures, form_ids=identities)

    if filename.startswith(("p0-truncated", "p0-bad-")):
        # A dump/error message does not establish managed structural rejection.
        return {**result, "status": "unqualified_negative",
                "failure": "malformed input completed a dump; structural rejection not established"}

    if filename in ("p0-hand-full.esp", "p0-unknown-repeated.esp"):
        expected_signatures = ["TES4", "GLOB", "GMST", "STAT", "CELL", "REFR"]
        expected_ids = ["00000000", "00000800", "00000801", "00000802", "00000803", "00000804"]
        tokens = ["P0HandGlobal", "FLTV - Value: 4.250000", "Float: 12.500000",
                  "MODL - Model FileName: meshes\\p0_hand_stat.nif",
                  "NAME - Base: STAT - Static [00000802]", "Pos:(12.5, -25, 100)",
                  "Rot:(0, 90, 0)", "Initially Disabled (0x00000800)"]
    elif filename in ("p0-base.esm", "p0-override.esp"):
        expected_signatures = ["TES4", "GLOB", "STAT", "CELL", "REFR"]
        expected_ids = ["00000000", "00000800", "00000801", "00000802", "00000803"]
        tokens = ["P0OverrideGlobal", "FLTV - Value: " + ("9.500000" if filename == "p0-override.esp" else "1.000000")]
        if filename == "p0-override.esp":
            tokens += ["Deleted (0x00000020)", "Pos:(50, 60, 70)", "MAST - FileName: p0-base.esm"]
    elif filename == "p0-localized.esp":
        expected_signatures, expected_ids = ["TES4", "ARMO"], ["00000000", "00000800"]
        tokens = ["P0LocalizedArmor", "No strings file for lstring ID 12345678"]
    elif filename == "p0-hand-light.esl":
        expected_signatures, expected_ids = ["TES4", "GLOB"], ["00000000", "00000800"]
        tokens = ["P0LightGlobal", "FLTV - Value: 2.500000"]
    else:
        raise QualificationError(f"unknown pilot input: {filename}")
    if signatures != expected_signatures or identities != expected_ids or any(token not in stdout for token in tokens):
        return {**result, "status": "failed_observation", "failure": "pilot identities or known values did not match"}
    return {**result, "status": "observed_with_diagnostics" if diagnostics else "observed",
            "fixture_schema_validated": False,
            "schema_diagnostics_observed": bool(diagnostics),
            "known_values_checked": True,
            "physical_offsets": "unavailable; dump offset mode uses memory addresses",
            "opaque_payload_completeness": "unavailable",
            "translated_text": "unavailable" if filename == "p0-localized.esp" else "not_checked"}


def _retain_undecodable_output(logs: Path, label: str, error: ProcessOutputDecodeError) -> dict:
    raw_files = {}
    for stream, raw in (("stdout", error.stdout_bytes), ("stderr", error.stderr_bytes)):
        name = f"{label}.{stream}.raw.bin"
        (logs / name).write_bytes(raw)
        raw_files[stream] = f"logs/{name}"
    return {**error.failure_record(), "raw_files": raw_files}


def run_probe(args: argparse.Namespace) -> tuple[dict, Path]:
    tool = Path(args.xdump).expanduser().absolute()
    artifact = Path(args.artifact_dir).expanduser().resolve() if args.artifact_dir else (
        Path(os.environ.get("TMPDIR", "/tmp")) / f"mudcrab-p0-xedit-{uuid.uuid4().hex}"
    ).resolve()
    validate_artifact_destination(artifact, tool.parent)
    # Check the declared binary size before capturing it in memory.
    if tool.lstat().st_size != XDUMP_SIZE:
        raise QualificationError("xDump binary size differs from the pinned release")
    digest, size, binary = _read_verified_bytes(tool, max_bytes=XDUMP_SIZE)
    if digest != XDUMP_SHA256 or size != XDUMP_SIZE:
        raise QualificationError("xDump binary digest differs from the pinned release")
    wine_path = shutil.which(args.wine)
    if not wine_path:
        raise QualificationError(f"Wine executable not found: {args.wine}")
    wine = Path(wine_path).resolve()
    validate_artifact_destination(artifact, tool.parent, additional_protected=(wine.parent,))
    if artifact.exists() and (not artifact.is_dir() or any(artifact.iterdir())):
        raise QualificationError("artifact directory must be new or empty")
    artifact.mkdir(parents=True, exist_ok=True)
    (artifact / "xDump64.exe").write_bytes(binary)
    data = artifact / "Data"
    logs = artifact / "logs"
    task_tmp = artifact / "tmp"
    data.mkdir()
    logs.mkdir()
    task_tmp.mkdir()
    env = dict(os.environ, WINEPREFIX=str(artifact / "wine-prefix"), WINEDEBUG="-all",
               WINEDLLOVERRIDES="mscoree,mshtml=", XDG_CACHE_HOME=str(artifact / "cache"),
               XDG_CONFIG_HOME=str(artifact / "config"),
               TMPDIR=str(task_tmp), TMP=str(task_tmp), TEMP=str(task_tmp))
    cases = p0_fixtures.hand_encoded_cases()
    for name, (raw, _) in cases.items():
        (data / name).write_bytes(raw)
    report = {
        "report_version": 1, "qualified": False, "completed_verdict_saved": False,
        "runner": runner_provenance(Path(__file__)),
        "target": "Steam Skyrim SE/AE 1.7.104.0 / build 24914197",
        "tool": {"release": RELEASE, "release_commit": RELEASE_COMMIT,
                 "binary_sha256": digest, "binary_size": size, "wine_path": str(wine),
                 "source_definition_pin": "separate; this release predates the mined source",
                 "source_to_binary_relationship": "unverified; tag commit is release metadata, not a reproducible build"},
        "cases": [],
        "limits": ["synthetic inputs only", "no retail or native-runtime acceptance",
                   "schema diagnostics retained; original fixtures not certified valid",
                   "not a source framing or complete field adapter"],
    }
    # Wine version and prefix startup are also deadline-bound; no host prefix is used.
    for label, command in [("wine-version", [str(wine), "--version"]),
                           ("help", [str(wine), str(artifact / "xDump64.exe"), "-SSE", "-?"])]:
        try:
            result = _run_supervised(command, cwd=artifact, env=env, label=label, timeout=COMMAND_TIMEOUT)
            (logs / f"{label}.stdout.txt").write_text(result.stdout, encoding="utf-8")
            (logs / f"{label}.stderr.txt").write_text(result.stderr, encoding="utf-8")
            if result.returncode != 0 or (label == "help" and "SSEDump 4.1.5f x64" not in result.stderr):
                raise QualificationError(f"{label} did not confirm the required tool")
            if label == "wine-version":
                report["tool"]["wine_version"] = result.stdout.strip()
        except ProcessOutputDecodeError as exc:
            report["startup_failure"] = str(exc)
            report["startup_output_decode_failure"] = _retain_undecodable_output(logs, label, exc)
            (artifact / "probe-results.json").write_text(
                json.dumps(report, indent=2) + "\n", encoding="utf-8"
            )
            return report, artifact
        except (ProcessTimeout, QualificationError, OSError) as exc:
            if isinstance(exc, ProcessTimeout):
                (logs / f"{label}.stdout.txt").write_text(exc.stdout, encoding="utf-8")
                (logs / f"{label}.stderr.txt").write_text(exc.stderr, encoding="utf-8")
            report["startup_failure"] = str(exc)
            (artifact / "probe-results.json").write_text(
                json.dumps(report, indent=2) + "\n", encoding="utf-8"
            )
            return report, artifact
    for name, (raw, provenance) in cases.items():
        command = [str(wine), str(artifact / "xDump64.exe"), "-SSE", "-q",
                   "-D:" + windows_path(data), "-dcr", "-nobsa", name]
        (logs / f"{name}.command.json").write_text(
            json.dumps(command, indent=2) + "\n", encoding="utf-8"
        )
        try:
            result = _run_supervised(command, cwd=artifact, env=env, label=name, timeout=COMMAND_TIMEOUT)
            stdout, stderr = result.stdout, result.stderr
            observation = classify_dump(name, result.returncode, stdout, stderr)
        except ProcessOutputDecodeError as exc:
            stdout, stderr = "", "[child output could not be decoded; raw bytes are retained]"
            observation = {
                "file_name": name,
                "status": "incomplete",
                "failure": str(exc),
                "diagnostic_output_decode_failure": _retain_undecodable_output(logs, name, exc),
                "completed_verdict_saved": False,
            }
        except ProcessTimeout as exc:
            stdout, stderr = exc.stdout, exc.stderr
            observation = {"file_name": name, "status": "incomplete", "failure": str(exc),
                           "completed_verdict_saved": False}
        except OSError as exc:
            stdout, stderr = "", str(exc)
            observation = {"file_name": name, "status": "incomplete", "failure": str(exc),
                           "completed_verdict_saved": False}
        (logs / f"{name}.stdout.txt").write_text(stdout, encoding="utf-8")
        (logs / f"{name}.stderr.txt").write_text(stderr, encoding="utf-8")
        observation.update(input_sha256=hashlib.sha256(raw).hexdigest(), size_bytes=len(raw),
                           input_provenance=provenance)
        report["cases"].append(observation)
    (artifact / "probe-results.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return report, artifact


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xdump", required=True, help="xDump64.exe from the pinned official release")
    parser.add_argument("--wine", default="wine", help="local Wine executable")
    parser.add_argument("--artifact-dir", help="new/empty destination outside protected source roots")
    try:
        report, artifact = run_probe(parser.parse_args())
    except (OSError, QualificationError) as exc:
        print(f"xEdit probe failed: {exc}", file=sys.stderr)
        return 2
    print(artifact)
    return 0 if report["qualified"] else 2


if __name__ == "__main__":
    sys.exit(main())
