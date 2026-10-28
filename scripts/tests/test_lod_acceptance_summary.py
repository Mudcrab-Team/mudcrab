"""The LOD evidence CLI must compare its result with the committed proof."""

import contextlib
import copy
import io
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest import mock


SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "docs/research/lod-acceptance-20261005/verify.py"
)
verifier = types.ModuleType("lod_acceptance_verifier")
verifier.__file__ = str(SCRIPT)
if sys.platform != "win32":
    exec(compile(SCRIPT.read_text(), str(SCRIPT), "exec"), verifier.__dict__)


@unittest.skipIf(sys.platform == "win32", "Fiji verifier requires POSIX package locks")
class LodAcceptanceSummaryTests(unittest.TestCase):
    def setUp(self):
        self.summary = json.loads(SCRIPT.with_name("summary.json").read_text())
        self.metadata = copy.deepcopy(self.summary)
        del self.metadata["published_hash_check"]

    def invoke(self, result, published=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = [str(SCRIPT), str(root)]
            if published is not None:
                argv += ["--assets", str(root)]
            output = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(
                    verifier, "check_evidence", return_value=(result, {}, {})
                ),
                mock.patch.object(
                    verifier, "check_published", return_value=published
                ),
                contextlib.redirect_stdout(output),
            ):
                verifier.main()
            return json.loads(output.getvalue())

    def test_metadata_only_accepts_the_committed_summary(self):
        self.assertEqual(self.invoke(self.metadata), self.metadata)

    def test_matching_altered_cold_warm_fingerprints_are_rejected(self):
        # check_evidence can accept matching altered inputs in both runs. The
        # resulting digest still has to match the independently committed proof.
        self.metadata["chunk_inputs_digest"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "committed summary"):
            self.invoke(self.metadata)

    def test_changed_source_bytes_are_rejected(self):
        self.metadata["source_sha256"]["measurement-v4.json"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "committed summary"):
            self.invoke(self.metadata)

    def test_published_output_hashes_are_compared_when_requested(self):
        published = copy.deepcopy(self.summary["published_hash_check"])
        self.assertEqual(self.invoke(copy.deepcopy(self.metadata), published), self.summary)
        published["lod_manifest_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "committed summary"):
            self.invoke(self.metadata, published)


if __name__ == "__main__":
    unittest.main()
