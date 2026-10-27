# Dataset capabilities

Dataset capabilities describe optional, versioned payloads alongside the upstream core. The core
is the ordinary converter's tables, columns, files, glTF data and manifest fields. An extension
owns whole additional tables, indexes, files or glTF extension names. It must not change core
rows, add columns to core tables, or add keys inside a core glTF extension. Promoting payloads
into core requires an ordinary core change and any applicable schema version change.

This contract provides the generic capability mechanism and compatibility for legacy door-link
declarations. It registers no production payloads and adds no engine feature beyond that
compatibility. Core-only output has no `extensions` key. Existing core datasets need no
reconversion, and this contract does not bump any core schema version.

The implementation is `shared::extensions` for declarations, status and hashing,
`converter::extensions` for descriptors and publication, and `engine::extensions` for runtime
capability state and ordinary-door compatibility.

## Declarations and loading

`shared::extensions::{ExtensionEntry, ExtensionOwns}` are format-independent types implementing
Serde serialization and deserialization. They do not depend on a particular producer, payload
reader or enclosing manifest format. A JSON document can embed an optional `extensions` array;
an absent array loads as empty. The base `conversion-manifest.json` omits the array when empty.
Existing core fields retain their meanings.

```json
{
  "extensions": [
    {
      "id": "example.payload",
      "version": 1,
      "owns": {
        "tables": ["example_payload"],
        "indexes": [],
        "files": ["example/payload/"],
        "gltf_extensions": []
      },
      "sha256": "<64 hex digits>",
      "built_from": "<current core build identity>",
      "stats": { "rows": 2 }
    }
  ]
}
```

| Field | Contract |
|---|---|
| `id` | Stable dotted name; never reused for different data. |
| `version` | Payload contract version, independent of the core schema. |
| `owns` | Sorted lists of table names, index names, relative file paths and glTF extension names. Ownership must be disjoint between extensions. SQL table/index names use SQLite ASCII case equivalence; file paths and glTF names compare exactly. Declared spelling remains part of the content hash. |
| `sha256` | `mudcrab-ext-hash-v1` digest of the owned content. |
| `built_from` | Core build identity from which the payload was derived. Base dataset loading obtains the current identity from `lod_build.build_identity`; other manifest loaders supply it explicitly. |
| `stats` | Optional producer-defined counts. The generic mechanism does not assign meanings to them. |

`DataExtensions::from_manifest_json(text, core_identity)` reads only the document's `extensions`
array and ignores unrelated fields. It rejects duplicate IDs. An overlay loader can pass the
text of `overlay.json` and the explicit current core identity to this same API. The identity
must come from the current core, rather than from the extension's own `built_from` claim.
Cluster capability declarations belong to the feature that introduces them and live in
`overlay.json`, not the base `conversion-manifest.json`; this contract declares none.

`DataExtensions::load(root)` is a convenience loader for the base `conversion-manifest.json`
and the current identity in `skyrim_world.db`. It does not discover or load overlays. Only an
absent identity table or no identity row counts as an absent recorded identity. Schema,
value-type and query failures are contextual load errors.

`get` and `entries` expose declarations. `status(id, supported_versions)` returns `Missing`
when the ID is absent, `UnsupportedVersion` when its version is outside the reader's range,
`Stale` when its `built_from` differs from a recorded current identity, or `Present` otherwise.
Version support is checked before staleness. Without a recorded current identity, status does
not classify a declaration as stale. Status does not recompute the payload hash or certify that
a payload reader can open the data. Unknown IDs remain available to their owners.

## Owned file paths

Every input file path must be canonical and relative to the payload root, using `/` separators.
Reject absolute paths, any colon (including a drive prefix), backslashes, control characters,
and empty, `.` or `..` segments, including segments ending in a dot or space. A single trailing `/` is allowed for a directory ownership
entry; it is not an empty interior segment. Reject noncanonical spellings rather than silently
normalizing them before validation, overlap detection or hashing.

An exact file entry must exist. A directory entry owns every file below that directory; an
absent or empty directory contributes no files. Directory ownership overlaps any owned file or
subdirectory below that prefix. For example, `example/payload/` conflicts with
`example/payload/data.bin` even though the declarations are different strings.

## Converter API and publication

`ExtensionSpec` describes an extension's ID, version and ownership. The public `specs()` and
`build_entries(output)` APIs remain available, but the built-in descriptor list is empty and
`build_entries` emits zero production entries. Finding extra tables, files or glTF values does
not register a capability. A feature must introduce its own producer and declaration contract.

`refresh_manifest(manifest, output)` replaces the manifest's declarations with the built-in
entries. With the empty built-in list, it removes old declarations and leaves core-only output
without an `extensions` key when serialized. Full conversion, metadata rebuild and asset repair
refresh declarations after payload finalization and before saving the staged manifest.

`refresh_manifest_with_finalized(manifest, output, finalized)` also accepts explicit entries
from payload finalizers. The generic mechanism can hash and validate these entries even though
it registers no built-in payloads. With no supplied entries, it behaves like `refresh_manifest`.
This API updates a base manifest; an overlay producer remains responsible for serializing its
own declarations into its overlay document.

The caller must hold the existing exclusive asset lock and supply entries from finalizers that
ran under it. Publishing explicit finalized entries requires a nonempty current core identity,
matching `built_from` values, unique IDs, disjoint ownership including nested file prefixes,
canonical file paths and hashes matching the actual owned content. Errors leave the in-memory
manifest unchanged.
`recompute_hash(output, entry)` hashes an explicit entry's owned content without requiring a
built-in descriptor; missing owned tables, indexes or exact files are errors.

Finalized publication rejects `DOOR_LINKS` (`mudcrab.door_links`) and known core table/index
claims, including the six core RTree shadow tables, using ASCII case-insensitive SQL name
comparison. The static guard is not a complete registry of all core files or glTF names;
producers remain responsible for preserving core ownership. Versions, statistics and derivation
rules belong to each producer. The generic mechanism does not establish provenance.

Existing asset locks, provenance checks, record-data completeness, schema add-on identity,
derived-output proofs and transactional publication remain responsible for dataset publication.

## Content hash: `mudcrab-ext-hash-v1`

`ExtensionHasher` accepts owned tables, indexes, files and glTF extension values, then returns
the canonical digest with `finish`. The digest is SHA-256 over UTF-8 lines terminated by LF:
the algorithm name, extension ID and version, followed by owned tables, indexes, files and glTF
values in deterministic order. Table rows are canonically encoded and sorted before column
declarations. Indexes contribute their stored SQL; files contribute their content SHA-256.
Strings and numeric bits are encoded without depending on a float printer. JSON parsing enables
`serde_json/float_roundtrip` to preserve binary64 values for the canonical encoding.

For each owned glTF name declared in `extensionsUsed`, hashing recursively covers its
occurrences throughout the JSON document, including nodes and mesh primitives. Occurrence
locations use escaped JSON pointers, so moving a payload to a different location changes the
hash. The established root and material occurrence streams retain their pinned encoding.
Extension names are matched after JSON parsing: an escaped spelling of a name has the same
meaning as its unescaped spelling and cannot bypass discovery or hashing.

`shared::extensions::ExtensionHasher` defines the Rust stream. The independent standard-library
reference is `crates/converter/tests/support/extension_hash_reference.py`. It can check content
hashes recorded in a base manifest:

```text
python -I crates/converter/tests/support/extension_hash_reference.py <dataset-root>
```

Exit 0 means every listed content hash matches. This check does not establish producer
provenance, certify producer-defined statistics or enforce runtime version support. Qualified
database hashing and core identity lookup reject duplicate objects and temporary/view shadowing
rather than silently reading a different object.

## Runtime state and legacy doors

The engine reads base declarations once under its existing shared asset lock.
`engine::extensions::DataSetExtensions::data` exposes the generic registry to runtime consumers.
`read_data_extensions`, `open_features` and `RuntimeFeatures::install` handle declaration state,
`ExtensionNotices` and the ordinary database's door policy. A missing manifest supplies no
declarations; existing runtime asset validation still decides whether the dataset can open.
Malformed manifests are errors. Payload readers must validate their data before installing
resources or systems; capability metadata alone does not install a reader.

Current core owns `door_links` and `emittance`. New converter output does not declare
`DOOR_LINKS`, and refreshing a manifest retires an old declaration. The engine retains support
for legacy `mudcrab.door_links` version 1 declarations:

| Door state | Ordinary database behavior |
|---|---|
| No legacy declaration, complete core table | Read core doors using the existing ten-column detector. |
| No declaration, absent or incomplete old core table | Preserve the existing optional-table behavior; references have no door link. |
| Supported legacy declaration, not stale | Require a complete table at open; missing or incomplete schema is an error. |
| Stale or unsupported legacy declaration | Disable the door join, even when a complete core table is present. |

`WorldDatabase::open` retains the default core policy. `open_with` carries the validated policy
through the existing worker and reference query while preserving NULL column positions, arrival
handling and enable-parent resolution. No additional payload catalog, producer or engine reader
is registered by this mechanism.
