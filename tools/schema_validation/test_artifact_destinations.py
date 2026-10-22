import os
import tempfile
import unittest
from contextlib import contextmanager, redirect_stderr
from io import StringIO
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import p0_tools
import run_mutagen_p0
import run_xedit_p0


@contextmanager
def configured_environment(values):
    # Keep PATH and Windows process-launch variables while isolating root config.
    """Isolate protected-root settings while preserving process-launch variables."""
    with mock.patch.dict(os.environ):
        for name in p0_tools.PROTECTED_ROOT_ENVIRONMENT:
            os.environ.pop(name, None)
        os.environ.update(values)
        yield


class ConfiguredArtifactDestinationTests(unittest.TestCase):
    def test_configured_external_roots_reject_descendants_and_ancestors(self):
        """Verify configured roots reject overlapping artifacts and allow separate paths."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            for name in p0_tools.PROTECTED_ROOT_ENVIRONMENT:
                protected = root / name / "source"
                protected.mkdir(parents=True)
                with self.subTest(name=name), configured_environment({name: str(protected)}):
                    for candidate in (protected, protected / "artifacts", protected.parent):
                        with self.subTest(candidate=candidate), self.assertRaisesRegex(
                            p0_tools.QualificationError, "protected path"
                        ):
                            p0_tools.validate_artifact_destination(candidate, root / "oracle")
                    p0_tools.validate_artifact_destination(root / "safe", root / "oracle")
                self.assertEqual(list(protected.iterdir()), [])

    def test_configured_symlink_alias_is_protected(self):
        """Verify a configured directory alias protects its resolved target."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            protected = root / "store"
            protected.mkdir()
            alias = root / "alias"
            try:
                alias.symlink_to(protected, target_is_directory=True)
            except OSError as exc:
                self.skipTest(f"directory symlinks unavailable: {exc}")
            with configured_environment({"MCRAB_STORE": str(alias)}):
                with self.assertRaisesRegex(p0_tools.QualificationError, "protected path"):
                    p0_tools.validate_artifact_destination(protected / "artifacts", root / "oracle")
            self.assertFalse((protected / "artifacts").exists())

    def test_empty_or_relative_configuration_fails_closed(self):
        """Verify invalid protected-root settings cannot permit artifact writes."""
        for name in p0_tools.PROTECTED_ROOT_ENVIRONMENT:
            for value in ("", " ", "relative/store"):
                with self.subTest(name=name, value=value), configured_environment({name: value}):
                    with self.assertRaisesRegex(p0_tools.QualificationError, "absolute protected directory"):
                        p0_tools.validate_artifact_destination(Path("/unused/artifacts"), Path("/unused/oracle"))

    def test_mutagen_cli_protects_each_explicit_root_before_tool_lookup(self):
        """Verify repeated CLI roots are protected before resolving the SDK."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            sources = (root / "research", root / "store")
            for source in sources:
                source.mkdir()
            for source in sources:
                candidate = source / "artifacts"
                with self.subTest(source=source), mock.patch.object(
                    run_mutagen_p0, "_resolve_dotnet"
                ) as resolve, redirect_stderr(StringIO()):
                    result = run_mutagen_p0.main([
                        "--oracle-source", str(root / "oracle"),
                        "--artifact-dir", str(candidate),
                        "--protected-root", str(sources[0]),
                        "--protected-root", str(sources[1]),
                    ])
                    self.assertEqual(result, 1)
                    resolve.assert_not_called()
                self.assertFalse(candidate.exists())

    def test_mutagen_programmatic_entry_protects_explicit_roots(self):
        """Verify the Mutagen API rejects protected artifacts before SDK lookup."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "research"
            source.mkdir()
            args = SimpleNamespace(artifact_dir=str(source / "artifacts"),
                                   oracle_source=str(root / "oracle"), dotnet=None,
                                   protected_root=[source])
            with mock.patch.object(run_mutagen_p0, "_resolve_dotnet") as resolve:
                with self.assertRaisesRegex(p0_tools.QualificationError, "protected path"):
                    run_mutagen_p0.run_suite(args)
                resolve.assert_not_called()
            self.assertEqual(list(source.iterdir()), [])

    def test_xedit_protects_explicit_roots_before_tool_read(self):
        """Verify xEdit rejects protected artifacts before reading its tool binary."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "store"
            source.mkdir()
            args = SimpleNamespace(artifact_dir=str(source / "artifacts"),
                                   xdump=str(root / "tool" / "xDump64.exe"), wine="wine",
                                   protected_root=[source])
            with mock.patch.object(run_xedit_p0, "_read_verified_bytes") as read:
                with self.assertRaisesRegex(p0_tools.QualificationError, "protected path"):
                    run_xedit_p0.run_probe(args)
                read.assert_not_called()
            self.assertEqual(list(source.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
