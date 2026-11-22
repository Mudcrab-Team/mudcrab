"""Exhaust process-crash prefixes of the real workspace cleanup on small directory trees."""
import argparse
from contextlib import ExitStack
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[3]
WORKSPACE = "scripts/workspace.py"


def load(path):
    spec = importlib.util.spec_from_file_location("workspace", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Crash(BaseException):
    """A process stop bypasses the production handlers for recoverable OS errors."""


def run_prefix(operation, cut):
    """Cut 0 stops before the call. Cut N stops just after successful mutation N."""
    steps = []

    def interruptible(name):
        original = getattr(os, name)

        def call(path, *args, **kwargs):
            result = original(path, *args, **kwargs)
            steps.append(f"{name} {Path(path).name}")
            if len(steps) == cut:
                raise Crash
            return result
        return call

    if cut != 0:
        with ExitStack() as stack:
            for name in ("replace", "rename", "unlink", "rmdir"):
                stack.enter_context(patch.object(os, name, interruptible(name)))
            try:
                operation()
            except Crash:
                pass
    if cut is not None and len(steps) != cut:
        raise AssertionError(f"scheduled cut {cut} was never reached: {steps}")
    return steps


class ForeignDataLost(AssertionError):
    pass


def schedule(module, repo, kind, cleanup_cut, recovery_cut):
    with tempfile.TemporaryDirectory() as temp:
        ws = module.Workspace(repo, Path(temp) / "storage")
        ws.setup()
        builds = ws.root / "builds"
        slot = builds / "slot-0"
        slot.mkdir()
        module.write_json(slot / "build.json", {"version": 1, "last_used": 0})
        if kind == "retire":
            sources = [slot]
            foreign = [builds / ".retiring-slot-0-1"]

            def cleanup():
                ws.retire(slot, "manual", allow_slot=True)
        else:
            profiles = [slot / "target" / p for p in ("debug", "release")]
            sources = [p / "incremental" for p in profiles]
            foreign = [p / ".trim-incremental-1" for p in profiles]

            def cleanup():
                ws.trim_slot_incremental(slot, leased=False)

        for source in sources:
            source.mkdir(parents=True, exist_ok=True)
            (source / "payload").write_bytes(b"old output")

        def recover():
            module.Workspace(repo, ws.root).recover_interrupted()

        with patch.object(module.Workspace, "consumer_hold", return_value=None), \
                patch.dict(os.environ, {"MUDCRAB_SLOT_INCREMENTAL_GIB": "0.000000001"}):
            prepare = run_prefix(cleanup, cleanup_cut)
            if kind == "retire":
                partial = any(not (path / "build.json").exists() for path in builds.glob(".retiring-*"))
            else:
                partial = sum(source.exists() for source in sources) == 1
            expected = {}
            for source in sources:
                payload = source / "payload"
                if source.exists():
                    assert cleanup_cut is not None, f"{kind} failed to remove {source}"
                    assert payload.read_bytes() == b"old output", f"damaged live build at {prepare}"
                    expected[payload] = b"old output"
                else:
                    source.mkdir(parents=True)
                    payload.write_bytes(b"new output")
                    expected[payload] = b"new output"

            # Give another profile the real trim's stamp. Only the renamed profiles qualify.
            if kind == "trim":
                for receipt in (ws.root / "evidence").glob("trimmed-slot-0-*.json"):
                    stamp = receipt.stem[len("trimmed-slot-0-"):]
                    foreign.append(slot / "target" / "other" / f".trim-incremental-{stamp}")
            for path in foreign:
                path.mkdir(parents=True)
                (path / "personal").write_bytes(b"keep")

            retry = run_prefix(recover, recovery_cut)
            for _ in range(2):
                recover()
                for path in foreign:
                    personal = path / "personal"
                    if not personal.is_file() or personal.read_bytes() != b"keep":
                        raise ForeignDataLost(f"{kind}, cleanup {prepare}, recovery {retry}: lost {path.relative_to(builds)}")
                for payload, contents in expected.items():
                    assert payload.read_bytes() == contents, f"live output changed after {prepare}, {retry}"
                remaining = {*builds.glob(".retiring-*"), *slot.glob("target/*/.trim-incremental-*")}
                assert remaining == set(foreign), f"unfinished removal after {prepare}, {retry}: {remaining - set(foreign)}"
        return len(prepare), len(retry), partial


def verify(module, repo):
    for kind in ("retire", "trim"):
        count = 1
        saw_partial = False
        cleanup_steps, _, _ = schedule(module, repo, kind, None, None)
        for first in range(cleanup_steps + 1):
            _, recovery_steps, partial = schedule(module, repo, kind, first, None)
            saw_partial |= partial
            count += 1
            for second in range(recovery_steps + 1):
                schedule(module, repo, kind, first, second)
                count += 1
        assert saw_partial, f"{kind} never reached its partial removal state"
        print(f"PASS {kind}: {count} schedules")


def check_history(repo):
    # Each old version must delete the personal file. An import error or a held build fails CI.
    for revision, cut in (("2bddabdabfde22c0db0c19bd1cfae5184a5738ff", 0),
                          ("8eb68b1539e2fff4aa31ab81f60eedcd8b9988aa", None)):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "workspace.py"
            path.write_bytes(subprocess.check_output(["git", "show", f"{revision}:{WORKSPACE}"], cwd=ROOT))
            module = load(path)
            for kind in ("retire", "trim"):
                try:
                    schedule(module, repo, kind, cut, None)
                except ForeignDataLost as failure:
                    print(f"EXPECTED FAILURE {revision[:8]}: {failure}")
                else:
                    raise AssertionError(f"{revision[:8]} passed the historical {kind} failure")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--history", action="store_true", help="also require the #797 and #800 counterexamples")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        repo = Path(directory)
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        if args.history:
            check_history(repo)
        verify(load(ROOT / WORKSPACE), repo)
