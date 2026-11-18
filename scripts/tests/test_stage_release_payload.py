"""Checks the release payload that scripts/stage-release.sh builds.

The payload is what every archive and the Windows wizard install, so a missing
binary or a missing licence notice is a release defect. The test runs the real
script against a throwaway repository tree: no Cargo build, no network and no
game data.
"""

import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "stage-release.sh"

LINUX_TARGET = "x86_64-unknown-linux-gnu"
WINDOWS_TARGET = "x86_64-pc-windows-msvc"
BINARIES = ("launcher", "engine", "converter", "world-inspect")
NOTICES = ("LICENSE", "THIRD_PARTY.md", "LEGAL.md")


class PayloadFixture:
    """A repository tree with stub binaries and the real notice files."""

    def __init__(self, target: str) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="mudcrab-payload-")
        self.root = Path(self._tmp.name) / "repo"
        self.target = target
        self.dest = Path(self._tmp.name) / "payload"
        release = self.root / "target" / target / "release"
        release.mkdir(parents=True)
        suffix = ".exe" if "windows" in target else ""
        for name in BINARIES:
            binary = release / f"{name}{suffix}"
            binary.write_text(f"stub {name}\n", encoding="utf-8")
            binary.chmod(0o755)
        for notice in NOTICES:
            shutil.copyfile(REPO_ROOT / notice, self.root / notice)

    def stage(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", str(SCRIPT), "--target", self.target, "--dest", str(self.dest)],
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )

    def cleanup(self) -> None:
        self._tmp.cleanup()


class StageReleasePayloadTest(unittest.TestCase):
    def fixture(self, target: str = LINUX_TARGET) -> PayloadFixture:
        fixture = PayloadFixture(target)
        self.addCleanup(fixture.cleanup)
        return fixture

    def test_payload_holds_binaries_and_notices(self) -> None:
        fixture = self.fixture()
        result = fixture.stage()
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in BINARIES:
            binary = fixture.dest / name
            self.assertTrue(binary.is_file(), f"{name} is missing from the payload")
            self.assertTrue(binary.stat().st_mode & 0o111, f"{name} is not executable")
        for notice in NOTICES:
            self.assertTrue(
                (fixture.dest / notice).is_file(),
                f"{notice} is missing from the payload",
            )

    def test_unix_archive_holds_the_notices(self) -> None:
        """Packs the staged tree the way release.yml does and lists the archive."""
        fixture = self.fixture()
        result = fixture.stage()
        self.assertEqual(result.returncode, 0, result.stderr)
        archive = fixture.dest.parent / "payload.tar.gz"
        subprocess.run(
            "tar -czf ../payload.tar.gz *",
            cwd=fixture.dest,
            shell=True,
            check=True,
        )
        with tarfile.open(archive) as bundle:
            names = {name.removeprefix("./") for name in bundle.getnames()}
        self.assertTrue(set(BINARIES + NOTICES) <= names, sorted(names))

    def test_missing_notice_stops_staging(self) -> None:
        fixture = self.fixture()
        (fixture.root / "THIRD_PARTY.md").unlink()
        result = fixture.stage()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("THIRD_PARTY.md", result.stderr)

    def test_missing_binary_stops_staging(self) -> None:
        fixture = self.fixture()
        (fixture.root / "target" / LINUX_TARGET / "release" / "engine").unlink()
        result = fixture.stage()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("engine", result.stderr)

    def test_windows_target_uses_exe_suffix(self) -> None:
        fixture = self.fixture(WINDOWS_TARGET)
        result = fixture.stage()
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in BINARIES:
            self.assertTrue((fixture.dest / f"{name}.exe").is_file())
        for notice in NOTICES:
            self.assertTrue((fixture.dest / notice).is_file())

    def test_destination_is_replaced_not_merged(self) -> None:
        fixture = self.fixture()
        fixture.dest.mkdir(parents=True)
        (fixture.dest / "stale.bin").write_text("old", encoding="utf-8")
        result = fixture.stage()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((fixture.dest / "stale.bin").exists())


if __name__ == "__main__":
    unittest.main()
