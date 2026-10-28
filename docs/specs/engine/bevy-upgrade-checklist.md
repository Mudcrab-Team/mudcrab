# Bevy upgrade checklist

Engine code that depends on Bevy internals beyond the public plugin API. Before a Bevy version bump
is merged, check every entry against the new version's source and changelog, and run the tests
named in the entry.

## Room culling (`crates/engine/src/room_culling.rs`, `--room-culling`)

- **Hook:** `cull_room_views` runs in `VisibilitySystems::CheckVisibility`, ordered after
  `check_visibility_cpu_culling`, and removes entities from the main camera's `VisibleEntities`
  list for the `Mesh3d` class.
- **Assumptions:**
  - `check_visibility_cpu_culling` fills `VisibleEntities` in that set, and nothing later in
    `PostUpdate` refills it.
  - The render world takes the view's `RenderVisibleEntities` from `VisibleEntities` at extraction.
  - Shadow views keep their own lists (`CascadesVisibleEntities`, `CubemapVisibleEntities`), and
    `ViewVisibility` is left as Bevy's check set it, so shadows still see culled objects.
- **What breaks if one changes:** if the system is renamed or moved, or the list is rebuilt after
  it, room culling silently stops hiding anything. If shadow passes start reading the main view's
  list, culled rooms lose their shadows.
- **Check:** `cargo nextest run -p engine room_culling`. The tests run the plugin in a real Bevy
  app with `VisibilityPlugin` and assert that an unreached room's mesh leaves the main view's list
  while `ViewVisibility` stays set. Then run the measurement poses
  (`crates/engine/tests/fixtures/room-culling-shots.json`) with `--room-culling`, and compare
  `python3 scripts/room_culling_counts.py <shots-out>` with the previous version's counts.
