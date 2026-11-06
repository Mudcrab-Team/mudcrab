#!/usr/bin/env python3
"""Seal a managed test build, or supervise one separately admitted MODS Metal fixture.

No command builds, converts assets or launches native rendering. `check` inspects
source/receipts. The legacy `run` command always refuses; native execution needs a
separately frozen and admitted controller.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import signal
import shutil
import subprocess
import sys
import time

TEST = "object_materials::native_tests::object_diffuse_two_instance_readback"
GIB = 1024**3
SECONDS = 120
RSS_MIB = 4096
RESERVE_GIB = 20
HOST_GIB = 12


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def save_new(path, data):
    with path.open("x") as output:
        json.dump(data, output, indent=2)
        output.write("\n")


def command(argv):
    return subprocess.check_output(argv, text=True, timeout=5)


def below(path, parent):
    return path != parent and path.is_relative_to(parent)


def verify_source(packet_path):
    packet_path = packet_path.resolve(strict=True)
    packet = json.loads(packet_path.read_text())
    repo = Path(packet["checkout"]).resolve(strict=True)
    manifest_path = packet_path.parent / "source-manifest.json"
    require(sha(manifest_path) == packet["source_manifest_sha256"], "source manifest changed")
    require(sha(packet_path.parent / "delta.patch") == packet["delta_patch_sha256"], "R1 delta changed")
    manifest = json.loads(manifest_path.read_text())
    require(command(["git", "-C", str(repo), "rev-parse", "HEAD"]).strip() == packet["head"], "source head changed")
    paths = set(command(["git", "-C", str(repo), "diff", "--name-only", "HEAD"]).splitlines())
    paths.update(command(["git", "-C", str(repo), "ls-files", "--others", "--exclude-standard"]).splitlines())
    require(paths == set(manifest), "dirty/untracked path set changed")
    for name, expected in manifest.items():
        relative = Path(name)
        require(not relative.is_absolute() and ".." not in relative.parts, "unsafe source path")
        source = repo / relative
        require(not source.is_symlink() and below(source.resolve(strict=True), repo), "source escapes checkout")
        require(sha(source) == expected, f"source changed: {name}")
    return packet, repo


def verify_binary(receipt_path, packet_path, packet):
    receipt = json.loads(receipt_path.read_text())
    require(receipt["packet_sha256"] == sha(packet_path), "binary belongs to another frozen packet")
    binary = Path(receipt["binary"]).resolve(strict=True)
    build = Path(packet["build_directory"]).resolve(strict=True)
    require(below(binary, build / "target"), "binary outside admitted build")
    require(binary.is_file() and os.access(binary, os.X_OK), "test binary is not executable")
    require(sha(binary) == receipt["binary_sha256"], "binary checksum changed")
    marker = json.loads((build / "build.json").read_text())
    require(marker["checkout"] == packet["checkout"] and marker["commit"] == packet["head"], "build owner/source changed")
    return binary, build


def seal(args):
    packet, _ = verify_source(args.packet)
    require(args.cargo_log.stat().st_mtime >= packet["frozen_unix"], "Cargo log predates freeze")
    messages = []
    for line in args.cargo_log.read_text().splitlines():
        try:
            messages.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    require(any(m.get("reason") == "build-finished" and m.get("success") is True for m in messages), "no successful build-finished receipt")
    candidates = {m["executable"] for m in messages if m.get("reason") == "compiler-artifact"
                  and m.get("target", {}).get("name") == "engine"
                  and "lib" in m.get("target", {}).get("kind", [])
                  and m.get("profile", {}).get("test") and m.get("executable")}
    require(len(candidates) == 1, "expected exactly one engine library test executable")
    binary = Path(candidates.pop()).resolve(strict=True)
    require(below(binary, Path(packet["build_directory"]).resolve() / "target"), "unexpected build output path")
    require(binary.stat().st_mtime >= packet["frozen_unix"], "test executable predates new fixture source")
    save_new(args.receipt, {"packet_sha256": sha(args.packet), "binary": str(binary),
                           "binary_sha256": sha(binary), "cargo_log_sha256": sha(args.cargo_log),
                           "test": TEST, "sealed_unix": time.time()})
    verify_binary(args.receipt, args.packet, packet)
    print(args.receipt)


def preserve(args):
    """Retain the six R0 receipt binaries before an admitted rebuild can replace them."""
    packet, _ = verify_source(args.packet)
    build = Path(packet["build_directory"]).resolve(strict=True)
    require(Path(os.environ.get("MUDCRAB_BUILD_LEASE", "")).resolve() == build,
            "preservation requires the matching workspace.py exec lease")
    prior_path = Path(packet["r0_receipt"])
    require(sha(prior_path) == packet["r0_receipt_sha256"], "R0 receipt changed")
    prior = json.loads(prior_path.read_text())
    destination = Path(packet["retained_binary_directory"])
    require(not destination.is_symlink() and destination.parent.resolve() == args.packet.resolve().parent,
            "retain R0 binaries only beside this task's frozen packet")
    destination.mkdir(exist_ok=True)
    retained = {}
    for name, data in prior["binaries"].items():
        source = Path(name).resolve(strict=True)
        require(below(source, build / "target"), "R0 binary is outside the owned build")
        target = destination / source.name
        if not target.exists():
            require(sha(source) == data["sha256"], "R0 binary changed before preservation")
            with source.open("rb") as reader, target.open("xb") as writer:
                shutil.copyfileobj(reader, writer, 1024 * 1024)
            target.chmod(source.stat().st_mode & 0o777)
        require(not target.is_symlink() and sha(target) == data["sha256"], "retained R0 binary mismatch")
        retained[str(target)] = data["sha256"]
    index = destination / "receipt.json"
    if index.exists():
        require(json.loads(index.read_text()) == retained, "retained binary index changed")
    else:
        save_new(index, retained)
    print(index)


def parse_processes(text):
    rows = {}
    # Four integers, five lstart words, then the command path (which can contain spaces).
    for line in text.splitlines():
        fields = line.split(None, 9)
        require(len(fields) == 10, "cannot parse process snapshot")
        pid, ppid, pgid, rss = map(int, fields[:4])
        rows[pid] = {"ppid": ppid, "pgid": pgid, "rss_kib": rss,
                     "birth": " ".join(fields[4:9]), "command": fields[9]}
    return rows


def processes():
    return parse_processes(command(["ps", "-axo", "pid=,ppid=,pgid=,rss=,lstart=,comm="]))


def family(rows, leader, known):
    owned = {pid for pid, row in rows.items() if pid in known and known[pid] == row["birth"]}
    if not known and leader in rows:
        owned.add(leader)
    # A numeric process-group ID alone is insufficient after the leader PID is reused.
    if any(rows[pid]["pgid"] == leader for pid in owned):
        owned.update(pid for pid, row in rows.items() if row["pgid"] == leader)
    changed = True
    while changed:
        extra = {pid for pid, row in rows.items() if row["ppid"] in owned}
        changed = not extra.issubset(owned)
        owned.update(extra)
    return owned


def host():
    require(sys.platform == "darwin", "this frozen native proposal is for macOS/Metal only")
    pressure = int(command(["sysctl", "-n", "kern.memorystatus_vm_pressure_level"]).strip())
    text = command(["vm_stat"])
    page = re.search(r"page size of (\d+) bytes", text)
    require(page is not None, "vm_stat page size unavailable")
    counts = dict((name, int(value)) for name, value in re.findall(r"^([^:\n]+):\s+(\d+)\.", text, re.M))
    names = ["Pages free", "Pages inactive", "Pages speculative"]
    require(all(name in counts for name in names), "vm_stat available-page counters missing")
    available = sum(counts[name] for name in names) * int(page.group(1))
    require(pressure == 1, f"host memory pressure is not normal: {pressure}")
    require(available >= HOST_GIB * GIB, f"host available memory below {HOST_GIB} GiB: {available}")
    return {"pressure_level": pressure, "available_bytes": available,
            "available_definition": "free + inactive + speculative pages; excludes compressed and purgeable double counts"}


def cleanup(child, known):
    # Only signal the group we created and descendants whose PID/start identity still matches.
    for sig in [signal.SIGTERM, signal.SIGKILL]:
        rows = processes()
        owned = family(rows, child.pid, known)
        for pid in owned:
            known[pid] = rows[pid]["birth"]
        if any(rows[pid]["pgid"] == child.pid for pid in owned):
            try:
                os.killpg(child.pid, sig)
            except ProcessLookupError:
                pass
        for pid in owned:
            if rows[pid]["pgid"] != child.pid:
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass
        try:
            child.wait(timeout=1)
        except subprocess.TimeoutExpired:
            pass
        time.sleep(0.1)
    rows = processes()
    require(not family(rows, child.pid, known), "owned native descendants remain; keep workspace lease until resolved")


def run(_args):
    raise RuntimeError(
        "Legacy native launcher disabled. Use a separately frozen controller with "
        "exact authorization and qualified lifecycle/host evidence."
    )


def self_check():
    # Pure source/synthetic checks: no Cargo, conversion, process launch, renderer or GPU.
    text = "10 1 10 100 Sun Nov 1 00:00:00 2026 /tmp/test binary\n11 10 10 200 Sun Nov 1 00:00:01 2026 /tmp/worker\n12 11 12 300 Sun Nov 1 00:00:02 2026 /tmp/escaped\n99 1 99 900 Sun Nov 1 00:00:02 2026 /tmp/unrelated"
    rows = parse_processes(text)
    require(family(rows, 10, {}) == {10,11,12}, "descendant/group accounting failed")
    require(sum(rows[p]["rss_kib"] for p in family(rows,10,{})) == 600, "RSS aggregation failed")
    require(family({12:rows[12],99:rows[99]},10,{12:rows[12]["birth"]}) == {12}, "orphan identity tracking failed")
    require(family({99:rows[99]},10,{99:"different birth"}) == set(), "PID reuse must not retain ownership")
    require(family({10:dict(rows[10], birth="reused"), 11:rows[11]},10,
                   {10:rows[10]["birth"]}) == set(), "process-group reuse must not retain ownership")
    require(not below(Path("/tmp/pack"),Path("/tmp/pack")), "output must be a descendant")
    require(not below(Path("/tmp/pack-other/file"),Path("/tmp/pack")), "prefix escape accepted")
    print("Seven pure watchdog safety checks passed; no child process launched.")


def check_oracle_source():
    source = Path(__file__).resolve().parents[1] / "crates/engine/src/object_materials/native_tests.rs"
    text = source.read_text()
    declaration = text.split("const PROBES:", 1)[1].split("];", 1)[0]
    probes = [(int(x), int(y), tuple(v == "true" for v in (r,g,b))) for x,y,r,g,b in
              re.findall(r"\((\d+),\s*(\d+),\s*\[(true|false),\s*(true|false),\s*(true|false)\]\)", declaration)]
    require(len(probes) == 8, "native source probe declaration changed")
    def block(x,y):
        return [(True,False,False), (False,True,False), (False,False,True), (True,True,False)][(x>=4)+2*(y>=4)]
    mutants = ["identity", "offset_u_lost", "offset_v_lost", "clamp_u", "repeat_v"]
    rejected = dict.fromkeys(mutants, 0)
    source_positions_checked = 0
    for x,y,expected in probes:
        for dx in [-1,0,1]:
            for dy in [-1,0,1]:
                u,v = (x+dx+0.5-32)/192, (y+dy+0.5-32)/192
                source_positions_checked += 1
                # Check every contributing texel, including the linear-filter footprint.
                s,t = ((2*u+0.25)%1)*8-0.5, min(1,max(0,v+0.25))*8-0.5
                for i in [math.floor(s),math.ceil(s)]:
                    for j in [math.floor(t),math.ceil(t)]:
                        require(block(i%8,min(7,max(0,j))) == expected, "probe crosses a BC1 color/filter boundary")
        u,v = (x+0.5-32)/192, (y+0.5-32)/192
        variants = [(u,v), (2*u,v+0.25), (2*u+0.25,v),
                    (min(1,2*u+0.25),v+0.25), (2*u+0.25,(v+0.25)%1)]
        for name,(s,t) in zip(mutants,variants):
            s = min(7,int(s*8)) if name == "clamp_u" else int((s%1)*8)
            t = min(7,max(0,int(t*8)))
            rejected[name] += block(s,t) != expected
    require(all(rejected.values()), "native oracle fails to distinguish a sampling mutant")
    print(json.dumps({"native_source_probes":len(probes), "linear_filter_footprints_checked":source_positions_checked,
                      "mutant_probe_mismatches":rejected, "rendering_executed":False}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    check = commands.add_parser("check")
    check.add_argument("--packet", type=Path)
    preserve_parser = commands.add_parser("preserve")
    preserve_parser.add_argument("--packet", type=Path, required=True)
    seal_parser = commands.add_parser("seal")
    seal_parser.add_argument("--packet", type=Path, required=True)
    seal_parser.add_argument("--cargo-log", type=Path, required=True)
    seal_parser.add_argument("--receipt", type=Path, required=True)
    run_parser = commands.add_parser("run")
    run_parser.add_argument("--packet", type=Path, required=True)
    run_parser.add_argument("--receipt", type=Path, required=True)
    run_parser.add_argument("--output", type=Path, required=True)
    run_parser.add_argument("--native-grant", required=True)
    args = parser.parse_args()
    if args.action == "check":
        self_check()
        check_oracle_source()
        if args.packet:
            packet, _ = verify_source(args.packet)
            print(f"Frozen source verified: {packet['head']}")
    elif args.action == "seal":
        seal(args)
    elif args.action == "preserve":
        preserve(args)
    else:
        run(args)


if __name__ == "__main__":
    main()
