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
398 `STAT` placements are outside the current proxy policy.
The selected rock placements include 24 `RockCliff` instances across five models.

The converter exports render geometry to GLB but does not export Havok
collision shapes. Original NIF files are present in the larger conversion
input tree, but the installed NIF parser's Havok block definitions have no
established collision-shape extraction path. No runtime collider is labeled
as original collision.

Current policy: only base type `STAT` with a converted path under
`meshes/landscape/rocks/` or `meshes/architecture/` can receive a fixed
render-triangle proxy in interactive world runs. The proxy uses opaque mesh
primitives and the converted scene's node transforms; the reference transform
supplies placement, rotation, and scale. Riverwood's `RockCliff` GLBs put their
large rock faces in `BLEND` primitives, so those particular blended primitives
also contribute triangles. Their `MASK` detail primitives remain excluded.
Other blended, alpha masked, material-excluded, empty, invalid, or unloaded
mesh primitives cannot make a proxy. A model whose supported
primitives make no triangles is skipped. `TREE`, `FLOR`, `MSTT`, `FURN`, `DOOR`,
`MISC`, and all other base types remain without fixed colliders, even when
they have a visual mesh. The runtime report counts proxy placements and
eligible placements skipped.

This proxy follows rendered openings but may differ from Skyrim's authored
Havok geometry, especially stairs, door thresholds, roofs, and small details.
Riverwood playtesting must check those contacts before static collision can
be treated as accepted.
