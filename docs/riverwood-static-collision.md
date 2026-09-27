# Riverwood fixed collision inventory

The current package uses schema 4 and contains a `records` table with each base
form's record type. The `statics` table is a model and bounds catalog, not a
list of fixed physical objects: it also contains `MISC`, `WEAP`, `CONT`, `DOOR`,
`FURN`, and other types. Collision eligibility therefore comes from
`records.record_type` joined through `references.base_form_id`.

The 5 × 5 exterior-cell window at grid X 3–7, Y −14–−10 contains 1,002
`STAT` references, 1,005 `TREE`, 68 `MSTT`, and 24 `MISC` references.
Within `STAT`, 409 placements use `Landscape/Rocks/` models and 195 use
`Architecture/` models. All 604 of those placements have a converted GLB in
the current Riverwood package, spanning 74 distinct models. The remaining
398 `STAT` placements include the firewood, pine, and road-ramp families now
covered below, as well as objects still outside the proxy policy.
The selected rock placements include 24 `RockCliff` instances across five models.

The converter exports render geometry to GLB but does not export Havok
collision shapes. Original NIF files are present in the larger conversion
input tree, but the installed NIF parser's Havok block definitions have no
established collision-shape extraction path. No runtime collider is labeled
as original collision.

Current policy: base type `STAT` under `meshes/landscape/rocks/` or
`meshes/architecture/` can receive a fixed render-triangle proxy. Riverwood
also admits `STAT` pine logs/stumps, firewood piles, and road ramps, plus
`TREE` pine models under `meshes/landscape/trees/treepineforest`. Other `TREE`
models, `FLOR`, `MSTT`, `FURN`, `DOOR`, `MISC`, and other base types remain
without fixed colliders. This keeps shrubs and movable clutter passable.

The proxy uses opaque mesh primitives and the converted scene's node
transforms; the reference transform supplies placement, rotation, and scale.
Riverwood's `RockCliff` rock faces use `BLEND`, and the lumbermill's walkway
and ramp use a named `MASK` primitive. Those specific primitives contribute
triangles. Other blended, masked, material-excluded, empty, invalid, or unloaded
primitives cannot make a proxy. A model with no supported triangles is skipped.
The runtime report counts proxy placements and eligible placements skipped.

This proxy follows rendered openings but may differ from Skyrim's authored
Havok geometry, especially stairs, door thresholds, roofs, foliage, and small
details. The lumbermill walkway's alpha cutouts are treated as solid triangles.
Riverwood playtesting must check those contacts before static collision can
be treated as accepted.
