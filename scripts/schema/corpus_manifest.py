#!/usr/bin/env python3
"""Create a local-first, reproducible pin candidate for the Skyrim SE corpus."""

from __future__ import annotations

import argparse
from graphlib import CycleError, TopologicalSorter
import hashlib
import json
import math
import os
import stat
import struct
import sys
import tempfile
import zlib
from pathlib import Path


MANIFEST_VERSION = 1
TARGET_RUNTIME = {
    "game": "Skyrim Special Edition",
    "executable_version": "1.7.104.0",
    "steam_build": "24914197",
    "expected_executable_sha256": "846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f",
    "expected_executable_size": 37_910_440,
}
REQUIRED_BASE_PLUGINS = (
    "Skyrim.esm",
    "Update.esm",
    "Dawnguard.esm",
    "HearthFires.esm",
    "Dragonborn.esm",
)
PLUGIN_SUFFIXES = {".esm", ".esp", ".esl"}
STRING_TABLE_SUFFIXES = {".strings", ".dlstrings", ".ilstrings"}
ARCHIVE_SUFFIXES = {".bsa", ".ba2"}
RECORD_HEADER_SIZE = 24
FLAG_COMPRESSED = 0x00040000
MAX_TES4_HEADER_BYTES = 16 * 1024 * 1024
MAX_CCC_BYTES = 1024 * 1024
HASH_CHUNK_SIZE = 1024 * 1024


class PluginFormatError(ValueError):
    def __init__(self, code: str, detail: str):
        super().__init__(detail)
        self.code = code
        self.detail = detail


class SourceDriftError(RuntimeError):
    """Raised when a source changes or its path is replaced during the read."""


class MissingSourceError(SourceDriftError):
    """Raised when a required source file is absent at scan time."""


def _stat_token(file_stat: os.stat_result) -> tuple[int, int, int, int, int, int]:
    return (
        file_stat.st_dev,
        file_stat.st_ino,
        stat.S_IFMT(file_stat.st_mode),
        file_stat.st_size,
        getattr(file_stat, "st_mtime_ns", int(file_stat.st_mtime * 1_000_000_000)),
        getattr(file_stat, "st_ctime_ns", int(file_stat.st_ctime * 1_000_000_000)),
    )


def _path_stat(path: Path) -> os.stat_result:
    try:
        result = path.lstat()
    except OSError as exc:
        if isinstance(exc, FileNotFoundError):
            raise MissingSourceError(f"required source is missing: {path}") from exc
        raise SourceDriftError(f"cannot stat source path {path}: {exc}") from exc
    if stat.S_ISLNK(result.st_mode):
        raise SourceDriftError(f"source path is a symlink: {path}")
    if not stat.S_ISREG(result.st_mode):
        raise SourceDriftError(f"source path is not a regular file: {path}")
    return result


def _read_verified_file(
    path: Path, *, capture_tes4: bool = False, capture_bytes: int = 0
) -> tuple[str, int, bytes]:
    """Hash one descriptor and capture only the bounded TES4 prefix, if requested."""
    path_before = _path_stat(path)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        if isinstance(exc, FileNotFoundError):
            raise MissingSourceError(f"required source is missing: {path}") from exc
        raise SourceDriftError(f"cannot open source path {path}: {exc}") from exc

    digest = hashlib.sha256()
    captured = bytearray()
    byte_count = 0
    try:
        with os.fdopen(descriptor, "rb", closefd=True) as stream:
            descriptor = -1
            before = os.fstat(stream.fileno())
            if _stat_token(before) != _stat_token(path_before):
                raise SourceDriftError(f"source path changed while opening: {path}")

            initial_capture = RECORD_HEADER_SIZE if capture_tes4 else min(capture_bytes, before.st_size)
            first = stream.read(initial_capture)
            if first:
                captured.extend(first)
                digest.update(first)
                byte_count += len(first)

            capture_length = initial_capture
            if capture_tes4 and len(first) == RECORD_HEADER_SIZE and first[:4] == b"TES4":
                data_size = struct.unpack_from("<I", first, 4)[0]
                record_size = RECORD_HEADER_SIZE + data_size
                if record_size <= MAX_TES4_HEADER_BYTES and record_size <= before.st_size:
                    capture_length = record_size

            while len(captured) < capture_length:
                chunk = stream.read(capture_length - len(captured))
                if not chunk:
                    break
                captured.extend(chunk)
                digest.update(chunk)
                byte_count += len(chunk)

            while True:
                chunk = stream.read(HASH_CHUNK_SIZE)
                if not chunk:
                    break
                digest.update(chunk)
                byte_count += len(chunk)

            after = os.fstat(stream.fileno())

        path_after = _path_stat(path)
        if _stat_token(before) != _stat_token(after):
            raise SourceDriftError(f"source changed while hashing: {path}")
        if _stat_token(after) != _stat_token(path_after):
            raise SourceDriftError(f"source path identity changed while hashing: {path}")
        if byte_count != after.st_size:
            raise SourceDriftError(
                f"source size changed while hashing {path}: read {byte_count}, now {after.st_size}"
            )
    finally:
        if descriptor >= 0:
            os.close(descriptor)

    return digest.hexdigest(), byte_count, bytes(captured)


def _decode_plugin_filename(raw: bytes) -> str:
    if not raw or raw[-1] != 0 or b"\0" in raw[:-1]:
        raise PluginFormatError("invalid_master_filename", "MAST filename must be one NUL-terminated name")
    try:
        name = raw[:-1].decode("ascii")
    except UnicodeDecodeError as exc:
        raise PluginFormatError(
            "master_filename_encoding_unknown",
            "MAST filename is not ASCII; this manifest does not guess its encoding",
        ) from exc
    if not name or name in {".", ".."} or "/" in name or "\\" in name:
        raise PluginFormatError("invalid_master_filename", "MAST filename is not a plain plugin filename")
    return name


def _decode_zlib_payload(payload: bytes) -> bytes:
    if len(payload) < 4:
        raise PluginFormatError("truncated_compressed_tes4", "compressed TES4 payload lacks its decoded-size word")
    expected_size = struct.unpack_from("<I", payload, 0)[0]
    if expected_size > MAX_TES4_HEADER_BYTES - RECORD_HEADER_SIZE:
        raise PluginFormatError("tes4_header_over_limit", "decoded TES4 payload exceeds the metadata bound")

    inflater = zlib.decompressobj()
    try:
        decoded = inflater.decompress(payload[4:], expected_size + 1)
        if len(decoded) > expected_size or inflater.unconsumed_tail:
            raise PluginFormatError("tes4_decompression_over_limit", "compressed TES4 payload exceeds its declared size")
        decoded += inflater.flush()
    except zlib.error as exc:
        raise PluginFormatError("invalid_compressed_tes4", f"compressed TES4 payload is invalid: {exc}") from exc
    if not inflater.eof:
        raise PluginFormatError("truncated_compressed_tes4", "compressed TES4 stream has no complete end marker")
    if inflater.unused_data:
        raise PluginFormatError("trailing_compressed_tes4_data", "compressed TES4 stream has trailing bytes")
    if len(decoded) != expected_size:
        raise PluginFormatError("compressed_tes4_size_mismatch", "decoded TES4 size differs from its declared size")
    return decoded


def parse_tes4_header(raw_record: bytes, file_size: int) -> dict:
    """Parse bounded TES4 framing and report selected metadata plus framed tag sizes."""
    if file_size < RECORD_HEADER_SIZE or len(raw_record) < RECORD_HEADER_SIZE:
        raise PluginFormatError("truncated_tes4_record_header", "plugin is shorter than its 24-byte TES4 header")
    if raw_record[:4] != b"TES4":
        raise PluginFormatError("missing_tes4_header", "first plugin record signature is not TES4")

    data_size, flags, form_id, version_control, form_version, unknown = struct.unpack_from(
        "<IIIIHH", raw_record, 4
    )
    encoded_record_size = RECORD_HEADER_SIZE + data_size
    if encoded_record_size > MAX_TES4_HEADER_BYTES:
        raise PluginFormatError("tes4_header_over_limit", "encoded TES4 record exceeds the metadata bound")
    if encoded_record_size > file_size:
        raise PluginFormatError("truncated_tes4_payload", "TES4 record declares bytes beyond end of plugin")
    if len(raw_record) < encoded_record_size:
        raise PluginFormatError("truncated_tes4_payload", "captured TES4 record is shorter than its declared size")

    encoded_payload = raw_record[RECORD_HEADER_SIZE:encoded_record_size]
    payload = _decode_zlib_payload(encoded_payload) if flags & FLAG_COMPRESSED else encoded_payload
    if len(payload) > MAX_TES4_HEADER_BYTES - RECORD_HEADER_SIZE:
        raise PluginFormatError("tes4_header_over_limit", "decoded TES4 record exceeds the metadata bound")

    subrecords = []
    masters = []
    master_dependencies = []
    pending_master = None
    hedr = None
    extended_subrecord_count = 0
    offset = 0
    extended_size = None
    while offset < len(payload):
        remaining = len(payload) - offset
        if remaining < 6:
            raise PluginFormatError("truncated_subrecord_header", "TES4 payload ends inside a subrecord header")
        signature_bytes = payload[offset : offset + 4]
        short_size = struct.unpack_from("<H", payload, offset + 4)[0]
        offset += 6
        if signature_bytes == b"XXXX":
            if extended_size is not None:
                raise PluginFormatError("nested_xxxx", "XXXX length markers cannot be nested")
            if short_size != 4 or len(payload) - offset < 4:
                raise PluginFormatError("invalid_xxxx_length", "XXXX marker must contain exactly one u32 length")
            extended_size = struct.unpack_from("<I", payload, offset)[0]
            offset += 4
            extended_subrecord_count += 1
            continue

        subrecord_size = extended_size if extended_size is not None else short_size
        extended_size = None
        if subrecord_size > len(payload) - offset:
            raise PluginFormatError("truncated_subrecord_payload", "subrecord declares bytes beyond TES4 payload")
        value = payload[offset : offset + subrecord_size]
        offset += subrecord_size
        try:
            signature = signature_bytes.decode("ascii") if all(0x20 <= byte <= 0x7E for byte in signature_bytes) else None
        except UnicodeDecodeError:
            signature = None
        subrecords.append({
            "signature": signature,
            "signature_hex": signature_bytes.hex(),
            "size": subrecord_size,
        })

        if signature_bytes == b"HEDR":
            if hedr is not None or subrecord_size != 12:
                raise PluginFormatError("invalid_hedr", "TES4 must contain one 12-byte HEDR subrecord")
            header_version, record_count, next_object_id = struct.unpack("<fII", value)
            if not math.isfinite(header_version):
                raise PluginFormatError("nonfinite_hedr_version", "HEDR version is not finite")
            hedr = {
                "version": header_version,
                "record_count": record_count,
                "next_object_id": next_object_id,
            }
        elif signature_bytes == b"MAST":
            name = _decode_plugin_filename(value)
            masters.append(name)
            master_dependencies.append({"name": name, "data_size_bytes": None})
            pending_master = len(master_dependencies) - 1
        elif signature_bytes == b"DATA" and pending_master is not None:
            if len(value) >= 8:
                master_dependencies[pending_master]["data_size_bytes"] = struct.unpack_from("<Q", value)[0]
            pending_master = None

    if extended_size is not None:
        raise PluginFormatError("dangling_xxxx", "XXXX length marker has no following subrecord")
    if hedr is None:
        raise PluginFormatError("missing_hedr", "TES4 metadata has no HEDR subrecord")
    if len({name.casefold() for name in masters}) != len(masters):
        raise PluginFormatError("duplicate_master_name", "TES4 master list repeats a case-insensitive name")

    return {
        "status": "decoded",
        "record_size": encoded_record_size,
        "record_flags": flags,
        "form_id": form_id,
        "version_control": version_control,
        "form_version": form_version,
        "unknown": unknown,
        "compressed": bool(flags & FLAG_COMPRESSED),
        "header": hedr,
        "masters": masters,
        "master_dependencies": master_dependencies,
        "subrecords": subrecords,
        "extended_subrecord_count": extended_subrecord_count,
    }


def _source_path(source_root: Path, path: Path, label: str) -> dict:
    try:
        relative_path = path.relative_to(source_root).as_posix()
    except ValueError as exc:
        raise ValueError(f"{path} is outside source root {source_root}") from exc
    return {"root": label, "relative_path": relative_path}


def scan_plugin(path: Path, source_root: Path, root_label: str = "data") -> dict:
    provenance = _source_path(source_root, path, root_label)
    try:
        digest, size, header_bytes = _read_verified_file(path, capture_tes4=True)
    except SourceDriftError:
        raise
    try:
        tes4 = parse_tes4_header(header_bytes, size)
    except PluginFormatError as exc:
        tes4 = {"status": "invalid", "error": {"code": exc.code, "detail": exc.detail}}
    return {
        "name": path.name,
        "source": provenance,
        "size": size,
        "sha256": digest,
        "hash_status": "observed_candidate_pin",
        "tes4": tes4,
    }


def _scan_hashed_file(path: Path, source_root: Path, root_label: str, kind: str) -> dict:
    provenance = _source_path(source_root, path, root_label)
    digest, size, _ = _read_verified_file(path)
    return {
        "name": path.name,
        "source": provenance,
        "size": size,
        "sha256": digest,
        "hash_status": "observed_candidate_pin",
        "kind": kind,
    }


def _discover_files(data_root: Path, suffixes: set[str], issues: list[dict]) -> list[Path]:
    found = []

    def on_walk_error(error: OSError) -> None:
        issues.append({"code": "data_walk_error", "path": str(error.filename or data_root), "detail": str(error)})

    for directory, child_dirs, filenames in os.walk(data_root, followlinks=False, onerror=on_walk_error):
        directory_path = Path(directory)
        retained_dirs = []
        for name in sorted(child_dirs, key=lambda item: (item.casefold(), item)):
            child = directory_path / name
            if child.is_symlink():
                issues.append({"code": "symlink_directory_skipped", "path": str(child)})
            else:
                retained_dirs.append(name)
        child_dirs[:] = retained_dirs
        for name in sorted(filenames, key=lambda item: (item.casefold(), item)):
            path = directory_path / name
            if path.suffix.lower() not in suffixes:
                continue
            try:
                info = path.lstat()
            except OSError as exc:
                issues.append({"code": "source_stat_error", "path": str(path), "detail": str(exc)})
                continue
            if stat.S_ISLNK(info.st_mode):
                issues.append({"code": "symlink_file_skipped", "path": str(path)})
                continue
            if not stat.S_ISREG(info.st_mode):
                issues.append({"code": "non_file_source_skipped", "path": str(path)})
                continue
            found.append(path)
    return sorted(found, key=lambda path: (path.relative_to(data_root).as_posix().casefold(), path.relative_to(data_root).as_posix()))


def _scan_ccc(path: Path, game_root: Path) -> dict:
    provenance = _source_path(game_root, path, "game")
    digest, size, raw = _read_verified_file(path, capture_bytes=MAX_CCC_BYTES + 1)
    if size > MAX_CCC_BYTES:
        return {
            "source": provenance,
            "size": size,
            "sha256": digest,
            "hash_status": "observed_candidate_pin",
            "status": "invalid",
            "listed_plugin_names": [],
            "error": {"code": "ccc_over_limit", "detail": "Skyrim.ccc exceeds the bounded list size"},
        }
    try:
        decoded = raw.decode("utf-8-sig")
    except UnicodeDecodeError:
        return {
            "source": provenance,
            "size": size,
            "sha256": digest,
            "hash_status": "observed_candidate_pin",
            "status": "invalid",
            "listed_plugin_names": [],
            "error": {"code": "ccc_encoding_unknown", "detail": "Skyrim.ccc is not valid UTF-8"},
        }
    names = [line.strip() for line in decoded.splitlines() if line.strip()]
    return {
        "source": provenance,
        "size": size,
        "sha256": digest,
        "hash_status": "observed_candidate_pin",
        "status": "observed",
        "listed_plugin_names": names,
        "interpretation": "ordered names declared by Skyrim.ccc; not active load order or entitlement proof",
    }


def _dependency_closure(plugins: list[dict], issues: list[dict]) -> dict:
    by_name: dict[str, list[dict]] = {}
    for plugin in plugins:
        by_name.setdefault(plugin["name"].casefold(), []).append(plugin)

    duplicates = []
    for folded, matches in sorted(by_name.items()):
        if len(matches) > 1:
            names = sorted(item["source"]["relative_path"] for item in matches)
            duplicates.append({"casefold_name": folded, "paths": names})
            issues.append({"code": "duplicate_casefold_plugin_name", "name": folded, "paths": names})

    missing = []
    ambiguous = []
    graph: dict[str, list[str]] = {}
    for plugin in plugins:
        node = plugin["source"]["relative_path"]
        graph[node] = []
        tes4 = plugin["tes4"]
        if tes4.get("status") != "decoded":
            continue
        for master in tes4["masters"]:
            matches = by_name.get(master.casefold(), [])
            if not matches:
                missing.append({"plugin": plugin["name"], "master": master})
            elif len(matches) > 1:
                ambiguous.append({"plugin": plugin["name"], "master": master})
            else:
                graph[node].append(matches[0]["source"]["relative_path"])

    if missing:
        issues.extend({"code": "missing_master", **item} for item in missing)
    if ambiguous:
        issues.extend({"code": "ambiguous_master", **item} for item in ambiguous)

    cycles = []
    try:
        TopologicalSorter(graph).prepare()
    except CycleError as exc:
        reported_cycle = exc.args[1] if len(exc.args) > 1 else ()
        cycle = list(reported_cycle)
        if cycle:
            cycles.append(cycle)
            issues.append({"code": "master_dependency_cycle", "paths": cycle})

    all_metadata_decoded = all(plugin["tes4"].get("status") == "decoded" for plugin in plugins)
    complete = not (missing or ambiguous or cycles or duplicates) and all_metadata_decoded
    return {
        "complete": complete,
        "missing_masters": sorted(missing, key=lambda item: (item["plugin"].casefold(), item["master"].casefold())),
        "ambiguous_masters": sorted(ambiguous, key=lambda item: (item["plugin"].casefold(), item["master"].casefold())),
        "duplicate_casefold_names": duplicates,
        "cycles": sorted(cycles),
    }


def _source_observation(path: Path, data_root: Path, kind: str) -> dict:
    provenance = _source_path(data_root, path, "data")
    info = _path_stat(path)
    return {
        "name": path.name,
        "source": provenance,
        "size": info.st_size,
        "hash_status": "not_hashed",
        "kind": kind,
        "gap": "archive contents and bundled string tables were not read",
    }


def build_manifest(game_root: Path, data_root: Path, executable: Path, ccc_path: Path | None = None) -> dict:
    game_root = Path(game_root).resolve(strict=True)
    data_root = Path(data_root).resolve(strict=True)
    executable = Path(executable).expanduser().absolute()
    ccc_path = Path(ccc_path).expanduser().absolute() if ccc_path is not None else game_root / "Skyrim.ccc"
    if not game_root.is_dir():
        raise ValueError(f"game root is not a directory: {game_root}")
    if not data_root.is_dir():
        raise ValueError(f"Data root is not a directory: {data_root}")
    if executable.parent != game_root:
        raise ValueError("executable must be an immediate child of the supplied game root")
    if ccc_path.parent != game_root:
        raise ValueError("Skyrim.ccc must be an immediate child of the supplied game root")
    ccc_source = _source_path(game_root, ccc_path, "game")

    issues = []
    runtime = {
        "source": _source_path(game_root, executable, "game"),
        "expected_sha256": TARGET_RUNTIME["expected_executable_sha256"],
        "expected_size": TARGET_RUNTIME["expected_executable_size"],
        "pin_status": "expected_target_pin",
    }
    try:
        runtime_digest, runtime_size, _ = _read_verified_file(executable)
        runtime.update({"sha256": runtime_digest, "size": runtime_size})
        runtime["sha256_matches"] = runtime_digest == runtime["expected_sha256"]
        runtime["size_matches"] = runtime_size == runtime["expected_size"]
        runtime["status"] = "matched" if runtime["sha256_matches"] and runtime["size_matches"] else "mismatch"
        if runtime["status"] != "matched":
            issues.append({"code": "runtime_pin_mismatch", "path": runtime["source"]["relative_path"]})
    except SourceDriftError as exc:
        missing = isinstance(exc, MissingSourceError)
        runtime.update({"status": "missing" if missing else "source_drift", "sha256": None, "size": None})
        issues.append({
            "code": "missing_required_input" if missing else "source_drift",
            "path": runtime["source"]["relative_path"],
            "detail": str(exc),
        })

    plugin_paths = _discover_files(data_root, PLUGIN_SUFFIXES, issues)
    plugins = []
    for path in plugin_paths:
        try:
            plugin = scan_plugin(path, data_root)
            plugins.append(plugin)
            if plugin["tes4"].get("status") != "decoded":
                issues.append({
                    "code": plugin["tes4"]["error"]["code"],
                    "plugin": plugin["name"],
                    "detail": plugin["tes4"]["error"]["detail"],
                })
        except SourceDriftError as exc:
            missing = isinstance(exc, MissingSourceError)
            plugins.append({
                "name": path.name,
                "source": _source_path(data_root, path, "data"),
                "size": None,
                "sha256": None,
                "hash_status": "failed_source_missing" if missing else "failed_source_drift",
                "tes4": {"status": "unread"},
            })
            issues.append({
                "code": "missing_required_input" if missing else "source_drift",
                "path": str(path.relative_to(data_root)),
                "detail": str(exc),
            })

    by_plugin_name: dict[str, list[dict]] = {}
    for plugin in plugins:
        by_plugin_name.setdefault(plugin["name"].casefold(), []).append(plugin)
    base_plugins = []
    for name in REQUIRED_BASE_PLUGINS:
        matches = by_plugin_name.get(name.casefold(), [])
        base_plugins.append({
            "name": name,
            "present": bool(matches),
            "paths": [item["source"]["relative_path"] for item in matches],
        })
        if not matches:
            issues.append({"code": "missing_base_plugin", "name": name})
    dependency_closure = _dependency_closure(plugins, issues)

    table_paths = _discover_files(data_root, STRING_TABLE_SUFFIXES, issues)
    string_tables = []
    for path in table_paths:
        try:
            string_tables.append(_scan_hashed_file(path, data_root, "data", "loose_string_table"))
        except SourceDriftError as exc:
            missing = isinstance(exc, MissingSourceError)
            string_tables.append({
                "name": path.name,
                "source": _source_path(data_root, path, "data"),
                "size": None,
                "sha256": None,
                "hash_status": "failed_source_missing" if missing else "failed_source_drift",
                "kind": "loose_string_table",
            })
            issues.append({
                "code": "missing_required_input" if missing else "source_drift",
                "path": str(path.relative_to(data_root)),
                "detail": str(exc),
            })

    archive_paths = _discover_files(data_root, ARCHIVE_SUFFIXES, issues)
    archives = []
    for path in archive_paths:
        try:
            archives.append(_source_observation(path, data_root, "archive_unhashed"))
        except SourceDriftError as exc:
            issues.append({"code": "source_stat_error", "path": str(path.relative_to(data_root)), "detail": str(exc)})

    if not ccc_path.exists():
        ccc = {
            "status": "missing",
            "source": ccc_source,
            "listed_plugin_names": [],
        }
        issues.append({"code": "missing_ccc_descriptor", "path": ccc_source["relative_path"]})
    else:
        try:
            ccc = _scan_ccc(ccc_path, game_root)
            if ccc.get("status") != "observed":
                issues.append({"code": ccc["error"]["code"], "path": ccc["source"]["relative_path"]})
        except SourceDriftError as exc:
            ccc = {
                "status": "missing" if isinstance(exc, MissingSourceError) else "source_drift",
                "source": ccc_source,
                "listed_plugin_names": [],
                "sha256": None,
                "size": None,
            }
            issues.append({
                "code": "missing_required_input" if isinstance(exc, MissingSourceError) else "source_drift",
                "path": ccc_source["relative_path"],
                "detail": str(exc),
            })

    declared_names = ccc.get("listed_plugin_names", [])
    by_plugin_name = {plugin["name"].casefold(): plugin for plugin in plugins}
    ccc["matching_data_plugin_names"] = [name for name in declared_names if name.casefold() in by_plugin_name]
    ccc["declared_names_missing_from_data"] = [name for name in declared_names if name.casefold() not in by_plugin_name]
    ccc["data_plugin_names_not_declared_by_ccc"] = [
        plugin["name"] for plugin in plugins if plugin["name"].casefold() not in {name.casefold() for name in declared_names}
    ]

    if not string_tables:
        string_table_status = "no_loose_tables_observed"
    else:
        string_table_status = "loose_tables_observed"

    plugin_complete = all(plugin["tes4"].get("status") == "decoded" for plugin in plugins)
    no_hard_issues = not issues
    archive_coverage_complete = not archives
    load_order = {"status": "unresolved", "source": None, "entries": []}
    locale = {"status": "unresolved", "string_table_observation": string_table_status}
    corpus_pin_status = "first_observation_only; no prior accepted corpus digest set was supplied"
    completion_blockers = []
    if not no_hard_issues:
        completion_blockers.append("input_or_metadata_issues")
    if not plugin_complete:
        completion_blockers.append("plugin_metadata_incomplete")
    if not dependency_closure["complete"]:
        completion_blockers.append("master_dependency_closure_incomplete")
    if not all(item["present"] for item in base_plugins):
        completion_blockers.append("required_base_plugin_missing")
    if not archive_coverage_complete:
        completion_blockers.append("archive_contents_unhashed")
    if load_order["status"] != "resolved":
        completion_blockers.append("active_load_order_unresolved")
    if locale["status"] != "resolved":
        completion_blockers.append("locale_unresolved")
    if corpus_pin_status != "accepted":
        completion_blockers.append("accepted_corpus_pin_set_missing")
    complete = not completion_blockers
    verdict = "complete" if complete else "incomplete"
    return {
        "manifest_version": MANIFEST_VERSION,
        "complete": complete,
        "verdict": verdict,
        "successful_pin": complete,
        "completion_blockers": completion_blockers,
        "target": {
            "game": TARGET_RUNTIME["game"],
            "executable_version": TARGET_RUNTIME["executable_version"],
            "steam_build": TARGET_RUNTIME["steam_build"],
            "runtime": runtime,
        },
        "source_roots": {"game": str(game_root), "data": str(data_root)},
        "corpus_hashes": {
            "status": corpus_pin_status,
            "plugins": plugins,
            "loose_string_tables": string_tables,
        },
        "plugin_count": len(plugins),
        "string_table_count": len(string_tables),
        "archive_observations": {
            "status": "filenames_and_sizes_only; archive bytes and bundled tables were not read",
            "count": len(archives),
            "files": archives,
            "coverage_complete": archive_coverage_complete,
        },
        "base_plugins": base_plugins,
        "dependency_closure": dependency_closure,
        "ccc": ccc,
        "load_order": load_order,
        "locale": locale,
        "unresolved": [
            "Skyrim.ccc names are a declared content list, not active load order or entitlement proof",
            "BSA/BA2 contents, including bundled string tables, were not inspected or hashed",
            "locale is unresolved; no locale was inferred from installed filenames",
            "plugin and loose string-table hashes are first observations without a prior accepted corpus pin set",
        ],
        "issues": sorted(issues, key=lambda item: (item["code"], item.get("path", item.get("plugin", item.get("name", ""))).casefold())),
    }


def ensure_output_outside_sources(output: Path, game_root: Path, data_root: Path) -> Path:
    output = Path(output).expanduser().resolve(strict=False)
    for source_root in (Path(game_root).resolve(strict=True), Path(data_root).resolve(strict=True)):
        try:
            output.relative_to(source_root)
        except ValueError:
            continue
        raise ValueError(f"manifest output must be outside source root {source_root}")
    return output


def write_manifest(output: Path, manifest: dict, game_root: Path, data_root: Path) -> None:
    output = ensure_output_outside_sources(output, game_root, data_root)
    output.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(manifest, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    temporary_path = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=output.parent,
            prefix=f".{output.name}.",
            suffix=".tmp",
            delete=False,
        ) as stream:
            temporary_path = Path(stream.name)
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_path, output)
    finally:
        if temporary_path is not None and temporary_path.exists():
            temporary_path.unlink()


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--game-root", type=Path, required=True, help="installed Skyrim Special Edition root")
    parser.add_argument("--data-dir", type=Path, required=True, help="selected installed Data source directory")
    parser.add_argument("--executable", type=Path, required=True, help="SkyrimSE.exe to compare with the fixed target pin")
    parser.add_argument("--ccc", type=Path, help="Skyrim.ccc path (defaults to <game-root>/Skyrim.ccc)")
    parser.add_argument("--output", type=Path, required=True, help="manifest JSON path outside all source roots")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    try:
        game_root = args.game_root.expanduser().resolve(strict=True)
        data_root = args.data_dir.expanduser().resolve(strict=True)
        executable = args.executable.expanduser().absolute()
        ccc_path = args.ccc.expanduser().absolute() if args.ccc else game_root / "Skyrim.ccc"
        output = ensure_output_outside_sources(args.output, game_root, data_root)
        manifest = build_manifest(game_root, data_root, executable, ccc_path)
        write_manifest(output, manifest, game_root, data_root)
    except (OSError, ValueError, SourceDriftError) as exc:
        print(f"corpus manifest failed: {exc}", file=sys.stderr)
        return 2

    print(json.dumps({
        "verdict": manifest["verdict"],
        "successful_pin": manifest["successful_pin"],
        "output": str(output),
        "plugin_count": manifest["plugin_count"],
        "string_table_count": manifest["string_table_count"],
        "archive_count": manifest["archive_observations"]["count"],
        "issue_count": len(manifest["issues"]),
    }, sort_keys=True))
    return 0 if manifest["successful_pin"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
