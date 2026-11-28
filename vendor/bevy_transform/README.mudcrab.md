# Vendored `bevy_transform` 0.19.0, with a dirty-tree marking fix

Source: the published [`bevy_transform` 0.19.0 crate](https://crates.io/crates/bevy_transform/0.19.0),
SHA-256 `c95fefe69c9223d10e6b52a0fdf12811bab9d0c7dcb27570cb86824b451f180d`. Upstream provenance is
kept in `.cargo_vcs_info.json` (Bevy commit `c6f634ca9f406d68ba5109d921247b654cb42c10`,
`crates/bevy_transform`), and the unmodified published manifest is `Cargo.toml.orig`. The manifest,
both license files (`MIT OR Apache-2.0`) and `README.md` are unchanged. The crate's own lockfile
and its `.cargo-ok` marker are omitted, so Mudcrab's root `Cargo.lock` controls resolution.

The first commit that added this directory is identical to the published crate, except for those two
omitted files. To see every Mudcrab change, download the published crate
(`https://static.crates.io/crates/bevy_transform/bevy_transform-0.19.0.crate`), unpack it and run
`diff -r` against this directory. `grep -rn "mudcrab:" src` lists the touched spots.
The ledger of all carried Bevy changes is [`../BEVY_PATCHES.md`](../BEVY_PATCHES.md).

## Why

The parallel `mark_dirty_trees` climbs from each changed entity to its root and stops at an entity
it has already visited. Its visited set is keyed by the entity index alone. The entities of
`RemovedComponents<ChildOf>` include despawned ones. When more of them than one channel chunk
(1024) are sent, they can be visited before the changed entities. A new entity that reuses one of
those indices then counts as visited, so the climb stops below the root. The root stays clean, the
static-scene optimization skips it, and the new subtree keeps `GlobalTransform::IDENTITY` until the
root's transform changes.

In Mudcrab an object LOD chunk loaded after a cell unload drew at raw file coordinates, high above
the world, and cast a large shadow.

## Carried change

| Change | Where it comes from | Files |
|---|---|---|
| Send only live entities of `RemovedComponents<ChildOf>` to the climb | Mudcrab. No upstream issue or pull request was found in a search on 2026-11-28 | `src/systems.rs` |

Every Mudcrab comment starts with `mudcrab:`, so `grep -rn "mudcrab:" src` lists the touched spots.

## Updating or removing this copy

Drop this directory and the `bevy_transform` entry in the root `[patch.crates-io]` when a Bevy
release that Mudcrab uses fixes the fault. The tests
`a_new_subtree_that_reuses_a_despawned_index_gets_its_transform` and
`streamed_transforms_hold_over_rounds_of_reused_indices` in
`crates/engine/src/vendored_bevy_patches.rs` must pass without it; remove only the source check
there.
