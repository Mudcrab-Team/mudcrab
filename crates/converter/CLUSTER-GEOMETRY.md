# Cluster conversion geometry

`--lod-mode baked` remains the default and preserves the previous manifest fields and asset
behavior. `auto` retains the historical baked base plus an optional overlay; its terrain overlay
still requires `--cluster-terrain`. Standalone overlay derivation keeps its explicit options.

`--lod-mode cluster` builds a cluster-only conversion. It always includes TREE placements and
terrain, even without `--cluster-terrain`. It rejects contradictory `--no-lod` and
`--extract-all-archive-files` requests before staging. No baked fallback is generated.

Before publishing the base, its conversion manifest records `geometry_mode: "cluster"` and
`cluster_package: "derived/clusters-<settings-id>"`. The selector and the actual overlay use the
same resolved derivation settings. The manifest is never edited after deriving that overlay.
The label is descriptive; consumers select the recorded relative folder and verify the existing
overlay binding and certificates. Legacy manifests default to baked geometry without adding
fields when serialized again.

## Omitted products

- Native `.btr` and `.bto` containers, including those outside the usual inventory namespace,
  are not extracted, converted, reused, or copied into the runtime pack as GLBs.
- Native `.lst` and `.btt` tree inventory under `meshes/terrain/**` is not extracted or parsed.
  Generated `meshes/terrain/<world>/trees/<world>.tree.<index>.glb` billboards are not written.
- Baked-only DDS under `textures/terrain/**` and `textures/lod/**` is not extracted or converted
  into KTX2 or its sampler/color aliases. This includes terrain, object, and tree atlases.
- Compiled baked terrain `lod/<world-id>/{4,8,16}/cell_<x>_<y>.glb` is not written. Those GLBs
  include their albedo KTX2; there is no separate baked atlas asset to preserve.
- No inventory or chunk rows are written to `lod_grid`, `lod_block`, `lod_tree_type`,
  `lod_tree_instance`, `lod_chunks`, or its spatial index. Their empty core-schema tables remain.

## Retained shared sources

The texture namespaces above identify candidates, not unconditional deletions. A surviving near
model, record texture reference, LAND diffuse source, or configured near-terrain default keeps
its DDS and runtime texture. Restoration preserves archive priority and winning loose/MO2
overrides, and verifies that archives still match the ingestion source hashes. A previous prune
audit participates before mesh reuse so a restored DDS cannot remain pruned from a cached GLB.
Other source DDS namespaces and full-detail NIFs retain their existing conversion behavior.

The canonical cell cache, worldspace origins, and optional merged near-terrain products remain.
`lod-manifest.json` remains as the verified terrain DDS recipe with zero baked chunks, and
`lod_build` remains the core identity. The overlay producer needs both. Runtime cluster admission
uses `shared::world_assets::validate_geometry_build_contract` and does not read baked LOD tables
or recipes; the cluster resolver separately validates the selected overlay.

Resume regenerates the database, terrain products, and manifest. Ingestion coverage distinguishes
cluster-only recipes from baked inventories, so switching back to baked cannot accept a cache
that omitted native inputs. Metadata rebuild can remove baked products while retaining verified
shared assets. Restoring baked products from a cluster-only source requires normal conversion.

Omitted file bytes and eliminated native mesh, billboard, texture-encoding, and terrain-baking
work are structural savings. They do not establish net package size or elapsed-time improvement:
cluster output and retained shared textures must be accounted for separately. No full conversion
or runtime timing is required to run the CPU-only regression fixtures.
