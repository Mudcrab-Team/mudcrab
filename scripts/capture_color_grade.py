#!/usr/bin/env python3
"""Capture and validate three color-grade modes using identical settled poses."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import struct


VISUAL_PRESET = "cool-gray"


def main():
    repository = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", required=True, type=Path)
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument(
        "--shots", type=Path,
        default=repository / "crates/engine/tests/fixtures/color-grade-riverwood-shots.json",
    )
    args = parser.parse_args()
    engine = args.engine.resolve(strict=True)
    assets = args.assets.resolve(strict=True)
    shots = args.shots.resolve(strict=True)
    output = args.output.resolve()
    # Never mix a failed or older capture set with a fresh run.
    output.mkdir(parents=True, exist_ok=False)
    poses = json.loads(shots.read_text())
    (output / "shots.json").write_text(shots.read_text())
    baseline = None
    manifest = {
        "kind": "mudcrab-color-grade-ab", "retail_parity": False,
        "engine": str(engine), "engine_sha256": hashlib.sha256(engine.read_bytes()).hexdigest(),
        "assets": str(assets), "visual_preset": VISUAL_PRESET, "shots": poses, "runs": [], "passed": False,
    }
    manifest_path = output / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    for mode in ("off", "saturation", "full"):
        directory = output / mode
        directory.mkdir()
        command = [
            str(engine), "--assets", str(assets), "--shots", str(shots),
            "--shots-out", str(directory), "--hidden-window", "--lights",
            "--stream-radius", "2", "--color-grade", mode,
            "--visual-preset", VISUAL_PRESET,
            "--shots-settle-timeout", "120",
        ]
        print(f"Capturing {mode}: {directory}", flush=True)
        with (directory / "run.log").open("w") as log:
            result = subprocess.run(command, cwd=repository, stdout=log,
                                    stderr=subprocess.STDOUT, timeout=600)
        manifest["runs"].append({"mode": mode, "command": command, "exit_code": result.returncode})
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        if result.returncode:
            raise SystemExit(f"{mode} failed ({result.returncode}); inspect {directory / 'run.log'}")
        receipts = []
        for line in (directory / "shots.log").read_text().splitlines():
            if line.startswith("{"):
                value = json.loads(line)
                if "capture" in value:
                    receipts.append(value)
        if len(receipts) != len(poses["shots"]):
            raise SystemExit(f"{mode}: missing or extra capture receipts")
        signature = []
        images = []
        for index, (receipt, pose) in enumerate(zip(receipts, poses["shots"])):
            if (receipt["capture"] != index or receipt["kind"] != "settled"
                    or receipt["timed_out"] or receipt["color_grade"] != mode
                    or receipt.get("visual_preset") != VISUAL_PRESET
                    or receipt["water_time_seconds"] != 1.0):
                raise SystemExit(f"{mode}: invalid capture receipt for {pose['name']}")
            image = directory / f"{pose['name']}.png"
            if not image.is_file() or image.stat().st_size == 0:
                raise SystemExit(f"{mode}: missing image {image}")
            with image.open("rb") as stream:
                header = stream.read(24)
            expected_dimensions = [poses["width"], poses["height"]]
            if (header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR"
                    or list(struct.unpack(">II", header[16:24])) != expected_dimensions
                    or receipt["dimensions"] != expected_dimensions):
                raise SystemExit(f"{mode}: wrong PNG or receipt dimensions for {pose['name']}")
            if receipt["requested_camera_creation"] != pose["position"]:
                raise SystemExit(f"{mode}: camera position differs from fixture for {pose['name']}")
            images.append({"name": pose["name"], "sha256": hashlib.sha256(image.read_bytes()).hexdigest()})
            signature.append([
                receipt["requested_camera_creation"], receipt["requested_rotation_runtime_xyzw"],
                receipt["requested_vertical_fov_radians"], receipt["dimensions"],
            ])
        if baseline is not None and signature != baseline:
            raise SystemExit(f"{mode}: camera poses or dimensions differ from the baseline")
        baseline = signature
        manifest["runs"][-1]["receipts_validated"] = len(receipts)
        manifest["runs"][-1]["images"] = images
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        print(f"Validated {len(receipts)} settled {mode} captures", flush=True)
    manifest["passed"] = True
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
