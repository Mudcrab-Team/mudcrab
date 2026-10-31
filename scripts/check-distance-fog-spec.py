#!/usr/bin/env python3
"""Check the distance-fog specification's original numerical fixtures.

This is an analytical reference, not a retail GPU capture or production renderer.
It uses binary64 arithmetic except where a recovered binary32 constant is explicit.
DXBC log/exp rounding and depth quantization need separate GPU comparisons.
"""

from __future__ import annotations

import json
import math
import struct
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def f32(value: float) -> float:
    return struct.unpack("<f", struct.pack("<f", value))[0]


def fog_vector(near: float, far: float, power: float, maximum: float,
               *, lighting: bool = False) -> tuple[float, float, float, float]:
    def unordered_or_equal(a: float, b: float) -> bool:
        return math.isnan(a) or math.isnan(b) or a == b
    disabled = (unordered_or_equal(near, 0.0) and unordered_or_equal(far, 0.0)
                if lighting else unordered_or_equal(near, far))
    if disabled:
        return 5_000_000.0, f32(0.1), 1.0, 0.0
    inverse_range = 1.0 / (far - near)
    return near * inverse_range, inverse_range, power, maximum


def amount(distance: float, vector: tuple[float, float, float, float]) -> float:
    offset, inverse_range, power, maximum = vector
    t = max(0.0, min(1.0, distance * inverse_range - offset))
    if power <= 0.0:
        raise ValueError("Nonpositive powers require the retail GPU edge contract")
    powered = 0.0 if t == 0.0 else 2.0 ** (power * math.log2(t))
    return min(maximum, powered)


def opaque_distance(depth: float, camera_near: float, camera_far: float) -> float:
    biased_depth = f32(1.01) * depth - f32(0.01)
    q = 2.0 * biased_depth - 1.0
    return 2.0 * camera_near * camera_far / (
        camera_near + camera_far - q * (camera_far - camera_near)
    )


def finite_d3d_depth(view_z: float, camera_near: float, camera_far: float) -> float:
    return camera_far / (camera_far - camera_near) - (
        camera_near * camera_far / ((camera_far - camera_near) * view_z)
    )


def mix(a: list[float], b: list[float], weight: float) -> list[float]:
    assert len(a) == len(b)
    return [x + weight * (y - x) for x, y in zip(a, b)]


def fog_rgb(source: list[float], near: list[float], far: list[float],
            weight: float, framebuffer_scale: float) -> list[float]:
    fog_color = mix(near, far, weight)
    return [framebuffer_scale * x for x in mix(source, fog_color, weight)]


def opaque_output(depth: float, source_rgba: list[float], weight: float,
                  near: list[float], far: list[float], scale: float) -> list[float]:
    rgb = source_rgba[:3]
    if depth < f32(0.999998987):
        rgb = fog_rgb(rgb, near, far, weight, scale)
    return [max(0.0, min(1.0, value)) for value in rgb + [source_rgba[3]]]


def lighting_output(diffuse: list[float], varying_rgb: list[float], weight: float,
                    scale: float, flag_f: float, flag_g: float, lit_clamp: float,
                    specular: list[float] = None, spec_clamp: float = 1.0) -> list[float]:
    def delta(color: list[float]) -> list[float]:
        fogged = [scale * value for value in mix(color, varying_rgb, weight)]
        return [a - b for a, b in zip(color, fogged)]
    first_delta = delta(diffuse)
    lit = [min(value, lit_clamp + flag_f * d) for value, d in zip(diffuse, first_delta)]
    if specular is None:
        return [value - flag_g * flag_f * d for value, d in zip(lit, first_delta)]
    color = [value + spec for value, spec in zip(lit, specular)]
    second_delta = delta(color)
    return [min(value, spec_clamp + flag_f * d) - flag_g * flag_f * d
            for value, d in zip(color, second_delta)]


def close(actual: float, expected: float, label: str, tolerance: float = 1e-10) -> None:
    if not math.isclose(actual, expected, rel_tol=tolerance, abs_tol=tolerance):
        raise AssertionError(f"{label}: {actual!r} != {expected!r}")


def main() -> None:
    fixture_path = ROOT / "docs/research/skyrim-distance-fog-math.json"
    fixtures = json.loads(fixture_path.read_text())
    checks = 0
    for case in fixtures["curve_cases"]:
        vector = fog_vector(*case["parameters"])
        close(amount(case["distance"], vector), case["expected"], case["name"])
        checks += 1
    for case in fixtures["blend_cases"]:
        output = fog_rgb(case["source"], case["near_rgb"], case["far_rgb"],
                         case["amount"], case["framebuffer_scale"])
        assert len(output) == len(case["expected_rgb"])
        for channel, (actual, expected) in enumerate(zip(output, case["expected_rgb"])):
            close(actual, expected, f"{case['name']}[{channel}]")
            checks += 1

    # Closed-form results distinguish nested fog-color interpolation from a far-color-only blend.
    for case in fixtures["depth_cases"]:
        z = finite_d3d_depth(case["view_z"], case["camera_near"], case["camera_far"])
        d = opaque_distance(z, case["camera_near"], case["camera_far"])
        # The closed form below uses exact decimal bias constants, unlike the binary32 shader.
        closed = case["view_z"] / (1.01 - 0.01 * case["view_z"] / case["camera_far"])
        close(d, closed, case["name"], tolerance=5e-4)
        checks += 1

    disabled = (5_000_000.0, f32(0.1), 1.0, 0.0)
    assert fog_vector(25.0, 25.0, 0.4, 0.85) == disabled
    assert fog_vector(0.0, 0.0, 0.4, 0.85, lighting=True) == disabled
    assert fog_vector(float("nan"), 25.0, 0.4, 0.85) == disabled
    assert fog_vector(float("nan"), 0.0, 0.4, 0.85, lighting=True) == disabled
    assert fog_vector(0.0, float("nan"), 0.4, 0.85, lighting=True) == disabled
    assert math.isnan(fog_vector(float("nan"), 25.0, 0.4, 0.85, lighting=True)[1])
    try:
        fog_vector(25.0, 25.0, 0.4, 0.85, lighting=True)
    except ZeroDivisionError:
        pass  # Native Lighting has no guard here; a safe fallback would change its edge contract.
    else:
        raise AssertionError("Lighting must retain the nonzero-equality distinction")
    checks += 7

    # The maximum is a cap, not fog-color alpha or an exponential multiplier.
    clear = fog_vector(0.0, 80_000.0, 0.4, 0.85)
    close(amount(80_000.0, clear), 0.85, "clear far cap")
    assert abs(amount(2_048.0, clear) - 0.85 * (1.0 - math.exp(-6.2e-5 * 2_048.0))) > 0.1
    assert amount(40_000.0, fog_vector(0.0, 40_000.0, 0.4, 0.85)) == 0.85
    assert amount(5_000.0, fog_vector(350.0, 2_500.0, 0.9, 1_000.0)) == 1.0
    checks += 4

    # Geometry fog has an off-axis clip-XYZ metric. Opaque fog takes only axial depth.
    close(math.hypot(0.0, 0.0, 1_000.0), 1_000.0, "on-axis clip metric")
    close(math.hypot(750.0, 0.0, 1_000.0), 1_250.0, "off-axis clip metric")
    assert amount(1_250.0, clear) > amount(1_000.0, clear)
    assert f32(0.999998987) < 1.0
    checks += 4

    # Valid near-far-plane geometry shares the original bypass with clear depth.
    source = [0.2, 0.3, 0.4, 0.7]
    threshold = f32(0.999998987)
    assert opaque_output(1.0, source, 0.5, [0.5]*3, [0.8]*3, 0.5) == source
    assert opaque_output(threshold, source, 0.5, [0.5]*3, [0.8]*3, 0.5) == source
    fogged = opaque_output(threshold - 1e-7, source, 0.5, [0.5]*3, [0.8]*3, 0.5)
    assert fogged[:3] != source[:3] and fogged[3] == source[3]
    assert opaque_output(0.5, [0.9, 0.9, 0.9, 1.5], 1, [1]*3, [1]*3, 2) == [1, 1, 1, 1]
    checks += 4

    # A bright alpha surface exposes the clamp-coupled behavior that a simple fog lerp loses.
    rgb = lighting_output([2, 0.4, 0.1], [0.5]*3, 0.5, 1, 1, 1, 1)
    for i, expected in enumerate([1, 0.45, 0.3]):
        close(rgb[i], expected, f"alpha Lighting clamp[{i}]")
    rgb = lighting_output([2, 0.4, 0.1], [0.5]*3, 0.5, 1, 1, 1, 1,
                          [0.8, 0.9, 0.6], 0.8)
    for i, expected in enumerate([0.8, 0.8, 0.6]):
        close(rgb[i], expected, f"two-stage Lighting clamp[{i}]")
    assert lighting_output([2, 0.4, 0.1], [0.5]*3, 0.5, 1, 0, 1, 1) == [1, 0.4, 0.1]
    checks += 7

    # The public record fixture must preserve two independent colors and per-field ownership.
    inputs = json.loads((ROOT / "docs/research/skyrim-distance-fog-reference.json").read_text())
    records = {record["editor_id"]: record for record in inputs["records"]}
    authored_clear = records["SkyrimClear"]
    assert authored_clear["fog"]["day_far"] == 80_000.0
    assert authored_clear["colors_rgba"]["fog_near"]["day"] == [35, 98, 124, 0]
    assert authored_clear["colors_rgba"]["fog_far"]["day"] == [116, 168, 203, 0]
    inn = records["RiverwoodSleepingGiantInn"]
    assert inn["lighting"]["fog_near"] == 0.0
    assert inn["schema_resolved_fog"]["fog_near"]["value"] == 100.0
    assert inn["schema_resolved_fog"]["fog_near"]["source_identity"] == "Skyrim.esm:0A1196"
    assert inn["schema_resolved_fog"]["fog_power"]["source_identity"] == "Skyrim.esm:0133C6"
    checks += 7
    print(f"PASS: {checks} analytical fixture checks; no retail GPU or frame parity claimed")


if __name__ == "__main__":
    main()
