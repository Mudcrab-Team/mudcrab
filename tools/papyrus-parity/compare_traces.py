#!/usr/bin/env python3
"""Compare complete, tagged Skyrim traces with the current converter/helper run."""
import argparse
import hashlib
import json
from pathlib import Path
import re

CASES = ("inheritance", "nested_end", "nested_begin", "latent")
TAG = re.compile(r"(?:^|\]\s+)MCPARITY\|([^|]+)\|([^\r\n]+)")


def retail_traces(text):
    traces = {name: [] for name in CASES}
    for line in text.splitlines():
        if match := TAG.search(line):
            if match[1] not in traces:
                raise ValueError(f"Unknown tagged case {match[1]}")
            traces[match[1]].append("MCPARITY|" + match[1] + "|" + match[2])
    for name, trace in traces.items():
        prefix = f"MCPARITY|{name}|"
        if not trace or trace[0] != prefix + "begin" or trace[-1] != prefix + "done":
            raise ValueError(f"Incomplete retail trace for {name}")
        if trace.count(prefix + "begin") != 1 or trace.count(prefix + "done") != 1:
            raise ValueError(f"Multiple executions mixed in retail trace for {name}")
    return traces


def compare(text, luau, deployed):
    traces = retail_traces(text)
    if deployed["pex_sha256"] != luau["pex_sha256"]:
        raise ValueError("Skyrim and Luau did not use identical PEX files")
    cases = {}
    for name in CASES:
        actual = luau["cases"][name]
        expected = traces[name]
        difference = next((i for i in range(max(len(expected), len(actual["trace"])))
                           if expected[i:i + 1] != actual["trace"][i:i + 1]), None)
        error = actual["error"]
        status = "match" if difference is None and not error and actual["finished"] else "mismatch"
        if error and "unregistered Papyrus native: Utility::Wait" in error:
            status = "unsupported_native"
        cases[name] = {"status": status, "retail": expected, "luau": actual["trace"],
                       "first_difference": difference,
                       "error": error.splitlines()[0] if error else None}
    adapter = luau["probe_only_wait_adapter"]
    return {"status": "match" if all(c["status"] == "match" for c in cases.values()) else "differences",
            "scope": "Per-case event order, not relative timing or cross-quest scheduling",
            "game_exe_sha256": deployed["game_exe_sha256"],
            "plugin_sha256": deployed["plugin_sha256"], "pex_sha256": deployed["pex_sha256"],
            "helper_sha256": luau["helper_sha256"], "cases": cases,
            "probe_only_wait_adapter_matches": adapter["trace"] == traces["latent"]
                and not adapter["error"] and adapter["finished"]
                and adapter.get("yields") == [{"kind": "Wait", "seconds": 0.5,
                    "trace_before_resume": ["MCPARITY|latent|begin", "MCPARITY|latent|outer.before",
                                            "MCPARITY|latent|inner.before"]}]}


def verify_deployment(manifest, inputs):
    game = manifest.parent / "game"
    files = {game / "SkyrimSE.exe": inputs["game_exe_sha256"],
             game / "Data/MCParity.esp": inputs["plugin_sha256"]}
    for name, digest in inputs["pex_sha256"].items():
        if not re.fullmatch(r'MCParity[A-Za-z]+\.pex', name):
            raise ValueError("Unexpected deployed script name")
        files[game / "Data/Scripts" / name] = digest
    for filename, digest in files.items():
        if hashlib.sha256(filename.read_bytes()).hexdigest() != digest:
            raise ValueError(f"Deployed input changed: {filename.name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--retail", required=True, type=Path)
    parser.add_argument("--luau", required=True, type=Path)
    parser.add_argument("--deployed", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    raw = None
    try:
        raw = args.retail.read_bytes()
        inputs = json.loads(args.deployed.read_text())
        verify_deployment(args.deployed, inputs)
        result = compare(raw.decode("utf-8-sig"), json.loads(args.luau.read_text()), inputs)
    except (OSError, UnicodeError, ValueError, KeyError) as error:
        result = {"status": "incomplete", "error": str(error)}
    result["raw_log_sha256"] = hashlib.sha256(raw).hexdigest() if raw is not None else None
    try:
        with args.output.open("x", encoding="utf-8") as stream:
            stream.write(json.dumps(result, indent=2) + "\n")
    except OSError as error:
        parser.exit(2, f"Cannot create comparison output: {error}\n")
    print(json.dumps({"status": result["status"], "cases":
        {k: v["status"] for k, v in result.get("cases", {}).items()}}, indent=2))
    raise SystemExit(0 if result["status"] == "match" else 2 if result["status"] == "incomplete" else 1)
