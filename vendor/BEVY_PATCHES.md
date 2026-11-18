# Bevy changes carried by Mudcrab

Mudcrab builds on Bevy 0.19.0. When Bevy has not released a fix that Mudcrab needs, Mudcrab
carries the change in a vendored copy of the crate, wired in through `[patch.crates-io]` in the
root `Cargo.toml`. A carried change is temporary. Replace it with Bevy's own version when Bevy
releases one. Each vendored crate has a `README.mudcrab.md` with its source hash and details.

To add an entry: vendor the published crate unchanged in one commit, apply the change in the next
commit, add a `// mudcrab:` comment at each touched spot, and fill in a row below.

Upstream states below are as of the date in the table.

| Crate | Change | Upstream | Upstream state | Applied | Files | Mudcrab additions | Removal rule |
|---|---|---|---|---|---|---|---|
| `bevy_pbr` | Do not give a morphed mesh the missing-descriptor index `u32::MAX`; bound the `layer_count` read | [bevy#26504](https://github.com/bevyengine/bevy/pull/26504), fixes [bevy#25966](https://github.com/bevyengine/bevy/issues/25966) | **Open**, not merged, 2026-11-18 | PR head `f9a00902b05f4583828f55b4c9d905d3f7e187a2` (3 commits). The Rust hunks apply unchanged to 0.19.0. The shader hunk is ported by hand from `morph.wesl` to `morph.wgsl`. | `src/render/morph.rs`, `src/render/mesh.rs`, `src/render/morph.wgsl` | A `mudcrab:` marker comment in `layer_count`; and the `prev_weight_at` bound (own addition, in the second commit) | Drop the patch when a Bevy release contains #26504 or an equivalent. If Bevy merges a different change, use Bevy's. Keep the `prev_weight_at` bound only if Bevy does not fix that read. |
| `bevy_image` | Widen the `zstd` requirement to `0.14.3` for four RustSec advisories | none (Mudcrab-only manifest change) | n/a | see [`bevy_image/README.mudcrab.md`](bevy_image/README.mudcrab.md) | `Cargo.toml` | the whole change | Drop it when the Bevy image release Mudcrab uses accepts the fixed Zstd dependencies. |

## Checking a morph change

The out-of-bounds read does not fault on every run, so a plain run proves nothing. Use Metal
shader validation in a hidden window (the commands and the investigation are in the PR that added
this file), and count `Invalid Metal usage` lines. Bevy 0.19.0 gave 20 in 5 minutes. The target is 0.
