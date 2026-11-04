import importlib.util
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
import zlib
from pathlib import Path
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
PERF_PATH = REPO / "scripts" / "perf" / "perf.py"
spec = importlib.util.spec_from_file_location("perf_harness", PERF_PATH)
perf = importlib.util.module_from_spec(spec)
sys.modules["perf_harness"] = perf
spec.loader.exec_module(perf)

FAKE_ENGINE = """#!{python}
import json, os, sys, time
args = sys.argv[1:]
mode = os.environ.get("FAKE_MODE", "ok")
out = args[args.index("--benchmark-output") + 1]
report = {{
    "frame_ms_mean": 10.0, "frame_ms_p50": 9.0, "frame_ms_p95": 14.0, "frame_ms_p99": 18.0,
    "average_fps": 100.0, "passed": True, "warmup_frames": 60,
    "fingerprint": {{"commit": "x", "resolution": [100, 100]}},
    "scene": {{"mesh_entities": 10, "terrain_patches": 1, "water_surfaces": 0}},
}}
def write():
    with open(out, "w") as handle:
        json.dump(report, handle)
if mode == "never":
    time.sleep(600)
elif mode == "crash":
    sys.exit(3)
elif mode == "hang":
    write()
    time.sleep(600)
elif mode == "nonzero":
    write()
    sys.exit(1)
else:
    write()
"""


def make_fake_engine(directory):
    path = Path(directory) / "fake-engine"
    path.write_text(FAKE_ENGINE.format(python=sys.executable), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def run_entry(valid=True, fingerprint=None, scene=None, metrics=None, classification="ok"):
    return {
        "run": {"valid": valid, "classification": classification, "warnings": []},
        "fingerprint": fingerprint if fingerprint is not None else {"commit": "a", "resolution": [10, 10],
                                                                    "graphics": {"stream_radius": 2}},
        "scene": scene if scene is not None else {"mesh_entities": 1000, "terrain_patches": 10,
                                                  "water_surfaces": 2},
        "metrics": metrics if metrics is not None else {"frame_ms_mean": 10.0},
    }


def frame_metrics(scale, jitter=0.0):
    return {"frame_ms_mean": 10.0 * scale + jitter, "frame_ms_p50": 9.0 * scale + jitter,
            "frame_ms_p95": 14.0 * scale + jitter, "frame_ms_p99": 18.0 * scale + jitter,
            "average_fps": 100.0 / (scale + jitter / 10)}


class StatsTest(unittest.TestCase):
    def test_bootstrap_is_deterministic_for_a_seed(self):
        ratios = [0.98, 1.01, 1.03, 0.99, 1.0]
        a = perf.bootstrap_ci(ratios, seed=7, samples=2000)
        b = perf.bootstrap_ci(ratios, seed=7, samples=2000)
        self.assertEqual(a, b)
        self.assertLessEqual(a[0], a[1])
        self.assertGreaterEqual(a[0], min(ratios))
        self.assertLessEqual(a[1], max(ratios))

    def test_bootstrap_changes_with_the_seed(self):
        ratios = [0.9, 1.0, 1.1, 1.2, 0.95, 1.05, 1.15, 0.93, 1.02, 1.08]
        draws = {perf.bootstrap_ci(ratios, seed=seed, samples=20) for seed in range(1, 6)}
        self.assertGreater(len(draws), 1)

    def test_verdict_needs_ci_and_effect_size(self):
        v = perf.verdict_for
        self.assertEqual(v(0.90, 0.88, 0.93, min_effect=0.02), "improved")
        self.assertEqual(v(1.10, 1.05, 1.14, min_effect=0.02), "regressed")
        # CI contains 1.0
        self.assertEqual(v(0.95, 0.90, 1.02, min_effect=0.02), "noise")
        # CI excludes 1.0 but the effect is below the minimum
        self.assertEqual(v(0.99, 0.985, 0.995, min_effect=0.02), "noise")
        # exactly at the minimum effect counts
        self.assertEqual(v(1.02, 1.01, 1.03, min_effect=0.02), "regressed")

    def test_higher_is_better_flips_direction(self):
        self.assertEqual(perf.verdict_for(1.2, 1.1, 1.3, min_effect=0.02, higher_is_better=True), "improved")
        self.assertEqual(perf.verdict_for(0.8, 0.7, 0.9, min_effect=0.02, higher_is_better=True), "regressed")

    def test_compare_metric_detects_a_consistent_regression(self):
        base = [10.0, 10.2, 9.9, 10.1, 10.0]
        cand = [11.5, 11.6, 11.3, 11.6, 11.4]
        result = perf.compare_metric("frame_ms_mean", base, cand, seed=3, min_effect=0.02, samples=2000)
        self.assertEqual(result["verdict"], "regressed")
        self.assertEqual(result["pairs"], 5)
        self.assertGreater(result["ratio_median"], 1.1)

    def test_compare_metric_is_noise_when_pairs_disagree(self):
        base = [10.0, 10.0, 10.0, 10.0, 10.0]
        cand = [10.5, 9.6, 10.2, 9.8, 10.1]
        result = perf.compare_metric("frame_ms_mean", base, cand, seed=3, min_effect=0.02, samples=2000)
        self.assertEqual(result["verdict"], "noise")

    def test_too_few_pairs_gives_no_verdict(self):
        result = perf.compare_metric("frame_ms_mean", [10.0, 10.0], [20.0, 20.0], seed=1, min_effect=0.02)
        self.assertEqual(result["verdict"], "insufficient_pairs")

    def test_missing_values_are_dropped_pairwise(self):
        result = perf.compare_metric("m", [1.0, None, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0], seed=1, min_effect=0.02)
        self.assertEqual(result["pairs"], 3)


class GuardTest(unittest.TestCase):
    def analyze(self, base, cand, **kwargs):
        pairs = [{"base": b, "cand": c} for b, c in zip(base, cand)]
        return perf.analyze_scenario("dense", pairs, seed=1, min_effect=0.02, samples=500, **kwargs)

    def runs(self, count, **kwargs):
        return [run_entry(metrics=frame_metrics(1.0, 0.01 * i), **kwargs) for i in range(count)]

    def test_same_conditions_compare(self):
        result = self.analyze(self.runs(4), self.runs(4))
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["summary_verdict"], "noise")

    def test_commit_and_dirty_differences_are_ignored(self):
        cand = [run_entry(metrics=frame_metrics(1.0), fingerprint={
            "commit": "b", "dirty_worktree": True, "resolution": [10, 10], "graphics": {"stream_radius": 2}})
            for _ in range(3)]
        result = self.analyze(self.runs(3), cand)
        self.assertEqual(result["status"], "ok")

    def test_resolution_mismatch_refuses(self):
        cand = [run_entry(fingerprint={"commit": "a", "resolution": [20, 10], "graphics": {"stream_radius": 2}})
                for _ in range(3)]
        result = self.analyze(self.runs(3), cand)
        self.assertEqual(result["status"], "refused")
        self.assertEqual(result["error"], "fingerprint_mismatch")
        self.assertTrue(any("resolution" in line for line in result["details"]))

    def test_graphics_setting_mismatch_refuses(self):
        cand = [run_entry(fingerprint={"commit": "a", "resolution": [10, 10], "graphics": {"stream_radius": 3}})
                for _ in range(3)]
        result = self.analyze(self.runs(3), cand)
        self.assertEqual(result["error"], "fingerprint_mismatch")
        self.assertTrue(any("graphics.stream_radius" in line for line in result["details"]))

    def test_backend_mismatch_within_one_side_refuses(self):
        base = [run_entry(fingerprint={"commit": "a", "renderer": {"backend": "Metal"}}) for _ in range(2)]
        base.append(run_entry(fingerprint={"commit": "a", "renderer": {"backend": "Vulkan"}}))
        cand = [run_entry(fingerprint={"commit": "a", "renderer": {"backend": "Metal"}}) for _ in range(3)]
        result = self.analyze(base, cand)
        self.assertEqual(result["error"], "fingerprint_mismatch")

    def test_inventory_mismatch_refuses(self):
        cand = [run_entry(scene={"mesh_entities": 700, "terrain_patches": 10, "water_surfaces": 2})
                for _ in range(3)]
        result = self.analyze(self.runs(3), cand)
        self.assertEqual(result["status"], "refused")
        self.assertEqual(result["error"], "inventory_mismatch")
        self.assertIn("mesh_entities", result["details"][0])

    def test_inventory_within_tolerance_passes(self):
        cand = [run_entry(scene={"mesh_entities": 1010, "terrain_patches": 10, "water_surfaces": 2},
                          metrics=frame_metrics(1.0)) for _ in range(3)]
        result = self.analyze(self.runs(3), cand)
        self.assertEqual(result["status"], "ok")

    def test_small_scene_counts_get_absolute_slack(self):
        self.assertEqual(perf.check_inventory([{"water_surfaces": 0}], [{"water_surfaces": 2}]), [])
        self.assertTrue(perf.check_inventory([{"water_surfaces": 0}], [{"water_surfaces": 5}]))

    def test_invalid_runs_drop_their_pair(self):
        base = self.runs(4)
        cand = self.runs(4)
        cand[1] = run_entry(valid=False, classification="timeout_no_report")
        result = self.analyze(base, cand)
        self.assertEqual(result["pairs_used"], 3)
        self.assertEqual(result["dropped_pairs"], [1])

    def test_too_few_valid_pairs_refuses(self):
        cand = [run_entry(valid=False, classification="failed") for _ in range(3)]
        result = self.analyze(self.runs(3), cand)
        self.assertEqual(result["error"], "too_few_valid_pairs")

    def test_a_real_regression_is_reported(self):
        base = [run_entry(metrics=frame_metrics(1.0, 0.02 * i)) for i in range(5)]
        cand = [run_entry(metrics=frame_metrics(1.2, 0.02 * i)) for i in range(5)]
        result = self.analyze(base, cand)
        self.assertEqual(result["summary_verdict"], "regressed")
        self.assertEqual(result["metrics"]["average_fps"]["verdict"], "regressed")

    def test_overall_verdict(self):
        ok = {"status": "ok", "summary_verdict": "noise"}
        self.assertEqual(perf.overall_verdict({"a": ok}), "noise")
        self.assertEqual(perf.overall_verdict({"a": ok, "b": {"status": "refused"}}), "refused")
        self.assertEqual(perf.overall_verdict({"a": ok, "b": {"status": "ok", "summary_verdict": "improved"}}),
                         "improved")
        self.assertEqual(perf.overall_verdict({"a": {"status": "ok", "summary_verdict": "regressed"},
                                               "b": {"status": "ok", "summary_verdict": "improved"}}), "mixed")


class MetricExtractionTest(unittest.TestCase):
    def test_zero_gpu_passes_and_rare_spans_are_left_out(self):
        report = {
            "frame_ms_mean": 10.0, "average_fps": 100.0, "peak_process_memory_gib": None,
            "gpu_passes": {"passes": {"a/elapsed_gpu": {"mean": 0.0}, "b/elapsed_gpu": {"mean": 1.5}}},
            "cpu": {"schedules": {"main/Update": {"mean": 0.2, "count": 100}},
                    "spans": {"once": {"mean": 5.0, "count": 1}, "often": {"mean": 2.0, "count": 100}}},
        }
        metrics = perf.extract_metrics(report)
        self.assertEqual(metrics["gpu/b/elapsed_gpu"], 1.5)
        self.assertNotIn("gpu/a/elapsed_gpu", metrics)
        self.assertNotIn("cpu/once", metrics)
        self.assertEqual(metrics["cpu/often"], 2.0)
        self.assertEqual(metrics["cpu/main/Update"], 0.2)
        self.assertNotIn("memory_peak_gib", metrics)

    def test_memory_peak_falls_back_to_the_bundle(self):
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "memory.json").write_text(json.dumps({"samples": [{"process_gib": 1.0},
                                                                         {"process_gib": 2.5}]}))
            metrics = perf.extract_metrics({"peak_process_memory_gib": None}, tmp)
        self.assertEqual(metrics["memory_peak_gib"], 2.5)


class WatchdogTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.engine = make_fake_engine(self.tmp.name)
        self.scenario = {"needs_assets": False, "args": ["--benchmark-only"]}

    def run_mode(self, mode, **kwargs):
        out = Path(self.tmp.name) / mode
        options = {"assets": None, "out_dir": out, "duration": 1, "timeout": 5, "grace": 1, "poll": 0.05}
        options.update(kwargs)
        with mock.patch.dict(os.environ, {"FAKE_MODE": mode}):
            return perf.run_scenario(self.engine, "synthetic", self.scenario, **options)

    def test_clean_exit_is_ok(self):
        run = self.run_mode("ok")
        self.assertEqual(run["classification"], "ok")
        self.assertTrue(run["valid"])
        self.assertEqual(run["binary_sha256"], perf.sha256_file(self.engine))
        self.assertTrue((Path(self.tmp.name) / "ok" / "run.json").is_file())
        self.assertIn("--benchmark-output", run["command"])

    def test_hang_after_report_is_killed_and_still_valid(self):
        run = self.run_mode("hang", grace=0.5, timeout=20)
        self.assertEqual(run["classification"], "hung_after_report")
        self.assertTrue(run["valid"])
        self.assertTrue(run["killed"])
        self.assertLess(run["wall_seconds"], 10)

    def test_no_report_times_out(self):
        run = self.run_mode("never", timeout=1)
        self.assertEqual(run["classification"], "timeout_no_report")
        self.assertFalse(run["valid"])
        self.assertLess(run["wall_seconds"], 10)

    def test_crash_without_report_is_failed(self):
        run = self.run_mode("crash")
        self.assertEqual(run["classification"], "failed")
        self.assertEqual(run["exit_code"], 3)
        self.assertFalse(run["valid"])

    def test_nonzero_exit_after_a_complete_report_is_kept_with_a_warning(self):
        run = self.run_mode("nonzero")
        self.assertEqual(run["classification"], "ok")
        self.assertTrue(run["valid"])
        self.assertTrue(any("exited with 1" in w for w in run["warnings"]))

    def test_stale_report_is_not_mistaken_for_a_result(self):
        out = Path(self.tmp.name) / "never"
        out.mkdir()
        (out / "report.json").write_text(json.dumps({"frame_ms_mean": 1, "fingerprint": {}}))
        run = self.run_mode("never", timeout=1)
        self.assertEqual(run["classification"], "timeout_no_report")

    def test_half_written_report_does_not_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp, "report.json")
            path.write_text('{"frame_ms_mean": 1.0, "finger')
            self.assertIsNone(perf.read_complete_report(path))

    def test_load_run_reads_metrics(self):
        run = self.run_mode("ok")
        loaded = perf.load_run(Path(run["report"]).parent)
        self.assertEqual(loaded["metrics"]["frame_ms_mean"], 10.0)
        self.assertEqual(loaded["scene"]["mesh_entities"], 10)

    def test_missing_binary_does_not_hang(self):
        out = Path(self.tmp.name) / "nobin"
        watch = perf.watch_process([str(Path(self.tmp.name) / "absent")], stdout_path=Path(self.tmp.name) / "o",
                                   stderr_path=Path(self.tmp.name) / "e", is_done=lambda: False, timeout=1,
                                   grace=1)
        self.assertEqual(perf.classify(watch), "failed")
        self.assertFalse(out.exists())

    def test_assets_are_required_for_world_scenarios(self):
        scenario = {"needs_assets": True, "args": []}
        with self.assertRaises(perf.HarnessError) as caught:
            perf.run_scenario(self.engine, "dense", scenario, assets=None, out_dir=Path(self.tmp.name) / "x")
        self.assertEqual(caught.exception.name, "assets_required")


def encode_png(width, height, channels, rows, filter_type):
    """A PNG whose rows are stored with the given filter type, to exercise the decoder."""
    stride = width * channels
    out = bytearray()
    prev = bytes(stride)
    for row in rows:
        out.append(filter_type)
        for i in range(stride):
            a = row[i - channels] if i >= channels else 0
            b = prev[i]
            c = prev[i - channels] if i >= channels else 0
            if filter_type == 0:
                predictor = 0
            elif filter_type == 1:
                predictor = a
            elif filter_type == 2:
                predictor = b
            elif filter_type == 3:
                predictor = (a + b) // 2
            else:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                predictor = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
            out.append((row[i] - predictor) & 255)
        prev = row

    def chunk(kind, body):
        return len(body).to_bytes(4, "big") + kind + body + (zlib.crc32(kind + body) & 0xFFFFFFFF).to_bytes(4, "big")

    ctype = {1: 0, 2: 4, 3: 2, 4: 6}[channels]
    header = width.to_bytes(4, "big") + height.to_bytes(4, "big") + bytes([8, ctype, 0, 0, 0])
    return perf.PNG_SIGNATURE + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(out))) + chunk(b"IEND", b"")


def gradient_rows(width, height, channels):
    return [bytes((x * 7 + y * 13 + c * 29) % 256 for x in range(width) for c in range(channels))
            for y in range(height)]


def solid_png(path, width, height, value, channels=3, noise=0):
    rows = []
    for y in range(height):
        rows.append(bytes(min(255, max(0, value + ((x * 31 + y * 17) % (noise + 1) if noise else 0)))
                          for x in range(width) for _ in range(channels)))
    perf.write_png(path, width, height, channels, b"".join(rows))


class PngTest(unittest.TestCase):
    def test_every_filter_type_decodes(self):
        for channels in (1, 2, 3, 4):
            rows = gradient_rows(9, 6, channels)
            for filter_type in range(5):
                with self.subTest(channels=channels, filter=filter_type), tempfile.TemporaryDirectory() as tmp:
                    path = Path(tmp, "a.png")
                    path.write_bytes(encode_png(9, 6, channels, rows, filter_type))
                    width, height, got_channels, pixels = perf.decode_png(path)
                    self.assertEqual((width, height, got_channels), (9, 6, channels))
                    self.assertEqual(pixels, b"".join(rows))

    def test_write_png_round_trips(self):
        with tempfile.TemporaryDirectory() as tmp:
            rows = gradient_rows(5, 4, 3)
            path = Path(tmp, "a.png")
            perf.write_png(path, 5, 4, 3, b"".join(rows))
            self.assertEqual(perf.decode_png(path)[3], b"".join(rows))

    def test_rejects_unsupported_and_corrupt_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp, "bad.png")
            path.write_bytes(b"not a png at all")
            with self.assertRaises(perf.PngError):
                perf.decode_png(path)
            good = encode_png(4, 4, 3, gradient_rows(4, 4, 3), 0)
            interlaced = bytearray(good)
            interlaced[8 + 8 + 12] = 1  # interlace byte of IHDR
            path.write_bytes(bytes(interlaced))
            with self.assertRaises(perf.PngError):
                perf.decode_png(path)

    def test_blank_and_black_detection(self):
        with tempfile.TemporaryDirectory() as tmp:
            black = Path(tmp, "black.png")
            solid_png(black, 32, 32, 0)
            grey = Path(tmp, "grey.png")
            solid_png(grey, 32, 32, 128)
            real = Path(tmp, "real.png")
            perf.write_png(real, 8, 8, 3, bytes((x * 9 + y * 5 + c * 40) % 256
                                                for y in range(8) for x in range(8) for c in range(3)))
            self.assertTrue(perf.blank_stats(perf.decode_png(black))["black"])
            self.assertTrue(perf.blank_stats(perf.decode_png(grey))["blank"])
            self.assertFalse(perf.blank_stats(perf.decode_png(grey))["black"])
            stats = perf.blank_stats(perf.decode_png(real))
            self.assertFalse(stats["blank"] or stats["black"])

    def test_pixel_diff(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp, "a.png"), Path(tmp, "b.png")
            solid_png(a, 16, 16, 100)
            solid_png(b, 16, 16, 120)
            same = perf.pixel_diff(perf.decode_png(a), perf.decode_png(a))
            self.assertEqual(same["mean_abs_diff"], 0)
            diff = perf.pixel_diff(perf.decode_png(a), perf.decode_png(b))
            self.assertAlmostEqual(diff["mean_abs_diff"], 20.0)
            self.assertEqual(diff["fraction_over"], 1.0)
            solid_png(Path(tmp, "c.png"), 8, 8, 100)
            with self.assertRaises(perf.PngError):
                perf.pixel_diff(perf.decode_png(a), perf.decode_png(Path(tmp, "c.png")))

    def test_rgba_alpha_is_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp, "a.png"), Path(tmp, "b.png")
            pixels = bytes([10, 20, 30, 255] * 16)
            perf.write_png(a, 4, 4, 4, pixels)
            perf.write_png(b, 4, 4, 4, bytes([10, 20, 30, 0] * 16))
            self.assertEqual(perf.pixel_diff(perf.decode_png(a), perf.decode_png(b))["mean_abs_diff"], 0)


class VisualTest(unittest.TestCase):
    def textured(self, path, shift=0, size=24):
        pixels = bytes(((x * 9 + y * 5 + c * 40 + shift) * 7) % 256
                       for y in range(size) for x in range(size) for c in range(3))
        perf.write_png(path, size, size, 3, pixels)

    def make_dirs(self, tmp, names, shift=0):
        base, cand = Path(tmp, "base"), Path(tmp, "cand")
        base.mkdir()
        cand.mkdir()
        for i, name in enumerate(names):
            self.textured(base / f"{name}.png", size=20 + i)
            self.textured(cand / f"{name}.png", shift=shift, size=20 + i)
        return base, cand

    def test_identical_captures_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, cand = self.make_dirs(tmp, ["a", "b"])
            result = perf.visual_compare(base, cand, Path(tmp, "out"), force_stdlib=True)
            self.assertEqual(result["overall"], "pass")
            self.assertEqual(result["metric"], "stdlib")
            self.assertEqual(result["review"], [])
            self.assertTrue(Path(tmp, "out", "visual.json").is_file())

    def test_a_changed_shot_fails_and_is_queued_for_review(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, cand = self.make_dirs(tmp, ["a", "b"])
            self.textured(cand / "b.png", shift=40, size=21)
            result = perf.visual_compare(base, cand, Path(tmp, "out"), force_stdlib=True)
            self.assertEqual(result["shots"]["a"]["status"], "pass")
            self.assertEqual(result["shots"]["b"]["status"], "fail")
            self.assertEqual(result["review"], ["b"])

    def test_black_capture_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, cand = self.make_dirs(tmp, ["a"])
            solid_png(cand / "a.png", 20, 20, 0)
            result = perf.visual_compare(base, cand, Path(tmp, "out"), force_stdlib=True)
            self.assertIn("blank_frame_cand", result["shots"]["a"]["reasons"])
            self.assertEqual(result["overall"], "fail")

    def test_identical_file_sizes_mark_a_suspect_black_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, cand = Path(tmp, "base"), Path(tmp, "cand")
            base.mkdir()
            cand.mkdir()
            for name in ("a", "b", "c"):
                solid_png(base / f"{name}.png", 20, 20, 0)
                solid_png(cand / f"{name}.png", 20, 20, 0)
            result = perf.visual_compare(base, cand, Path(tmp, "out"), force_stdlib=True)
            self.assertTrue(result["suspect_black_run"])
            self.assertEqual(result["review"], ["a", "b", "c"])

    def test_missing_shot_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, cand = self.make_dirs(tmp, ["a", "b"])
            (cand / "b.png").unlink()
            result = perf.visual_compare(base, cand, Path(tmp, "out"), force_stdlib=True)
            self.assertIn("missing_cand", result["shots"]["b"]["reasons"])

    def test_noise_floor_raises_the_limit(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, cand = self.make_dirs(tmp, ["a"])
            self.textured(cand / "a.png", shift=1, size=20)
            strict = perf.visual_compare(base, cand, Path(tmp, "o1"), force_stdlib=True, margin_mean=0.0,
                                         margin_fraction=0.0)
            self.assertEqual(strict["shots"]["a"]["status"], "fail")
            floor = Path(tmp, "floor.json")
            floor.write_text(json.dumps({"metric": "stdlib", "measured": strict["measured"]}))
            relaxed = perf.visual_compare(base, cand, Path(tmp, "o2"), noise_floor=floor, force_stdlib=True,
                                          margin_mean=0.0, margin_fraction=0.0)
            self.assertEqual(relaxed["shots"]["a"]["status"], "pass")

    def test_noise_floor_of_another_metric_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, cand = self.make_dirs(tmp, ["a"])
            floor = Path(tmp, "floor.json")
            floor.write_text(json.dumps({"metric": "flip", "measured": {}}))
            with self.assertRaises(perf.HarnessError) as caught:
                perf.visual_compare(base, cand, Path(tmp, "o"), noise_floor=floor, force_stdlib=True)
            self.assertEqual(caught.exception.name, "noise_floor_metric_mismatch")


class ScenarioFileTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        source = (REPO / "crates" / "engine" / "src" / "config.rs").read_text(encoding="utf-8")
        cls.known_flags = set(re.findall(r'"(--[a-z0-9][a-z0-9-]*)"', source))
        cls.scenarios = perf.load_scenarios()

    def test_flags_exist_in_the_engine(self):
        for name, scenario in self.scenarios.items():
            args = perf.build_engine_args(name, scenario, assets="/assets", out_dir="/out", duration=10,
                                          warmup_frames=None, hidden=True, commit="c", run_id="r",
                                          frame_budget_ms=6.25)
            for flag in (a for a in args if a.startswith("--")):
                self.assertIn(flag, self.known_flags, f"{name}: {flag} is not an engine flag")

    def test_shape(self):
        self.assertIn("synthetic", self.scenarios)
        self.assertIn("terrain-water", self.scenarios)
        for name, scenario in self.scenarios.items():
            self.assertIsInstance(scenario["needs_assets"], bool, name)
            self.assertIsInstance(scenario["args"], list, name)
            self.assertTrue(scenario.get("description"), name)

    def test_phase2_baseline_places_are_kept(self):
        script = (REPO / "scripts" / "phase2-baseline.ps1").read_text(encoding="utf-8")
        defaults = dict(re.findall(r"\[int\]\$(\w+GridX|\w+GridY) = (-?\d+)", script))
        for scenario, prefix in (("rural", "Rural"), ("dense", "Dense"), ("dense-legacy", "DenseLegacy"),
                                 ("water", "Water")):
            args = self.scenarios[scenario]["args"]
            self.assertEqual(args[args.index("--grid-x") + 1], defaults[f"{prefix}GridX"], scenario)
            self.assertEqual(args[args.index("--grid-y") + 1], defaults[f"{prefix}GridY"], scenario)

    def test_route_scenarios_point_at_route_files_and_get_long_enough_windows(self):
        for name in ("travel", "check"):
            scenario = self.scenarios[name]
            self.assertTrue((REPO / "scripts" / scenario["route"]).is_file())
            travel = perf.route_travel_seconds(REPO / "scripts" / scenario["route"])
            self.assertEqual(perf.effective_duration(scenario, 10), travel + perf.ROUTE_READINESS_SECONDS)

    def test_world_scenarios_need_assets_and_fixtures_do_not(self):
        self.assertFalse(self.scenarios["synthetic"]["needs_assets"])
        self.assertFalse(self.scenarios["terrain-water"]["needs_assets"])
        self.assertTrue(self.scenarios["dense"]["needs_assets"])

    def test_shots_file_is_valid(self):
        shots = json.loads((REPO / "scripts" / "perf" / "shots" / "riverwood.json").read_text())
        self.assertEqual((shots["width"], shots["height"]), (1280, 720))
        names = [s["name"] for s in shots["shots"]]
        self.assertEqual(len(names), len(set(names)))
        self.assertTrue(3 <= len(names) <= 4)
        for shot in shots["shots"]:
            self.assertEqual(shot["worldspace_id"], 60)
            self.assertEqual(len(shot["position"]), 3)
            for key in ("yaw", "pitch", "hfov"):
                self.assertIsInstance(shot[key], (int, float))


PMSET_BATT_AC = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t81%; charging; 0:53 remaining present: true\n"
PMSET_BATT_BATTERY = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t81%; discharging\n"
PMSET_THERM_OK = "Note: No thermal warning level has been recorded\nNote: No performance warning level has been recorded\n"
PMSET_THERM_LIMITED = "CPU_Scheduler_Limit = 100\nCPU_Available_CPUs = 12\nCPU_Speed_Limit = 70\n"


class PreflightTest(unittest.TestCase):
    def checks(self, batt=PMSET_BATT_AC, therm=PMSET_THERM_OK, general=" powermode 0\n", busy=(), free=100 * 1024**3,
               system="Darwin"):
        def output(command):
            return {"batt": batt, "therm": therm}.get(command[2] if len(command) > 2 else "", general)

        return perf.preflight_checks(".", system=system, command_output=output, free_bytes=free,
                                     processes=lambda names: [n for n in names if n in busy])

    def status(self, checks, name):
        return next(c["status"] for c in checks if c["name"] == name)

    def test_parsers(self):
        self.assertEqual(perf.parse_power_source(PMSET_BATT_AC), "AC Power")
        self.assertEqual(perf.parse_power_source(PMSET_BATT_BATTERY), "Battery Power")
        self.assertEqual(perf.parse_cpu_speed_limit(PMSET_THERM_LIMITED), 70)
        self.assertIsNone(perf.parse_cpu_speed_limit(PMSET_THERM_OK))
        self.assertTrue(perf.parse_low_power_mode(" lowpowermode 1\n"))
        self.assertFalse(perf.parse_low_power_mode(" powermode 0\n"))
        self.assertIsNone(perf.parse_low_power_mode("nothing here"))

    def test_healthy_machine_passes(self):
        checks = self.checks()
        self.assertFalse([c for c in checks if c["status"] == "fail"])

    def test_each_failure_is_reported(self):
        self.assertEqual(self.status(self.checks(batt=PMSET_BATT_BATTERY), "ac_power"), "fail")
        self.assertEqual(self.status(self.checks(therm=PMSET_THERM_LIMITED), "thermal"), "fail")
        self.assertEqual(self.status(self.checks(general=" lowpowermode 1\n"), "low_power_mode"), "fail")
        self.assertEqual(self.status(self.checks(busy=("cargo",)), "no_builds"), "fail")
        self.assertEqual(self.status(self.checks(busy=("engine",)), "no_other_engine"), "fail")
        self.assertEqual(self.status(self.checks(free=1024**3), "disk"), "fail")

    def test_non_macos_skips_the_machine_checks(self):
        checks = self.checks(system="Linux")
        self.assertEqual({c["name"] for c in checks}, {"disk", "macos"})
        self.assertEqual(self.status(checks, "macos"), "skip")


class BuildTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com"}
        for command in (["git", "init", "-q"], ["git", "commit", "-q", "--allow-empty", "-m", "first"]):
            subprocess.run(command, cwd=self.repo, env=env, check=True, capture_output=True)
        self.sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.repo, capture_output=True, text=True,
                                  check=True).stdout.strip()
        self.worktrees = root / "worktrees"
        self.bins = root / "bins"
        self.calls = []

    def runner(self, command, **kwargs):
        self.calls.append((command, kwargs))
        if command[0] == "cargo":
            binary = Path(kwargs["cwd"], "target", "release", "engine")
            binary.parent.mkdir(parents=True)
            binary.write_text("fake engine")
            return subprocess.CompletedProcess(command, 0, "", "")
        return perf._run(command, **kwargs)

    def build(self, **kwargs):
        options = {"repo": self.repo, "worktree_root": self.worktrees, "bin_dir": self.bins, "runner": self.runner,
                   "free_bytes": 100 * 1024**3}
        options.update(kwargs)
        return perf.build("HEAD", **options)

    def test_existing_binary_is_reused_without_building(self):
        self.bins.mkdir()
        (self.bins / f"engine-{self.sha[:8]}").write_text("already built")
        plan = self.build()
        self.assertTrue(plan["reused"])
        self.assertFalse([c for c, _ in self.calls if c[0] == "cargo"])
        self.assertFalse(self.worktrees.exists())

    def test_dry_run_builds_nothing(self):
        plan = self.build(dry_run=True)
        self.assertFalse(plan["reused"])
        self.assertEqual(plan["sha"], self.sha)
        self.assertFalse(self.worktrees.exists())
        self.assertFalse(self.calls and any(c[0] == "cargo" for c, _ in self.calls))

    def test_low_disk_refuses_before_creating_a_worktree(self):
        with self.assertRaises(perf.HarnessError) as caught:
            self.build(free_bytes=10 * 1024**3)
        self.assertEqual(caught.exception.name, "insufficient_disk")
        self.assertFalse((self.worktrees / f"perf-{self.sha[:8]}").exists())
        self.assertFalse([c for c, _ in self.calls if c[0] == "cargo"])

    def test_build_uses_its_own_target_dir_and_copies_the_binary(self):
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": "/shared/target"}):
            plan = self.build(keep_worktree=True)
        cargo = [(c, k) for c, k in self.calls if c[0] == "cargo"]
        self.assertEqual(len(cargo), 1)
        command, kwargs = cargo[0]
        self.assertEqual(command[:3], ["cargo", "build", "--locked"])
        self.assertIn("--release", command)
        self.assertEqual(kwargs["env"]["CARGO_INCREMENTAL"], "0")
        self.assertNotIn("CARGO_TARGET_DIR", kwargs["env"])
        self.assertEqual(kwargs["cwd"], str(self.worktrees / f"perf-{self.sha[:8]}"))
        self.assertEqual(plan["binary"].read_text(), "fake engine")
        self.assertTrue(os.access(plan["binary"], os.X_OK))
        self.assertTrue((self.worktrees / f"perf-{self.sha[:8]}").exists())

    def test_worktree_is_removed_by_default(self):
        self.build()
        self.assertFalse((self.worktrees / f"perf-{self.sha[:8]}").exists())

    def test_unknown_ref_is_a_named_error(self):
        with self.assertRaises(perf.HarnessError) as caught:
            perf.build("no-such-ref", repo=self.repo, worktree_root=self.worktrees, bin_dir=self.bins,
                       runner=self.runner)
        self.assertEqual(caught.exception.name, "unknown_ref")

    def test_failed_build_is_a_named_error(self):
        def failing(command, **kwargs):
            if command[0] == "cargo":
                return subprocess.CompletedProcess(command, 101, "", "error: boom")
            return perf._run(command, **kwargs)

        with self.assertRaises(perf.HarnessError) as caught:
            self.build(runner=failing, keep_worktree=True)
        self.assertEqual(caught.exception.name, "build_failed")
        self.assertFalse((self.bins / f"engine-{self.sha[:8]}").exists())


class SummaryTest(unittest.TestCase):
    def test_summary_lists_primary_metrics_and_refusals(self):
        verdict = {
            "base": {"path": "b", "sha256": "a" * 64}, "cand": {"path": "c", "sha256": "a" * 64},
            "pairs": 3, "min_effect": 0.02, "seed": 1, "same_binary": True, "overall": "refused",
            "preflight": [{"name": "ac_power", "status": "fail", "detail": "battery"}],
            "scenarios": {
                "dense": {"status": "refused", "error": "inventory_mismatch", "details": ["mesh_entities: 1 vs 2"]},
                "synthetic": {"status": "ok", "pairs_used": 3, "pairs_requested": 3, "summary_verdict": "noise",
                              "metrics": {"frame_ms_mean": {"verdict": "noise", "base_median": 10.0,
                                                            "cand_median": 10.0, "ratio_median": 1.0,
                                                            "ci_lo": 0.99, "ci_hi": 1.01},
                                          "cpu/x": {"verdict": "noise", "base_median": 1.0, "cand_median": 1.0,
                                                    "ratio_median": 1.0, "ci_lo": 0.9, "ci_hi": 1.1}}},
            },
        }
        text = perf.summary_markdown(verdict)
        self.assertIn("REFUSED `inventory_mismatch`", text)
        self.assertIn("| frame_ms_mean |", text)
        self.assertNotIn("| cpu/x |", text)
        self.assertIn("noise-floor run", text)
        self.assertIn("preflight fail", text)


if __name__ == "__main__":
    unittest.main()
