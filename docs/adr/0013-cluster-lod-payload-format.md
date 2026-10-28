# ADR-0013: Cluster LOD payloads for opaque statics, keyed by mesh content and chunk

- **Scope:** Shared payload and certification contract
- **Date:** 2026-10-11

## Context

Mesh geometry and placed instances change for different reasons. A converted
primitive can be shared by many placements, while winning records, transforms,
worldspace origins and material inputs can invalidate individual instance blocks.
The shared contract separates those lifetimes and validates data before a renderer
uses it. It does not establish runtime visual parity or performance acceptance.

The serialization and validation implementation is
[`shared::cluster`](../../crates/shared/src/cluster.rs). Companion coverage and
provenance are defined in
[`shared::cluster_mapping`](../../crates/shared/src/cluster_mapping.rs), and the
resolved eligibility recipe is in
[`shared::cluster_recipe`](../../crates/shared/src/cluster_recipe/mod.rs).

## Decision

### 1. Payload identity

Both binary payload kinds are little-endian and carry format version **3** and a
32-byte builder identity. `BuilderIdentityInputs` hashes canonical JSON with sorted
object keys and no whitespace. Its inputs are payload, vertex-layout and eligibility
versions; clusterlod and meshoptimizer source digests; builder configuration;
triangle threshold; block side; and converter schema. Vertex-layout and eligibility
versions are currently 1; the object block side is 16 cells.

Builder identity, payload content hash, source-model hash, per-block input key,
core build identity and extension hash have separate purposes. A matching builder
does not prove current reference inputs or current payload bytes. Producers must
invalidate changed source/material inputs and placements at their responsible layer.

### 2. Mesh cluster payload (`.mcl`)

A mesh payload describes one converted GLB primitive. Its 160-byte header contains
`MCL1`, format version, builder identity, model content hash, primitive index, flags,
vertex/cluster/group/BVH counts, root error, bounding sphere and UV decode bounds.
The following 160-byte section table has ten `{offset: u64, length: u64}` entries.
Present sections start at 256-byte boundaries; absent sections have zero entries.

| Section | Stored representation |
|---|---|
| Positions | `f32[3]` per vertex, mesh space |
| Normals | Octahedral `snorm16[2]` per vertex |
| UVs | `unorm16[2]`, decoded with the header's UV minimum and scale |
| Tangents | Optional octahedral encoding with handedness in the low bit of y |
| Colours | Optional `unorm8[4]` per vertex |
| Clusters | 48-byte `ClusterRecord` entries |
| Cluster vertices | Concatenated `u32` references into vertex streams |
| Cluster triangles | Concatenated local `u8[3]` triangles |
| Groups | 32-byte `GroupRecord` entries |
| BVH | 32-byte `BvhNode` entries |

Flag bit 0 indicates vertex colours and bit 1 tangents. Other flag bits are reserved.
Records are explicitly serialized; Rust struct layout is not the wire format.

A cluster record stores its bounding sphere, error, parent error, vertex and
triangle offsets, byte-sized vertex/triangle counts, group and refined-group
indices, and zero padding. `NO_GROUP` (`0xFFFF`) marks source geometry;
`TERMINAL_ERROR` is `f32::MAX`. Group records store sphere, error, depth and their
cluster range. BVH records store sphere, error, child range and group index.

The hierarchy selection rule compares projected error and parent error against a
screen-space threshold. Structural validation checks section bounds/alignment,
record ranges and error monotonicity. Shader behavior, material parity and the
choice of draw backend require separate runtime validation.

### 3. Instance payload (`.mci`) and database keys

An instance payload uses `MCI1` and a 96-byte header: format version, builder
identity, worldspace, tier, signed block anchor, instance/mesh counts and world
bounds. The mesh table has 48-byte entries containing a model hash, primitive index
and reserved bytes. It is sorted by `(mesh_hash, primitive_index)` with no duplicates.
The instance section starts on a 256-byte boundary.

Each 96-byte instance record contains a 3x4 row-major mesh-to-world transform,
mesh-table index, signed source-cell coordinates, 16 reserved identity bytes
(`stable_id`), flags, base-record ID, snow-material ID, source worldspace and zero
padding. Reference identity for certified ownership comes from the companion map;
the reserved `stable_id` field does not supply cross-dataset identity.

Instance ordering is `(cell_y, cell_x, mesh, stable_id)`. `write_mci_ordered` returns
the actual writer permutation so a companion map binds final ordinals rather than
assuming input order. Flag bit 0 identifies a placement from a child worldspace;
its nonzero source-worldspace field identifies that child. IDs are scoped to the
recorded core dataset. Grid membership uses the block worldspace's LOD origin;
placed worldspace, source record cell and position-derived cell remain distinct.

`CLUSTER_TABLES_DDL` defines `cluster_build`, `cluster_meshes`, `cluster_chunks`,
`cluster_chunks_spatial` and `cluster_rejects`. The R-tree has SQLite shadow tables.
Mesh rows bind payload paths/hashes to model content and primitive/material metadata.
Chunk rows bind logical worldspace/tier/anchor keys to payload hashes and input keys.
These are extension objects; their presence alone is not certification.

### 4. Vertex and material inputs

Positions remain mesh-space floats. Normal/tangent octahedral encoding, UV
quantization and colour encoding are defined by the shared codec. Cluster vertex
references address these streams; local triangle indices address cluster vertices.
Optional streams are controlled by header flags and validated against their counts.

Material keys do not replace provenance. Certified publication must verify the
actual model path, material slot and resolved material flags as well as source and
texture content. Changes to geometry, textures, overrides or builder inputs must
invalidate derived data even when a material key happens to remain unchanged.

### 5. Eligibility and coverage

The shared recipe resolves reference scope, winning inputs, override guards and
model facts. A reference refusal is distinct from a primitive refusal. Supported
resting-model types and optional tree inclusion are defined by the recipe, rather
than by hard-coded worldspace or asset lists. Unresolved state-changing inputs are
refusals; runtime drawability cannot be inferred from classifier admission alone.

`cluster_provenance` binds the mapping to the core identity, builder identity,
classifier scope and map/coverage/digest versions (currently 1).
`cluster_instance_map` binds every object record's block key, payload content hash
and final ordinal to its reference, placed world, cell, model, primitive and input
digest. A unique source index prevents duplicate ownership of one reference primitive.

`cluster_reference_coverage` has one reference-scope row: `expanded`, `unknown` or
`not_eligible`. Only `expanded` references have primitive rows, each `clustered` or
`rejected`. Expected primitives come from the resolved source, never from emitted
records. Missing coverage is unknown. Partial coverage permits ownership only of
the mapped primitives. Synthetic terrain records are not placed-reference mappings.

### 6. Extension and overlay binding

The cluster IDs are `mudcrab.clusters` and `mudcrab.cluster_terrain`, wrapper
version 1. Wrapper version, MCL/MCI format version, mapping versions and builder
identity are checked independently. The generic capability/hash contract remains
[`extensions.md`](../specs/converters/extensions.md).

[`shared::cluster_overlay`](../../crates/shared/src/cluster_overlay.rs) defines
`<base>/derived/clusters-<settings-id>/` with `overlay.json`, `clusters.db`, payloads
and manifests. Cluster extension entries live in overlay metadata. A published
overlay must declare `mudcrab.clusters` at wrapper version 1; a declared
`mudcrab.cluster_terrain` must also use wrapper version 1. Empty or terrain-only
certificate lists cannot admit the geometry store. Derivation reads the base
without rewriting its core database or conversion manifest.
The base fingerprint binds the recorded core identity and conversion-manifest
bytes; changing either invalidates the overlay.

The settings ID is the first 16 lowercase hex digits of the SHA-256 of canonical
resolved build settings. Those settings include converter version, builder identity,
triangle threshold and terrain/tree flags; scheduling, labels and host paths are
excluded. A published overlay's folder name must match those settings exactly.
Staging metadata is serialized before publication; a `.staging-*` directory is
not admitted as an overlay, even when selected by an explicit path. Renaming the
completed stage to its settings-addressed folder makes it eligible for validation.
Callers retain the required asset locks while checking bindings and opening the
base and overlay databases read-only.

`GeometryManifest` separately declares baked or cluster base geometry. Legacy
manifests default to baked and serialize without new geometry fields. Cluster mode
requires a portable `derived/clusters-<settings-id>` selector; baked mode cannot
select one. Cluster mode enforces the current converter producer schema required
by baked LOD packages without requiring baked LOD tables or a baked LOD manifest.
Legacy baked datasets retain their existing admission rules. This shared declaration
does not itself generate either geometry mode.

### 7. Runtime ownership and failures

Coverage certifies source correspondence, not draw readiness. A consumer must
separately establish current payload integrity, source eligibility, material support,
pipeline readiness, view/pass membership and representation ownership before
suppressing source draws. Unknown, stale or unsupported data must be reported with
its reason; it cannot certify complete replacement or silently omit coverage.

### 8. Interfaces and fixtures

Shared readers return validated borrowed payload sections; writers serialize
deterministically. Producers own source resolution and final publication. Runtime
consumers own GPU resources, view selection and current draw proof. Neither may
reconstruct reference identity from payload order, model paths or counts alone.

The committed `.mcl`/`.mci` fixtures and `expected.json` pin payload bytes,
sizes and hashes through `committed_fixture_pins` in
`crates/shared/src/cluster.rs`. Timing reports and threshold sweeps are run
artifacts and are not part of the shared fixture contract. Codec and mapping
tests exercise structural and provenance failures, including refusal of terrain
payloads in object chunks. Chunk payload names must be portable relative paths
with `/` separators and no empty, dot, parent, drive or stream components, or
components ending in a dot or space. Terrain namespace checks also account for
case-insensitive filesystems. These tests do not establish real-world visual
parity, hardware support or performance.

## Consequences

Mesh content can be shared across placements while instance blocks follow their
own inputs. Explicit binary and wrapper versions make incompatible data detectable.
Coverage and binding checks add validation cost and metadata, while avoiding
unproven ownership from counts or guessed identities. Integration measurements
must account for payload bytes, memory, conversion cost and complete rendered content.

### Contract invalidation

Recipe or classifier behavior changes must bump the applicable contract version.
`ELIGIBILITY_RULES_VERSION` participates in builder identity, so changing it also
changes the settings ID. `SCOPE_VERSION` instead invalidates existing certificates
through a `classifier_scope` mismatch; it does not change the settings ID. Without
the corresponding invalidation, changed rules can leave old products apparently
current or require an explicit `--replace` to rebuild.

Producers record their release version as `converter_version`; the separate
builder identity covers shared contract and producer source inputs. Engine
implementation files are never converter identity inputs.
