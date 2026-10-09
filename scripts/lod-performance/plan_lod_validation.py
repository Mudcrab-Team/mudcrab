#!/usr/bin/env python3
"""Print an isolated validation matrix. This script never starts workloads."""
import argparse
import json
from pathlib import Path


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--converter", type=Path, required=True)
    p.add_argument("--data", type=Path, required=True)
    p.add_argument("--source-pack", type=Path, required=True)
    p.add_argument("--root", type=Path, required=True)
    p.add_argument("--guard-pid", type=int, required=True, help="Guard the generated runs against this PID; pass 0 explicitly when no job needs guarding")
    p.add_argument("--expected-chunks", type=int, default=4673, help="Expected complete LOD coverage; set a smaller count for fixture packs")
    p.add_argument("--stage-summary", action="store_true", help="Have the observer save reported phase durations and progress event spans")
    p.add_argument("--cpu-jobs", type=int, default=8)
    p.add_argument("--io-jobs", type=int, default=4)
    p.add_argument("--gpu-quality", type=int, default=2)
    p.add_argument("--gpu-batch-mb", type=int, default=256)
    p.add_argument("--texture-encoder", choices=["cpu", "gpu"], default="gpu", help="Ordinary texture recipe; must match the retained source pack")
    a = p.parse_args()
    if a.guard_pid < 0:
        p.error("--guard-pid must be nonnegative")
    if a.expected_chunks <= 0:
        p.error("--expected-chunks must be positive")
    root = a.root.resolve()
    binary = a.converter.resolve()
    data = a.data.resolve()
    source = a.source_pack.resolve()
    audit = Path(__file__).with_name("audit_lod_pack.py").resolve()
    observe = Path(__file__).with_name("run_conversion_observed.py").resolve()
    runs = []
    for label, encoder, parent, expected_hits, comparison in [
        ("meta-cpu-cold", "cpu", source, 0, None),
        ("meta-cpu-warm", "cpu", root / "meta-cpu-cold", a.expected_chunks, ("meta-cpu-cold", "exact")),
        ("meta-gpu-cold", "gpu", root / "meta-cpu-cold", 0, ("meta-cpu-cold", "encoder-transition")),
        ("meta-gpu-warm", "gpu", root / "meta-gpu-cold", a.expected_chunks, ("meta-gpu-cold", "exact")),
        ("full-gpu-fresh", "gpu", None, 0, ("meta-gpu-cold", "exact")),
        ("full-gpu-warm", "gpu", None, a.expected_chunks, ("full-gpu-fresh", "exact")),
    ]:
        # Normal warm conversion uses the same isolated output; metadata rebuilds require a new output each time.
        output = root / ("full-gpu-fresh" if label == "full-gpu-warm" else label)
        logs = root / (label + "-evidence")
        command = [str(binary), str(data), str(output), "--texture-encoder", a.texture_encoder, "--lod-encoder", encoder, "--gpu-quality", str(a.gpu_quality), "--gpu-batch-mb", str(a.gpu_batch_mb), "--cpu-jobs", str(a.cpu_jobs), "--io-jobs", str(a.io_jobs), "--report-json", str(logs / "conversion-report.json")]
        if parent:
            command += ["--reuse-assets", str(parent)]
        if label == "full-gpu-fresh":
            command += ["--invalidate-cache"]
        audit_command = ["python3", str(audit), str(output), "--output", str(logs / "lod-audit.json"), "--expected-chunks", str(a.expected_chunks), "--report", str(logs / "conversion-report.json"), "--expected-hits", str(expected_hits), "--encoder", encoder, "--coverage-reference-pack", str(source), "--data-root", str(data)]
        if comparison:
            old_label, mode = comparison
            audit_command += ["--compare", str(root / (old_label + "-evidence") / "lod-audit.json"), "--comparison", mode]
        run_command = ["python3", str(observe), "--guard-pid", str(a.guard_pid), "--evidence", str(logs), "--gpu-stats", "--measure-output-size"]
        if a.stage_summary:
            run_command.append("--stage-summary")
        run_command += ["--", *command]
        runs.append({"label": label, "output": str(output), "source": str(parent) if parent else None, "expected_lod_chunks": a.expected_chunks, "expected_lod_cache_hits": expected_hits, "run": run_command, "ordinary_validation": [str(binary), "check", str(output), "--full"], "lod_validation": audit_command})
    print(json.dumps({"guard_pid": a.guard_pid, "execution": "Run sequentially only after the guarded conversion exits. Save each audit before running a normal warm conversion that replaces the same isolated output.", "limits": ["Metadata cold means fresh LOD compilation; retained ordinary assets are verified and linked. It does not test full cold archive ingestion.", "The initial metadata CPU source must lack current CPU LOD proof, or use a separate source copy with LOD proof removed. Otherwise its hit count can be nonzero.", "Healthy GPU phases require zero fallback notices, all input proofs, and the expected hit count. Intentional fallback uses audit --allow-unproven and a later healthy retry.", "Missing authored origins are accepted only when all worldspace origins, LAND identities and plugin checksums match the explicit source-pack reference. Metadata archive omissions must also match reference package rules and archive hashes; other warnings fail.", "GPU utilization counters are global; converter phase timings and actual GPU output/fallback counts establish attribution."], "runs": runs}, indent=2))


if __name__ == "__main__":
    main()
