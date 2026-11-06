"""Exercise research byte pins through Git's checkout conversion on any host."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
CORPUS = "docs/research/skyrim-render-map/"
LOADER = CORPUS + "native-shader-loader.json"
VENDOR_INPUT = "vendor/project-wormhole-nif/src/bs/vertex.rs"
CHECKER = "scripts/check-skyrim-render-map.py"
NOTES_CHECKER = "scripts/check-skyrim-research-notes.py"


def run(*args, cwd=ROOT, env=None, input=None):
    return subprocess.run(args, cwd=cwd, env=env, input=input,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)


class ResearchLineEndingTests(unittest.TestCase):
    def test_autocrlf_checkout_preserves_research_pins(self):
        # Read the real index/objects only. All conversion and index updates take
        # place in a disposable repository; no real files are staged or rewritten.
        head = run("git", "rev-parse", "HEAD").stdout.strip().decode("ascii")
        object_dir = run("git", "rev-parse", "--git-path", "objects").stdout.strip().decode()
        object_dir = str((ROOT / object_dir).resolve())
        tracked = run("git", "ls-files", "-z").stdout.split(b"\0")
        paths = [p for p in tracked if p and (
            p.startswith((b"docs/research/", b"docs/specs/")) or p.decode() == VENDOR_INPUT or
            p.decode().startswith(("scripts/check-skyrim-", "scripts/build-skyrim-")))]
        attributes = (ROOT / ".gitattributes").read_bytes()
        expected = run("git", "show", f"{head}:{LOADER}").stdout

        with tempfile.TemporaryDirectory(prefix="mudcrab-research-eol-") as directory:
            checkout = Path(directory)
            env = dict(os.environ)
            for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE",
                         "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"):
                env.pop(name, None)
            env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                       GIT_ALTERNATE_OBJECT_DIRECTORIES=object_dir)

            def git(*args, input=None):
                return run("git", *args, cwd=checkout, env=env, input=input).stdout

            def restore():
                for path in paths:
                    (checkout / path.decode()).unlink(missing_ok=True)
                git("checkout-index", "--force", "--stdin", "-z",
                    input=b"\0".join(paths) + b"\0")

            def check(script):
                return subprocess.run([sys.executable, str(checkout / script)],
                                      cwd=checkout, env=env, capture_output=True)

            git("init", "--quiet")
            git("config", "core.autocrlf", "false")
            git("config", "core.attributesfile", os.devnull)
            git("read-tree", head)
            # Keep the negative control valid after .gitattributes is committed.
            git("update-index", "--force-remove", ".gitattributes")
            restore()
            baseline_notes = check(NOTES_CHECKER)
            baseline = json.loads(baseline_notes.stdout)
            self.assertTrue(baseline["index_consistent"])

            # Negative control: reproduce the reviewer's clean CRLF checkout.
            git("config", "core.autocrlf", "true")
            restore()
            converted = (checkout / LOADER).read_bytes()
            self.assertEqual(converted, expected.replace(b"\n", b"\r\n"))
            self.assertNotEqual(hashlib.sha256(converted).digest(),
                                hashlib.sha256(expected).digest())
            self.assertEqual(git("diff", "--exit-code", "--", LOADER), b"")
            broken = check(CHECKER)
            self.assertNotEqual(broken.returncode, 0)
            self.assertIn(b"ValueError: Stale native group proof", broken.stderr)

            # Install the proposed attributes in the temporary index, then do a
            # fresh checkout from the original LF blobs with autocrlf still true.
            blob = git("hash-object", "-w", "--stdin", input=attributes).strip().decode("ascii")
            git("update-index", "--add", "--cacheinfo", f"100644,{blob},.gitattributes")
            git("checkout-index", "--force", ".gitattributes")
            restore()
            self.assertEqual((checkout / LOADER).read_bytes(), expected)
            self.assertEqual(git("diff", "--exit-code", "--", LOADER), b"")

            # Check every covered research dependency, including nested receipts,
            # supplied Markdown/TXT, historical review snapshots and helper scripts.
            attrs = git("check-attr", "--stdin", "-z", "eol",
                        input=b"\0".join(paths) + b"\0").split(b"\0")
            covered = [attrs[i] for i in range(0, len(attrs) - 1, 3)
                       if attrs[i + 2] == b"lf"]
            self.assertGreater(len(covered), 78)
            for path in covered:
                with self.subTest(path=path.decode()):
                    self.assertEqual((checkout / path.decode()).read_bytes(),
                                     git("show", f"{head}:{path.decode()}"))

            fixed = check(CHECKER)
            self.assertEqual(fixed.returncode, 0, fixed.stderr.decode(errors="replace"))
            summary = json.loads(fixed.stdout)
            self.assertFalse(summary["original_pe_and_embedded_bytes_checked"])
            self.assertFalse(summary["original_shader_package_records_checked"])
            self.assertFalse(summary["native_output_transfer"]["private_exports_checked"])
            fixed_notes = check(NOTES_CHECKER)
            notes = json.loads(fixed_notes.stdout)
            self.assertEqual(fixed_notes.returncode, baseline_notes.returncode)
            self.assertTrue(notes["index_consistent"])
            self.assertEqual(notes["errors"], baseline["errors"])

            # No checkout policy for game binaries or historical source pins.
            for path in [CORPUS + "fixture.exe", CORPUS + "fixture.bsa",
                         "crates/converter/src/material.rs"]:
                self.assertEqual(git("check-attr", "eol", "--", path).strip(),
                                 f"{path}: eol: unspecified".encode())

    def test_digest_keeps_raw_binary_and_text_bytes(self):
        spec = importlib.util.spec_from_file_location("render_map_checker", ROOT / CHECKER)
        checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(checker)
        with tempfile.TemporaryDirectory(prefix="mudcrab-research-digest-") as directory:
            for name, raw in [("fixture.exe", b"MZ\0\n\r\n\xff"),
                              ("fixture.json", b'{"text": true}\r\n')]:
                path = Path(directory) / name
                path.write_bytes(raw)
                self.assertEqual(checker.digest(path), hashlib.sha256(raw).hexdigest())
                self.assertNotEqual(checker.digest(path),
                                    hashlib.sha256(raw.replace(b"\r\n", b"\n")).hexdigest())


if __name__ == "__main__":
    unittest.main()
