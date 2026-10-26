# Dataset capabilities

A converted dataset contains the upstream core and optional, versioned extensions. The core is
what the ordinary converter produces: its tables, columns, files, glTF data and manifest fields.
Extensions own whole additional tables, indexes, files or glTF extension names. They must not
change core rows, add columns to core tables, or add keys inside a core glTF extension.
Promoting extension output into core requires an ordinary core change and the applicable schema
version change. This capability contract itself does not change the core schema versions.

The implementation is `shared::extensions` (types, status and hashing),
`converter::extensions` (descriptors and publication), and `engine::extensions` (runtime state and
ordinary-door policy). Optional descriptors do not install producers or readers.

## Manifest and publication

`conversion-manifest.json` has an optional `extensions` array. An absent array loads as empty;
an empty array is omitted on serialization. Existing core fields retain their meanings.

```json
"extensions": [
  { "id": "example.payload", "version": 1,
    "owns": { "tables": ["extra_payload"], "indexes": [], "files": [], "gltf_extensions": [] },
    "sha256": "<64 hex digits>", "built_from": "<core build identity>",
    "stats": { "extra_payload": 2 } }
]
```

| Field | Contract |
|---|---|
| `id` | Stable dotted name; never reused for different data. |
| `version` | Payload contract version, independent of the core schema. |
| `owns` | Sorted lists of table/index names, dataset-relative files, and glTF extension names. A file entry ending in `/` owns files below that directory. Extensions must have disjoint ownership. SQL table/index names use SQLite ASCII case equivalence; file and glTF names compare exactly. Declared spelling remains part of the content hash. |
| `sha256` | `mudcrab-ext-hash-v1` digest of the owned content. |
| `built_from` | Recorded `lod_build.build_identity`, or the empty string when the dataset has no recorded identity. Readers compare it with the current core identity. |
| `stats` | Producer-defined counts. Converter descriptors count owned table rows; the inventory descriptor also counts terrain/object blocks. |

Full conversion, metadata rebuild and asset repair refresh the converter's entries after payload
finalization and before saving the staged manifest. Existing asset locks, provenance checks,
record-data completeness, schema add-on identity, derived-output proofs and transactional
publication remain responsible for publishing the dataset. Capability publication does not
invent a native LOD inventory or change ordinary door output.

`refresh_manifest_with_finalized` composes entries supplied by other finalizers. The caller must
hold the exclusive asset lock and supply entries from finalizers that ran under it; this function
mints no provenance. It requires a nonempty matching core identity, unique IDs, disjoint table,
index, file and glTF ownership (including nested file prefixes), and hashes matching actual owned
bytes. Errors leave the in-memory manifest unchanged. It rejects the retired door extension ID
and known core table/index claims, including all six core RTree shadow tables, ASCII
case-insensitively. The static guard is not a complete registry of all possible core files or glTF
names; callers remain responsible for those ownership rules.
Foreign version/statistic meanings belong to their producers.

## Recognized payload descriptors

These describe already-produced payloads. This change adds no optional payload producer or reader.
A table-based entry is emitted only when every owned table exists, including when all are empty.
A material entry requires an actual mesh declaration in `extensionsUsed`.

| ID | Version | Ownership |
|---|---|---|
| `mudcrab.lod_inventory` | 1 | `lod_grid`, `lod_block`, `lod_tree_type`, `lod_tree_instance`; `idx_lod_tree_block`; files below `meshes/terrain/`. |
| `mudcrab.space_lighting` | 1 | `space_lighting`. |
| `mudcrab.directional_snow` | 1 | `matos`, `static_materials`. |
| `mudcrab.material` | 1 | glTF `MUDCRAB_material`, `OPEN_SKYRIM_material_animation`. |

`mudcrab.door_links` remains a supported legacy ID. Current upstream owns `door_links` and
`emittance`, so new converter manifests do not declare that extension. Publication retires an old
door declaration. Native inventory hashing is covered by synthetic schemas and files in tests;
ordinary real-conversion tests expect core output only.

## Content hash: `mudcrab-ext-hash-v1`

The digest is SHA-256 over UTF-8 lines terminated by LF: algorithm name, extension ID, version,
then owned tables, indexes, files and glTF extension values in deterministic order. Table rows
are canonically encoded and sorted before column declarations; indexes use their stored SQL;
files contribute their content SHA-256; glTF values contribute a hash of their canonical encoding.
Strings and numeric bits are encoded without depending on a float printer. JSON parsing enables
`serde_json/float_roundtrip` so Rust and the independent Python reference agree on binary64 values.
That workspace feature is shared with other JSON consumers and requires their usual fixture checks.

`shared::extensions::ExtensionHasher` defines the Rust stream. The neutral standard-library oracle
is `crates/converter/tests/support/extension_hash_reference.py`. It can be run directly:

```text
python -I crates/converter/tests/support/extension_hash_reference.py <dataset-root>
```

Exit 0 means every listed content hash matches. The oracle checks hashes and duplicate ownership;
it does not establish producer provenance, certify foreign statistics, or enforce runtime version
support. Rust integration tests compare synthetic inventory and foreign-finalizer output with it,
retain pinned SQL/glTF sample hashes, and reject tampered bytes. Qualified database hashing and
core identity lookup reject duplicate objects and temporary/view shadowing rather than silently
hashing a different object.

## Engine behavior

The engine reads declarations once under its existing shared asset lock. It inserts
`DataSetExtensions` before renderer plugins build, then installs the validated state and
`ExtensionNotices` for runtime consumers. A missing manifest is treated as no declarations by the
capability loader; the existing runtime asset validation still decides whether the run is allowed.
Malformed manifests are errors. Generic capability status checks declaration presence, supported
version and core identity when one is recorded; without a recorded identity, status does not
classify a declaration as stale. Only an absent identity table or no identity row counts as absence;
identity query, schema and value-type failures are contextual load errors. It does not recompute
payload SHA-256 on every open.

| Door state | Ordinary database behavior |
|---|---|
| No legacy declaration, complete core table | Read core doors using the existing ten-column detector. |
| No declaration, absent or incomplete old core table | Preserve the existing optional-table behavior; references have no door link. |
| Supported legacy declaration, not stale | Require a complete table at open; missing/incomplete schema is an error. |
| Stale or unsupported legacy declaration | Disable the door join, even when a complete core table is present. |

`WorldDatabase::open` retains the default core policy. `open_with` carries the validated policy
through the existing worker and reference query while preserving NULL column positions, arrival
handling and enable-parent resolution. Material status is exposed as metadata for a subsequent
reader; this change registers no material playback, portal graph, private lighting/snow catalog,
or native LOD reader. Unknown IDs are retained for their owners. Refresh/inspection CLI commands
are deferred to a separate capability-maintenance change.
