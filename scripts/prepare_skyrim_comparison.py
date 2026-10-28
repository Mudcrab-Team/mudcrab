#!/usr/bin/env python3
"""Prepare camera presets from a native Photo Mode export and measured calibration."""
import argparse
import copy
import hashlib
import json
import math
from pathlib import Path
import re


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def prepare(shots, template, calibration):
    required = {"name", "timestamp", "position", "rotation", "fov", "viewRoll", "pcPosition", "pcRotation", "pcCell"}
    if not required.issubset(template):
        raise ValueError("Use a complete native Photo Mode camera export as the template")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", calibration.get("runtime_sha256", "")):
        raise ValueError("Calibration must identify the native executable SHA256")
    if not calibration.get("evidence"):
        raise ValueError("Calibration must identify its native measurement evidence")
    if calibration.get("pitch_sign") not in (-1, 1) or calibration.get("yaw_sign") not in (-1, 1):
        raise ValueError("Calibration pitch_sign and yaw_sign must be -1 or 1")
    if calibration.get("fov_axis") not in ("horizontal", "vertical"):
        raise ValueError("Calibration fov_axis must be horizontal or vertical")
    offset = calibration.get("yaw_offset_degrees")
    if not finite(offset):
        raise ValueError("Calibration requires finite yaw_offset_degrees")
    width, height = shots.get("width"), shots.get("height")
    if any(not isinstance(v, int) or isinstance(v, bool) or not 0 < v <= 8192 for v in (width, height)):
        raise ValueError("Shot dimensions must be integers from 1 to 8192")
    if not shots.get("shots"):
        raise ValueError("No shots supplied")
    result = []
    names = set()
    worldspaces = set()
    for shot in shots["shots"]:
        name = shot.get("name", "")
        if not re.fullmatch(r"[A-Za-z0-9_-]+", name) or name in names:
            raise ValueError("Shot names must be unique plain file stems")
        names.add(name)
        if shot.get("interior_cell_id") is not None or not isinstance(shot.get("worldspace_id"), int):
            raise ValueError("This exporter requires explicit exterior worldspace IDs")
        worldspaces.add(shot["worldspace_id"])
        position = shot.get("position")
        if not isinstance(position, list) or len(position) != 3 or not all(map(finite, position)):
            raise ValueError("Shot position must contain three finite values")
        if not all(finite(shot.get(k)) for k in ("yaw", "pitch", "hfov")) or not 0 < shot["hfov"] < 180:
            raise ValueError("Shot angles must be finite and FOV between 0 and 180")
        pitch = math.radians(calibration["pitch_sign"] * shot["pitch"])
        yaw = math.radians((calibration["yaw_sign"] * shot["yaw"] + offset) % 360)
        yaw_wrapped = min(yaw % math.tau, math.tau - yaw % math.tau)
        if abs(pitch) <= 1e-9 and yaw_wrapped <= 1e-9:
            raise ValueError(f"{name}: Photo Mode skips [0, 0] rotation; use a calibrated framework client for this pose")
        fov = shot["hfov"]
        if calibration["fov_axis"] == "vertical":
            fov = math.degrees(2 * math.atan(math.tan(math.radians(fov) / 2) * height / width))
        preset = copy.deepcopy(template)
        preset.update(name=f"Mudcrab {name}", timestamp=f"mudcrab-{name}",
                      position=position, rotation=[pitch, yaw], fov=fov, viewRoll=0.0)
        result.append((name, preset))
    if len(worldspaces) != 1:
        raise ValueError("Use a separate native template and bundle per worldspace")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shots", required=True, type=Path)
    parser.add_argument("--native-preset", required=True, type=Path)
    parser.add_argument("--calibration", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    sources = {name: path.read_bytes() for name, path in
               [("shots", args.shots), ("native_preset", args.native_preset), ("calibration", args.calibration)]}
    inputs = {k: json.loads(v) for k, v in sources.items()}
    try:
        presets = prepare(inputs["shots"], inputs["native_preset"], inputs["calibration"])
    except ValueError as error:
        parser.error(str(error))
    args.output.mkdir(parents=True, exist_ok=False)
    manifest = {"kind": "mudcrab-photo-mode-inputs", "native_capture_verified": False,
                "inputs": {k: hashlib.sha256(v).hexdigest() for k, v in sources.items()},
                "calibration": inputs["calibration"], "shots": inputs["shots"], "presets": []}
    for name, preset in presets:
        filename = f"CameraPosition_{preset['timestamp']}.json"
        data = (json.dumps(preset, indent=2, allow_nan=False) + "\n").encode()
        (args.output / filename).write_bytes(data)
        manifest["presets"].append({"pose": name, "filename": filename, "sha256": hashlib.sha256(data).hexdigest()})
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Prepared {len(presets)} camera presets in {args.output}; native capture remains unverified")


if __name__ == "__main__":
    main()
