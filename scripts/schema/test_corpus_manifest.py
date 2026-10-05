import errno
import hashlib
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import json
import struct
import tempfile
import unittest
import zlib
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import corpus_manifest as manifest_tool


BASE_PLUGINS = (
    "Skyrim.esm",
    "Update.esm",
    "Dawnguard.esm",
    "HearthFires.esm",
    "Dragonborn.esm",
)


def subrecord(signature: bytes, data: bytes) -> bytes:
    return signature + struct.pack("<H", len(data)) + data


def tes4_plugin(masters=(), extra_subrecords=()) -> bytes:
    header_data = bytearray(subrecord(b"HEDR", struct.pack("<fII", 1.7, 0, 0x800)))
    for master in masters:
        header_data += subrecord(b"MAST", master.encode("ascii") + b"\0")
        header_data += subrecord(b"DATA", b"\0" * 8)
    for raw in extra_subrecords:
        header_data += raw
    record_header = (
        b"TES4"
        + struct.pack("<I", len(header_data))
        + struct.pack("<I", 0)
        + struct.pack("<I", 0)
        + struct.pack("<I", 0)
        + struct.pack("<H", 44)
        + struct.pack("<H", 0)
    )
    return record_header + header_data


def make_tree(root: Path, plugins=None, executable=b"test runtime", ccc=b""):
    game_root = root / "game"
    data_root = game_root / "Data"
    data_root.mkdir(parents=True)
    (game_root / "SkyrimSE.exe").write_bytes(executable)
    (game_root / "Skyrim.ccc").write_bytes(ccc)
    if plugins is None:
        plugins = {name: () for name in BASE_PLUGINS}
    for name, masters in plugins.items():
        path = data_root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(tes4_plugin(masters=masters))
    return game_root, data_root, game_root / "SkyrimSE.exe", game_root / "Skyrim.ccc"


def make_corpus_evidence(root: Path, active_plugins, unloaded_optional_plugins=(), target=None):
    evidence_root = root / "evidence"
    evidence_root.mkdir(parents=True, exist_ok=True)
    artifact_bytes = {
        "profile-source.txt": b"Steam Skyrim SE/AE build 24914197 profile evidence\n",
        "locale-source.ini": b"[General]\nsLanguage=ENGLISH\n",
        "load-order-source.txt": (
            "\n".join(f"*{name}" for name in active_plugins) + "\n"
        ).encode("utf-8"),
    }
    for name, raw in artifact_bytes.items():
        (evidence_root / name).write_bytes(raw)

    target = target or {
        "game": manifest_tool.TARGET_RUNTIME["game"],
        "executable_version": manifest_tool.TARGET_RUNTIME["executable_version"],
        "steam_build": manifest_tool.TARGET_RUNTIME["steam_build"],
    }

    def artifact(name):
        return {
            "path": name,
            "sha256": hashlib.sha256(artifact_bytes[name]).hexdigest(),
        }

    descriptor = {
        "schema_version": manifest_tool.CORPUS_EVIDENCE_VERSION,
        "corpus_profile": {
            "id": "steam-se-ae-build-24914197-fixture",
            "target": target,
            "evidence": artifact("profile-source.txt"),
        },
        "locale": {"value": "ENGLISH", "evidence": artifact("locale-source.ini")},
        "load_order": {
            "active_plugins": list(active_plugins),
            "unloaded_optional_plugins": list(unloaded_optional_plugins),
            "evidence": artifact("load-order-source.txt"),
        },
    }
    path = evidence_root / "corpus-evidence.json"
    path.write_text(json.dumps(descriptor), encoding="utf-8")
    return path, descriptor


def make_symlink_or_skip(link: Path, target: str | Path, *, target_is_directory=False):
    try:
        link.symlink_to(target, target_is_directory=target_is_directory)
    except OSError as exc:
        unavailable_errnos = {
            errno.EACCES,
            errno.EPERM,
            getattr(errno, "ENOSYS", -1),
            getattr(errno, "ENOTSUP", -1),
            getattr(errno, "EOPNOTSUPP", -1),
        }
        if exc.errno in unavailable_errnos or getattr(exc, "winerror", None) == 1314:
            raise unittest.SkipTest(f"the OS denied or does not support symlinks: {exc}") from exc
        raise


class CorpusManifestTests(unittest.TestCase):
    def test_v150_deep_evidence_json_fails_cleanly_without_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            game, data, executable, _ = make_tree(root)
            evidence = root / "deep.json"
            evidence.write_text("[" * 20000 + "0" + "]" * 20000, encoding="utf-8")
            output = root / "manifest.json"
            stderr = StringIO()
            with redirect_stderr(stderr):
                status = manifest_tool.main([
                    "--game-root", str(game), "--data-dir", str(data),
                    "--executable", str(executable), "--corpus-evidence", str(evidence),
                    "--output", str(output),
                ])
            self.assertEqual(status, 2)
            self.assertIn("invalid corpus evidence JSON", stderr.getvalue())
            self.assertFalse(output.exists())

    def test_hash_and_tes4_metadata_share_verified_read_with_path_provenance(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(
                root,
                {name: () for name in BASE_PLUGINS},
                ccc=b"ccExample.esl\n",
            )
            plugin_path = data_root / "Skyrim.esm"
            raw = plugin_path.read_bytes()

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertEqual(manifest["manifest_version"], 3)
            plugin = next(item for item in manifest["corpus_hashes"]["plugins"] if item["name"] == "Skyrim.esm")
            self.assertEqual(plugin["source"], {"root": "data", "relative_path": "Skyrim.esm"})
            self.assertEqual(plugin["size"], len(raw))
            self.assertEqual(plugin["sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual(plugin["tes4"]["form_version"], 44)
            self.assertEqual(plugin["tes4"]["header"]["record_count"], 0)
            self.assertTrue(manifest["base_plugins"][0]["present"])
            self.assertEqual(manifest["target"]["runtime"]["status"], "mismatch")
            self.assertFalse(manifest["successful_pin"])
            self.assertEqual(manifest["load_order"]["status"], "unresolved")
            self.assertEqual(manifest["locale"]["status"], "unresolved")
            self.assertEqual(manifest["corpus_profile"]["status"], "unresolved")
            self.assertEqual(manifest["archive_observations"]["status"], "no_archives_observed")
            self.assertEqual(manifest["ccc"]["listed_plugin_names"], ["ccExample.esl"])

    def test_missing_base_and_master_are_explicit_and_break_closure(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(
                root,
                {"Skyrim.esm": ("Missing.esm",), "Addon.esl": ("Skyrim.esm",)},
            )

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            missing = {item["name"] for item in manifest["base_plugins"] if not item["present"]}
            self.assertEqual(missing, {"Update.esm", "Dawnguard.esm", "HearthFires.esm", "Dragonborn.esm"})
            self.assertEqual(manifest["dependency_closure"]["missing_masters"], [
                {"plugin": "Skyrim.esm", "master": "Missing.esm"}
            ])
            self.assertFalse(manifest["dependency_closure"]["complete"])
            self.assertIn("missing_base_plugin", {item["code"] for item in manifest["issues"]})

    def test_nested_plugins_are_observed_but_do_not_satisfy_loadable_dependencies_or_bases(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(
                root,
                {"Addon.esl": ("NestedMaster.esm",)},
                ccc=b"NestedMaster.esm\nUpdate.esm\n",
            )
            nested = data_root / "nested"
            nested.mkdir()
            (nested / "NestedMaster.esm").write_bytes(tes4_plugin())
            (nested / "Update.esm").write_bytes(tes4_plugin())

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertEqual(manifest["plugin_count"], 1)
            self.assertEqual([item["name"] for item in manifest["corpus_hashes"]["plugins"]], ["Addon.esl"])
            self.assertEqual(
                [item["source"]["relative_path"] for item in manifest["corpus_hashes"]["nested_plugins"]],
                ["nested/NestedMaster.esm", "nested/Update.esm"],
            )
            self.assertEqual(manifest["dependency_closure"]["missing_masters"], [
                {"plugin": "Addon.esl", "master": "NestedMaster.esm"}
            ])
            update = next(item for item in manifest["base_plugins"] if item["name"] == "Update.esm")
            self.assertFalse(update["present"])
            self.assertEqual(update["paths"], [])
            self.assertEqual(
                {item["code"] for item in manifest["issues"]} & {"nested_plugin_not_loadable"},
                {"nested_plugin_not_loadable"},
            )
            self.assertEqual(manifest["ccc"]["matching_data_plugin_names"], [])
            self.assertEqual(manifest["ccc"]["matching_nested_plugin_names"], ["NestedMaster.esm", "Update.esm"])
            self.assertEqual(manifest["ccc"]["declared_names_only_nested"], ["NestedMaster.esm", "Update.esm"])
            self.assertEqual(manifest["ccc"]["declared_names_missing_from_data"], [])

    def test_empty_plugin_fixture_is_really_empty(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root, {})

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertEqual(manifest["plugin_count"], 0)
            self.assertEqual(manifest["corpus_hashes"]["plugins"], [])

    def test_nested_duplicate_name_does_not_create_loadable_plugin_ambiguity(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(
                root,
                {name: () for name in BASE_PLUGINS} | {"Addon.esl": ()},
            )
            duplicate = data_root / "nested" / "addon.ESL"
            duplicate.parent.mkdir()
            duplicate.write_bytes(tes4_plugin())

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertTrue(manifest["dependency_closure"]["complete"])
            self.assertNotIn("duplicate_casefold_plugin_name", {item["code"] for item in manifest["issues"]})
            self.assertIn("nested_plugin_not_loadable", {item["code"] for item in manifest["issues"]})

    def test_dependency_graph_is_stack_safe_and_reports_a_cycle(self):
        long_chain = []
        for index in range(1500):
            name = f"Plugin{index:04}.esl"
            masters = [] if index == 0 else [f"Plugin{index - 1:04}.esl"]
            long_chain.append({
                "name": name,
                "source": {"relative_path": name},
                "tes4": {"status": "decoded", "masters": masters},
            })
        issues = []

        closure = manifest_tool._dependency_closure(long_chain, issues)

        self.assertTrue(closure["complete"])
        self.assertEqual(issues, [])

        cycle_plugins = [
            {"name": "A.esm", "source": {"relative_path": "A.esm"}, "tes4": {"status": "decoded", "masters": ["B.esm"]}},
            {"name": "B.esm", "source": {"relative_path": "B.esm"}, "tes4": {"status": "decoded", "masters": ["C.esm"]}},
            {"name": "C.esm", "source": {"relative_path": "C.esm"}, "tes4": {"status": "decoded", "masters": ["A.esm"]}},
        ]
        cycle_issues = []

        cycle_closure = manifest_tool._dependency_closure(cycle_plugins, cycle_issues)

        self.assertFalse(cycle_closure["complete"])
        self.assertEqual(len(cycle_closure["cycles"]), 1)
        self.assertEqual(set(cycle_closure["cycles"][0]), {"A.esm", "B.esm", "C.esm"})
        self.assertEqual([item["code"] for item in cycle_issues], ["master_dependency_cycle"])

    def test_truncated_tes4_header_is_reported_without_a_complete_verdict(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root, {})
            (data_root / "Skyrim.esm").write_bytes(b"TES4\x18\x00")

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            plugin = next(item for item in manifest["corpus_hashes"]["plugins"] if item["name"] == "Skyrim.esm")
            self.assertEqual(plugin["tes4"]["status"], "invalid")
            self.assertIn("truncated_tes4_record_header", {item["code"] for item in manifest["issues"]})
            self.assertNotEqual(manifest["verdict"], "complete")

    def test_extended_subrecord_length_and_malformed_xxxx(self):
        extended_payload = b"x" * 70_000
        extended = b"XXXX" + struct.pack("<H", 4) + struct.pack("<I", len(extended_payload))
        extended += b"JUNK" + struct.pack("<H", 0) + extended_payload
        parsed = manifest_tool.parse_tes4_header(tes4_plugin(extra_subrecords=(extended,)), len(tes4_plugin(extra_subrecords=(extended,))))
        self.assertEqual(parsed["extended_subrecord_count"], 1)
        self.assertIn("JUNK", [item["signature"] for item in parsed["subrecords"]])

        malformed = b"XXXX" + struct.pack("<H", 3) + b"abc"
        with self.assertRaises(manifest_tool.PluginFormatError) as raised:
            raw = tes4_plugin(extra_subrecords=(malformed,))
            manifest_tool.parse_tes4_header(raw, len(raw))
        self.assertEqual(raised.exception.code, "invalid_xxxx_length")

    def test_compressed_tes4_metadata_is_bounded_and_decoded(self):
        original = tes4_plugin(masters=("Skyrim.esm",))
        decoded_payload = original[manifest_tool.RECORD_HEADER_SIZE :]
        encoded_payload = struct.pack("<I", len(decoded_payload)) + zlib.compress(decoded_payload)
        header = bytearray(original[: manifest_tool.RECORD_HEADER_SIZE])
        struct.pack_into("<I", header, 4, len(encoded_payload))
        struct.pack_into("<I", header, 8, manifest_tool.FLAG_COMPRESSED)
        compressed_plugin = bytes(header) + encoded_payload

        parsed = manifest_tool.parse_tes4_header(compressed_plugin, len(compressed_plugin))

        self.assertTrue(parsed["compressed"])
        self.assertEqual(parsed["masters"], ["Skyrim.esm"])
        self.assertEqual(parsed["master_dependencies"][0]["data_size_bytes"], 0)

    def test_truncated_tes4_payload_and_subrecord_bounds_are_rejected(self):
        valid = bytearray(tes4_plugin())
        struct.pack_into("<I", valid, 4, len(valid))
        with self.assertRaises(manifest_tool.PluginFormatError) as raised:
            manifest_tool.parse_tes4_header(bytes(valid), len(valid))
        self.assertEqual(raised.exception.code, "truncated_tes4_payload")

        malformed_payload = b"HEDR" + struct.pack("<H", 12) + b"\0" * 6
        malformed_subrecord = (
            b"TES4"
            + struct.pack("<I", len(malformed_payload))
            + b"\0" * 16
            + malformed_payload
        )
        with self.assertRaises(manifest_tool.PluginFormatError) as raised:
            manifest_tool.parse_tes4_header(malformed_subrecord, len(malformed_subrecord))
        self.assertEqual(raised.exception.code, "truncated_subrecord_payload")

    def test_nonfinite_hedr_and_unknown_master_encoding_are_not_decoded(self):
        nonfinite_hedr = subrecord(b"HEDR", struct.pack("<fII", float("nan"), 0, 0x800))
        malformed_hedr = b"TES4" + struct.pack("<I", len(nonfinite_hedr)) + b"\0" * 16 + nonfinite_hedr
        with self.assertRaises(manifest_tool.PluginFormatError) as raised:
            manifest_tool.parse_tes4_header(malformed_hedr, len(malformed_hedr))
        self.assertEqual(raised.exception.code, "nonfinite_hedr_version")

        bad_master = subrecord(b"HEDR", struct.pack("<fII", 1.7, 0, 0x800))
        bad_master += subrecord(b"MAST", b"\xe9.esm\0")
        malformed_master = b"TES4" + struct.pack("<I", len(bad_master)) + b"\0" * 16 + bad_master
        with self.assertRaises(manifest_tool.PluginFormatError) as raised:
            manifest_tool.parse_tes4_header(malformed_master, len(malformed_master))
        self.assertEqual(raised.exception.code, "master_filename_encoding_unknown")

    def test_tes4_declared_header_limit_is_checked_before_reading_payload(self):
        raw = bytearray(tes4_plugin())
        raw[4:8] = struct.pack("<I", manifest_tool.MAX_TES4_HEADER_BYTES)
        with self.assertRaises(manifest_tool.PluginFormatError) as raised:
            manifest_tool.parse_tes4_header(bytes(raw[:24]), len(raw))
        self.assertEqual(raised.exception.code, "tes4_header_over_limit")

    def test_source_mutation_during_hash_fails_the_pin(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            data_root = root / "Data"
            data_root.mkdir()
            source = data_root / "Skyrim.esm"
            source.write_bytes(tes4_plugin())
            original_fstat = manifest_tool.os.fstat
            calls = 0

            def mutate_before_final_fstat(fd):
                nonlocal calls
                calls += 1
                if calls == 2:
                    before_mutation = source.stat()
                    changed = bytearray(source.read_bytes())
                    changed[-1] ^= 0x01
                    source.write_bytes(changed)
                    # Same-tick rewrites can retain mtime/ctime on this host.
                    # Make the metadata drift under test deterministic.
                    manifest_tool.os.utime(
                        source,
                        ns=(before_mutation.st_atime_ns, before_mutation.st_mtime_ns + 1_000_000_000),
                    )
                return original_fstat(fd)

            with mock.patch.object(manifest_tool.os, "fstat", side_effect=mutate_before_final_fstat):
                with self.assertRaises(manifest_tool.SourceDriftError):
                    manifest_tool.scan_plugin(source, data_root, "data")

    def test_path_descriptor_timestamp_skew_keeps_same_file_identity(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            data_root = root / "Data"
            data_root.mkdir()
            source = data_root / "Skyrim.esm"
            source.write_bytes(tes4_plugin())
            original_path_stat = manifest_tool._path_stat

            def skewed_path_stat(path):
                observed = original_path_stat(path)
                return SimpleNamespace(
                    st_dev=observed.st_dev,
                    st_ino=observed.st_ino,
                    st_mode=observed.st_mode,
                    st_size=observed.st_size,
                    st_mtime=observed.st_mtime + 2,
                    st_ctime=observed.st_ctime + 2,
                    st_mtime_ns=observed.st_mtime_ns + 2_000_000_000,
                    st_ctime_ns=observed.st_ctime_ns + 2_000_000_000,
                )

            with (
                mock.patch.object(manifest_tool, "_path_stat", side_effect=skewed_path_stat),
                mock.patch.object(manifest_tool.os, "name", "nt"),
            ):
                plugin = manifest_tool.scan_plugin(source, data_root, "data")

            self.assertEqual(plugin["sha256"], hashlib.sha256(source.read_bytes()).hexdigest())
            with (
                mock.patch.object(manifest_tool, "_path_stat", side_effect=skewed_path_stat),
                mock.patch.object(manifest_tool.os, "name", "posix"),
                self.assertRaises(manifest_tool.SourceDriftError),
            ):
                manifest_tool.scan_plugin(source, data_root, "data")

    def test_source_path_replacement_during_hash_fails_the_pin(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            data_root = root / "Data"
            data_root.mkdir()
            source = data_root / "Skyrim.esm"
            source.write_bytes(tes4_plugin())
            replacement = data_root / "replacement.esm"
            replacement.write_bytes(tes4_plugin())
            original_path_stat = manifest_tool._path_stat
            calls = 0

            def replace_before_final_path_stat(path):
                nonlocal calls
                calls += 1
                if calls == 2:
                    replacement.replace(source)
                return original_path_stat(path)

            with mock.patch.object(manifest_tool, "_path_stat", side_effect=replace_before_final_path_stat):
                with self.assertRaises(manifest_tool.SourceDriftError):
                    manifest_tool.scan_plugin(source, data_root, "data")

    def test_missing_runtime_is_explicit_and_cannot_complete_pin(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            executable.unlink()

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertEqual(manifest["target"]["runtime"]["status"], "missing")
            self.assertFalse(manifest["successful_pin"])
            self.assertIn("missing_required_input", {item["code"] for item in manifest["issues"]})

    def test_missing_alternate_ccc_uses_its_own_source_path(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, _ = make_tree(root)
            alternate_ccc = game_root / "Alternate.ccc"

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, alternate_ccc)

            self.assertEqual(manifest["ccc"]["status"], "missing")
            self.assertEqual(manifest["ccc"]["source"]["relative_path"], "Alternate.ccc")
            issue = next(item for item in manifest["issues"] if item["code"] == "missing_ccc_descriptor")
            self.assertEqual(issue["path"], "Alternate.ccc")

    def test_alternate_ccc_drift_uses_its_own_source_path(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            alternate_ccc = game_root / "Alternate.ccc"
            make_symlink_or_skip(alternate_ccc, ccc.name)

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, alternate_ccc)

            self.assertEqual(manifest["ccc"]["status"], "source_drift")
            self.assertEqual(manifest["ccc"]["source"]["relative_path"], "Alternate.ccc")
            issue = next(item for item in manifest["issues"] if item["code"] == "source_drift")
            self.assertEqual(issue["path"], "Alternate.ccc")

    def test_api_and_cli_resolve_executable_and_ccc_directory_aliases_without_resolving_leaf(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            nested = game_root / "nested"
            nested.mkdir()
            alias = root / "install-parent-alias"
            make_symlink_or_skip(alias, nested, target_is_directory=True)
            executable_alias = alias / ".." / executable.name
            ccc_alias = alias / ".." / ccc.name

            api_manifest = manifest_tool.build_manifest(game_root, data_root, executable_alias, ccc_alias)

            self.assertEqual(api_manifest["target"]["runtime"]["source"]["relative_path"], "SkyrimSE.exe")
            self.assertEqual(api_manifest["ccc"]["status"], "observed")
            self.assertEqual(api_manifest["ccc"]["source"]["relative_path"], "Skyrim.ccc")

            output = root / "cli-manifest.json"
            status = manifest_tool.main([
                "--game-root", str(game_root),
                "--data-dir", str(data_root),
                "--executable", str(executable_alias),
                "--ccc", str(ccc_alias),
                "--output", str(output),
            ])

            self.assertEqual(status, 2)
            cli_manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(cli_manifest["target"]["runtime"]["source"]["relative_path"], "SkyrimSE.exe")
            self.assertEqual(cli_manifest["ccc"]["status"], "observed")
            self.assertEqual(cli_manifest["ccc"]["source"]["relative_path"], "Skyrim.ccc")

    def test_api_and_cli_reject_leaf_symlinks_after_parent_normalization(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            executable_link = game_root / "RuntimeAlias.exe"
            ccc_link = game_root / "CccAlias.ccc"
            make_symlink_or_skip(executable_link, executable.name)
            make_symlink_or_skip(ccc_link, ccc.name)

            api_manifest = manifest_tool.build_manifest(game_root, data_root, executable_link, ccc_link)

            self.assertEqual(api_manifest["target"]["runtime"]["status"], "source_drift")
            self.assertEqual(api_manifest["ccc"]["status"], "source_drift")

            output = root / "leaf-link-manifest.json"
            status = manifest_tool.main([
                "--game-root", str(game_root),
                "--data-dir", str(data_root),
                "--executable", str(executable_link),
                "--ccc", str(ccc_link),
                "--output", str(output),
            ])

            self.assertEqual(status, 2)
            cli_manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(cli_manifest["target"]["runtime"]["status"], "source_drift")
            self.assertEqual(cli_manifest["ccc"]["status"], "source_drift")

    def test_output_must_be_outside_game_and_data_sources(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root = root / "game"
            data_root = game_root / "Data"
            game_root.mkdir()
            data_root.mkdir()
            with self.assertRaises(ValueError):
                manifest_tool.ensure_output_outside_sources(game_root / "manifest.json", game_root, data_root)

    def test_archive_is_hashed_while_bundled_tables_still_block_completion(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            archive = data_root / "Skyrim - Misc.bsa"
            raw = b"archive fixture"
            archive.write_bytes(raw)

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertFalse(manifest["complete"])
            self.assertFalse(manifest["successful_pin"])
            observed = manifest["archive_observations"]["files"][0]
            self.assertEqual(observed["hash_status"], "observed_candidate_pin")
            self.assertEqual(observed["size"], len(raw))
            self.assertEqual(observed["sha256"], hashlib.sha256(raw).hexdigest())
            self.assertTrue(manifest["archive_observations"]["coverage_complete"])
            self.assertEqual(
                manifest["archive_observations"]["bundled_string_tables"]["status"],
                "uninspected",
            )
            self.assertIn("bundled_string_tables_uninspected", manifest["completion_blockers"])
            self.assertIn("ordered names declared by Skyrim.ccc", manifest["ccc"]["interpretation"])

    def test_archive_mutation_during_hash_is_rejected_by_verified_file_reader(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            _, data_root, _, _ = make_tree(root)
            archive = data_root / "Skyrim - Misc.bsa"
            archive.write_bytes(b"archive before mutation")
            original_path_stat = manifest_tool._path_stat
            calls = 0

            def mutate_before_final_path_stat(path):
                nonlocal calls
                if path == archive:
                    calls += 1
                    if calls == 2:
                        archive.write_bytes(b"archive after mutation")
                return original_path_stat(path)

            with mock.patch.object(
                manifest_tool, "_path_stat", side_effect=mutate_before_final_path_stat
            ):
                with self.assertRaises(manifest_tool.SourceDriftError):
                    manifest_tool._scan_hashed_file(archive, data_root, "data", "archive")

    def test_unresolved_order_locale_and_corpus_pins_block_success(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            executable_bytes = b"synthetic runtime matching test-only target"
            game_root, data_root, executable, ccc = make_tree(root, executable=executable_bytes)
            with mock.patch.dict(manifest_tool.TARGET_RUNTIME, {
                "expected_executable_sha256": hashlib.sha256(executable_bytes).hexdigest(),
                "expected_executable_size": len(executable_bytes),
            }):
                manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertEqual(manifest["target"]["runtime"]["status"], "matched")
            self.assertTrue(manifest["dependency_closure"]["complete"])
            self.assertEqual(manifest["issues"], [])
            self.assertFalse(manifest["complete"])
            self.assertFalse(manifest["successful_pin"])
            self.assertEqual(set(manifest["completion_blockers"]), {
                "active_load_order_unresolved",
                "locale_unresolved",
                "corpus_profile_unresolved",
                "official_content_provenance_unresolved",
                "accepted_corpus_pin_set_missing",
            })

    def test_loose_string_tables_are_hashed_without_inferring_locale(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            strings = data_root / "Strings" / "Addon_English.strings"
            strings.parent.mkdir()
            strings.write_bytes(b"fixture table")

            manifest = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertEqual(manifest["locale"]["status"], "unresolved")
            self.assertEqual(len(manifest["corpus_hashes"]["loose_string_tables"]), 1)
            self.assertEqual(manifest["corpus_hashes"]["loose_string_tables"][0]["source"]["relative_path"], "Strings/Addon_English.strings")
            self.assertEqual(manifest["corpus_hashes"]["loose_string_tables"][0]["sha256"], hashlib.sha256(b"fixture table").hexdigest())

    def test_supplied_profile_locale_and_order_are_hash_checked_but_not_accepted(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            plugins = {name: () for name in BASE_PLUGINS}
            plugins.update({
                "Parent.esl": ("Skyrim.esm",),
                "Addon.esp": ("Parent.esl",),
                "Inactive.esl": ("Skyrim.esm",),
            })
            game_root, data_root, executable, ccc = make_tree(root, plugins)
            archive = data_root / "Skyrim - Misc.bsa"
            archive.write_bytes(b"hashed archive with opaque entries")
            active = [*BASE_PLUGINS, "Parent.esl", "Addon.esp"]
            evidence_path, _ = make_corpus_evidence(
                root, active, unloaded_optional_plugins=["Inactive.esl"]
            )

            manifest = manifest_tool.build_manifest(
                game_root, data_root, executable, ccc, evidence_path
            )

            self.assertEqual(manifest["manifest_version"], 3)
            self.assertEqual(manifest["locale"]["value"], "ENGLISH")
            self.assertEqual(manifest["locale"]["status"], "supplied_evidence_hash_verified")
            self.assertEqual(manifest["locale"]["semantic_status"], "unverified")
            self.assertEqual(manifest["load_order"]["entries"], active)
            self.assertEqual(manifest["load_order"]["unloaded_optional_plugins"], ["Inactive.esl"])
            self.assertTrue(manifest["load_order"]["dependency_order_validated"])
            self.assertEqual(manifest["corpus_profile"]["status"], "supplied_evidence_hash_verified")
            self.assertEqual(manifest["input_evidence"]["status"], "supplied_input_hash_verified; semantic_claims_unverified")
            self.assertFalse(manifest["complete"])
            self.assertFalse(manifest["successful_pin"])
            self.assertIn("accepted_corpus_pin_set_missing", manifest["completion_blockers"])
            self.assertIn("official_content_provenance_unresolved", manifest["completion_blockers"])
            self.assertIn("corpus_profile_semantics_unverified", manifest["completion_blockers"])
            self.assertIn("bundled_string_tables_uninspected", manifest["completion_blockers"])
            self.assertTrue(manifest["archive_observations"]["coverage_complete"])

    def test_external_evidence_hash_mismatch_fails_before_manifest_creation(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            evidence_path, _ = make_corpus_evidence(root, BASE_PLUGINS)
            (root / "evidence" / "locale-source.ini").write_bytes(b"[General]\nsLanguage=FRENCH\n")

            with self.assertRaisesRegex(ValueError, "locale.evidence hash mismatch"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, evidence_path)

    def test_missing_external_evidence_artifact_fails_clearly(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            evidence_path, descriptor = make_corpus_evidence(root, BASE_PLUGINS)
            descriptor["corpus_profile"]["evidence"]["path"] = "missing-build-evidence.txt"
            evidence_path.write_text(json.dumps(descriptor), encoding="utf-8")

            with self.assertRaisesRegex(manifest_tool.MissingSourceError, "required source is missing"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, evidence_path)

    def test_missing_or_malformed_corpus_evidence_input_fails_clearly(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            missing_descriptor = root / "missing-evidence.json"

            with self.assertRaisesRegex(manifest_tool.MissingSourceError, "required source is missing"):
                manifest_tool.build_manifest(
                    game_root, data_root, executable, ccc, missing_descriptor
                )

            malformed_descriptor = root / "malformed-evidence.json"
            malformed_descriptor.write_text("{", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid corpus evidence JSON"):
                manifest_tool.build_manifest(
                    game_root, data_root, executable, ccc, malformed_descriptor
                )

    def test_oversized_evidence_descriptor_is_rejected_before_hash_read(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            descriptor = Path(temp_dir) / "oversized-evidence.json"
            with descriptor.open("wb") as stream:
                stream.truncate(manifest_tool.MAX_CORPUS_EVIDENCE_BYTES + 1)

            with mock.patch.object(
                manifest_tool, "_read_verified_file", side_effect=AssertionError("read attempted")
            ):
                with self.assertRaisesRegex(ValueError, "exceeds the 1 MiB input limit"):
                    manifest_tool._read_corpus_evidence(descriptor)

    def test_profile_target_must_match_fixed_newest_steam_build(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            wrong_target = {
                "game": manifest_tool.TARGET_RUNTIME["game"],
                "executable_version": "1.6.1170.0",
                "steam_build": "1170",
            }
            evidence_path, _ = make_corpus_evidence(root, BASE_PLUGINS, target=wrong_target)

            with self.assertRaisesRegex(ValueError, "does not match the required Steam Skyrim SE/AE"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, evidence_path)

    def test_profile_cannot_supply_acceptance_or_provenance_claims(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            evidence_path, descriptor = make_corpus_evidence(root, BASE_PLUGINS)
            descriptor["corpus_profile"]["accepted"] = True
            evidence_path.write_text(json.dumps(descriptor), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "unsupported keys: accepted"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, evidence_path)

    def test_cli_writes_supplied_evidence_candidate_and_keeps_incomplete_exit_status(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            evidence_path, _ = make_corpus_evidence(root, BASE_PLUGINS)
            output = root / "candidate.json"
            stdout = StringIO()
            stderr = StringIO()

            with redirect_stdout(stdout), redirect_stderr(stderr):
                status = manifest_tool.main([
                    "--game-root", str(game_root),
                    "--data-dir", str(data_root),
                    "--executable", str(executable),
                    "--ccc", str(ccc),
                    "--corpus-evidence", str(evidence_path),
                    "--output", str(output),
                ])

            self.assertEqual(status, 2)
            self.assertTrue(output.is_file())
            self.assertEqual(stderr.getvalue(), "")
            observed = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(observed["locale"]["value"], "ENGLISH")
            self.assertEqual(observed["load_order"]["entries"], list(BASE_PLUGINS))
            self.assertIn("accepted_corpus_pin_set_missing", observed["completion_blockers"])

    def test_cli_rejects_load_order_evidence_as_output_before_replacing_it(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            evidence_path, _ = make_corpus_evidence(root, BASE_PLUGINS)
            load_order_artifact = root / "evidence" / "load-order-source.txt"
            original = load_order_artifact.read_bytes()
            stdout = StringIO()
            stderr = StringIO()

            with mock.patch.object(manifest_tool, "write_manifest", wraps=manifest_tool.write_manifest) as writer:
                with redirect_stdout(stdout), redirect_stderr(stderr):
                    status = manifest_tool.main([
                        "--game-root", str(game_root),
                        "--data-dir", str(data_root),
                        "--executable", str(executable),
                        "--ccc", str(ccc),
                        "--corpus-evidence", str(evidence_path),
                        "--output", str(load_order_artifact),
                    ])

            self.assertEqual(status, 2)
            writer.assert_not_called()
            self.assertEqual(load_order_artifact.read_bytes(), original)
            self.assertIn("manifest output overlaps", stderr.getvalue())

    def test_cli_rejects_symlink_alias_and_ancestor_of_evidence_artifact(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)
            evidence_path, _ = make_corpus_evidence(root, BASE_PLUGINS)
            profile_artifact = root / "evidence" / "profile-source.txt"
            original = profile_artifact.read_bytes()
            alias = root / "artifact-alias.json"
            make_symlink_or_skip(alias, profile_artifact)
            stdout = StringIO()
            stderr = StringIO()

            with redirect_stdout(stdout), redirect_stderr(stderr):
                alias_status = manifest_tool.main([
                    "--game-root", str(game_root),
                    "--data-dir", str(data_root),
                    "--executable", str(executable),
                    "--ccc", str(ccc),
                    "--corpus-evidence", str(evidence_path),
                    "--output", str(alias),
                ])

            self.assertEqual(alias_status, 2)
            self.assertTrue(alias.is_symlink())
            self.assertEqual(profile_artifact.read_bytes(), original)
            self.assertIn("manifest output overlaps", stderr.getvalue())

            stdout = StringIO()
            stderr = StringIO()
            evidence_directory = root / "evidence"
            with redirect_stdout(stdout), redirect_stderr(stderr):
                ancestor_status = manifest_tool.main([
                    "--game-root", str(game_root),
                    "--data-dir", str(data_root),
                    "--executable", str(executable),
                    "--ccc", str(ccc),
                    "--corpus-evidence", str(evidence_path),
                    "--output", str(evidence_directory),
                ])

            self.assertEqual(ancestor_status, 2)
            self.assertTrue(evidence_directory.is_dir())
            self.assertEqual(profile_artifact.read_bytes(), original)
            self.assertIn("manifest output overlaps", stderr.getvalue())

    def test_supplied_load_order_rejects_unknown_and_unclassified_plugins(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            plugins = {name: () for name in BASE_PLUGINS} | {"Addon.esl": ()}
            game_root, data_root, executable, ccc = make_tree(root, plugins)
            unknown_path, _ = make_corpus_evidence(root, [*BASE_PLUGINS, "Unknown.esm"])

            with self.assertRaisesRegex(ValueError, "load_order names are not installed in Data: unknown.esm"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, unknown_path)

            omitted_path, _ = make_corpus_evidence(root, BASE_PLUGINS)
            with self.assertRaisesRegex(ValueError, "unclassified: addon.esl"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, omitted_path)

    def test_supplied_load_order_rejects_duplicates_and_dependency_order_errors(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            plugins = {name: () for name in BASE_PLUGINS} | {"Addon.esl": ("Skyrim.esm",)}
            game_root, data_root, executable, ccc = make_tree(root, plugins)

            duplicate_path, _ = make_corpus_evidence(
                root, [*BASE_PLUGINS, "Addon.esl", "addon.ESL"]
            )
            with self.assertRaisesRegex(ValueError, "repeats a case-insensitive plugin name"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, duplicate_path)

            wrong_order = ["Addon.esl", *BASE_PLUGINS]
            order_path, _ = make_corpus_evidence(root, wrong_order)
            with self.assertRaisesRegex(ValueError, "places master Skyrim.esm after dependent plugin Addon.esl"):
                manifest_tool.build_manifest(game_root, data_root, executable, ccc, order_path)

    def test_manifest_is_deterministic_and_serializable(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            game_root, data_root, executable, ccc = make_tree(root)

            first = manifest_tool.build_manifest(game_root, data_root, executable, ccc)
            second = manifest_tool.build_manifest(game_root, data_root, executable, ccc)

            self.assertEqual(
                json.dumps(first, sort_keys=True, separators=(",", ":")),
                json.dumps(second, sort_keys=True, separators=(",", ":")),
            )


if __name__ == "__main__":
    unittest.main()
