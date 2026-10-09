#!/usr/bin/env python3
"""Run an explicit validation plan sequentially; save evidence and stop on failure."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def timestamp():
    return datetime.now(timezone.utc).isoformat()


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
        temporary = Path(stream.name)
    os.replace(temporary, path)


def option(command, name):
    return command[command.index(name) + 1]


def overlap(left, right):
    return left == right or left.is_relative_to(right) or right.is_relative_to(left)


def load_resume_history(path, expected_hash=None, seen=None):
    """Load inherited records only through verified, acyclic status links."""
    path = path.resolve()
    seen = set() if seen is None else seen
    if path in seen or len(seen) >= 64:
        raise ValueError("resume status chain is cyclic or too deep")
    seen.add(path)
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if expected_hash is not None and digest != expected_hash:
        raise ValueError(f"recorded previous status changed: {path}")
    status = json.loads(data)
    records, history = {}, []
    resume = status.get("resume")
    if resume is not None:
        records, history = load_resume_history(
            Path(resume["previous_status"]), resume["previous_status_sha256"], seen
        )
    provenance = status["provenance"]
    for record in status["runs"]:
        if record["label"] in records:
            raise ValueError(f"duplicate run in resume status chain: {record['label']}")
        records[record["label"]] = {
            "record": record,
            "converter_sha256": provenance["converter_sha256"],
            "status": str(path),
        }
    history.append({"path": str(path), "sha256": digest, "provenance": provenance})
    return records, history


def validate(plan):
    runs = plan["runs"]
    if not runs or len({run["label"] for run in runs}) != len(runs):
        raise ValueError("plan must have uniquely named runs")
    protected = Path(runs[0]["source"]).resolve()
    root = Path(runs[0]["output"]).resolve().parent
    binary = None
    previous_outputs = set()
    for run in runs:
        observed = run["run"]
        command = observed[observed.index("--") + 1:]
        output = Path(run["output"]).resolve()
        data = Path(command[1]).resolve()
        evidence = Path(option(observed, "--evidence")).resolve()
        if output.parent != root or evidence.parent != root or overlap(output, protected) or overlap(output, data):
            raise ValueError("validation outputs must be disjoint from source assets and game data, under one root")
        if Path(command[2]).resolve() != output or Path(run["lod_validation"][2]).resolve() != output:
            raise ValueError("conversion and audit must target the declared output")
        current_binary = Path(command[0]).resolve()
        if binary is not None and binary != current_binary:
            raise ValueError("all legs must use one converter binary")
        binary = current_binary
        if Path(run["ordinary_validation"][0]).resolve() != binary or Path(run["ordinary_validation"][2]).resolve() != output:
            raise ValueError("ordinary validation must use the declared binary and output")
        if run["source"]:
            source = Path(run["source"]).resolve()
            if source != protected and source not in previous_outputs:
                raise ValueError("metadata source must be the protected pack or an earlier leg")
            if overlap(output, source) or Path(option(command, "--reuse-assets")).resolve() != source:
                raise ValueError("metadata output must be disjoint from its declared source")
        elif output in previous_outputs and run["label"] != "full-gpu-warm":
            raise ValueError("only the normal warm leg may replace an earlier isolated output")
        previous_outputs.add(output)
    return runs, root, protected, binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--status", type=Path, help="Status/provenance JSON; defaults to validation-status.json under the output root")
    parser.add_argument("--dry-run", action="store_true", help="Validate and print the sequence without starting commands or hashing assets")
    parser.add_argument("--resume-after", help="Skip an audited prefix of this plan; preserve its original evidence and start after this label")
    parser.add_argument("--previous-status", type=Path, help="Original status/provenance required with --resume-after; never overwritten")
    parser.add_argument("--allow-new-binary", action="store_true", help="Explicitly allow a different binary on resume after verifying protected source and completed-prefix audits; retain both binary hashes")
    args = parser.parse_args()
    plan = json.loads(args.plan.read_text())
    runs, root, protected, binary = validate(plan)
    prior_runs = []
    if args.resume_after is not None:
        if args.previous_status is None:
            parser.error("--resume-after requires --previous-status")
        labels = [run["label"] for run in runs]
        if args.resume_after not in labels or args.resume_after == labels[-1]:
            parser.error("--resume-after must name a nonfinal plan leg")
        index = labels.index(args.resume_after) + 1
        prior_runs, runs = runs[:index], runs[index:]
    elif args.previous_status is not None:
        parser.error("--previous-status requires --resume-after")
    if args.allow_new_binary and not prior_runs:
        parser.error("--allow-new-binary requires an audited --resume-after prefix")
    if args.dry_run:
        print(json.dumps({"output_root": str(root), "protected_pack": str(protected), "binary": str(binary), "preserved_prefix": [run["label"] for run in prior_runs], "runs": [{"label": run["label"], "sequence": ["run", "ordinary_validation", "lod_validation"]} for run in runs]}, indent=2))
        return
    status_path = args.status.resolve() if args.status else root / "validation-status.json"
    if not status_path.is_relative_to(root) or status_path.exists():
        parser.error("choose a new status path inside the isolated output root; existing evidence is preserved")
    if not binary.is_file():
        parser.error("converter binary is missing; finish its build before running validation")
    for run in runs:
        evidence = Path(option(run["run"], "--evidence"))
        if evidence.exists() and any(evidence.iterdir()):
            parser.error(f"evidence directory is already populated: {evidence}")
        if Path(run["output"]).exists():
            parser.error(f"isolated output already exists: {run['output']}")
    protected_manifest = protected / "conversion-manifest.json"
    source_hash = file_hash(protected_manifest)
    binary_hash = file_hash(binary)
    resume = None
    if prior_runs:
        try:
            old_records, history = load_resume_history(args.previous_status)
        except (OSError, ValueError, KeyError) as error:
            parser.error(str(error))
        for status in history:
            provenance = status["provenance"]
            if provenance["protected_manifest_sha256"] != source_hash or Path(provenance["protected_pack"]).resolve() != protected:
                parser.error("protected source differs from an inherited run")
        provenance = history[-1]["provenance"]
        binary_changed = provenance["converter_sha256"] != binary_hash
        if binary_changed and not args.allow_new_binary:
            parser.error("resume binary changed; explicitly pass --allow-new-binary after reviewing completed-prefix audits")
        completed = []
        for run in prior_runs:
            inherited = old_records.get(run["label"], {})
            record = inherited.get("record", {})
            if Path(record.get("output", "/")).resolve() != Path(run["output"]).resolve():
                parser.error(f"resume prefix output differs from its recorded run: {run['label']}")
            steps = {step["phase"]: step for step in record.get("steps", [])}
            if any(steps.get(phase, {}).get("exit_code") != 0 for phase in ("run", "ordinary_validation")):
                parser.error(f"resume prefix lacks successful conversion and full ordinary check: {run['label']}")
            audit_path = Path(option(run["lod_validation"], "--output"))
            audit = json.loads(audit_path.read_text())
            output = Path(run["output"]).resolve()
            if not audit.get("passed") or Path(audit["pack"]).resolve() != output or len(audit["chunks"]) != run["expected_lod_chunks"]:
                parser.error(f"resume prefix lacks a successful complete LOD audit: {run['label']}")
            if audit["ordinary_manifest_hash"] != file_hash(output / "conversion-manifest.json") or audit["lod_manifest"] != json.loads((output / "lod-manifest.json").read_text()):
                parser.error(f"resume prefix manifests changed after audit: {run['label']}")
            coverage_reference = audit.get("coverage_reference") or {}
            if Path(coverage_reference.get("pack", "/")).resolve() != protected or coverage_reference.get("manifest_hash") != source_hash:
                parser.error(f"resume prefix was not audited against the protected source: {run['label']}")
            completed.append({"label": run["label"], "output": str(output), "audit": str(audit_path), "audit_sha256": file_hash(audit_path), "ordinary_manifest_sha256": audit["ordinary_manifest_hash"], "converter_sha256": inherited["converter_sha256"], "recorded_status": inherited["status"]})
        resume = {"after": args.resume_after, "previous_status": str(args.previous_status.resolve()), "previous_status_sha256": history[-1]["sha256"], "previous_converter_sha256": provenance["converter_sha256"], "current_converter_sha256": binary_hash, "binary_changed": binary_changed, "explicitly_allowed_new_binary": args.allow_new_binary, "preserved_prefix": completed, "verified_status_chain": history}
    helpers = Path(__file__).parent
    status = {
        "started_at_utc": timestamp(), "passed": False,
        "provenance": {"plan": str(args.plan.resolve()), "plan_sha256": file_hash(args.plan), "converter": str(binary), "converter_sha256": binary_hash, "protected_pack": str(protected), "protected_manifest_sha256": source_hash, "helper_sha256": {path.name: file_hash(path) for path in sorted(helpers.glob("*.py"))}, "cwd": str(Path.cwd())},
        "runs": [],
    }
    if resume is not None:
        status["resume"] = resume
    save(status_path, status)
    try:
        for run in runs:
            record = {"label": run["label"], "output": run["output"], "passed": False, "steps": []}
            status["runs"].append(record)
            evidence = Path(option(run["run"], "--evidence"))
            evidence.mkdir(parents=True, exist_ok=True)
            for phase in ("run", "ordinary_validation", "lod_validation"):
                step = {"phase": phase, "command": run[phase], "started_at_utc": timestamp(), "state": "running", "log": str(evidence / f"{phase}-runner.log")}
                record["steps"].append(step)
                save(status_path, status)
                print(f"{run['label']}: {phase}", flush=True)
                started = time.monotonic()
                with Path(step["log"]).open("w") as stream:
                    result = subprocess.run(run[phase], stdout=stream, stderr=subprocess.STDOUT)
                step.update({"finished_at_utc": timestamp(), "wall_seconds": time.monotonic() - started, "exit_code": result.returncode, "state": "passed" if result.returncode == 0 else "failed"})
                save(status_path, status)
                if result.returncode:
                    raise RuntimeError(f"{run['label']} failed during {phase}; see {step['log']}")
            if file_hash(protected_manifest) != source_hash:
                raise RuntimeError("protected source manifest changed; stopping validation")
            record["passed"] = True
            save(status_path, status)
        status["passed"] = True
    except BaseException as error:
        status["error"] = str(error) or type(error).__name__
        raise
    finally:
        status["finished_at_utc"] = timestamp()
        save(status_path, status)
    print(json.dumps({"passed": True, "status": str(status_path), "runs": len(runs)}))


if __name__ == "__main__":
    main()
