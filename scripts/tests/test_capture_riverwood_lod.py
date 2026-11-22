from contextlib import closing
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "capture-riverwood-lod.sh"
SCHEMAS = SCRIPT.parents[1] / "crates/shared"
SCHEMA_FILES = ("converter-schema-version.txt", "world-database-schema-version.txt")
PROVENANCE_FILES = (
    "bin/engine",
    "assets/skyrim_world.db",
    "assets/cell_cache.rkyv",
    "assets/lod-manifest.json",
    "assets/conversion-manifest.json",
    "assets/integration-report.json",
)
POSTFLIGHT_COUNTERS = (
    "pending_lod_chunks",
    "failed_lod_chunks",
    "pending_lod_queries",
    "failed_lod_queries",
    "failed_cells",
    "loading_cells",
    "pending_asset_instances",
    "pending_surface_instances",
    "asset_load_failures",
    "material_validation_failures",
    "streaming_invariant_failures",
    "diagnostic_fallbacks",
)


LAUNCHER = """#!/usr/bin/env python3
import argparse
from pathlib import Path
import shutil
import sys

parser = argparse.ArgumentParser()
parser.add_argument("--acceptance-screenshot", type=Path, required=True)
parser.add_argument("--benchmark-output", type=Path, required=True)
parser.add_argument("--profile-output", type=Path, required=True)
parser.add_argument("--profile-scenario", required=True)
parser.add_argument("--profile-commit", required=True)
parser.add_argument("--profile-dirty-worktree", action="store_true")
parser.add_argument("--stream-radius", required=True)
args, remaining = parser.parse_known_args()
if "--log-file" in remaining:
    parser.error("engine does not support --log-file")
fixture = Path(__file__).parent / "capture-fixture"
name = args.profile_scenario
shutil.copyfile(fixture / f"{name}.png", args.acceptance_screenshot)
benchmark = fixture / f"{name}-benchmark.json"
if benchmark.exists():
    shutil.copyfile(benchmark, args.benchmark_output)
shutil.copytree(fixture / f"{name}-profile", args.profile_output)
print(f"radius={args.stream_radius} commit={args.profile_commit} dirty={args.profile_dirty_worktree}")
print("launcher stderr", file=sys.stderr)
"""


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def refresh_checksum(package, relative):
    path = package / "build-provenance.json"
    build = json.loads(path.read_text(encoding="utf-8"))
    build["checksums"][relative] = sha256(package / relative)
    write_json(path, build)


def make_package(package, converter_schema, world_schema):
    identity = "a" * 64
    (package / "bin").mkdir(parents=True)
    (package / "assets").mkdir()
    launcher = package / "run-riverwood-lod.sh"
    launcher.write_text(LAUNCHER, encoding="utf-8")
    launcher.chmod(0o755)
    fixture = package / "capture-fixture"
    fixture.mkdir()
    make_postflight_fixture(fixture)
    (package / "bin/engine").write_bytes(b"engine-binary-fixture")
    (package / "assets/cell_cache.rkyv").write_bytes(b"cell-cache-fixture")
    write_json(
        package / "assets/lod-manifest.json",
        {
            "converter_schema": converter_schema,
            "world_database_schema": world_schema,
            "build_identity": identity,
            "chunks": 1,
        },
    )
    write_json(
        package / "assets/conversion-manifest.json",
        {"schema_version": converter_schema, "complete": True},
    )
    write_json(
        package / "assets/integration-report.json",
        {"schema_version": world_schema, "passed": True},
    )
    with closing(sqlite3.connect(package / "assets/skyrim_world.db")) as database, database:
        database.execute("CREATE TABLE schema_info(version INTEGER NOT NULL)")
        database.execute("INSERT INTO schema_info(version) VALUES (?)", (world_schema,))
        database.execute(
            "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity TEXT NOT NULL)"
        )
        database.execute(
            "INSERT INTO lod_build(id, build_identity) VALUES (1, ?)", (identity,)
        )
    build = {
        "format_version": 1,
        "commit": "b" * 40,
        "dirty_worktree": False,
        "lod_build_identity": identity,
        "checksums": {
            relative: sha256(package / relative) for relative in PROVENANCE_FILES
        },
    }
    write_json(package / "build-provenance.json", build)
    return package


def make_postflight_fixture(root):
    for name in ("riverwood-radius2", "riverwood-radius0"):
        (root / f"{name}.png").write_bytes(b"screenshot-fixture")
        write_json(root / f"{name}-benchmark.json", {"passed": True, "scenario": name})
        profile = root / f"{name}-profile/streaming.json"
        write_json(
            profile,
            {
                "aggregate": {
                    **{key: 0 for key in POSTFLIGHT_COUNTERS},
                    "lod_chunks_ready": 1,
                    "visible_lod_terrain_patches": 1,
                }
            },
        )


class CaptureRiverwoodGateTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.output = self.root / "capture"
        self.icd = self.root / "radeon_icd.x86_64.json"
        self.icd.touch()
        self.versions = tuple(int((SCHEMAS / name).read_text()) for name in SCHEMA_FILES)

    def package(self, name="package"):
        return make_package(self.root / name, *self.versions)

    def run_capture(self, package, output=None, *, script=SCRIPT):
        return subprocess.run(
            ["bash", str(script), str(package), str(output or self.output)],
            env={**os.environ, "RIVERWOOD_RADEON_ICD": str(self.icd)},
            capture_output=True,
            text=True,
            check=False,
        )

    def assert_rejected_before_capture(self, package, message, output=None, *, script=SCRIPT):
        output = output or self.output
        result = self.run_capture(package, output, script=script)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(message, result.stderr)
        self.assertFalse(output.exists())

    def test_lod_only20_world6_is_not_combined_contract(self):
        package = make_package(self.root / "package", 20, 6)
        self.assert_rejected_before_capture(package, "Unsupported or incomplete converter package")

    def test_lod_capture_rejects_mixed_conversion_producer(self):
        package = self.package()
        relative = "assets/conversion-manifest.json"
        write_json(package / relative, {"schema_version": 20, "complete": True})
        refresh_checksum(package, relative)
        self.assert_rejected_before_capture(package, "Unsupported or incomplete converter package")

    def test_schema_bumps_flow_from_shared_contract_to_capture(self):
        converter, world = self.versions
        for versions, rejection in (
            ((converter + 1, world), "Unsupported or incomplete converter package"),
            ((converter, world + 1), "Unsupported LOD schemas"),
        ):
            with self.subTest(versions=versions):
                case = self.root / f"schema-{versions[0]}-{versions[1]}"
                repo = case / "checkout"
                schemas = repo / "crates/shared"
                schemas.mkdir(parents=True)
                script = repo / "scripts" / SCRIPT.name
                script.parent.mkdir()
                shutil.copyfile(SCRIPT, script)
                for name, version in zip(SCHEMA_FILES, versions, strict=True):
                    (schemas / name).write_text(f"{version}\n", encoding="utf-8")
                old_package = make_package(case / "old-package", *self.versions)
                output = case / "capture"
                self.assert_rejected_before_capture(old_package, rejection, output, script=script)
                package = make_package(case / "package", *versions)
                result = self.run_capture(package, output, script=script)
                self.assertEqual(result.returncode, 0, result.stderr)
                report = json.loads((output / "capture-report.json").read_text())
                self.assertIs(report["passed"], True)

    def test_valid_package_produces_report_and_launcher_logs(self):
        package = self.package()
        result = self.run_capture(package)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.output / "capture-report.json").read_text())
        self.assertIs(report["passed"], True)
        self.assertEqual(report["errors"], [])
        for radius in (2, 0):
            log = (self.output / f"riverwood-radius{radius}.log").read_text().splitlines()
            self.assertIn(f"radius={radius} commit={'b' * 40} dirty=False", log)
            self.assertIn("launcher stderr", log)

    def test_provenance_requires_format_commit_boolean_and_checksum_map(self):
        invalid_cases = (
            ("format version", lambda build: build.update(format_version=2), "Unsupported build provenance"),
            ("commit", lambda build: build.update(commit="not-a-commit"), "Invalid build commit"),
            ("boolean type", lambda build: build.update(dirty_worktree=1), "Invalid dirty_worktree flag"),
            ("checksum map", lambda build: build.update(checksums=[]), "Missing build checksums"),
            (
                "required checksum",
                lambda build: build["checksums"].pop("bin/engine"),
                "Invalid build checksum: bin/engine",
            ),
        )
        for label, mutate, expected in invalid_cases:
            with self.subTest(label=label):
                package = self.package(label.replace(" ", "-"))
                path = package / "build-provenance.json"
                build = json.loads(path.read_text(encoding="utf-8"))
                mutate(build)
                write_json(path, build)
                self.assert_rejected_before_capture(package, expected)

    def test_swapped_engine_binary_fails_checksum_validation(self):
        package = self.package()
        (package / "bin/engine").write_bytes(b"different-engine-binary")
        self.assert_rejected_before_capture(package, "Build checksum mismatch: bin/engine")

    def test_metadata_schema_and_integration_gates(self):
        cases = (
            (
                "converter schema",
                "assets/lod-manifest.json",
                lambda value: value.update(converter_schema=17),
                "Unsupported LOD schemas",
            ),
            (
                "database schema",
                "assets/lod-manifest.json",
                lambda value: value.update(world_database_schema=5),
                "Unsupported LOD schemas",
            ),
            (
                "integration schema",
                "assets/integration-report.json",
                lambda value: value.update(schema_version=4),
                "Asset integration did not pass",
            ),
            (
                "integration result type",
                "assets/integration-report.json",
                lambda value: value.update(passed=1),
                "Asset integration did not pass",
            ),
        )
        for label, relative, mutate, expected in cases:
            with self.subTest(label=label):
                package = self.package(label.replace(" ", "-"))
                path = package / relative
                metadata = json.loads(path.read_text(encoding="utf-8"))
                mutate(metadata)
                write_json(path, metadata)
                refresh_checksum(package, relative)
                self.assert_rejected_before_capture(package, expected)

    def test_database_identity_mismatch_fails(self):
        package = self.package()
        with closing(sqlite3.connect(package / "assets/skyrim_world.db")) as database, database:
            database.execute(
                "UPDATE lod_build SET build_identity = ? WHERE id = 1", ("c" * 64,)
            )
        refresh_checksum(package, "assets/skyrim_world.db")
        self.assert_rejected_before_capture(package, "Database and manifest LOD identities differ")

    def test_database_schema_mismatch_fails(self):
        package = self.package()
        with closing(sqlite3.connect(package / "assets/skyrim_world.db")) as database, database:
            database.execute("UPDATE schema_info SET version = 4")
        refresh_checksum(package, "assets/skyrim_world.db")
        self.assert_rejected_before_capture(package, "Database and manifest LOD identities differ")

    def test_manifest_identity_mismatch_fails(self):
        package = self.package()
        relative = "assets/lod-manifest.json"
        path = package / relative
        manifest = json.loads(path.read_text(encoding="utf-8"))
        manifest["build_identity"] = "c" * 64
        write_json(path, manifest)
        refresh_checksum(package, relative)
        self.assert_rejected_before_capture(package, "Database and manifest LOD identities differ")

    def test_nested_output_is_rejected_before_writes(self):
        package = self.root / "package"
        package.mkdir()
        outputs = [
            package / "new-capture",
            package / "missing-parent" / "new-capture",
        ]
        if sys.platform != "win32":
            alias = self.root / "package-alias"
            alias.symlink_to(package, target_is_directory=True)
            outputs.append(alias / "new-capture")
        for output in outputs:
            with self.subTest(output=output):
                self.assert_rejected_before_capture(
                    package, "Capture output must be outside the package", output
                )
        self.assertEqual(list(package.iterdir()), [])

    def test_missing_benchmark_fails_despite_zero_exit_and_ready_profile(self):
        package = self.package()
        (package / "capture-fixture/riverwood-radius0-benchmark.json").unlink()
        result = self.run_capture(package)
        self.assertNotEqual(result.returncode, 0)
        report = json.loads((self.output / "capture-report.json").read_text())
        self.assertIn(
            "riverwood-radius0: missing or invalid benchmark report", report["errors"]
        )
        self.assertEqual(
            (self.output / "riverwood-radius0-exit-code.txt").read_text().strip(), "0"
        )

    def test_postflight_rejects_nonpassing_or_mislabeled_benchmark(self):
        cases = (
            ("not-passed", {"passed": False}),
            ("wrong-scenario", {"scenario": "other-run"}),
        )
        for label, changes in cases:
            with self.subTest(label=label):
                package = self.package(label)
                output = self.root / f"capture-{label}"
                benchmark = package / "capture-fixture/riverwood-radius2-benchmark.json"
                report = json.loads(benchmark.read_text(encoding="utf-8"))
                report.update(changes)
                write_json(benchmark, report)
                result = self.run_capture(package, output)
                self.assertNotEqual(result.returncode, 0)
                capture_report = json.loads(
                    (output / "capture-report.json").read_text(encoding="utf-8")
                )
                self.assertIn(
                    "riverwood-radius2: benchmark acceptance did not pass",
                    capture_report["errors"],
                )

    def test_postflight_rejects_every_nonzero_failure_or_pending_counter(self):
        package = self.package()
        profile = package / "capture-fixture/riverwood-radius2-profile/streaming.json"
        data = json.loads(profile.read_text(encoding="utf-8"))
        for key in POSTFLIGHT_COUNTERS:
            data["aggregate"][key] = 1
        write_json(profile, data)
        result = self.run_capture(package)
        self.assertNotEqual(result.returncode, 0)
        report = json.loads((self.output / "capture-report.json").read_text())
        for key in POSTFLIGHT_COUNTERS:
            self.assertIn(f"riverwood-radius2: {key}=1", report["errors"])

    def test_postflight_rejects_missing_counter(self):
        package = self.package()
        profile = package / "capture-fixture/riverwood-radius0-profile/streaming.json"
        data = json.loads(profile.read_text(encoding="utf-8"))
        del data["aggregate"]["pending_lod_queries"]
        write_json(profile, data)
        result = self.run_capture(package)
        self.assertNotEqual(result.returncode, 0)
        report = json.loads((self.output / "capture-report.json").read_text())
        self.assertIn("riverwood-radius0: pending_lod_queries=None", report["errors"])


if __name__ == "__main__":
    unittest.main()
