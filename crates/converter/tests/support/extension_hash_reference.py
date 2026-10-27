"""Neutral test oracle for the mudcrab-ext-hash-v1 extension hash stream.

Extracted from tools/extension_hash.py at c1233aa80c71d3ea3a485917ef5ffe28f6b43431.
SQL ownership uses SQLite ASCII case equivalence, and index lookup accepts SQL case aliases.
The canonical hash encoding and base dataset check are retained.
Run with a dataset root to print problems and exit 1 when invalid.
"""


from __future__ import annotations


import argparse


import hashlib


import json


import sqlite3


import struct


import sys


from pathlib import Path


ALGORITHM = "mudcrab-ext-hash-v1"
DATABASE = "skyrim_world.db"
MANIFEST = "conversion-manifest.json"
GLB_MAGIC = 0x46546C67
GLB_JSON_CHUNK = 0x4E4F534A
SQL_IDENTIFIER_CASE = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")


def _hex_text(text: str) -> str:
    return text.encode("utf-8").hex()


def _real(value: float) -> str:
    return "f" + struct.pack(">d", value).hex()


def encode_sql(value) -> str:
    """One SQLite value as `n`, `i<dec>`, `f<bits>`, `t<utf8 hex>` or `b<hex>`."""
    if value is None:
        return "n"
    if isinstance(value, bool):  # sqlite3 never returns bool, but keep ints honest
        return f"i{int(value)}"
    if isinstance(value, int):
        return f"i{value}"
    if isinstance(value, float):
        return _real(value)
    if isinstance(value, str):
        return "t" + _hex_text(value)
    if isinstance(value, (bytes, bytearray, memoryview)):
        return "b" + bytes(value).hex()
    raise TypeError(f"unsupported SQLite value {value!r}")


def encode_json(value) -> str:
    """Canonical JSON: sorted keys, numbers as binary64 bits, strings as UTF-8 hex."""
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return _real(float(value))
    if isinstance(value, str):
        return "t" + _hex_text(value)
    if isinstance(value, list):
        return "[" + ",".join(encode_json(item) for item in value) + "]"
    if isinstance(value, dict):
        keys = sorted(value, key=lambda key: key.encode("utf-8"))
        return "{" + ",".join(f"t{_hex_text(key)}:{encode_json(value[key])}" for key in keys) + "}"
    raise TypeError(f"unsupported JSON value {value!r}")


def _open_read_only(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(f"{path.resolve().as_uri()}?mode=ro", uri=True)


def _quote(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def table_lines(connection: sqlite3.Connection, table: str) -> list[str]:
    columns = connection.execute(f"PRAGMA table_info({_quote(table)})").fetchall()
    if not columns:
        raise LookupError(f"table {table} is missing")
    rows = sorted(
        ",".join(encode_sql(value) for value in row)
        for row in connection.execute(f"SELECT * FROM {_quote(table)}")
    )
    declared = ",".join(f"{column[1]}:{column[2]}" for column in columns)
    return [f"table {table}", *rows, f"columns {declared}"]


def index_line(connection: sqlite3.Connection, index: str) -> str:
    row = connection.execute(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ? COLLATE NOCASE", (index,)
    ).fetchone()
    if row is None:
        raise LookupError(f"index {index} is missing")
    return f"index {index} {row[0]}"


def owned_files(root: Path, entries: list[str]) -> list[str]:
    """Expand directory entries (ending in `/`) to every file below them, sorted bytewise.

    A directory that does not exist covers no files; an exact file entry that is missing is an error.
    """
    paths: set[str] = set()
    for entry in entries:
        relative = entry[:-1] if entry.endswith('/') else entry
        if (not relative or "\\" in entry or ":" in entry or any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in entry)
                or any(part in ("", ".", "..") or part.endswith((".", " ")) for part in relative.split('/'))):
            raise ValueError(f"owned file path {entry!r} must be a canonical dataset-relative path")
        if entry.endswith("/"):
            folder = root / entry
            if not folder.is_dir():
                continue  # a directory entry covers whatever is below it, which can be nothing
            paths.update(p.relative_to(root).as_posix() for p in folder.rglob("*") if p.is_file())
        else:
            if not (root / entry).is_file():
                raise LookupError(f"file {entry} is missing")
            paths.add(entry)
    return sorted(paths, key=lambda path: path.encode("utf-8"))


def glb_json(path: Path) -> dict:
    data = path.read_bytes()
    if path.suffix.lower() == ".gltf":
        return json.loads(data)
    magic, _version, _length = struct.unpack_from("<III", data, 0)
    if magic != GLB_MAGIC:
        raise ValueError(f"{path} is not a GLB")
    chunk_length, chunk_type = struct.unpack_from("<II", data, 12)
    if chunk_type != GLB_JSON_CHUNK:
        raise ValueError(f"{path} has no JSON chunk first")
    return json.loads(data[20 : 20 + chunk_length])


def gltf_extension_value(document: dict, name: str) -> str | None:
    """All extension occurrences, located by JSON pointers in stable object/array order."""
    parts = []

    def pointer(segment: str) -> str:
        return segment.replace("~", "~0").replace("/", "~1")

    def visit(value, path: str) -> None:
        if isinstance(value, dict):
            extensions = value.get("extensions")
            if isinstance(extensions, dict) and name in extensions:
                parts.append(f"{path}/extensions/{pointer(name)}={encode_json(extensions[name])}")
            for key in sorted(value, key=lambda key: key.encode("utf-8")):
                visit(value[key], f"{path}/{pointer(key)}")
        elif isinstance(value, list):
            for index, item in enumerate(value):
                visit(item, f"{path}/{index}")

    visit(document, "")
    return ";".join(parts) if parts else None


def mesh_files(root: Path) -> list[Path]:
    meshes = [p for p in root.rglob("*") if p.suffix.lower() in (".glb", ".gltf") and p.is_file()]
    return sorted(meshes, key=lambda p: p.relative_to(root).as_posix().encode("utf-8"))


def extension_lines(
    root: Path, entry: dict, meshes: list[Path] | None = None, database: Path | None = None
) -> list[str]:
    """The hashed stream of one entry. `database` is the file holding its tables and indexes
    (default `<root>/skyrim_world.db`; an overlay passes its `clusters.db`)."""
    owns = entry.get("owns", {})
    lines = [ALGORITHM, str(entry["id"]), str(int(entry["version"]))]
    tables = sorted(owns.get("tables", []))
    indexes = sorted(owns.get("indexes", []))
    if tables or indexes:
        connection = _open_read_only(database if database is not None else root / DATABASE)
        try:
            for table in tables:
                lines.extend(table_lines(connection, table))
            for index in indexes:
                lines.append(index_line(connection, index))
        finally:
            connection.close()
    for path in owned_files(root, owns.get("files", [])):
        lines.append(f"file {path} {hashlib.sha256((root / path).read_bytes()).hexdigest()}")
    gltf_names = sorted(owns.get("gltf_extensions", []))
    if gltf_names:
        if meshes is None:
            meshes = mesh_files(root)
        documents = [(m.relative_to(root).as_posix(), glb_json(m)) for m in meshes]
        for name in gltf_names:
            for path, document in documents:
                if name not in document.get("extensionsUsed", []):
                    continue
                value = gltf_extension_value(document, name)
                if value is not None:
                    digest = hashlib.sha256(value.encode("utf-8")).hexdigest()
                    lines.append(f"gltf {name} {path} {digest}")
    return lines


def extension_hash(
    root: Path, entry: dict, meshes: list[Path] | None = None, database: Path | None = None
) -> str:
    digest = hashlib.sha256()
    for line in extension_lines(root, entry, meshes, database):
        digest.update(line.encode("utf-8"))
        digest.update(b"\n")
    return digest.hexdigest()


def load_extensions(root: Path) -> list[dict]:
    manifest = json.loads((root / MANIFEST).read_text(encoding="utf-8"))
    return list(manifest.get("extensions", []))


def ownership_problems(extensions: list[dict]) -> list[str]:
    """Disjoint ownership: SQL names compare ASCII case-insensitively; files/glTF exactly."""
    seen: dict[tuple[str, str], str] = {}
    problems = []
    ids = set()
    for entry in extensions:
        if entry["id"] in ids:
            problems.append(f"extension {entry['id']} is listed twice")
        ids.add(entry["id"])
        for kind, names in entry.get("owns", {}).items():
            for name in names:
                key = name.translate(SQL_IDENTIFIER_CASE) if kind in ("tables", "indexes") else name
                owner = seen.setdefault((kind, key), entry["id"])
                if owner != entry["id"]:
                    problems.append(f"{kind[:-1]} {name} is owned by {owner} and {entry['id']}")
    return problems


def check(root: Path) -> list[str]:
    """Problems with the data set's extension list; empty means every hash matches."""
    extensions = load_extensions(root)
    problems = ownership_problems(extensions)
    meshes = None
    if any(entry.get("owns", {}).get("gltf_extensions") for entry in extensions):
        meshes = mesh_files(root)
    for entry in extensions:
        try:
            actual = extension_hash(root, entry, meshes)
        except (LookupError, ValueError) as error:
            problems.append(f"{entry['id']}: {error}")
            continue
        if actual != entry.get("sha256"):
            problems.append(f"{entry['id']}: sha256 {entry.get('sha256')} in the manifest, {actual} on disk")
    return problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Check a dataset's extension hashes.")
    parser.add_argument("root", type=Path, help="the dataset root")
    args = parser.parse_args(argv)
    try:
        problems = check(args.root)
    except (
        OSError, ValueError, TypeError, LookupError, AttributeError, OverflowError,
        sqlite3.DatabaseError, struct.error,
    ) as error:
        problems = [str(error)]
    for problem in problems:
        print(f"PROBLEM {problem}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
