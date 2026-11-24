"""Synthetic wrapper regressions for explicit default-cache guard paths."""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import run


class WrapperTests(unittest.TestCase):
    """Exercise argument handling without launching either dump executable."""

    def invoke(self, root, extra=(), environment=None):
        """Provide synthetic inputs and intercept all executable invocations."""
        cells = root / "cells.csv"
        cells.write_text("0,0\n", encoding="utf-8")
        arguments = ["run.py"]
        for name in ("ours-bin", "grass-cache", "skytool", "cache", "world"):
            arguments += ["--" + name, str(cells)]
        arguments += ["--cells", str(cells), "--out", str(root / "output"), *extra]
        with patch.dict(os.environ, environment or {}, clear=True), patch.object(sys, "argv", arguments), \
                patch.object(run.subprocess, "run") as execute, contextlib.redirect_stdout(io.StringIO()):
            execute.return_value.returncode = 0
            run.main()
            self.assertEqual(execute.call_count, 3)

    def test_unset_localappdata_has_friendly_error(self):
        """A missing platform environment requires an explicit guard path."""
        with tempfile.TemporaryDirectory() as temporary, contextlib.redirect_stderr(io.StringIO()) as error:
            with self.assertRaises(SystemExit) as raised:
                self.invoke(Path(temporary))
            self.assertEqual(raised.exception.code, 2)
            self.assertIn("--local-appdata", error.getvalue())
            self.assertFalse((Path(temporary) / "output").exists())

    def test_explicit_path_works_without_localappdata(self):
        """An explicit path supports platforms without LOCALAPPDATA."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.invoke(root, ["--local-appdata", str(root / "local")])
            self.assertTrue((root / "output" / "receipt.json").exists())

    def test_environment_path_remains_the_default(self):
        """Existing Windows invocations retain their configured guard."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.invoke(root, environment={"LOCALAPPDATA": str(root / "local")})

    def test_existing_default_cache_refuses_execution(self):
        """Explicit guard paths preserve the default-cache refusal."""
        with tempfile.TemporaryDirectory() as temporary, contextlib.redirect_stderr(io.StringIO()) as error:
            root = Path(temporary)
            (root / "local" / "spud").mkdir(parents=True)
            with self.assertRaises(SystemExit) as raised:
                self.invoke(root, ["--local-appdata", str(root / "local")])
            self.assertEqual(raised.exception.code, 2)
            self.assertIn("default spud cache already exists", error.getvalue())
            self.assertFalse((root / "output").exists())


if __name__ == "__main__":
    unittest.main()
