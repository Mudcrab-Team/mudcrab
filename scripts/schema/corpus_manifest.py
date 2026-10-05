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


MANIFEST_VERSION = 3
CORPUS_EVIDENCE_VERSION = 1
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
MAX_CORPUS_EVIDENCE_BYTES = 1024 * 1024
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


def _file_identity_token(file_stat: os.stat_result) -> tuple[int, int, int]:
    return (
        file_stat.st_dev,
        file_stat.st_ino,
        stat.S_IFMT(file_stat.st_mode),
    )


def _same_path_descriptor_state(path_stat: os.stat_result, descriptor_stat: os.stat_result) -> bool:
    if _file_identity_token(path_stat) != _file_identity_token(descriptor_stat):
        return False
    if path_stat.st_size != descriptor_stat.st_size:
        return False
    # Windows can report different timestamps through lstat and fstat for the
    # same file. Descriptor-to-descriptor checks below still detect mutations.
    return os.name == "nt" or _stat_token(path_stat) == _stat_token(descriptor_stat)


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
            if not _same_path_descriptor_state(path_before, before):
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
        if not _same_path_descriptor_state(path_after, after):
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


def _exact_object(value: object, keys: set[str], label: str) -> dict:
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    actual = set(value)
    missing = sorted(keys - actual)
    unknown = sorted(actual - keys)
    if missing or unknown:
        detail = []
        if missing:
            detail.append(f"missing keys: {', '.join(missing)}")
        if unknown:
            detail.append(f"unsupported keys: {', '.join(unknown)}")
        raise ValueError(f"{label} has an invalid shape ({'; '.join(detail)})")
    return value


def _required_text(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{label} must be a non-empty string")
    return value.strip()


def _required_name_list(value: object, label: str, *, allow_empty: bool) -> list[str]:
    if not isinstance(value, list) or (not value and not allow_empty):
        qualifier = "a list" if allow_empty else "a non-empty list"
        raise ValueError(f"{label} must be {qualifier} of plugin filenames")
    names = []
    seen = set()
    for index, raw_name in enumerate(value):
        name = _required_text(raw_name, f"{label}[{index}]")
        if name in {".", ".."} or "/" in name or "\\" in name:
            raise ValueError(f"{label}[{index}] must be a plain plugin filename")
        if Path(name).suffix.lower() not in PLUGIN_SUFFIXES:
            raise ValueError(f"{label}[{index}] does not have a supported plugin suffix: {name}")
        folded = name.casefold()
        if folded in seen:
            raise ValueError(f"{label} repeats a case-insensitive plugin name: {name}")
        seen.add(folded)
        names.append(name)
    return names


def _unique_json_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"corpus evidence JSON repeats object key: {key}")
        result[key] = value
    return result


def _read_evidence_artifact(
    descriptor_path: Path, value: object, label: str
) -> dict:
    evidence = _exact_object(value, {"path", "sha256"}, f"{label}.evidence")
    declared_path = _required_text(evidence["path"], f"{label}.evidence.path")
    expected_digest = evidence["sha256"]
    if (
        not isinstance(expected_digest, str)
        or len(expected_digest) != 64
        or any(character not in "0123456789abcdef" for character in expected_digest)
    ):
        raise ValueError(f"{label}.evidence.sha256 must be 64 lowercase hexadecimal characters")

    evidence_path = Path(declared_path).expanduser()
    if not evidence_path.is_absolute():
        evidence_path = descriptor_path.parent / evidence_path
    evidence_path = evidence_path.absolute()
    if evidence_path == descriptor_path:
        raise ValueError(f"{label}.evidence.path cannot point to the evidence descriptor itself")
    digest, size, _ = _read_verified_file(evidence_path)
    if digest != expected_digest:
        raise ValueError(
            f"{label}.evidence hash mismatch for {evidence_path}: "
            f"declared {expected_digest}, observed {digest}"
        )
    return {
        "path": str(evidence_path),
        "size": size,
        "sha256": digest,
        "status": "source_artifact_hash_verified; semantic_claim_unverified",
    }


def _read_corpus_evidence(path: Path | None) -> dict | None:
    if path is None:
        return None
    descriptor_path = Path(path).expanduser().absolute()
    descriptor_size = _path_stat(descriptor_path).st_size
    if descriptor_size > MAX_CORPUS_EVIDENCE_BYTES:
        raise ValueError("corpus evidence JSON exceeds the 1 MiB input limit")
    digest, size, raw = _read_verified_file(
        descriptor_path, capture_bytes=MAX_CORPUS_EVIDENCE_BYTES + 1
    )
    if size > MAX_CORPUS_EVIDENCE_BYTES:
        raise ValueError("corpus evidence JSON exceeds the 1 MiB input limit")
    try:
        payload = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_json_object)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError, RecursionError) as exc:
        raise ValueError(f"invalid corpus evidence JSON {descriptor_path}: {exc}") from exc

    root = _exact_object(
        payload,
        {"schema_version", "corpus_profile", "locale", "load_order"},
        "corpus evidence",
    )
    if type(root["schema_version"]) is not int or root["schema_version"] != CORPUS_EVIDENCE_VERSION:
        raise ValueError(
            f"corpus evidence schema_version must be {CORPUS_EVIDENCE_VERSION}"
        )

    profile = _exact_object(
        root["corpus_profile"], {"id", "target", "evidence"}, "corpus_profile"
    )
    profile_id = _required_text(profile["id"], "corpus_profile.id")
    target = _exact_object(
        profile["target"], {"game", "executable_version", "steam_build"}, "corpus_profile.target"
    )
    expected_target = {
        "game": TARGET_RUNTIME["game"],
        "executable_version": TARGET_RUNTIME["executable_version"],
        "steam_build": TARGET_RUNTIME["steam_build"],
    }
    if target != expected_target:
        raise ValueError(
            "corpus_profile.target does not match the required Steam Skyrim SE/AE "
            f"1.7.104.0 build 24914197 target: expected {expected_target}"
        )
    profile_evidence = _read_evidence_artifact(
        descriptor_path, profile["evidence"], "corpus_profile"
    )

    locale = _exact_object(root["locale"], {"value", "evidence"}, "locale")
    locale_value = _required_text(locale["value"], "locale.value")
    locale_evidence = _read_evidence_artifact(
        descriptor_path, locale["evidence"], "locale"
    )

    load_order = _exact_object(
        root["load_order"],
        {"active_plugins", "unloaded_optional_plugins", "evidence"},
        "load_order",
    )
    active_plugins = _required_name_list(
        load_order["active_plugins"], "load_order.active_plugins", allow_empty=False
    )
    unloaded_plugins = _required_name_list(
        load_order["unloaded_optional_plugins"],
        "load_order.unloaded_optional_plugins",
        allow_empty=True,
    )
    overlap = {name.casefold() for name in active_plugins} & {
        name.casefold() for name in unloaded_plugins
    }
    if overlap:
        raise ValueError(
            "load_order classifies plugins as both active and unloaded: "
            + ", ".join(sorted(overlap))
        )
    load_order_evidence = _read_evidence_artifact(
        descriptor_path, load_order["evidence"], "load_order"
    )

    return {
        "descriptor": {
            "path": str(descriptor_path),
            "size": size,
            "sha256": digest,
            "status": "supplied_input_hash_verified; semantic_claims_unverified",
        },
        "corpus_profile": {
            "id": profile_id,
            "target": expected_target,
            "evidence": profile_evidence,
            "status": "supplied_evidence_hash_verified",
            "semantic_status": "unverified",
        },
        "locale": {
            "value": locale_value,
            "evidence": locale_evidence,
            "status": "supplied_evidence_hash_verified",
            "semantic_status": "unverified",
        },
        "load_order": {
            "active_plugins": active_plugins,
            "unloaded_optional_plugins": unloaded_plugins,
            "evidence": load_order_evidence,
            "status": "supplied_evidence_hash_verified",
            "semantic_status": "unverified",
        },
    }


def _validate_supplied_load_order(load_order: dict, plugins: list[dict]) -> dict:
    by_name: dict[str, list[dict]] = {}
    for plugin in plugins:
        by_name.setdefault(plugin["name"].casefold(), []).append(plugin)
    duplicates = sorted(name for name, matches in by_name.items() if len(matches) != 1)
    if duplicates:
        raise ValueError(
            "cannot validate supplied load order against duplicate installed plugin names: "
            + ", ".join(duplicates)
        )

    installed_names = {folded: matches[0]["name"] for folded, matches in by_name.items()}
    active = load_order["active_plugins"]
    unloaded = load_order["unloaded_optional_plugins"]
    active_folded = [name.casefold() for name in active]
    unloaded_folded = [name.casefold() for name in unloaded]
    supplied = set(active_folded) | set(unloaded_folded)
    unknown = sorted(supplied - installed_names.keys())
    omitted = sorted(installed_names.keys() - supplied)
    if unknown:
        raise ValueError("load_order names are not installed in Data: " + ", ".join(unknown))
    if omitted:
        raise ValueError(
            "load_order must classify every installed plugin as active or unloaded optional; "
            "unclassified: " + ", ".join(omitted)
        )

    active_index = {name: index for index, name in enumerate(active_folded)}
    inactive_required = [
        name for name in REQUIRED_BASE_PLUGINS if name.casefold() not in active_index
    ]
    if inactive_required:
        raise ValueError(
            "load_order cannot mark required base plugins unloaded: "
            + ", ".join(inactive_required)
        )

    for index, folded_name in enumerate(active_folded):
        plugin = by_name[folded_name][0]
        tes4 = plugin["tes4"]
        if tes4.get("status") != "decoded":
            raise ValueError(
                f"load_order cannot validate {plugin['name']}: TES4 masters were not decoded"
            )
        for master in tes4["masters"]:
            master_folded = master.casefold()
            master_index = active_index.get(master_folded)
            if master_index is None:
                raise ValueError(
                    f"load_order omits active master {master} required by {plugin['name']}"
                )
            if master_index >= index:
                raise ValueError(
                    f"load_order places master {master} after dependent plugin {plugin['name']}"
                )

    return {
        "status": load_order["status"],
        "semantic_status": "dependency_order_checked; active-state_claim_unverified",
        "source": load_order["evidence"],
        "entries": [installed_names[name] for name in active_folded],
        "unloaded_optional_plugins": [installed_names[name] for name in unloaded_folded],
        "dependency_order_validated": True,
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


def _normalize_leaf_path(path: Path) -> Path:
    """Resolve directory aliases and `..` while keeping the final name unresolved."""
    path = Path(path).expanduser()
    return path.parent.resolve(strict=True) / path.name


def build_manifest(
    game_root: Path,
    data_root: Path,
    executable: Path,
    ccc_path: Path | None = None,
    corpus_evidence_path: Path | None = None,
) -> dict:
    game_root = Path(game_root).resolve(strict=True)
    data_root = Path(data_root).resolve(strict=True)
    executable = _normalize_leaf_path(executable)
    ccc_path = _normalize_leaf_path(ccc_path) if ccc_path is not None else game_root / "Skyrim.ccc"
    if not game_root.is_dir():
        raise ValueError(f"game root is not a directory: {game_root}")
    if not data_root.is_dir():
        raise ValueError(f"Data root is not a directory: {data_root}")
    if executable.parent != game_root:
        raise ValueError("executable must be an immediate child of the supplied game root")
    if ccc_path.parent != game_root:
        raise ValueError("Skyrim.ccc must be an immediate child of the supplied game root")
    ccc_source = _source_path(game_root, ccc_path, "game")
    supplied_evidence = _read_corpus_evidence(corpus_evidence_path)

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

    discovered_plugin_paths = _discover_files(data_root, PLUGIN_SUFFIXES, issues)
    plugin_paths = [path for path in discovered_plugin_paths if path.parent == data_root]
    nested_plugin_paths = [path for path in discovered_plugin_paths if path.parent != data_root]
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
                "path": path.relative_to(data_root).as_posix(),
                "detail": str(exc),
            })

    nested_plugins = []
    for path in nested_plugin_paths:
        relative_path = path.relative_to(data_root).as_posix()
        issues.append({
            "code": "nested_plugin_not_loadable",
            "name": path.name,
            "path": relative_path,
            "detail": "plugin files below Data's top level are observed but excluded from the loadable plugin set",
        })
        try:
            plugin = scan_plugin(path, data_root)
            nested_plugins.append(plugin)
            if plugin["tes4"].get("status") != "decoded":
                issues.append({
                    "code": plugin["tes4"]["error"]["code"],
                    "plugin": plugin["name"],
                    "path": relative_path,
                    "detail": plugin["tes4"]["error"]["detail"],
                })
        except SourceDriftError as exc:
            missing = isinstance(exc, MissingSourceError)
            nested_plugins.append({
                "name": path.name,
                "source": _source_path(data_root, path, "data"),
                "size": None,
                "sha256": None,
                "hash_status": "failed_source_missing" if missing else "failed_source_drift",
                "tes4": {"status": "unread"},
            })
            issues.append({
                "code": "missing_required_input" if missing else "source_drift",
                "path": relative_path,
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
                "path": path.relative_to(data_root).as_posix(),
                "detail": str(exc),
            })

    archive_paths = _discover_files(data_root, ARCHIVE_SUFFIXES, issues)
    archives = []
    for path in archive_paths:
        try:
            archives.append(_scan_hashed_file(path, data_root, "data", "archive"))
        except SourceDriftError as exc:
            missing = isinstance(exc, MissingSourceError)
            archives.append({
                "name": path.name,
                "source": _source_path(data_root, path, "data"),
                "size": None,
                "sha256": None,
                "hash_status": "failed_source_missing" if missing else "failed_source_drift",
                "kind": "archive",
            })
            issues.append({
                "code": "missing_required_input" if missing else "source_drift",
                "path": path.relative_to(data_root).as_posix(),
                "detail": str(exc),
            })

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
    loadable_plugin_names = {plugin["name"].casefold() for plugin in plugins}
    nested_plugin_names = {plugin["name"].casefold() for plugin in nested_plugins}
    observed_plugin_names = loadable_plugin_names | nested_plugin_names
    ccc["matching_data_plugin_names"] = [name for name in declared_names if name.casefold() in loadable_plugin_names]
    ccc["matching_nested_plugin_names"] = [
        name for name in declared_names
        if name.casefold() in nested_plugin_names and name.casefold() not in loadable_plugin_names
    ]
    ccc["declared_names_only_nested"] = [
        name for name in declared_names
        if name.casefold() in nested_plugin_names and name.casefold() not in loadable_plugin_names
    ]
    ccc["declared_names_missing_from_data"] = [name for name in declared_names if name.casefold() not in observed_plugin_names]
    ccc["data_plugin_names_not_declared_by_ccc"] = [
        plugin["name"] for plugin in plugins if plugin["name"].casefold() not in {name.casefold() for name in declared_names}
    ]
    ccc["nested_plugin_names_not_declared_by_ccc"] = [
        plugin["name"] for plugin in nested_plugins if plugin["name"].casefold() not in {name.casefold() for name in declared_names}
    ]

    if not string_tables:
        string_table_status = "no_loose_tables_observed"
    else:
        string_table_status = "loose_tables_observed"

    plugin_complete = all(plugin["tes4"].get("status") == "decoded" for plugin in plugins)
    no_hard_issues = not issues
    archive_hashes_complete = all(
        archive["hash_status"] == "observed_candidate_pin" for archive in archives
    )
    inventory_gap_codes = {
        "data_walk_error",
        "non_file_source_skipped",
        "source_stat_error",
        "symlink_directory_skipped",
        "symlink_file_skipped",
    }
    archive_inventory_complete = not any(
        issue["code"] in inventory_gap_codes for issue in issues
    )
    archive_coverage_complete = archive_hashes_complete and archive_inventory_complete
    if supplied_evidence is None:
        load_order = {
            "status": "unresolved",
            "semantic_status": "no_explicit_evidence_supplied",
            "source": None,
            "entries": [],
            "unloaded_optional_plugins": [],
            "dependency_order_validated": False,
        }
        locale = {
            "status": "unresolved",
            "semantic_status": "no_explicit_evidence_supplied",
            "value": None,
            "evidence": None,
            "string_table_observation": string_table_status,
        }
        corpus_profile = {
            "status": "unresolved",
            "semantic_status": "no_explicit_evidence_supplied",
            "id": None,
            "evidence": None,
        }
    else:
        load_order = _validate_supplied_load_order(supplied_evidence["load_order"], plugins)
        locale = supplied_evidence["locale"]
        locale["string_table_observation"] = string_table_status
        corpus_profile = supplied_evidence["corpus_profile"]

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
    if not archive_hashes_complete:
        completion_blockers.append("archive_hash_coverage_incomplete")
    if not archive_inventory_complete:
        completion_blockers.append("archive_inventory_incomplete")
    if archives or not archive_inventory_complete:
        completion_blockers.append("bundled_string_tables_uninspected")
    if load_order["status"] == "unresolved":
        completion_blockers.append("active_load_order_unresolved")
    else:
        completion_blockers.append("active_load_order_semantics_unverified")
    if locale["status"] == "unresolved":
        completion_blockers.append("locale_unresolved")
    else:
        completion_blockers.append("locale_semantics_unverified")
    if corpus_profile["status"] == "unresolved":
        completion_blockers.append("corpus_profile_unresolved")
    else:
        completion_blockers.append("corpus_profile_semantics_unverified")
    completion_blockers.append("official_content_provenance_unresolved")
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
        "input_evidence": None if supplied_evidence is None else supplied_evidence["descriptor"],
        "corpus_profile": corpus_profile,
        "corpus_hashes": {
            "status": corpus_pin_status,
            "plugins": plugins,
            "nested_plugins": nested_plugins,
            "loose_string_tables": string_tables,
        },
        "plugin_count": len(plugins),
        "nested_plugin_count": len(nested_plugins),
        "string_table_count": len(string_tables),
        "archive_observations": {
            "status": "archive_inventory_incomplete" if not archive_inventory_complete else (
                "archive_hash_coverage_incomplete" if not archive_hashes_complete else (
                    "whole_archive_bytes_hashed; bundled_string_tables_not_inspected"
                    if archives
                    else "no_archives_observed"
                )
            ),
            "count": len(archives),
            "files": archives,
            "coverage_complete": archive_coverage_complete,
            "hashes_complete": archive_hashes_complete,
            "inventory_complete": archive_inventory_complete,
            "bundled_string_tables": {
                "status": "uninspected" if archives or not archive_inventory_complete else "no_archives_observed",
                "inspection_complete": not archives and archive_inventory_complete,
            },
            "hash_method": (
                "SHA-256 from one opened descriptor in bounded chunks; pre/post file identity "
                "and stat checks detect observed drift but do not provide an immutable snapshot"
            ),
        },
        "base_plugins": base_plugins,
        "dependency_closure": dependency_closure,
        "ccc": ccc,
        "load_order": load_order,
        "locale": locale,
        "unresolved": [
            "Skyrim.ccc names are a declared content list, not active load order or entitlement proof",
            *(
                ["BSA/BA2 archive inventory is incomplete; discovered hashes and bundled-table status are partial"]
                if not archive_inventory_complete
                else ["some BSA/BA2 whole-file hashes are incomplete; bundled string tables were not inspected"]
                if not archive_hashes_complete
                else ["BSA/BA2 file bytes were hashed, but archive entries and bundled string tables were not inspected"]
                if archives
                else []
            ),
            *(
                ["locale is unresolved; no locale was inferred from installed filenames"]
                if locale["status"] == "unresolved"
                else ["locale value and evidence bytes were supplied; the semantic claim is not independently verified"]
            ),
            *(
                ["active load order is unresolved"]
                if load_order["status"] == "unresolved"
                else ["active load-order evidence bytes were hashed and dependencies checked; the active-state claim is not independently verified"]
            ),
            "corpus-profile input cannot establish official-content provenance or entitlement",
            "official-content provenance is unresolved; filenames and CCC declarations are not proof",
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


def _paths_overlap(first: Path, second: Path) -> bool:
    try:
        second.relative_to(first)
        return True
    except ValueError:
        try:
            first.relative_to(second)
            return True
        except ValueError:
            return False


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
    parser.add_argument(
        "--corpus-evidence",
        type=Path,
        help="strict JSON candidate locale/load-order/corpus-profile inputs with SHA-256-bound evidence files",
    )
    parser.add_argument("--output", type=Path, required=True, help="manifest JSON path outside all source roots")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    try:
        game_root = args.game_root.expanduser().resolve(strict=True)
        data_root = args.data_dir.expanduser().resolve(strict=True)
        executable = args.executable.expanduser()
        ccc_path = args.ccc.expanduser() if args.ccc else game_root / "Skyrim.ccc"
        output = ensure_output_outside_sources(args.output, game_root, data_root)
        manifest = build_manifest(game_root, data_root, executable, ccc_path, args.corpus_evidence)
        evidence_paths = set()
        if manifest["input_evidence"] is not None:
            evidence_paths.add(Path(manifest["input_evidence"]["path"]).resolve(strict=True))
            for name in ("corpus_profile", "locale", "load_order"):
                artifact = manifest[name]["source"] if name == "load_order" else manifest[name]["evidence"]
                evidence_paths.add(Path(artifact["path"]).resolve(strict=True))
        for evidence_path in evidence_paths:
            if _paths_overlap(output, evidence_path):
                raise ValueError(
                    "manifest output overlaps the corpus evidence descriptor or a referenced artifact: "
                    f"{evidence_path}"
                )
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
