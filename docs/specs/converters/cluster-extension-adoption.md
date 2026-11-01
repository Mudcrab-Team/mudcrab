# Cluster extension binding and publication

Cluster payloads use the generic [dataset capability contract](extensions.md).
Their binary layout and reference coverage are documented in
[ADR-0013](../../adr/0013-cluster-lod-payload-format.md). Cluster availability is
separate from the base's core schema versions and from the user's runtime selection.

## Ownership

| Extension | Wrapper version | Owned content |
|---|---|---|
| `mudcrab.clusters` | 1 | Geometry store tables (`cluster_build`, `cluster_meshes`, `cluster_chunks`, `cluster_chunks_spatial`, its SQLite shadow tables, `cluster_rejects`), companion provenance/map/coverage tables and explicit indexes, `cluster-manifest.json`, and files below `clusters/`. |
| `mudcrab.cluster_terrain` | 1 | `cluster_terrain`, `cluster_terrain_heights`, `cluster_terrain_build`, and `cluster-terrain-manifest.json`; any additional terrain-only files must be explicitly declared. |

Ownership covers whole tables and files. Object and terrain producers share the
geometry store: terrain geometry and atlases below `clusters/` are hashed by
`mudcrab.clusters`; terrain metadata owns their interpretation without claiming
those files again. Timings and adapter reports are run artifacts, outside the owned
payload tree. There is no owned-but-unhashed exception.

Cluster overlays put their extension entries in `overlay.json`, alongside
`clusters.db` and their payloads. The base `conversion-manifest.json` stays unchanged
during derivation. The reader binds the overlay to the base's core identity and
conversion-manifest fingerprint under retained asset locks. Different settings
variants use separate settings-addressed folders under `derived/`. Readers reject
published folders whose `clusters-<settings-id>` name differs from the hash of
their recorded build settings. Staging metadata is written before the final rename;
a `.staging-*` directory cannot be selected as a published overlay.

Terrain source identity is the base-bound recipe of canonical DDS asset keys and
content hashes. A raw source folder is supplied for the current invocation; new
`overlay.json` files never record its host path. When the base has no `vfs`, a
rebuild hint requires `--terrain-sources '<raw-dds-root>'`: replace that placeholder
with matching sources. Moving the folder does not change source identity, and
changed or missing source bytes refuse derivation. Older optional recovery-path
fields remain readable for diagnostic compatibility.

## Integrity and dependencies

`sha256` uses `mudcrab-ext-hash-v1` over declared tables, indexes and file bytes.
Every overlay requires `mudcrab.clusters` wrapper version 1. If
`mudcrab.cluster_terrain` is declared, its wrapper version must also be 1. An empty
or terrain-only certificate list is refused before opening the geometry store.
Both cluster entries require a nonempty `built_from` matching the base's actual
`lod_build.build_identity`. A builder identity or empty string cannot substitute
for core provenance. Refreshing a declaration cannot regenerate stale geometry.

MCL/MCI format version 3, builder identity, per-file content hashes, model/texture
inputs, block input keys and map/coverage versions remain independent checks.
Object chunk payload paths must be canonical portable relative names. Backslashes,
empty components, dot/parent components, components ending in a dot or space,
drive or stream prefixes and control characters are refused before file access. The `clusters/terrain/` namespace is
reserved for terrain, including case variants on case-insensitive filesystems.
Mapping must bind the writer's final instance order to authoritative reference,
placement, primitive and material inputs. Counts alone do not prove completeness.
Unresolved overrides and missing source inputs require explicit refusal reasons.

The generic envelope has no dependency field. The terrain-owned manifest records
its dependency on the exact `mudcrab.clusters` wrapper version and final extension
hash, and the core cell-cache digest and terrain builder/input identities. Readers
must check these before treating terrain capability as usable. An ignored JSON
field on the generic envelope provides no dependency enforcement.

Publication is acyclic: finish all object/terrain geometry; validate the final
store and mapping; write its manifest and compute its extension hash; record that
hash in the terrain manifest; then hash terrain metadata. The geometry manifest
must not include the dependent terrain extension's hash. Publish the complete
overlay through staging while retaining the appropriate locks.

Geometry that incorporates another extension's material behavior must record and
validate that exact input digest. Unrelated optional extensions do not become
dependencies merely by being present.

## Consumer requirements

A capability declaration does not establish current draw readiness. The runtime
checks source correspondence, payloads, enabled pass pipelines and view-specific
ownership before replacing source geometry. Missing, stale or unsupported data
must name the failed dependency and a remedy; selection remains distinct from
capability availability. The shared `validate_geometry_build_contract` helper
defines cluster-mode admission using the conversion producer schema required by
baked LOD packages without requiring baked LOD products. The helper is added for
cluster reader integration; startup wiring is a separate change. Existing baked
startup continues through `validate_lod_build_contract`. Legacy baked datasets
retain their existing admission rules. The base geometry declaration
determines which products were generated, so the presence of an overlay does not
imply a baked fallback.

Validate publication and consumption with synthetic fixtures for deterministic
hashes, disjoint ownership, missing/stale dependencies, altered input bytes,
partial coverage and failed publication. GPU/visual parity and performance require
separate integration evidence on the complete rendered content.
