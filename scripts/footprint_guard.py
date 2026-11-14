#!/usr/bin/env python3
"""Watch one process tree's physical footprint and kill it at a limit.

This tool launches nothing. Start the program yourself (inside its workspace lease), then
pass its PID here. The guard samples `phys_footprint` from `proc_pid_rusage` and sends
SIGKILL to the PID and its descendants when the sum reaches the limit.

Why footprint and not RSS: on Apple Silicon, graphics and swapped memory count towards
`phys_footprint` but not towards RSS. The `--clusters full` run reached 34.5 GiB of
footprint at 14.1 GiB of RSS. RSS is recorded in the log and is never compared.

Limits of a polling guard: memory can grow between two samples (about 1 GiB/s was
seen), so the process can pass the limit before the kill. The log records the largest
gap between samples. This is not a hard quota. The limit is capped at 20 GiB and the
guard refuses a higher value.

Exit codes: 0 the target exited by itself, 3 killed at the limit, 4 killed because
sampling failed, 5 killed at the wall deadline, 2 refused (bad arguments).
"""
from __future__ import annotations

import argparse
import ctypes
import json
import os
import signal
import sys
import time
from dataclasses import dataclass
from typing import Callable, Dict, Iterable, List, Optional, Tuple

GIB = 1024 ** 3
MAX_LIMIT_BYTES = 20 * GIB
DEFAULT_LIMIT_BYTES = 20 * GIB
MAX_INTERVAL_S = 0.1  # "at least every 100 ms"
MIN_INTERVAL_S = 0.01

EXIT_GONE = 0
EXIT_REFUSED = 2
EXIT_OVER_LIMIT = 3
EXIT_SAMPLE_FAILURE = 4
EXIT_DEADLINE = 5


class Refused(ValueError):
    """The arguments or the platform cannot give a safe guard."""


class ProcessGone(Exception):
    """The root process no longer exists (or its PID was reused)."""


class SampleError(Exception):
    """The footprint of a live process could not be read."""


@dataclass(frozen=True)
class Config:
    limit_bytes: int = DEFAULT_LIMIT_BYTES
    interval_s: float = MAX_INTERVAL_S
    deadline_s: Optional[float] = None
    # A failed read of a live root process is treated as unsafe and kills at once.
    max_sample_failures: int = 1

    def validate(self) -> "Config":
        if not 0 < self.limit_bytes <= MAX_LIMIT_BYTES:
            raise Refused(
                f"limit must be above 0 and at most {MAX_LIMIT_BYTES // GIB} GiB, "
                f"got {self.limit_bytes} bytes"
            )
        if not MIN_INTERVAL_S <= self.interval_s <= MAX_INTERVAL_S:
            raise Refused(
                f"interval must be {MIN_INTERVAL_S} to {MAX_INTERVAL_S} s, got {self.interval_s}"
            )
        if self.deadline_s is not None and self.deadline_s <= 0:
            raise Refused("deadline must be above 0 seconds")
        if self.max_sample_failures < 1:
            raise Refused("max_sample_failures must be at least 1")
        return self


@dataclass(frozen=True)
class Sample:
    footprint: int  # sum of phys_footprint over the tree, in bytes
    rss: int  # sum of resident size over the tree. Logged only. Never compared.
    pids: Tuple[int, ...]


@dataclass(frozen=True)
class Verdict:
    action: str  # "continue" or "kill" or "exit"
    reason: str
    exit_code: int = 0


CONTINUE = Verdict("continue", "within limits")


def decide(
    config: Config,
    elapsed_s: float,
    sample: Optional[Sample],
    consecutive_failures: int,
    gone: bool = False,
) -> Verdict:
    """The whole kill policy. Pure, so a unit test covers every branch.

    `sample` is None when the read failed. `gone` is True when the root process is gone.
    """
    if gone:
        return Verdict("exit", "target exited", EXIT_GONE)
    if sample is None:
        if consecutive_failures >= config.max_sample_failures:
            return Verdict("kill", "footprint sampling failed", EXIT_SAMPLE_FAILURE)
        return CONTINUE
    # The comparison uses footprint only. `sample.rss` is deliberately ignored.
    if sample.footprint >= config.limit_bytes:
        return Verdict(
            "kill",
            f"footprint {sample.footprint / GIB:.2f} GiB reached limit "
            f"{config.limit_bytes / GIB:.2f} GiB",
            EXIT_OVER_LIMIT,
        )
    if config.deadline_s is not None and elapsed_s >= config.deadline_s:
        return Verdict("kill", f"wall deadline {config.deadline_s:g} s", EXIT_DEADLINE)
    return CONTINUE


def descendants(rows: Iterable[Tuple[int, int]], root: int) -> List[int]:
    """`root` and every process below it. `rows` are (pid, ppid). Cycle-safe, root first."""
    children: Dict[int, List[int]] = {}
    for pid, ppid in rows:
        children.setdefault(ppid, []).append(pid)
    seen = {root}
    order = [root]
    index = 0
    while index < len(order):
        for child in sorted(children.get(order[index], ())):
            if child not in seen:
                seen.add(child)
                order.append(child)
        index += 1
    return order


@dataclass
class Result:
    verdict: Verdict
    samples: int
    peak_footprint: int
    last_footprint: int
    max_gap_s: float
    elapsed_s: float
    killed: List[int]


def watch(
    root: int,
    config: Config,
    sample_fn: Callable[[int], Sample],
    kill_fn: Callable[[int], None],
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
    log: Optional[Callable[[dict], None]] = None,
) -> Result:
    """Sample until the target exits or the policy says kill. Launches nothing."""
    config.validate()
    started = clock()
    last_sample_time: Optional[float] = None
    max_gap = 0.0
    peak = 0
    last_footprint = 0
    count = 0
    failures = 0
    last_pids: List[int] = []
    while True:
        now = clock()
        sample: Optional[Sample] = None
        gone = False
        error = ""
        try:
            sample = sample_fn(root)
        except ProcessGone:
            gone = True
        except SampleError as exc:
            error = str(exc)
        if sample is not None:
            failures = 0
            count += 1
            peak = max(peak, sample.footprint)
            last_footprint = sample.footprint
            last_pids = list(sample.pids)
            if last_sample_time is not None:
                max_gap = max(max_gap, now - last_sample_time)
            last_sample_time = now
        elif not gone:
            failures += 1
        verdict = decide(config, now - started, sample, failures, gone)
        if log is not None:
            log(
                {
                    "t": round(now - started, 4),
                    "footprint": sample.footprint if sample else None,
                    "rss": sample.rss if sample else None,
                    "pids": list(sample.pids) if sample else None,
                    "error": error or None,
                    "action": verdict.action,
                    "reason": verdict.reason,
                }
            )
        if verdict.action != "continue":
            killed: List[int] = []
            if verdict.action == "kill":
                # Kill the root first so it cannot respawn children, then its descendants.
                # On a failed read, fall back to the last tree seen, then to the root alone.
                targets = list(sample.pids) if sample else (last_pids or [root])
                for pid in targets:
                    try:
                        kill_fn(pid)
                        killed.append(pid)
                    except ProcessLookupError:
                        pass
            return Result(verdict, count, peak, last_footprint, max_gap, now - started, killed)
        sleep(config.interval_s)


# ----- macOS sampler -------------------------------------------------------------------


class _RusageV0(ctypes.Structure):
    # struct rusage_info_v0 from <sys/resource.h>. ri_phys_footprint is part of v0.
    _fields_ = [("ri_uuid", ctypes.c_uint8 * 16)] + [
        (name, ctypes.c_uint64)
        for name in (
            "ri_user_time",
            "ri_system_time",
            "ri_pkg_idle_wkups",
            "ri_interrupt_wkups",
            "ri_pageins",
            "ri_wired_size",
            "ri_resident_size",
            "ri_phys_footprint",
            "ri_proc_start_abstime",
            "ri_proc_exit_abstime",
        )
    ]


RUSAGE_INFO_V0 = 0


class DarwinSampler:
    """Reads phys_footprint for a PID and its descendants through libproc."""

    def __init__(self, include_children: bool = True):
        if sys.platform != "darwin":
            raise Refused("the footprint guard needs macOS (proc_pid_rusage)")
        self.lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self.lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
        self.lib.proc_pid_rusage.restype = ctypes.c_int
        self.lib.proc_listchildpids.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_int]
        self.lib.proc_listchildpids.restype = ctypes.c_int
        if ctypes.sizeof(_RusageV0) != 96:
            raise Refused("unexpected rusage_info_v0 layout")
        self.include_children = include_children
        self.root_start: Optional[int] = None

    def _usage(self, pid: int) -> _RusageV0:
        info = _RusageV0()
        if self.lib.proc_pid_rusage(pid, RUSAGE_INFO_V0, ctypes.byref(info)) != 0:
            err = ctypes.get_errno()
            if err == 3:  # ESRCH
                raise ProcessGone(pid)
            raise SampleError(f"proc_pid_rusage({pid}) failed, errno {err}")
        return info

    def _children(self, pid: int) -> List[int]:
        capacity = 1024
        buf = (ctypes.c_int * capacity)()
        count = self.lib.proc_listchildpids(pid, buf, ctypes.sizeof(buf))
        return [buf[i] for i in range(max(0, min(count, capacity)))]

    def tree(self, root: int) -> List[int]:
        if not self.include_children:
            return [root]
        rows: List[Tuple[int, int]] = []
        frontier = [root]
        seen = {root}
        while frontier:
            parent = frontier.pop()
            for child in self._children(parent):
                if child not in seen:
                    seen.add(child)
                    rows.append((child, parent))
                    frontier.append(child)
        return descendants(rows, root)

    def __call__(self, root: int) -> Sample:
        root_info = self._usage(root)
        if self.root_start is None:
            self.root_start = root_info.ri_proc_start_abstime
        elif root_info.ri_proc_start_abstime != self.root_start:
            raise ProcessGone(root)  # the PID was reused by another process
        footprint = root_info.ri_phys_footprint
        rss = root_info.ri_resident_size
        pids = [root]
        for pid in self.tree(root)[1:]:
            try:
                info = self._usage(pid)
            except ProcessGone:
                continue  # a child that exits between listing and reading
            footprint += info.ri_phys_footprint
            rss += info.ri_resident_size
            pids.append(pid)
        return Sample(footprint, rss, tuple(pids))


def _refuse_pid(pid: int) -> None:
    if pid <= 1 or pid == os.getpid() or pid == os.getppid():
        raise Refused(f"refusing to watch PID {pid}: it is the guard, its parent or init")


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--pid", type=int, required=True, help="PID to watch (started by the caller)")
    parser.add_argument(
        "--limit-gib",
        type=float,
        default=DEFAULT_LIMIT_BYTES / GIB,
        help="kill at this phys_footprint in GiB (default and maximum: 20)",
    )
    parser.add_argument("--interval-ms", type=float, default=MAX_INTERVAL_S * 1000)
    parser.add_argument("--deadline-s", type=float, default=None, help="optional wall deadline")
    parser.add_argument("--no-children", action="store_true", help="watch only the PID itself")
    parser.add_argument("--log", help="write JSON lines of every sample to this file")
    args = parser.parse_args(argv)
    try:
        config = Config(
            limit_bytes=int(args.limit_gib * GIB),
            interval_s=args.interval_ms / 1000.0,
            deadline_s=args.deadline_s,
        ).validate()
        _refuse_pid(args.pid)
        sampler = DarwinSampler(include_children=not args.no_children)
    except Refused as exc:
        print(f"footprint_guard: refused: {exc}", file=sys.stderr)
        return EXIT_REFUSED

    log_file = open(args.log, "a", buffering=1) if args.log else None
    log = (lambda row: log_file.write(json.dumps(row) + "\n")) if log_file else None
    try:
        result = watch(args.pid, config, sampler, lambda pid: os.kill(pid, signal.SIGKILL), log=log)
    finally:
        if log_file:
            log_file.close()
    summary = {
        "verdict": result.verdict.action,
        "reason": result.verdict.reason,
        "samples": result.samples,
        "peak_footprint_gib": round(result.peak_footprint / GIB, 3),
        "last_footprint_gib": round(result.last_footprint / GIB, 3),
        "max_sample_gap_s": round(result.max_gap_s, 4),
        "elapsed_s": round(result.elapsed_s, 3),
        "killed_pids": result.killed,
    }
    print("footprint_guard: " + json.dumps(summary), file=sys.stderr)
    return result.verdict.exit_code


if __name__ == "__main__":
    sys.exit(main())
