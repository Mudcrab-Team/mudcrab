#!/usr/bin/env python3
"""Observe one explicit conversion on macOS using time -l, caffeinate, and iostat."""
import argparse
from contextlib import ExitStack
from datetime import datetime, timezone
import json
import math
import multiprocessing
import os
from pathlib import Path
import plistlib
import resource
import stat
import subprocess
import sys
import time
from xml.parsers.expat import ExpatError


GPU_STAT_KEYS = (
    "Device Utilization %",
    "Renderer Utilization %",
    "Tiler Utilization %",
    "In use system memory",
)
PROCESS_NAMES = {"converter", "rustc", "cargo", "engine", "launcher", "mudcrab"}


def filtered_gpu_statistics(registry):
    stack = [registry]
    statistics = []
    while stack:
        item = stack.pop()
        if isinstance(item, dict):
            values = item.get("PerformanceStatistics")
            if isinstance(values, dict):
                filtered = {
                    key: values[key] for key in GPU_STAT_KEYS
                    if key in values and isinstance(values[key], (int, float))
                    and not isinstance(values[key], bool) and math.isfinite(values[key])
                }
                if filtered:
                    statistics.append(filtered)
            stack.extend(value for value in item.values() if isinstance(value, (dict, list)))
        elif isinstance(item, list):
            stack.extend(item)
    return statistics


def gpu_sample():
    sample = {"timestamp_utc": datetime.now(timezone.utc).isoformat(), "available": False}
    try:
        result = subprocess.run(["/usr/sbin/ioreg", "-a", "-r", "-c", "IOAccelerator"], capture_output=True, timeout=10)
    except subprocess.TimeoutExpired:
        return {**sample, "error": "ioreg_timeout"}
    except OSError:
        return {**sample, "error": "ioreg_unavailable"}
    if result.returncode:
        return {**sample, "error": "ioreg_nonzero_exit", "returncode": result.returncode}
    try:
        statistics = filtered_gpu_statistics(plistlib.loads(result.stdout))
    except (plistlib.InvalidFileException, ValueError, TypeError, OverflowError, ExpatError):
        return {**sample, "error": "invalid_plist"}
    if not statistics:
        return {**sample, "error": "requested_statistics_unavailable"}
    return {**sample, "available": True, "statistics": statistics}


def disk_sample(path):
    sample = {"timestamp_utc": datetime.now(timezone.utc).isoformat(), "available": False}
    try:
        volume = os.statvfs(path)
    except OSError:
        return {**sample, "error": "filesystem_statistics_unavailable"}
    return {**sample, "available": True, "available_bytes": volume.f_bavail * volume.f_frsize, "free_bytes": volume.f_bfree * volume.f_frsize}


def process_sample(names):
    sample = {"timestamp_utc": datetime.now(timezone.utc).isoformat(), "available": False}
    try:
        result = subprocess.run(["/bin/ps", "-axo", "pid,ppid,time,%cpu,rss,state,comm"], capture_output=True, text=True, timeout=5)
    except subprocess.TimeoutExpired:
        return {**sample, "error": "process_statistics_timeout"}
    except (OSError, UnicodeError):
        return {**sample, "error": "process_statistics_unavailable"}
    if result.returncode:
        return {**sample, "error": "process_statistics_nonzero_exit", "returncode": result.returncode}
    processes = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 6)
        if len(fields) != 7 or not fields[0].isdigit() or not fields[1].isdigit():
            continue
        pid, ppid, cpu_time, percent_cpu, rss, state, comm = fields
        if Path(comm).name in names:
            processes.append({"pid": int(pid), "ppid": int(ppid), "cpu_time_raw": cpu_time, "percent_cpu_raw": percent_cpu, "rss_kib_raw": rss, "state": state, "comm": comm})
    return {**sample, "available": True, "processes": processes}


def sample_host_until_stopped(gpu_path, disk_path, process_path, process_names, volume_path, stop):
    with ExitStack() as files:
        disk_stream = files.enter_context(disk_path.open("w"))
        process_stream = files.enter_context(process_path.open("w"))
        gpu_stream = files.enter_context(gpu_path.open("w")) if gpu_path else None
        while not stop.is_set():
            started = time.monotonic()
            disk_stream.write(json.dumps(disk_sample(volume_path)) + "\n")
            disk_stream.flush()
            process_stream.write(json.dumps(process_sample(process_names)) + "\n")
            process_stream.flush()
            if gpu_stream:
                gpu_stream.write(json.dumps(gpu_sample(), allow_nan=False) + "\n")
                gpu_stream.flush()
            stop.wait(max(0, 5 - (time.monotonic() - started)))


def directory_size(path):
    if not path.is_dir():
        return {"exists": False}
    logical = 0
    unique_logical = 0
    files = 0
    seen = set()
    for directory, _, names in os.walk(path, followlinks=False):
        for name in names:
            info = os.stat(Path(directory) / name, follow_symlinks=False)
            if stat.S_ISREG(info.st_mode):
                files += 1
                logical += info.st_size
                inode = (info.st_dev, info.st_ino)
                if inode not in seen:
                    seen.add(inode)
                    unique_logical += info.st_size
    return {"exists": True, "files": files, "logical_bytes": logical, "unique_inode_logical_bytes": unique_logical, "method": "One post-run tree walk; hard links counted once in unique_inode_logical_bytes. APFS clones can share extents, so logical sizes do not measure physical space use."}


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def snapshot(path):
    commands = [["/bin/ps", "-axo", "pid,ppid,etime,time,%cpu,%mem,state,comm"], ["/usr/sbin/iostat", "-d", "-w", "1", "-c", "2"]]
    with path.open("w") as stream:
        stream.write(datetime.now(timezone.utc).isoformat() + "\n")
        for command in commands:
            result = subprocess.run(command, capture_output=True, text=True)
            stream.write(json.dumps(command) + "\n" + result.stdout + result.stderr)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--guard-pid", type=int, required=True, help="Refuse execution while this PID exists; pass 0 explicitly when no job needs guarding")
    p.add_argument("--evidence", type=Path, required=True)
    p.add_argument("--stage-summary", action="store_true", help="Write reported phase durations and stage event spans; CPU/I/O counters remain whole-run")
    p.add_argument("--gpu-stats", action="store_true", help="Sample four global IOAccelerator driver counters every five seconds; averaging window is unknown")
    p.add_argument("--measure-output-size", action="store_true", help="Walk output/cache once after timing finishes to record logical sizes")
    p.add_argument("command", nargs=argparse.REMAINDER)
    a = p.parse_args()
    command = a.command[1:] if a.command[:1] == ["--"] else a.command
    if not command:
        p.error("supply the exact converter command after --")
    if a.guard_pid < 0:
        p.error("--guard-pid must be nonnegative")
    if sys.platform != "darwin":
        p.error("this observer requires macOS time -l, caffeinate, and iostat; the plan and audit tools are platform-independent")
    if a.guard_pid > 0 and alive(a.guard_pid):
        raise SystemExit(f"Refusing to start: guarded PID {a.guard_pid} remains active.")
    a.evidence.mkdir(parents=True, exist_ok=True)
    snapshot(a.evidence / "host-before.txt")
    io_stream = (a.evidence / "iostat-during.txt").open("w")
    io_stream.write(datetime.now(timezone.utc).isoformat() + "\n")
    io_stream.flush()
    # Keep the monitor alive across both rusage readings so its counters are
    # not counted among the timed command's completed children.
    io_monitor = subprocess.Popen(["/usr/sbin/iostat", "-d", "-w", "5"], stdout=io_stream, stderr=io_stream)
    disk_before = disk_sample(a.evidence)
    statistics_monitor = None
    statistics_stop = None
    process_names = PROCESS_NAMES | {Path(command[0]).name}
    try:
        context = multiprocessing.get_context("spawn")
        statistics_stop = context.Event()
        gpu_path = a.evidence / "gpu-statistics.jsonl" if a.gpu_stats else None
        statistics_monitor = context.Process(target=sample_host_until_stopped, args=(gpu_path, a.evidence / "disk-space.jsonl", a.evidence / "process-statistics.jsonl", process_names, a.evidence, statistics_stop), daemon=True)
        statistics_monitor.start()
    except (OSError, RuntimeError):
        statistics_monitor = None
        statistics_stop = None
        unavailable = json.dumps({"timestamp_utc": datetime.now(timezone.utc).isoformat(), "available": False, "error": "statistics_sampler_unavailable"}) + "\n"
        (a.evidence / "disk-space.jsonl").write_text(unavailable)
        (a.evidence / "process-statistics.jsonl").write_text(unavailable)
        if a.gpu_stats:
            (a.evidence / "gpu-statistics.jsonl").write_text(unavailable)
    started_at = datetime.now(timezone.utc).isoformat()
    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    observed = ["/usr/bin/time", "-l", "/usr/bin/caffeinate", "-i", *command]
    try:
        with (a.evidence / "stdout.log").open("w") as out, (a.evidence / "stderr-and-time.log").open("w") as err:
            result = subprocess.run(observed, stdout=out, stderr=err)
    finally:
        wall = time.monotonic() - started
        usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
        finished_at = datetime.now(timezone.utc).isoformat()
        disk_after = disk_sample(a.evidence)
        if io_monitor.poll() is None:
            io_monitor.terminate()
        io_monitor.wait()
        io_stream.close()
        if statistics_monitor:
            statistics_stop.set()
            statistics_monitor.join(timeout=17)
            if statistics_monitor.is_alive():
                statistics_monitor.terminate()
                statistics_monitor.join(timeout=2)
    metadata = {"command": command, "observed_command": observed, "cwd": str(Path.cwd()), "guard_pid": a.guard_pid, "started_at_utc": started_at, "finished_at_utc": finished_at, "wall_seconds": wall, "exit_code": result.returncode, "child_user_seconds": usage_after.ru_utime - usage_before.ru_utime, "child_system_seconds": usage_after.ru_stime - usage_before.ru_stime, "input_blocks": usage_after.ru_inblock - usage_before.ru_inblock, "output_blocks": usage_after.ru_oublock - usage_before.ru_oublock, "resource_note": "Counters include the converter and transient time/caffeinate wrappers; host snapshot processes and the still-running I/O/host-statistics monitors are outside those CPU/counter totals. iostat samples every five seconds globally; its first sample is since boot and other processes can contribute."}
    disk_samples = [disk_before, disk_after]
    disk_log = a.evidence / "disk-space.jsonl"
    if disk_log.is_file():
        for line in disk_log.read_text().splitlines():
            try:
                disk_samples.append(json.loads(line))
            except ValueError:
                pass
    available_samples = [sample["available_bytes"] for sample in disk_samples if sample.get("available")]
    metadata["disk_space"] = {"path": str(disk_log), "before": disk_before, "after": disk_after, "minimum_sampled_available_bytes": min(available_samples) if available_samples else None, "sampling_interval_seconds": 5, "monitor_exit_code": statistics_monitor.exitcode if statistics_monitor else None, "method": "Volume-wide user-available bytes from statvfs, including unrelated writes. Minimum covers sampled readings; no staging tree walks during the command."}
    metadata["process_statistics"] = {"path": str(a.evidence / "process-statistics.jsonl"), "sampling_interval_seconds": 5, "executable_basenames": sorted(process_names), "monitor_exit_code": statistics_monitor.exitcode if statistics_monitor else None, "method": "Host-wide named converter/build/engine processes, filtered by executable basename. CPU time, %cpu, and RSS fields retain ps text; RSS is KiB on macOS. OS-reported %cpu uses a platform-dependent averaging window, can exceed 100 for multiple threads, and is not a stage average. Comm contains executable names/paths; argv and environment are excluded."}
    if a.gpu_stats:
        metadata["gpu_statistics"] = {
            "path": str(a.evidence / "gpu-statistics.jsonl"),
            "sampling_interval_seconds": 5,
            "monitor_exit_code": statistics_monitor.exitcode if statistics_monitor else None,
            "method": "Global IOAccelerator driver-reported values, with an unknown averaging window. Samples are not per-process and other workloads can contribute. Errors become unavailable samples; no full registry data is retained.",
        }
    metadata["average_cpu_cores"] = (metadata["child_user_seconds"] + metadata["child_system_seconds"]) / wall if wall > 0 else 0
    if a.measure_output_size and len(command) >= 3:
        output = Path(command[2]).resolve()
        cache = output.with_name(output.name + ".assets-cache")
        if "--cache-dir" in command:
            cache = Path(command[command.index("--cache-dir") + 1]).resolve()
        metadata["final_sizes"] = {"output": directory_size(output), "cache": directory_size(cache)}
    report_path = None
    if "--report-json" in command:
        report_path = Path(command[command.index("--report-json") + 1])
    if report_path and report_path.is_file():
        report = json.loads(report_path.read_text())
        metadata["conversion_scalars"] = {k: v for k, v in report.items() if not isinstance(v, (dict, list))}
        metadata["conversion_notices"] = report.get("notices", [])
        metadata["conversion_lod_warnings"] = report.get("lod_warnings", [])
        metadata["conversion_stages"] = report.get("stages", [])
        if a.stage_summary:
            summary = {
                "reported_phase_seconds": {key.removesuffix("_elapsed_ms"): report[key] / 1000 for key in ("lod_elapsed_ms", "publication_elapsed_ms") if key in report},
                "stage_event_spans": [{"stage": stage["stage"], "first_seconds": stage["first_seconds"], "last_seconds": stage["last_seconds"], "event_span_seconds": stage["last_seconds"] - stage["first_seconds"]} for stage in report.get("stages", [])],
                "method": "Stage spans cover first-to-last progress events, can overlap, and can omit gaps. CPU and I/O measurements cover the whole command and are not apportioned to stages.",
            }
            (a.evidence / "stage-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
            metadata["stage_summary"] = summary
    (a.evidence / "run.json").write_text(json.dumps(metadata, indent=2) + "\n")
    snapshot(a.evidence / "host-after.txt")
    print(json.dumps(metadata))
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
