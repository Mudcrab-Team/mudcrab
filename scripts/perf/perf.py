#!/usr/bin/env python3
"""Measure the Mudcrab engine, compare two binaries statistically, and compare screenshots.

Subcommands: run, compare, preflight, shots, visual, build, scenarios. The guide is
docs/contributing/perf-loop.md. Standard library only; numpy and flip-evaluator are used for the
visual metric when they happen to be installed.

Exit codes: 0 done, 2 bad usage or input, 3 refused (a guard, a failed preflight, or too few valid
runs), 4 a run or capture did not produce a valid result.
"""

import argparse
import hashlib
import itertools
import json
import math
import os
import platform
import random
import re
import shutil
import signal
import statistics
import subprocess
import sys
import time
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPTS_DIR = HERE.parent
SCENARIOS_FILE = HERE / "scenarios.json"

GIB = 1024**3
DEFAULT_DURATION = 60.0
DEFAULT_MIN_EFFECT = 0.02
DEFAULT_SEED = 1
BOOTSTRAP_SAMPLES = 10000
# A bootstrap over fewer pairs than this cannot exclude 1.0 for any reason except that every pair
# moved the same way, so a verdict needs at least this many usable pairs.
MIN_PAIRS_FOR_VERDICT = 3
MIN_FREE_BUILD_GIB = 15
MIN_FREE_RUN_GIB = 5
ROUTE_READINESS_SECONDS = 60
# The report keys the verdict is drawn from; the other metrics are listed but never decide it.
PRIMARY_METRICS = ("frame_ms_mean", "frame_ms_p50", "frame_ms_p95", "frame_ms_p99")
FRAME_METRICS = (
    "frame_ms_mean",
    "frame_ms_p50",
    "frame_ms_p95",
    "frame_ms_p99",
    "average_fps",
)
HIGHER_IS_BETTER = {"average_fps"}
# Fingerprint fields that name the build rather than the conditions it was measured under.
FINGERPRINT_IGNORED = ("commit", "dirty_worktree")
SCENE_COUNTS = ("mesh_entities", "terrain_patches", "water_surfaces")
# Counts below this absolute difference are never a mismatch: a streaming scene can differ by a
# handful of entities between two runs of the same binary.
INVENTORY_ABSOLUTE_SLACK = 2
DEFAULT_INVENTORY_TOLERANCE = 0.02
TOP_CPU_SPANS = 8
MIN_SPAN_SAMPLES = 5


class HarnessError(Exception):
    """An error with a stable name, so a verdict or an agent can match on it."""

    def __init__(self, name, message):
        super().__init__(f"{name}: {message}")
        self.name = name
        self.message = message


# --------------------------------------------------------------------------------------------
# Scenarios


def load_scenarios(path=SCENARIOS_FILE):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    return data["scenarios"]


def route_travel_seconds(route_path):
    route = json.loads(Path(route_path).read_text(encoding="utf-8"))
    if route.get("loop"):
        raise HarnessError("route_loops", f"{route_path} loops, so it has no travel time")
    points = route["waypoints"]
    length = sum(
        math.hypot(points[i][0] - points[i - 1][0], points[i][1] - points[i - 1][1])
        for i in range(1, len(points))
    )
    return int(math.ceil(length / float(route["speed"])))


def effective_duration(scenario, requested):
    """The window a scenario needs: a route is walked end to end, so it is never cut short."""
    duration = requested if requested is not None else scenario.get("default_duration", DEFAULT_DURATION)
    route = scenario.get("route")
    if route:
        needed = route_travel_seconds(SCRIPTS_DIR / route) + ROUTE_READINESS_SECONDS
        duration = max(duration, needed)
    return duration


def build_engine_args(name, scenario, *, assets, out_dir, duration, warmup_frames, hidden, commit, run_id,
                      frame_budget_ms, allow_incomplete=True):
    """The engine command line (without the binary) for one scenario run."""
    if scenario.get("needs_assets") and not assets:
        raise HarnessError("assets_required", f"scenario {name} needs --assets")
    out_dir = Path(out_dir)
    warmup = warmup_frames if warmup_frames is not None else scenario.get("warmup_frames")
    args = [
        "--benchmark-duration", fmt_number(effective_duration(scenario, duration)),
        "--benchmark-output", str(out_dir / "report.json"),
        "--profile-output", str(out_dir / "bundle"),
        "--profile-scenario", name,
        "--profile-run-id", run_id,
        "--profile-commit", commit,
        # The engine fails its own acceptance gate on a slow frame; a measurement run must not.
        "--accept-min-fps", "0",
        "--accept-p95-ms", "1000000",
        "--accept-max-memory-growth-gib", "1000000",
        "--frame-budget-ms", fmt_number(frame_budget_ms),
    ]
    if warmup is not None:
        args += ["--benchmark-warmup-frames", str(int(warmup))]
    args += list(scenario["args"])
    if scenario.get("route"):
        args += ["--route", str(SCRIPTS_DIR / scenario["route"])]
    if scenario.get("needs_assets"):
        args += ["--assets", str(assets)]
        if allow_incomplete:
            args.append("--allow-incomplete-assets")
    if hidden:
        args.append("--hidden-window")
    return args


def fmt_number(value):
    return str(int(value)) if float(value).is_integer() else repr(float(value))


# --------------------------------------------------------------------------------------------
# Running an engine under a watchdog


def sha256_file(path, _cache={}):
    path = str(Path(path).resolve())
    stat = os.stat(path)
    key = (path, stat.st_size, stat.st_mtime_ns)
    if key not in _cache:
        digest = hashlib.sha256()
        with open(path, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
        _cache[key] = digest.hexdigest()
    return _cache[key]


def read_complete_report(path):
    """The benchmark report once the engine has finished writing it, else None.

    The report is a single JSON object written at the end of the window; a half-written file does
    not parse, so parsing is the completeness check.
    """
    try:
        data = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if isinstance(data, dict) and "frame_ms_mean" in data and "fingerprint" in data:
        return data
    return None


def _terminate_group(proc, grace=5.0):
    """Stop the engine and anything it spawned. Returns True when SIGKILL was needed."""
    try:
        pgid = os.getpgid(proc.pid)
    except (ProcessLookupError, OSError):
        pgid = None

    def signal_group(sig):
        try:
            if pgid is not None:
                os.killpg(pgid, sig)
            else:
                proc.send_signal(sig)
        except (ProcessLookupError, OSError):
            pass

    signal_group(signal.SIGTERM)
    try:
        proc.wait(timeout=grace)
        return False
    except subprocess.TimeoutExpired:
        signal_group(signal.SIGKILL)
        proc.wait()
        return True


def watch_process(command, *, stdout_path, stderr_path, is_done, timeout, grace, poll=0.2, env=None):
    """Run a command and return what happened.

    `is_done()` says the output the run exists for is complete. The engine has been seen to write
    its report and then never exit (a hang in pipelined-render teardown), so once the output is
    complete the process gets `grace` seconds to exit by itself and is killed after that. A
    process that never produces the output is killed at `timeout`.
    """
    started = time.monotonic()
    done_at = None
    killed = False
    reason = None
    with open(stdout_path, "wb") as out, open(stderr_path, "wb") as err:
        try:
            proc = subprocess.Popen(command, stdout=out, stderr=err, stdin=subprocess.DEVNULL,
                                    start_new_session=True, env=env)
        except OSError as error:
            return {"spawned": False, "error": str(error), "exit_code": None, "killed": False,
                    "output_complete": False, "wall_seconds": 0.0, "complete_after_seconds": None,
                    "kill_reason": None}
        while True:
            code = proc.poll()
            now = time.monotonic() - started
            if done_at is None and is_done():
                done_at = now
            if code is not None:
                break
            if done_at is not None and now - done_at >= grace:
                killed, reason = True, "hung_after_output"
            elif now >= timeout:
                killed, reason = True, "timeout"
            if killed:
                _terminate_group(proc)
                code = proc.returncode
                break
            time.sleep(poll)
        if done_at is None and is_done():
            done_at = time.monotonic() - started
    return {"spawned": True, "exit_code": code, "killed": killed, "kill_reason": reason,
            "output_complete": done_at is not None, "wall_seconds": round(time.monotonic() - started, 3),
            "complete_after_seconds": None if done_at is None else round(done_at, 3)}


def classify(watch):
    """ok, hung_after_report, timeout_no_report or failed."""
    if not watch["spawned"]:
        return "failed"
    if watch["output_complete"]:
        return "hung_after_report" if watch["killed"] else "ok"
    return "timeout_no_report" if watch["killed"] else "failed"


VALID_CLASSES = ("ok", "hung_after_report")


def run_scenario(engine, name, scenario, *, assets, out_dir, duration=None, warmup_frames=None, timeout=None,
                 grace=20.0, hidden=True, run_id=None, commit=None, frame_budget_ms=6.25, dry_run=False,
                 extra_args=(), poll=0.2):
    """Run one scenario and write run.json beside the report. Returns the run.json dict."""
    out_dir = Path(out_dir)
    engine = str(engine)
    binary_sha = sha256_file(engine)
    commit = commit or binary_sha[:8]
    run_id = run_id or out_dir.name
    window = effective_duration(scenario, duration)
    args = build_engine_args(name, scenario, assets=assets, out_dir=out_dir, duration=duration,
                             warmup_frames=warmup_frames, hidden=hidden, commit=commit, run_id=run_id,
                             frame_budget_ms=frame_budget_ms) + list(extra_args)
    command = [engine] + args
    if dry_run:
        return {"command": command}
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)
    report_path = out_dir / "report.json"
    # The default wall limit leaves room for loading the world, which the window includes.
    timeout = timeout if timeout is not None else window + 120
    watch = watch_process(command, stdout_path=out_dir / "stdout.log", stderr_path=out_dir / "stderr.log",
                          is_done=lambda: read_complete_report(report_path) is not None,
                          timeout=timeout, grace=grace, poll=poll)
    report = read_complete_report(report_path)
    classification = classify(watch)
    warnings = []
    if report is not None:
        if watch["exit_code"] not in (0, None) and not watch["killed"]:
            warnings.append(f"engine exited with {watch['exit_code']} after writing a complete report")
        if report.get("world_ready_reached") is False:
            warnings.append("the world never became ready inside the window")
        ready = report.get("frames_to_world_ready")
        warmup = report.get("warmup_frames")
        if ready is not None and warmup is not None and ready > warmup:
            warnings.append(f"world ready at frame {ready}, after the {warmup}-frame warm-up: "
                            "the measured frames include loading; raise --warmup-frames")
    run = {
        "scenario": name,
        "classification": classification,
        "valid": classification in VALID_CLASSES and report is not None,
        "command": command,
        "binary": engine,
        "binary_sha256": binary_sha,
        "hidden_window": hidden,
        "duration_seconds": window,
        "timeout_seconds": timeout,
        "grace_seconds": grace,
        "exit_code": watch["exit_code"],
        "killed": watch["killed"],
        "kill_reason": watch["kill_reason"],
        "wall_seconds": watch["wall_seconds"],
        "report_complete_after_seconds": watch["complete_after_seconds"],
        "report_passed": None if report is None else report.get("passed"),
        "bundle_complete": (out_dir / "bundle" / "summary.md").exists(),
        "report": str(report_path) if report is not None else None,
        "warnings": warnings,
    }
    if watch.get("error"):
        run["error"] = watch["error"]
    (out_dir / "run.json").write_text(json.dumps(run, indent=2), encoding="utf-8")
    return run


# --------------------------------------------------------------------------------------------
# Metrics


def _mean_of(summary):
    return summary.get("mean") if isinstance(summary, dict) else None


def extract_metrics(report, bundle_dir=None):
    """Flatten a benchmark report into `name -> number`. Missing or zero-filled values are left out."""
    metrics = {}
    for key in FRAME_METRICS:
        value = report.get(key)
        if isinstance(value, (int, float)):
            metrics[key] = float(value)
    for name, summary in (report.get("gpu_passes", {}).get("passes") or {}).items():
        mean = _mean_of(summary)
        # Timestamp queries that read back all zero (Metal reports this) carry no information.
        if isinstance(mean, (int, float)) and mean > 0:
            metrics[f"gpu/{name}"] = float(mean)
    cpu = report.get("cpu") or {}
    for group in ("schedules", "spans"):
        for name, summary in (cpu.get(group) or {}).items():
            mean = _mean_of(summary)
            if isinstance(mean, (int, float)) and (summary.get("count") or 0) >= MIN_SPAN_SAMPLES:
                metrics[f"cpu/{name}"] = float(mean)
    peak = report.get("peak_process_memory_gib")
    if not isinstance(peak, (int, float)) or peak <= 0:
        peak = None
        if bundle_dir is not None:
            try:
                samples = json.loads((Path(bundle_dir) / "memory.json").read_text(encoding="utf-8"))["samples"]
                peak = max((s.get("process_gib") or 0.0) for s in samples) or None
            except (OSError, ValueError, KeyError):
                peak = None
    if peak:
        metrics["memory_peak_gib"] = float(peak)
    return metrics


def extract_scene(report):
    scene = report.get("scene") or {}
    return {key: scene.get(key) for key in SCENE_COUNTS}


def load_run(run_dir):
    """The run.json, report-derived metrics, fingerprint and scene counts of one run directory."""
    run = json.loads((Path(run_dir) / "run.json").read_text(encoding="utf-8"))
    entry = {"dir": str(run_dir), "run": run, "metrics": {}, "fingerprint": None, "scene": {}}
    if run.get("valid") and run.get("report"):
        report = json.loads(Path(run["report"]).read_text(encoding="utf-8"))
        entry["metrics"] = extract_metrics(report, Path(run_dir) / "bundle")
        entry["fingerprint"] = report.get("fingerprint")
        entry["scene"] = extract_scene(report)
        entry["warmup_frames"] = report.get("warmup_frames")
    return entry


# --------------------------------------------------------------------------------------------
# Statistics


def percentile_sorted(values, fraction):
    """Linear-interpolated percentile of an already sorted list."""
    if not values:
        raise ValueError("no values")
    position = fraction * (len(values) - 1)
    low = int(math.floor(position))
    high = min(low + 1, len(values) - 1)
    return values[low] + (values[high] - values[low]) * (position - low)


def median(values):
    return statistics.median(values)


def bootstrap_ci(ratios, *, seed, samples=BOOTSTRAP_SAMPLES, alpha=0.05):
    """Percentile bootstrap interval of the median of per-pair ratios, resampling whole pairs."""
    rng = random.Random(seed)
    count = len(ratios)
    medians = []
    for _ in range(samples):
        draw = sorted(ratios[rng.randrange(count)] for _ in range(count))
        middle = count // 2
        medians.append(draw[middle] if count % 2 else (draw[middle - 1] + draw[middle]) / 2.0)
    medians.sort()
    return percentile_sorted(medians, alpha / 2), percentile_sorted(medians, 1 - alpha / 2)


def verdict_for(ratio, lo, hi, *, min_effect, higher_is_better=False):
    """improved / regressed / noise. Significant only if the CI excludes 1.0 and the effect is big enough."""
    excludes_one = lo > 1.0 or hi < 1.0
    if not (excludes_one and abs(ratio - 1.0) >= min_effect):
        return "noise"
    worse = ratio < 1.0 if higher_is_better else ratio > 1.0
    return "regressed" if worse else "improved"


def compare_metric(name, base_values, cand_values, *, seed, min_effect, samples=BOOTSTRAP_SAMPLES):
    """Per-pair ratios candidate/base and their verdict. Lists are index-aligned pairs."""
    pairs = [(b, c) for b, c in zip(base_values, cand_values)
             if b is not None and c is not None and b > 0 and c >= 0]
    result = {"pairs": len(pairs), "higher_is_better": name in HIGHER_IS_BETTER}
    if not pairs:
        result["verdict"] = "no_data"
        return result
    ratios = [c / b for b, c in pairs]
    result.update({
        "base_median": median([b for b, _ in pairs]),
        "cand_median": median([c for _, c in pairs]),
        "ratios": ratios,
        "ratio_median": median(ratios),
    })
    if len(pairs) < MIN_PAIRS_FOR_VERDICT:
        result.update({"ci_lo": None, "ci_hi": None, "verdict": "insufficient_pairs"})
        return result
    lo, hi = bootstrap_ci(ratios, seed=seed, samples=samples)
    result.update({
        "ci_lo": lo,
        "ci_hi": hi,
        "verdict": verdict_for(result["ratio_median"], lo, hi, min_effect=min_effect,
                               higher_is_better=result["higher_is_better"]),
    })
    return result


# --------------------------------------------------------------------------------------------
# Guards


def fingerprint_diff(old, new, prefix=""):
    """`path: old -> new` lines for every field that differs, ignoring the build identity fields."""
    old, new = old or {}, new or {}
    lines = []
    for key in sorted(set(old) | set(new)):
        if not prefix and key in FINGERPRINT_IGNORED:
            continue
        path = f"{prefix}.{key}" if prefix else key
        a, b = old.get(key), new.get(key)
        if isinstance(a, dict) and isinstance(b, dict):
            lines += fingerprint_diff(a, b, path)
        elif a != b:
            lines.append(f"{path}: {json.dumps(a)} -> {json.dumps(b)}")
    return lines


def check_fingerprints(base_fingerprints, cand_fingerprints):
    """Every run, on both sides, must have been measured under the same conditions."""
    everything = [("base", i, f) for i, f in enumerate(base_fingerprints)] + \
                 [("cand", i, f) for i, f in enumerate(cand_fingerprints)]
    if not everything:
        return []
    reference_side, reference_index, reference = everything[0]
    problems = []
    for side, index, fingerprint in everything[1:]:
        for line in fingerprint_diff(reference, fingerprint):
            problems.append(f"{side}[{index}] vs {reference_side}[{reference_index}] {line}")
    return problems


def check_inventory(base_scenes, cand_scenes, *, tolerance=DEFAULT_INVENTORY_TOLERANCE):
    """The two sides must have drawn the same scene; a smaller scene is not an optimisation."""
    problems = []
    for key in SCENE_COUNTS:
        base = [s[key] for s in base_scenes if s.get(key) is not None]
        cand = [s[key] for s in cand_scenes if s.get(key) is not None]
        if not base or not cand:
            continue
        b, c = median(base), median(cand)
        allowed = max(tolerance * max(b, c), INVENTORY_ABSOLUTE_SLACK)
        if abs(b - c) > allowed:
            problems.append(f"{key}: base median {b:g} vs candidate median {c:g} "
                            f"(allowed difference {allowed:g})")
    return problems


# --------------------------------------------------------------------------------------------
# Compare


def analyze_scenario(name, pairs, *, seed, min_effect, inventory_tolerance=DEFAULT_INVENTORY_TOLERANCE,
                     samples=BOOTSTRAP_SAMPLES, top_spans=TOP_CPU_SPANS):
    """Verdict for one scenario from its pairs: [{"base": loaded run, "cand": loaded run}, ...]."""
    usable = [p for p in pairs if p["base"]["run"].get("valid") and p["cand"]["run"].get("valid")]
    result = {
        "status": "ok",
        "pairs_requested": len(pairs),
        "pairs_used": len(usable),
        "dropped_pairs": [i for i, p in enumerate(pairs) if p not in usable],
        "classifications": {
            "base": [p["base"]["run"]["classification"] for p in pairs],
            "cand": [p["cand"]["run"]["classification"] for p in pairs],
        },
    }
    if len(usable) < 2:
        result.update(status="refused", error="too_few_valid_pairs",
                      details=[f"{len(usable)} of {len(pairs)} pairs had a valid run on both sides"])
        return result
    problems = check_fingerprints([p["base"]["fingerprint"] for p in usable],
                                  [p["cand"]["fingerprint"] for p in usable])
    if problems:
        result.update(status="refused", error="fingerprint_mismatch", details=problems)
        return result
    problems = check_inventory([p["base"]["scene"] for p in usable], [p["cand"]["scene"] for p in usable],
                               tolerance=inventory_tolerance)
    if problems:
        result.update(status="refused", error="inventory_mismatch", details=problems)
        return result
    names = set()
    for p in usable:
        names |= set(p["base"]["metrics"]) & set(p["cand"]["metrics"])
    cpu = sorted((n for n in names if n.startswith("cpu/")),
                 key=lambda n: -median([p["base"]["metrics"][n] for p in usable]))
    chosen = [n for n in sorted(names) if not n.startswith("cpu/")] + cpu[:top_spans]
    metrics = {}
    for metric in chosen:
        metrics[metric] = compare_metric(
            metric, [p["base"]["metrics"].get(metric) for p in usable],
            [p["cand"]["metrics"].get(metric) for p in usable],
            seed=f"{seed}:{name}:{metric}", min_effect=min_effect, samples=samples)
    result["metrics"] = metrics
    verdicts = {metrics[m]["verdict"] for m in PRIMARY_METRICS if m in metrics}
    if len(usable) < MIN_PAIRS_FOR_VERDICT:
        result["summary_verdict"] = "insufficient_pairs"
    elif {"improved", "regressed"} <= verdicts:
        result["summary_verdict"] = "mixed"
    elif "regressed" in verdicts:
        result["summary_verdict"] = "regressed"
    elif "improved" in verdicts:
        result["summary_verdict"] = "improved"
    else:
        result["summary_verdict"] = "noise"
    warnings = []
    for p in usable:
        for side in ("base", "cand"):
            warnings += p[side]["run"].get("warnings", [])
    result["run_warnings"] = sorted(set(warnings))
    return result


def overall_verdict(scenarios):
    if any(s["status"] == "refused" for s in scenarios.values()):
        return "refused"
    verdicts = {s["summary_verdict"] for s in scenarios.values()}
    if "regressed" in verdicts and "improved" in verdicts or "mixed" in verdicts:
        return "mixed"
    for candidate in ("regressed", "improved", "insufficient_pairs"):
        if candidate in verdicts:
            return candidate
    return "noise"


def format_value(name, value):
    if value is None:
        return "n/a"
    if name == "average_fps":
        return f"{value:.1f}"
    return f"{value:.3f}"


def summary_markdown(verdict):
    lines = ["# Perf comparison", "",
             f"- base: `{verdict['base']['path']}` (sha256 {verdict['base']['sha256'][:12]})",
             f"- candidate: `{verdict['cand']['path']}` (sha256 {verdict['cand']['sha256'][:12]})",
             f"- pairs requested: {verdict['pairs']}, min effect {verdict['min_effect'] * 100:.1f}%, "
             f"seed {verdict['seed']}",
             f"- overall: **{verdict['overall']}**"]
    if verdict["same_binary"]:
        lines.append("- same binary on both sides: this is a noise-floor run, every verdict should be `noise`")
    for check in verdict.get("preflight", []):
        if check["status"] in ("warn", "fail"):
            lines.append(f"- preflight {check['status']}: {check['name']}: {check['detail']}")
    for name, scenario in verdict["scenarios"].items():
        lines += ["", f"## {name}", ""]
        if scenario["status"] == "refused":
            lines.append(f"REFUSED `{scenario['error']}`")
            lines += [f"- {d}" for d in scenario["details"]]
            continue
        lines.append(f"{scenario['pairs_used']} of {scenario['pairs_requested']} pairs used; "
                     f"summary verdict **{scenario['summary_verdict']}**")
        for warning in scenario.get("run_warnings", []):
            lines.append(f"- warning: {warning}")
        lines += ["", "| metric | base median | cand median | ratio (95% CI) | verdict |", "|---|---:|---:|---:|---|"]
        hidden = 0
        for metric, row in scenario["metrics"].items():
            if metric not in PRIMARY_METRICS and metric != "average_fps" and row["verdict"] == "noise":
                hidden += 1
                continue
            ratio = "n/a" if row.get("ratio_median") is None else f"{row['ratio_median']:.3f}"
            ci = "" if row.get("ci_lo") is None else f" ({row['ci_lo']:.3f} to {row['ci_hi']:.3f})"
            lines.append(f"| {metric} | {format_value(metric, row.get('base_median'))} | "
                         f"{format_value(metric, row.get('cand_median'))} | "
                         f"{ratio}{ci} | {row['verdict']} |")
        if hidden:
            lines.append("")
            lines.append(f"{hidden} other metrics (GPU passes, CPU spans, memory) were noise and are not listed; "
                         "see verdict.json.")
    return "\n".join(lines) + "\n"


def compare(args):
    scenarios_all = load_scenarios()
    names = [n.strip() for n in args.scenarios.split(",") if n.strip()]
    for name in names:
        if name not in scenarios_all:
            raise HarnessError("unknown_scenario", f"{name}; known: {', '.join(scenarios_all)}")
    for label, binary in (("base", args.base), ("cand", args.cand)):
        if not os.access(binary, os.X_OK):
            raise HarnessError("bad_binary", f"--{label} {binary} is not an executable file")
    out = Path(args.out)
    if out.exists() and any(out.iterdir()) and not args.force:
        raise HarnessError("out_not_empty", f"{out} is not empty; pick a new directory or pass --force")
    out.mkdir(parents=True, exist_ok=True)
    checks = preflight_checks(out)
    failures = [c for c in checks if c["status"] == "fail"]
    for check in checks:
        print(f"preflight {check['status']:5} {check['name']}: {check['detail']}", file=sys.stderr)
    if failures and not args.force:
        raise HarnessError("preflight_failed", "; ".join(f"{c['name']}: {c['detail']}" for c in failures)
                           + " (pass --force to measure anyway)")
    sides = {"base": args.base, "cand": args.cand}
    rows = {name: [] for name in names}
    hidden = not args.visible
    for pair in range(args.pairs):
        order = ("base", "cand") if pair % 2 == 0 else ("cand", "base")
        for name in names:
            loaded = {}
            for side in order:
                run_dir = out / name / f"pair-{pair:02d}" / side
                print(f"run {name} pair {pair} {side} ...", file=sys.stderr, flush=True)
                run = run_scenario(sides[side], name, scenarios_all[name], assets=args.assets, out_dir=run_dir,
                                   duration=args.duration, warmup_frames=args.warmup_frames,
                                   timeout=args.timeout, grace=args.grace, hidden=hidden,
                                   run_id=f"{name}-{pair:02d}-{side}", commit=side)
                print(f"  -> {run['classification']} in {run['wall_seconds']}s", file=sys.stderr, flush=True)
                loaded[side] = load_run(run_dir)
                time.sleep(args.cooldown)
            rows[name].append(loaded)
    verdict = {
        "tool": "perf.py compare",
        "base": {"path": str(args.base), "sha256": sha256_file(args.base)},
        "cand": {"path": str(args.cand), "sha256": sha256_file(args.cand)},
        "pairs": args.pairs, "min_effect": args.min_effect, "seed": args.seed,
        "hidden_window": hidden,
        "preflight": checks,
        "scenarios": {},
    }
    verdict["same_binary"] = verdict["base"]["sha256"] == verdict["cand"]["sha256"]
    for name in names:
        verdict["scenarios"][name] = analyze_scenario(
            name, rows[name], seed=args.seed, min_effect=args.min_effect,
            inventory_tolerance=args.inventory_tolerance)
    verdict["overall"] = overall_verdict(verdict["scenarios"])
    (out / "verdict.json").write_text(json.dumps(verdict, indent=2), encoding="utf-8")
    (out / "summary.md").write_text(summary_markdown(verdict), encoding="utf-8")
    print((out / "summary.md").read_text(encoding="utf-8"))
    return 3 if verdict["overall"] == "refused" else 0


# --------------------------------------------------------------------------------------------
# Preflight (macOS)


def parse_power_source(text):
    match = re.search(r"Now drawing from '([^']+)'", text)
    return match.group(1) if match else None


def parse_cpu_speed_limit(text):
    match = re.search(r"CPU_Speed_Limit\s*=\s*(\d+)", text)
    return int(match.group(1)) if match else None


def parse_low_power_mode(text):
    """True when Low Power Mode is on. Newer macOS prints `powermode` (0 normal, 1 low, 2 high)."""
    match = re.search(r"^\s*(?:lowpowermode|powermode)\s+(\d+)", text, re.MULTILINE)
    return None if match is None else int(match.group(1)) == 1


def _command_output(command):
    try:
        return subprocess.run(command, capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None


def running_processes(names):
    """Names from `names` with at least one running process, by exact executable name."""
    found = []
    for name in names:
        output = _command_output(["pgrep", "-x", name])
        if output and output.strip():
            found.append(name)
    return found


def preflight_checks(out_dir=None, *, system=None, command_output=_command_output, free_bytes=None,
                     processes=running_processes):
    """A list of {name, status: pass|warn|fail|skip, detail}."""
    system = system or platform.system()
    checks = []

    def add(name, status, detail):
        checks.append({"name": name, "status": status, "detail": detail})

    target = str(out_dir or ".")
    free = free_bytes if free_bytes is not None else shutil.disk_usage(
        target if os.path.exists(target) else os.path.dirname(os.path.abspath(target))).free
    add("disk", "pass" if free >= MIN_FREE_RUN_GIB * GIB else "fail",
        f"{free / GIB:.1f} GiB free, need {MIN_FREE_RUN_GIB}")
    if system != "Darwin":
        add("macos", "skip", f"not macOS ({system}); power, thermal and low-power checks skipped")
        return checks
    batt = command_output(["pmset", "-g", "batt"])
    source = parse_power_source(batt or "")
    if source is None:
        add("ac_power", "warn", "pmset gave no power source")
    elif source == "AC Power":
        add("ac_power", "pass", "on AC power")
    else:
        add("ac_power", "fail", f"drawing from {source}; plug in")
    therm = command_output(["pmset", "-g", "therm"])
    limit = parse_cpu_speed_limit(therm or "")
    if therm is None:
        add("thermal", "warn", "pmset -g therm failed")
    elif limit is not None and limit < 100:
        add("thermal", "fail", f"CPU_Speed_Limit {limit}: the machine is throttled, let it cool")
    else:
        add("thermal", "pass", "no thermal limit recorded")
    low = parse_low_power_mode(command_output(["pmset", "-g"]) or "")
    if low is None:
        add("low_power_mode", "warn", "pmset reports no lowpowermode setting")
    elif low:
        add("low_power_mode", "fail", "Low Power Mode is on")
    else:
        add("low_power_mode", "pass", "Low Power Mode is off")
    busy = processes(["cargo", "rustc"])
    add("no_builds", "fail" if busy else "pass", f"running: {', '.join(busy)}" if busy else "no cargo or rustc")
    engines = [name for name in processes(["engine"]) if name]
    add("no_other_engine", "fail" if engines else "pass",
        "another engine process is running" if engines else "no engine process running")
    return checks


def preflight(args):
    checks = preflight_checks(args.out or ".")
    for check in checks:
        print(f"{check['status']:5} {check['name']}: {check['detail']}")
    return 3 if any(c["status"] == "fail" for c in checks) else 0


# --------------------------------------------------------------------------------------------
# Screenshots: capture and PNG handling


def read_shot_names(shots_file):
    data = json.loads(Path(shots_file).read_text(encoding="utf-8"))
    return [shot["name"] for shot in data["shots"] if not shot.get("interior_cell_id")]


def capture_shots(engine, shots_file, assets, out_dir, *, worldspace="0x3c", timeout=300.0, grace=15.0,
                  extra_args=(), poll=0.2):
    """Capture with a visible window: a hidden window gives black frames on macOS."""
    out_dir = Path(out_dir)
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)
    names = read_shot_names(shots_file)
    command = [str(engine), "--assets", str(assets), "--allow-incomplete-assets", "--worldspace", worldspace,
               "--shots", str(shots_file), "--shots-out", str(out_dir)] + list(extra_args)

    def all_written():
        return all((out_dir / f"{n}.png").is_file() and (out_dir / f"{n}.png").stat().st_size > 0 for n in names)

    watch = watch_process(command, stdout_path=out_dir.parent / f"{out_dir.name}.stdout.log",
                          stderr_path=out_dir.parent / f"{out_dir.name}.stderr.log", is_done=all_written,
                          timeout=timeout, grace=grace, poll=poll)
    classification = classify(watch)
    run = {"classification": classification, "valid": classification in VALID_CLASSES, "command": command,
           "binary_sha256": sha256_file(engine), "shots": names, "exit_code": watch["exit_code"],
           "killed": watch["killed"], "wall_seconds": watch["wall_seconds"]}
    (out_dir.parent / f"{out_dir.name}.run.json").write_text(json.dumps(run, indent=2), encoding="utf-8")
    return run


class PngError(Exception):
    pass


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
_CHANNELS = {0: 1, 2: 3, 4: 2, 6: 4}


def _paeth_row(line, prev, bpp):
    out = bytearray(line)
    for i in range(len(out)):
        a = out[i - bpp] if i >= bpp else 0
        b = prev[i]
        c = prev[i - bpp] if i >= bpp else 0
        p = a + b - c
        pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
        predictor = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
        out[i] = (out[i] + predictor) & 255
    return out


def decode_png(path):
    """Decode an 8-bit, non-interlaced grey, grey+alpha, RGB or RGBA PNG: (width, height, channels, bytes)."""
    data = Path(path).read_bytes()
    if data[:8] != PNG_SIGNATURE:
        raise PngError(f"{path}: not a PNG")
    pos, header, idat = 8, None, []
    while pos + 8 <= len(data):
        length = int.from_bytes(data[pos:pos + 4], "big")
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = body
        elif kind == b"IDAT":
            idat.append(body)
        elif kind == b"IEND":
            break
    if header is None or not idat:
        raise PngError(f"{path}: truncated PNG")
    width, height = int.from_bytes(header[0:4], "big"), int.from_bytes(header[4:8], "big")
    depth, ctype, _, _, interlace = header[8], header[9], header[10], header[11], header[12]
    if depth != 8 or ctype not in _CHANNELS or interlace != 0:
        raise PngError(f"{path}: unsupported PNG (depth {depth}, colour type {ctype}, interlace {interlace})")
    channels = _CHANNELS[ctype]
    stride = width * channels
    try:
        raw = zlib.decompress(b"".join(idat))
    except zlib.error as error:
        raise PngError(f"{path}: bad image data ({error})") from error
    if len(raw) != (stride + 1) * height:
        raise PngError(f"{path}: image data is {len(raw)} bytes, expected {(stride + 1) * height}")
    pixels = bytearray(stride * height)
    prev = bytes(stride)
    for y in range(height):
        start = y * (stride + 1)
        kind = raw[start]
        line = raw[start + 1:start + 1 + stride]
        if kind == 0:
            row = bytearray(line)
        elif kind == 1:
            row = bytearray(line)
            for c in range(channels):
                row[c::channels] = bytes(itertools.accumulate(line[c::channels], lambda a, b: (a + b) & 255))
        elif kind == 2:
            row = bytearray((a + b) & 255 for a, b in zip(line, prev))
        elif kind == 3:
            row = bytearray(line)
            for i in range(stride):
                left = row[i - channels] if i >= channels else 0
                row[i] = (row[i] + ((left + prev[i]) >> 1)) & 255
        elif kind == 4:
            row = _paeth_row(line, prev, channels)
        else:
            raise PngError(f"{path}: unknown filter {kind} on row {y}")
        pixels[y * stride:(y + 1) * stride] = row
        prev = row
    return width, height, channels, bytes(pixels)


def write_png(path, width, height, channels, pixels):
    """Write an 8-bit non-interlaced PNG (grey, RGB or RGBA by channel count 1, 3, 4)."""
    ctype = {1: 0, 3: 2, 4: 6}[channels]
    stride = width * channels
    raw = b"".join(b"\x00" + pixels[y * stride:(y + 1) * stride] for y in range(height))

    def chunk(kind, body):
        return (len(body).to_bytes(4, "big") + kind + body
                + (zlib.crc32(kind + body) & 0xFFFFFFFF).to_bytes(4, "big"))

    header = width.to_bytes(4, "big") + height.to_bytes(4, "big") + bytes([8, ctype, 0, 0, 0])
    Path(path).write_bytes(PNG_SIGNATURE + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 6))
                           + chunk(b"IEND", b""))


def _color_planes(image):
    """The colour planes (alpha dropped) of a decoded image as a list of bytes objects."""
    _, _, channels, pixels = image
    colour = 1 if channels in (1, 2) else 3
    return [pixels[c::channels] for c in range(colour)]


def blank_stats(image, stride=7):
    """Mean and standard deviation of the colour samples; a blank frame has almost no spread."""
    samples = bytes().join(plane[::stride] for plane in _color_planes(image))
    count = len(samples)
    mean = sum(samples) / count
    variance = sum((s - mean) ** 2 for s in samples) / count
    std = math.sqrt(variance)
    return {"mean": mean, "std": std, "blank": std < 0.5, "black": mean < 3.0 and std < 3.0}


def pixel_diff(a, b, threshold=8):
    """Mean abs difference (0-255 scale) and the fraction of colour samples differing by more than threshold."""
    if a[:2] != b[:2]:
        raise PngError(f"size mismatch: {a[0]}x{a[1]} vs {b[0]}x{b[1]}")
    total, over, count, worst = 0, 0, 0, 0
    for pa, pb in zip(_color_planes(a), _color_planes(b)):
        if pa == pb:
            count += len(pa)
            continue
        diffs = bytes(map(abs, map(int.__sub__, pa, pb)))
        total += sum(diffs)
        over += sum(1 for d in diffs if d > threshold)
        worst = max(worst, max(diffs))
        count += len(diffs)
    return {"mean_abs_diff": total / count, "fraction_over": over / count, "max_diff": worst,
            "threshold": threshold}


def flip_available():
    try:
        import flip_evaluator  # noqa: F401
        import numpy  # noqa: F401
    except ImportError:
        return False
    return True


def flip_diff(a, b, heatmap_path=None):
    """NVIDIA FLIP between two decoded images; needs numpy and flip-evaluator."""
    import numpy as np
    import flip_evaluator as flip

    def rgb(image):
        width, height, channels, pixels = image
        array = np.frombuffer(pixels, dtype=np.uint8).reshape(height, width, channels)
        array = array[:, :, :3] if channels >= 3 else np.repeat(array[:, :, :1], 3, axis=2)
        return array.astype(np.float32) / 255.0

    error_map, mean, _ = flip.evaluate(rgb(a), rgb(b), "LDR", inputsRGB=True, applyMagmaMap=False,
                                       computeMeanError=True)
    error = np.asarray(error_map, dtype=np.float32).reshape(a[1], a[0])
    result = {"mean_flip": float(mean), "p99_flip": float(np.percentile(error, 99)),
              "fraction_over": float((error > 0.1).mean()), "threshold": 0.1}
    if heatmap_path is not None:
        ramp = np.clip(error * 2.0, 0, 1)
        heat = np.stack([ramp * 255, np.clip(error * 2.0 - 1.0, 0, 1) * 255, (1 - ramp) * 80], axis=2)
        write_png(heatmap_path, a[0], a[1], 3, heat.astype(np.uint8).tobytes())
    return result


def choose_metric(force_stdlib=False):
    """flip when it is installed and works, else the stdlib metric, with the reason it was not flip."""
    if force_stdlib:
        return "stdlib", None
    if not flip_available():
        return "stdlib", None
    try:
        probe = (4, 4, 3, bytes(range(48)))
        flip_diff(probe, probe)
    except Exception as error:  # the extra's API is outside this repo's control
        return "stdlib", f"flip-evaluator is installed but failed ({error}); used the stdlib metric"
    return "flip", None


DEFAULT_MARGINS = {
    "stdlib": {"mean": 0.5, "fraction": 0.005},
    "flip": {"mean": 0.005, "fraction": 0.005},
}


def visual_compare(base_dir, cand_dir, out_dir, *, noise_floor=None, margin_mean=None, margin_fraction=None,
                   force_stdlib=False):
    base_dir, cand_dir, out_dir = Path(base_dir), Path(cand_dir), Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    metric, metric_warning = choose_metric(force_stdlib)
    margins = dict(DEFAULT_MARGINS[metric])
    if margin_mean is not None:
        margins["mean"] = margin_mean
    if margin_fraction is not None:
        margins["fraction"] = margin_fraction
    floor = None
    if noise_floor:
        floor = json.loads(Path(noise_floor).read_text(encoding="utf-8"))
        if floor.get("metric") != metric:
            raise HarnessError("noise_floor_metric_mismatch",
                               f"the noise floor used {floor.get('metric')}, this run uses {metric}")
    names = sorted({p.stem for p in base_dir.glob("*.png")} | {p.stem for p in cand_dir.glob("*.png")})
    result = {"tool": "perf.py visual", "metric": metric, "margins": margins, "shots": {}, "review": []}
    if metric_warning:
        result["warnings"] = [metric_warning]
    sizes = {"base": {}, "cand": {}}
    images = {}
    for name in names:
        entry = {"status": "pass", "reasons": []}
        result["shots"][name] = entry
        paths = {"base": base_dir / f"{name}.png", "cand": cand_dir / f"{name}.png"}
        for side, path in paths.items():
            if not path.is_file():
                entry["reasons"].append(f"missing_{side}")
                continue
            sizes[side][name] = path.stat().st_size
            try:
                images[(side, name)] = decode_png(path)
            except PngError as error:
                entry["reasons"].append(f"undecodable_{side}: {error}")
                continue
            stats = blank_stats(images[(side, name)])
            entry[f"{side}_stats"] = {k: round(v, 3) if isinstance(v, float) else v for k, v in stats.items()}
            if stats["blank"] or stats["black"]:
                entry["reasons"].append(f"blank_frame_{side}")
        if ("base", name) in images and ("cand", name) in images:
            try:
                if metric == "flip":
                    heat = out_dir / f"{name}.flip.png"
                    measured = flip_diff(images[("base", name)], images[("cand", name)], heat)
                    measured["heatmap"] = str(heat)
                    mean, fraction = measured["mean_flip"], measured["fraction_over"]
                else:
                    measured = pixel_diff(images[("base", name)], images[("cand", name)])
                    mean, fraction = measured["mean_abs_diff"], measured["fraction_over"]
            except PngError as error:
                entry["reasons"].append(f"size_mismatch: {error}")
            else:
                entry["measured"] = measured
                base_floor = (floor or {}).get("measured", {}).get(name, {})
                limit_mean = base_floor.get("mean", 0.0) + margins["mean"]
                limit_fraction = base_floor.get("fraction", 0.0) + margins["fraction"]
                entry["limits"] = {"mean": limit_mean, "fraction": limit_fraction}
                entry["values"] = {"mean": mean, "fraction": fraction}
                if mean > limit_mean:
                    entry["reasons"].append(f"mean_difference {mean:.4f} > {limit_mean:.4f}")
                if fraction > limit_fraction:
                    entry["reasons"].append(f"fraction_over {fraction:.4f} > {limit_fraction:.4f}")
        if entry["reasons"]:
            entry["status"] = "fail"
    for side in ("base", "cand"):
        if len(sizes[side]) >= 2 and len(set(sizes[side].values())) == 1:
            result.setdefault("warnings", []).append(
                f"every {side} shot has the same file size ({next(iter(sizes[side].values()))} bytes): "
                "the signature of a black capture run")
            result["suspect_black_run"] = True
    result["measured"] = {n: e["values"] for n, e in result["shots"].items() if "values" in e}
    result["review"] = sorted(n for n, e in result["shots"].items() if e["status"] != "pass")
    if result.get("suspect_black_run"):
        result["review"] = sorted(names)
    result["overall"] = "fail" if result["review"] else "pass"
    (out_dir / "visual.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    return result


# --------------------------------------------------------------------------------------------
# Build


def _run(command, **kwargs):
    return subprocess.run(command, capture_output=True, text=True, check=False, **kwargs)


def git_toplevel(start):
    result = _run(["git", "-C", str(start), "rev-parse", "--show-toplevel"])
    if result.returncode != 0:
        raise HarnessError("not_a_repository", f"{start}: {result.stderr.strip()}")
    return Path(result.stdout.strip())


def resolve_sha(ref, repo, runner=_run):
    result = runner(["git", "-C", str(repo), "rev-parse", "--verify", f"{ref}^{{commit}}"])
    if result.returncode != 0:
        raise HarnessError("unknown_ref", f"{ref}: {result.stderr.strip()}")
    return result.stdout.strip()


def plan_build(ref, *, repo, worktree_root, bin_dir, runner=_run):
    sha = resolve_sha(ref, repo, runner)
    short = sha[:8]
    return {"ref": ref, "sha": sha, "short": short, "binary": Path(bin_dir) / f"engine-{short}",
            "worktree": Path(worktree_root) / f"perf-{short}"}


def build(ref, *, repo, worktree_root, bin_dir, runner=_run, free_bytes=None, keep_worktree=False,
          min_free_gib=MIN_FREE_BUILD_GIB, dry_run=False, environ=None):
    plan = plan_build(ref, repo=repo, worktree_root=worktree_root, bin_dir=bin_dir, runner=runner)
    plan["reused"] = plan["binary"].is_file()
    if plan["reused"] or dry_run:
        return plan
    Path(worktree_root).mkdir(parents=True, exist_ok=True)
    Path(bin_dir).mkdir(parents=True, exist_ok=True)
    free = free_bytes if free_bytes is not None else shutil.disk_usage(worktree_root).free
    if free < min_free_gib * GIB:
        raise HarnessError("insufficient_disk",
                           f"{free / GIB:.1f} GiB free under {worktree_root}, a build needs {min_free_gib}")
    worktree = plan["worktree"]
    if worktree.exists():
        head = runner(["git", "-C", str(worktree), "rev-parse", "HEAD"]).stdout.strip()
        if head != plan["sha"]:
            raise HarnessError("worktree_conflict", f"{worktree} exists at {head[:8]}, not {plan['short']}")
    else:
        added = runner(["git", "-C", str(repo), "worktree", "add", "--detach", str(worktree), plan["sha"]])
        if added.returncode != 0:
            raise HarnessError("worktree_failed", added.stderr.strip())
    env = dict(os.environ if environ is None else environ)
    # A shared target directory lets cargo reuse another tree's test and build outputs, and it
    # defeats sccache keys; each build gets the target directory inside its own worktree.
    env.pop("CARGO_TARGET_DIR", None)
    env["CARGO_INCREMENTAL"] = "0"
    built = runner(["cargo", "build", "--locked", "--release", "-p", "engine", "--bin", "engine"],
                   cwd=str(worktree), env=env)
    if built.returncode != 0:
        raise HarnessError("build_failed", (built.stderr or "").strip()[-2000:])
    produced = worktree / "target" / "release" / "engine"
    if not produced.is_file():
        raise HarnessError("binary_missing", f"{produced} was not produced")
    shutil.copy2(produced, plan["binary"])
    plan["binary"].chmod(0o755)
    if not keep_worktree:
        runner(["git", "-C", str(repo), "worktree", "remove", "--force", str(worktree)])
    plan["built"] = True
    return plan


# --------------------------------------------------------------------------------------------
# Command line


def cmd_run(args):
    scenarios = load_scenarios()
    if args.scenario not in scenarios:
        raise HarnessError("unknown_scenario", f"{args.scenario}; known: {', '.join(scenarios)}")
    run = run_scenario(args.engine, args.scenario, scenarios[args.scenario], assets=args.assets, out_dir=args.out,
                       duration=args.duration, warmup_frames=args.warmup_frames, timeout=args.timeout,
                       grace=args.grace, hidden=not args.visible, dry_run=args.dry_run)
    if args.dry_run:
        print(" ".join(run["command"]))
        return 0
    print(json.dumps({k: run[k] for k in ("classification", "valid", "exit_code", "wall_seconds", "warnings")}))
    return 0 if run["valid"] else 4


def cmd_shots(args):
    run = capture_shots(args.engine, args.shots, args.assets, args.out, worldspace=args.worldspace,
                        timeout=args.timeout, grace=args.grace)
    print(json.dumps({k: run[k] for k in ("classification", "valid", "exit_code", "wall_seconds")}))
    return 0 if run["valid"] else 4


def cmd_visual(args):
    result = visual_compare(args.base, args.cand, args.out, noise_floor=args.noise_floor,
                            margin_mean=args.margin_mean, margin_fraction=args.margin_fraction,
                            force_stdlib=args.stdlib)
    print(f"metric {result['metric']}: {result['overall']}")
    for name, entry in result["shots"].items():
        values = entry.get("values")
        detail = "" if not values else f" mean {values['mean']:.4f} fraction_over {values['fraction']:.4f}"
        print(f"  {name}: {entry['status']}{detail} {'; '.join(entry['reasons'])}")
    for warning in result.get("warnings", []):
        print(f"warning: {warning}")
    return 0 if result["overall"] == "pass" else 3


def cmd_build(args):
    repo = Path(args.repo) if args.repo else git_toplevel(Path.cwd())
    plan = build(args.ref, repo=repo, worktree_root=args.worktree_root, bin_dir=args.bin_dir,
                 keep_worktree=args.keep_worktree, dry_run=args.dry_run)
    action = "reused" if plan["reused"] else ("would build" if args.dry_run else "built")
    print(f"{action} {plan['binary']} ({plan['sha']})")
    return 0


def cmd_scenarios(_args):
    for name, scenario in load_scenarios().items():
        needs = "assets" if scenario.get("needs_assets") else "no assets"
        print(f"{name:14} {needs:10} {scenario.get('description', '')}")
    return 0


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    def timing_options(p):
        p.add_argument("--duration", type=float, help="seconds per window (scenario default, else 60)")
        p.add_argument("--warmup-frames", type=int, help="frames before measuring (scenario default)")
        p.add_argument("--timeout", type=float, help="wall-clock limit per run (default duration + 120)")
        p.add_argument("--grace", type=float, default=20.0,
                       help="seconds an engine gets to exit after writing its report before it is killed")
        p.add_argument("--visible", action="store_true", help="do not pass --hidden-window")

    p = sub.add_parser("run", help="run one scenario under the watchdog")
    p.add_argument("--engine", required=True)
    p.add_argument("--scenario", required=True)
    p.add_argument("--assets")
    p.add_argument("--out", required=True)
    p.add_argument("--dry-run", action="store_true", help="print the command and exit")
    timing_options(p)
    p.set_defaults(func=cmd_run)

    p = sub.add_parser("compare", help="interleaved base/candidate comparison")
    p.add_argument("--base", required=True)
    p.add_argument("--cand", required=True)
    p.add_argument("--scenarios", required=True, help="comma-separated scenario names")
    p.add_argument("--pairs", type=int, default=5)
    p.add_argument("--assets")
    p.add_argument("--out", required=True)
    p.add_argument("--min-effect", type=float, default=DEFAULT_MIN_EFFECT)
    p.add_argument("--seed", type=int, default=DEFAULT_SEED)
    p.add_argument("--inventory-tolerance", type=float, default=DEFAULT_INVENTORY_TOLERANCE)
    p.add_argument("--cooldown", type=float, default=3.0, help="seconds between runs")
    p.add_argument("--force", action="store_true",
                   help="measure despite failed preflight checks or a non-empty --out")
    timing_options(p)
    p.set_defaults(func=compare)

    p = sub.add_parser("preflight", help="check the machine is fit for timing runs")
    p.add_argument("--out", help="directory whose disk is checked")
    p.set_defaults(func=preflight)

    p = sub.add_parser("shots", help="capture reference shots with a visible window")
    p.add_argument("--engine", required=True)
    p.add_argument("--shots", required=True)
    p.add_argument("--assets", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--worldspace", default="0x3c")
    p.add_argument("--timeout", type=float, default=300.0)
    p.add_argument("--grace", type=float, default=15.0)
    p.set_defaults(func=cmd_shots)

    p = sub.add_parser("visual", help="compare two directories of PNGs")
    p.add_argument("--base", required=True)
    p.add_argument("--cand", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--noise-floor", help="visual.json of two captures of the same binary")
    p.add_argument("--margin-mean", type=float)
    p.add_argument("--margin-fraction", type=float)
    p.add_argument("--stdlib", action="store_true", help="skip FLIP even when it is installed")
    p.set_defaults(func=cmd_visual)

    p = sub.add_parser("build", help="build the engine at a git ref into a binary directory")
    p.add_argument("ref")
    p.add_argument("--worktree-root", required=True)
    p.add_argument("--bin-dir", required=True)
    p.add_argument("--repo")
    p.add_argument("--keep-worktree", action="store_true")
    p.add_argument("--dry-run", action="store_true")
    p.set_defaults(func=cmd_build)

    p = sub.add_parser("scenarios", help="list scenarios")
    p.set_defaults(func=cmd_scenarios)
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        return args.func(args)
    except HarnessError as error:
        print(f"error: {error}", file=sys.stderr)
        return 3 if error.name in ("preflight_failed", "insufficient_disk") else 2


if __name__ == "__main__":
    sys.exit(main())
