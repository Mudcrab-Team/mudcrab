import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "capture-lod-phase1.sh"
CLEAN_METRICS = {
    "lod_chunks_ready": 1,
    "visible_lod_terrain_patches": 1,
    "failed_lod_chunks": 0,
    "pending_lod_chunks": 0,
    "failed_lod_queries": 0,
    "pending_lod_queries": 0,
}

CARGO = """#!/usr/bin/env python3
import os
from pathlib import Path
import shutil
import sys

def arg(flag):
    return sys.argv[sys.argv.index(flag) + 1]

if arg("-p") == "engine":
    if "--log-file" in sys.argv:
        sys.exit("unsupported engine flag: --log-file")
    Path(arg("--acceptance-screenshot")).write_bytes(b"fixture frame")
    profile = Path(arg("--profile-output"))
    profile.mkdir()
    shutil.copyfile(os.environ["FIXTURE_METRICS"], profile / "streaming.json")
    print(os.environ["CARGO_TARGET_DIR"])
    print(arg("--profile-scenario"), file=sys.stderr)
"""


class Phase1CaptureTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.repo = self.root / "checkout"
        self.script = self.repo / "scripts" / SCRIPT.name
        self.script.parent.mkdir(parents=True)
        shutil.copyfile(SCRIPT, self.script)
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        subprocess.run([
            "git", "-C", str(self.repo), "-c", "user.email=test@example.invalid",
            "-c", "user.name=Test", "commit", "-qm", "fixture", "--allow-empty",
        ], check=True)
        cargo = self.root / "cargo"
        cargo.write_text(CARGO, encoding="utf-8")
        cargo.chmod(0o755)

    def capture(self, output, metrics):
        fixture = self.root / "metrics.json"
        fixture.write_text(json.dumps({"aggregate": metrics}), encoding="utf-8")
        env = {
            **os.environ,
            "PATH": str(self.root) + os.pathsep + os.environ["PATH"],
            "DISPLAY": ":fixture",
            "TMPDIR": str(self.root),
            "FIXTURE_METRICS": str(fixture),
        }
        env.pop("CARGO_TARGET_DIR", None)
        return subprocess.run(
            ["bash", str(self.script), str(output)], env=env,
            capture_output=True, text=True, check=False,
        )

    def test_capture_keeps_both_engine_logs_and_uses_the_checkout_target(self):
        output = self.root / "capture"
        result = self.capture(output, CLEAN_METRICS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((output / "capture-manifest.txt").is_file())
        for name in ("full-detail", "terrain-lod"):
            log = (output / f"{name}.log").read_text().splitlines()
            self.assertIn(str(self.repo / "target"), log)
            self.assertIn(name, log)

    def test_capture_rejects_failed_pending_or_missing_query_counts(self):
        for key in ("failed_lod_queries", "pending_lod_queries"):
            for value in (1, None):
                with self.subTest(key=key, value=value):
                    metrics = CLEAN_METRICS.copy()
                    if value is None:
                        del metrics[key]
                    else:
                        metrics[key] = value
                    output = self.root / f"{key}-{value}"
                    result = self.capture(output, metrics)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(f"LOD capture gate failed: {key}=", result.stderr)
                    self.assertFalse((output / "capture-manifest.txt").exists())
