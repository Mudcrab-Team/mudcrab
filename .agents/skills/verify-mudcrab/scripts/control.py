#!/usr/bin/env python3
"""Drive production Mudcrab binaries with disposable data and retained evidence."""

import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[4]
BINARIES = ("dummy-content", "converter", "engine", "launcher")
BUILD = ROOT / "target/verification/mudcrab-build.json"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def read_json(path):
    return json.loads(path.read_text())


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def tree(path):
    return {str(p.relative_to(path)): sha(p) for p in sorted(path.rglob("*")) if p.is_file()}


def source_hash():
    paths = [ROOT / name for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "devenv.nix", "devenv.lock")]
    for directory in ("crates", "assets", ".cargo"):
        paths.extend(p for p in (ROOT / directory).rglob("*") if p.is_file())
    digest = hashlib.sha256()
    for path in sorted(set(paths)):
        if path.exists():
            digest.update(str(path.relative_to(ROOT)).encode() + b"\0" + sha(path).encode())
    return digest.hexdigest()


def identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
        return {"pid": pid, "start_ticks": fields[19], "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip()}
    except FileNotFoundError:
        return None


def stop_owned(owner):
    if owner and identity(owner["pid"]) == owner:
        require(os.getpgid(owner["pid"]) == owner["pid"], "Recorded child no longer owns its process group")
        os.killpg(owner["pid"], signal.SIGTERM)
        for _ in range(30):
            if identity(owner["pid"]) != owner:
                return
            time.sleep(0.1)
        if identity(owner["pid"]) == owner:
            os.killpg(owner["pid"], signal.SIGKILL)


def state_at(evidence):
    state = read_json(evidence / "session.json")
    require(state["repo"] == str(ROOT), "Session belongs to another checkout")
    require(state["evidence"] == str(evidence), "Evidence directory does not match session")
    return state


def save_state(evidence, state):
    write_json(evidence / "session.json", state)


def scratch_at(state):
    scratch = Path(state["scratch"])
    require(scratch.name.startswith("mudcrab-verify-"), "Scratch directory has no verification prefix")
    require(scratch.parent == Path(tempfile.gettempdir()).resolve(), "Scratch is outside the temporary directory")
    require(not scratch.is_symlink(), "Scratch directory is a symlink")
    require((scratch / ".owner").read_text() == state["token"], "Scratch ownership marker does not match")
    return scratch


def run(argv, cwd, evidence, label, state=None, timeout=300, forward_input=False, interrupt_on=None):
    """A fresh PTY per command; retain its exact argv, transcript, and exit code."""
    transcript = evidence / f"{label}.terminal.txt"
    require(not transcript.exists(), f"Refusing to overwrite {transcript}")
    env = os.environ.copy()
    if state:
        scratch = scratch_at(state)
        for variable, folder in (("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache"), ("XDG_RUNTIME_DIR", "runtime")):
            env[variable] = str(scratch / folder)
    master, slave = pty.openpty()
    child = None
    record = {"argv": list(map(str, argv)), "cwd": str(cwd), "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "timeout_seconds": timeout}
    deadline = time.monotonic() + timeout
    observed = bytearray()
    try:
        child = subprocess.Popen(record["argv"], cwd=cwd, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        slave = None
        record["owner"] = identity(child.pid)
        if state is not None:
            state["active"] = record["owner"]
            save_state(evidence, state)
        write_json(evidence / f"{label}.command.json", record)
        with transcript.open("wb") as log:
            while True:
                require(time.monotonic() < deadline, f"{label} exceeded {timeout}s")
                readers = [master]
                if forward_input:
                    readers.append(sys.stdin.fileno())
                ready, _, _ = select.select(readers, [], [], 0.2)
                if forward_input and sys.stdin.fileno() in ready:
                    data = os.read(sys.stdin.fileno(), 4096)
                    if data:
                        os.write(master, data)
                        with (evidence / f"{label}.input.jsonl").open("a") as inputs:
                            inputs.write(json.dumps({"elapsed_seconds": timeout - (deadline - time.monotonic()), "text": data.decode(errors="replace")}) + "\n")
                    else:
                        forward_input = False
                if master in ready:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        data = b""
                    if not data:
                        break
                    log.write(data)
                    log.flush()
                    if interrupt_on is not None and "interrupt" not in record:
                        observed.extend(data)
                        if interrupt_on in observed:
                            require(identity(child.pid) == record["owner"], "Converter identity changed before SIGINT")
                            require(os.getpgid(child.pid) == child.pid, "Converter no longer owns its process group")
                            record["interrupt"] = {"signal": "SIGINT", "trigger": interrupt_on.decode(), "elapsed_seconds": timeout - (deadline - time.monotonic())}
                            write_json(evidence / f"{label}.command.json", record)
                            os.killpg(child.pid, signal.SIGINT)
                    if forward_input:
                        sys.stdout.buffer.write(data)
                        sys.stdout.buffer.flush()
                elif child.poll() is not None:
                    break
        record["exit_code"] = child.wait(timeout=5)
        return record["exit_code"]
    except BaseException as error:
        record["error"] = str(error)
        raise
    finally:
        if child is not None:
            if child.poll() is None:
                stop_owned(record.get("owner"))
                child.wait(timeout=10)
            record["exit_code"] = child.returncode
        os.close(master)
        if slave is not None:
            os.close(slave)
        write_json(evidence / f"{label}.command.json", record)
        if state is not None:
            state["active"] = None
            save_state(evidence, state)


def build():
    BUILD.parent.mkdir(parents=True, exist_ok=True)
    stamp = str(time.time_ns())
    before = source_hash()
    argv = ["cargo", "build", "--release"]
    for binary in BINARIES:
        argv.extend(("--bin", binary))
    require(run(argv, ROOT, BUILD.parent, f"build-{stamp}", timeout=1800) == 0, "Release build failed; inspect target/verification")
    require(before == source_hash(), "Sources changed during build; rebuild")
    write_json(BUILD, {"source_sha256": before, "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "binaries": {name: sha(ROOT / "target/release" / name) for name in BINARIES}, "command": f"build-{stamp}.command.json"})
    print(BUILD)


def doctor(evidence):
    state = state_at(evidence)
    scratch = scratch_at(state)
    receipt = read_json(BUILD)
    require(receipt == state["build"], "Build receipt changed since prepare; start a new session")
    require(receipt["source_sha256"] == source_hash(), "Build inputs changed; run build and prepare a new session")
    for name, expected in receipt["binaries"].items():
        require(sha(ROOT / "target/release" / name) == expected, f"{name} binary changed; rebuild")
    require(not state.get("active"), "Session has an active or interrupted child; finish or cleanup first")
    require(sha(scratch / "Data/Skyrim.esm") == state["plugin_sha256"], "Fixture plugin changed")
    require(tree(scratch / "Data") == state["fixture_files"], "Fixture files changed")
    return {"healthy": True, "repo": str(ROOT), "scratch": str(scratch), "worldspace": 1, "source_sha256": receipt["source_sha256"], "fixture_files": len(state["fixture_files"])}


def prepare(evidence):
    receipt = read_json(BUILD)
    require(receipt["source_sha256"] == source_hash(), "Build first; build inputs changed")
    scratch = Path(tempfile.mkdtemp(prefix="mudcrab-verify-")).resolve()
    token = uuid.uuid4().hex
    (scratch / ".owner").write_text(token)
    for folder in ("config", "cache", "runtime"):
        (scratch / folder).mkdir(mode=0o700)
    state = {"repo": str(ROOT), "evidence": str(evidence), "scratch": str(scratch), "token": token, "build": receipt, "active": None}
    save_state(evidence, state)
    require(run([ROOT / "target/release/dummy-content", "gen", scratch / "Data", "--seed", "20261114", "--formats", "dds,pex,nif,esm", "--with-interior"], scratch, evidence, "seed", state) == 0, "Fixture generator failed")
    state["plugin_sha256"] = sha(scratch / "Data/Skyrim.esm")
    state["fixture_files"] = tree(scratch / "Data")
    save_state(evidence, state)
    write_json(evidence / "fixture.json", state["fixture_files"])
    write_json(evidence / "build.json", receipt)
    print(json.dumps(doctor(evidence)))


def convert(evidence):
    write_json(evidence / "doctor.json", doctor(evidence))
    state = state_at(evidence)
    scratch = scratch_at(state)
    output = scratch / "modern_assets"
    require(not output.exists(), "Fresh conversion requires an empty output")
    require(run([ROOT / "target/release/converter", scratch / "Data", output, "--report-json", evidence / "conversion.json", "--texture-encoder", "cpu", "--lod-encoder", "cpu", "--cpu-jobs", "2", "--io-jobs", "2"], scratch, evidence, "convert", state, timeout=600) == 0, "Converter failed")
    report = read_json(evidence / "conversion.json")
    manifest = read_json(output / "conversion-manifest.json")
    integration = read_json(output / "integration-report.json")
    require(report["complete"] is True and report["converted"] > 0, "Conversion did not complete with converted assets")
    require(manifest["complete"] is True and not manifest["failures"], "Published manifest is incomplete")
    require(integration["passed"] is True, "Published integration validation failed")
    for name in ("conversion-manifest.json", "integration-report.json"):
        shutil.copy2(output / name, evidence / name)
    with sqlite3.connect((output / "skyrim_world.db").as_uri() + "?mode=ro", uri=True) as db:
        queries = {
            "integrity": "PRAGMA integrity_check",
            "schema": "SELECT version FROM schema_info",
            "worldspaces": "SELECT id FROM worldspaces ORDER BY id",
            "cells": "SELECT id,worldspace_id,grid_x,grid_y,interior_name FROM cells ORDER BY id",
            "references": 'SELECT count(*) FROM "references"',
            "doors": "SELECT ref_id,destination_ref_id,destination_cell_id FROM door_links ORDER BY ref_id",
            "reciprocal_doors": "SELECT count(*) FROM door_links a JOIN door_links b ON a.destination_ref_id=b.ref_id AND b.destination_ref_id=a.ref_id",
        }
        observations = {key: {"sql": sql, "rows": db.execute(sql).fetchall()} for key, sql in queries.items()}
    write_json(evidence / "database.json", observations)
    require(observations["integrity"]["rows"] == [("ok",)], "SQLite integrity failed")
    require((1,) in observations["worldspaces"]["rows"], "Generated worldspace missing")
    require(any(row[4] == "GeneratedInterior" for row in observations["cells"]["rows"]), "Generated interior missing")
    require(observations["references"]["rows"][0][0] > 0, "No placed references")
    require(observations["reciprocal_doors"]["rows"][0][0] == 2, "Fixture must retain both reciprocal doors")
    write_json(evidence / "pack-files.json", tree(output))
    print("Conversion published a complete pack with the generated world, interior, and two linked doors")


def check(evidence, full):
    doctor(evidence)
    state = state_at(evidence)
    scratch = scratch_at(state)
    output = scratch / "modern_assets"
    require(output.is_dir(), "Convert before checking")
    before = tree(output)
    label = "check-full" if full else "check-quick"
    if (evidence / f"{label}.terminal.txt").exists():
        label += f"-{time.time_ns()}"
    argv = [ROOT / "target/release/converter", "check", output]
    if full:
        argv.append("--full")
    code = run(argv, scratch, evidence, label, state)
    after = tree(output)
    write_json(evidence / f"{label}.observation.json", {"exit_code": code, "before": before, "after": after, "content_unchanged": before == after})
    require(code == 0 and before == after, "Integrity check failed or changed pack contents")
    require("All good:" in (evidence / f"{label}.terminal.txt").read_text(), "Check did not report success")
    print(f"{label}: passed; pack contents unchanged")


def integrity_negative(evidence):
    """Check the documented quick/full difference on a disposable pack copy."""
    state = state_at(evidence)
    scratch = scratch_at(state)
    original = scratch / "modern_assets"
    before = tree(original)
    damaged = scratch / "damaged_assets"
    shutil.copytree(original, damaged)
    manifest = read_json(damaged / "conversion-manifest.json")
    candidates = sorted({entry["output"] for entry in manifest["entries"].values() if entry.get("output")})
    relative = next(name for name in candidates if (damaged / name).is_file() and (damaged / name).stat().st_size > 0)
    artifact = damaged / relative
    original_sha = sha(artifact)
    size = artifact.stat().st_size
    with artifact.open("r+b") as stream:
        first = stream.read(1)
        stream.seek(0)
        stream.write(bytes([first[0] ^ 1]))
    require(artifact.stat().st_size == size and sha(artifact) != original_sha, "Corruption must change bytes without changing size")
    damaged_before = tree(damaged)
    quick = run([ROOT / "target/release/converter", "check", damaged], scratch, evidence, "damaged-quick", state)
    full = run([ROOT / "target/release/converter", "check", damaged, "--full"], scratch, evidence, "damaged-full", state)
    observed = {"artifact": relative, "size": size, "original_sha256": original_sha, "damaged_sha256": sha(artifact), "quick_exit": quick, "full_exit": full, "checks_preserved_damaged_pack": damaged_before == tree(damaged), "original_pack_unchanged": before == tree(original)}
    write_json(evidence / "integrity-negative.json", observed)
    require(quick == 0 and full == 1, "Expected quick check to accept same-size corruption and full check to reject it")
    require(observed["original_pack_unchanged"] and observed["checks_preserved_damaged_pack"], "Integrity checks changed pack contents")
    print("Full check rejected same-size corruption that quick check accepted; original pack unchanged")


def stop_resume(evidence):
    """Interrupt the real CLI, retain staging, and execute its printed resume command."""
    convert(evidence)
    write_json(evidence / "doctor-before-stop.json", doctor(evidence))
    state = state_at(evidence)
    scratch = scratch_at(state)
    output = scratch / "modern_assets"
    before = tree(output)
    argv = [ROOT / "target/release/converter", scratch / "Data", output,
            "--report-json", evidence / "stopped.json", "--invalidate-cache",
            "--texture-encoder", "cpu", "--lod-encoder", "cpu", "--cpu-jobs", "1", "--io-jobs", "1"]
    code = run(argv, scratch, evidence, "stop", state, timeout=600, interrupt_on=b"Textures")
    observation = {"stop_exit": code, "published_pack_unchanged": before == tree(output)}
    write_json(evidence / "stop-resume.json", observation)
    require(code == 130, "Conversion finished before graceful interruption; use a larger fixture")
    require(observation["published_pack_unchanged"], "Stopping changed the published pack")
    require(read_json(evidence / "stopped.json")["complete"] is False, "Stopped report claims completion")
    transcript = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", (evidence / "stop.terminal.txt").read_text())
    command = next((line.strip() for line in transcript.splitlines() if " --resume-staging " in line), None)
    require(command is not None, "Converter did not print a resume command")
    resume = shlex.split(command)
    require(Path(resume[0]).name == "converter", "Unexpected executable in resume command")
    staging = Path(resume[resume.index("--resume-staging") + 1])
    require(staging.parent == scratch and staging.name.startswith("modern_assets.staging-") and staging.is_dir(), "Resume staging is not inside owned scratch")
    observation.update({"printed_resume": command, "staging": str(staging), "staging_retained": True})
    write_json(evidence / "stop-resume.json", observation)
    write_json(evidence / "doctor-after-stop.json", doctor(evidence))
    resume[0] = str(ROOT / "target/release/converter")
    resume.extend(("--report-json", str(evidence / "resumed.json")))
    require(run(resume, scratch, evidence, "resume", state, timeout=600) == 0, "Resume failed")
    require(read_json(evidence / "resumed.json")["complete"] is True, "Resumed conversion is incomplete")
    manifest = read_json(output / "conversion-manifest.json")
    require(manifest["complete"] is True and not manifest["failures"], "Resumed manifest is incomplete")
    require(read_json(output / "integration-report.json")["passed"] is True, "Resumed integration check failed")
    check(evidence, True)
    observation["resume_complete"] = True
    observation["staging_removed_after_publication"] = not staging.exists()
    require(observation["staging_removed_after_publication"], "Published staging still exists")
    write_json(evidence / "stop-resume.json", observation)
    print("Graceful stop preserved the published pack; resume published a complete, fully checked replacement")


def cleanup(evidence):
    state = state_at(evidence)
    stop_owned(state.get("active"))
    scratch = Path(state["scratch"])
    if scratch.exists():
        scratch = scratch_at(state)
        require(not evidence.is_relative_to(scratch), "Evidence is inside scratch")
        shutil.rmtree(scratch)
    state["active"] = None
    state["cleaned"] = True
    save_state(evidence, state)
    remaining = sorted(p.name for p in evidence.iterdir() if p.is_file())
    write_json(evidence / "cleanup.json", {"scratch_removed": not scratch.exists(), "evidence_retained": remaining})
    print(f"Removed owned scratch; evidence retained at {evidence}")


def window(evidence, binary, extra, timeout):
    doctor(evidence)
    state = state_at(evidence)
    scratch = scratch_at(state)
    require(os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"), "Native display required; use a desktop session or xvfb-run")
    argv = [ROOT / "target/release" / binary]
    if binary == "engine":
        require((scratch / "modern_assets/skyrim_world.db").is_file(), "Convert the fixture first")
        argv.extend(("--assets", scratch / "modern_assets", "--worldspace", "1", "--grid-x", "0", "--grid-y", "0", "--stream-radius", "0", "--stdin-console", "--bug-report-dir", evidence / "bug-reports"))
    argv.extend(extra)
    code = run(argv, scratch, evidence, f"{binary}-{time.time_ns()}", state, timeout=timeout, forward_input=True)
    require(code == 0, f"{binary} exited with {code}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("build")
    for name in ("prepare", "doctor", "convert", "check", "window", "cleanup", "prove", "stop-resume"):
        item = sub.add_parser(name)
        item.add_argument("--evidence", type=Path, required=True)
        if name == "check":
            item.add_argument("--full", action="store_true")
        if name == "window":
            item.add_argument("--binary", choices=("engine", "launcher"), default="engine")
            item.add_argument("--timeout", type=int, default=180)
            item.add_argument("extra", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command == "build":
        build()
        return
    evidence = args.evidence.resolve()
    lock = None
    owns_lock = False
    if args.command in ("prepare", "prove", "stop-resume"):
        # Atomic creation refuses an existing session before entering failure cleanup.
        evidence.mkdir(parents=True, exist_ok=False)
    try:
        if args.command == "doctor":
            print(json.dumps(doctor(evidence)))
            return
        lock = (evidence / "control.lock").open("a")
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        owns_lock = True
        if args.command in ("prepare", "prove", "stop-resume"):
            prepare(evidence)
        if args.command == "convert":
            convert(evidence)
        elif args.command == "check":
            check(evidence, args.full)
        elif args.command == "window":
            extra = args.extra[1:] if args.extra[:1] == ["--"] else args.extra
            window(evidence, args.binary, extra, args.timeout)
        elif args.command == "cleanup":
            cleanup(evidence)
        elif args.command == "stop-resume":
            try:
                stop_resume(evidence)
            finally:
                cleanup(evidence)
            require((evidence / "stop-resume.json").is_file() and (evidence / "resume.terminal.txt").is_file(), "Cleanup lost stop/resume evidence")
        elif args.command == "prove":
            try:
                convert(evidence)
                check(evidence, False)
                check(evidence, True)
                integrity_negative(evidence)
                write_json(evidence / "result.json", {"passed": True, "feature": "convert-and-check", "entrypoint": "production CLI through PTY", "unverified": ["launcher controls", "MO2 input", "GPU encoders", "retail assets", "engine rendering", "stop and resume", "physical input", "settings", "bug reports"]})
            finally:
                cleanup(evidence)
            require((evidence / "conversion.json").is_file() and (evidence / "check-full.terminal.txt").is_file(), "Cleanup lost evidence")
    except BaseException as error:
        if owns_lock and (evidence / "session.json").exists():
            write_json(evidence / "failure.json", {"error": str(error), "command": args.command})
            cleanup(evidence)
        raise
    finally:
        if lock is not None:
            lock.close()


if __name__ == "__main__":
    main()
