#!/usr/bin/env python3
"""Build the offline adapter from an existing, clean pinned checkout; no network."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

PIN = "9e1f07b159d3cb777f1c67ed31fc11fd117986f4"


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--meshoptimizer", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="new binary path outside the upstream checkout")
    parser.add_argument("--cxx", default="c++")
    args = parser.parse_args()
    source = args.meshoptimizer.resolve(strict=True)
    output = args.output.resolve()
    if output.is_relative_to(source):
        parser.error("build output must be outside the pinned checkout")
    if any(Path(str(output) + suffix).exists() for suffix in ("", ".build.json", ".license.txt")):
        parser.error("output or its receipt/license already exists")
    commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    dirty = subprocess.check_output(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=all"], text=True)
    if commit != PIN or dirty:
        parser.error(f"require clean meshoptimizer v1.3 checkout at {PIN}")
    cpp = Path(__file__).with_name("builder.cpp").resolve()
    cpp_bytes = cpp.read_bytes()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="lod-builder-", dir=output.parent) as temporary:
        staged = Path(temporary) / "builder"
        snapshot = Path(temporary) / "source"
        snapshot.mkdir()
        # Read Git objects at the pin, not mutable checkout files during build.
        archive = subprocess.check_output(["git", "-C", str(source), "archive", PIN, "src", "demo/clusterlod.h", "LICENSE.md"])
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            for member in tar.getmembers():
                target = snapshot / member.name
                if not target.resolve().is_relative_to(snapshot) or not (member.isdir() or member.isfile()):
                    parser.error("unsupported upstream archive entry")
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True)
                else:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(tar.extractfile(member).read())
        staged_cpp = Path(temporary) / "builder.cpp"
        staged_cpp.write_bytes(cpp_bytes)
        sources = sorted((snapshot / "src").glob("*.cpp"))
        flags = ["-std=c++17", "-O2", "-DNDEBUG", "-Wall", "-Wextra"]
        command = [args.cxx, *flags, "-I", str(snapshot / "src"), "-I", str(snapshot / "demo"),
                   str(staged_cpp), *map(str, sources), "-o", str(staged)]
        subprocess.run(command, check=True)
        receipt = {
            "meshoptimizer_version": "1.3", "meshoptimizer_commit": commit,
            "builder_source_sha256": hashlib.sha256(cpp_bytes).hexdigest(), "builder_sha256": sha256(staged),
            "clusterlod_header_sha256": sha256(snapshot / "demo/clusterlod.h"),
            "compiler": subprocess.check_output([args.cxx, "--version"], text=True).splitlines()[0],
            "flags": flags,
        }
        # The compiler driver processes translation units sequentially; no -j build.
        shutil.move(str(staged), output)
        Path(str(output) + ".build.json").write_text(json.dumps(receipt, sort_keys=True, indent=2) + "\n")
        shutil.copyfile(snapshot / "LICENSE.md", Path(str(output) + ".license.txt"))
    print(output)


if __name__ == "__main__":
    main()
