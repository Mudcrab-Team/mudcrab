# Vendored `project-wormhole-nif` 0.1.0 (Mudcrab fork)

Source: the published [`project-wormhole-nif` 0.1.0 crate](https://crates.io/crates/project-wormhole-nif/0.1.0),
SHA-256 `ac440ff2923ce32facbe37b1a311f739a85d18a0383ce5f59975300874104cfa`, built from Project Wormhole
commit `cfd1c409d9a6e27d649ab276471c98c344b436cd` (path `nif`). The crate and this copy are
`GPL-3.0-or-later`, the same licence as Mudcrab.

Unlike `vendor/bevy_image`, this is a maintained fork, not a pristine copy. The copy vendored on
2026-08-18 was already reformatted, so every source file differs from the published one, and 34 later
commits changed it further (12 files, +6,191/-468 lines): legacy (pre-Skyrim) `NiTriShape` geometry
in the new `src/legacy.rs`, legacy skin instances, skin partitions, skeleton binding, merged-palette
checks and header fixes. The sibling crates `project-wormhole-esm`, `project-wormhole-shared` and
`project-wormhole-ba2` still come unchanged from crates.io. `crates/converter` and
`crates/dummy-content` use this copy.

## What this means for reviewers

- All of this crate parses untrusted NIF files from the game and from mods. Hold changes here to the
  same rules as the converter: a malformed file gives an error, never a panic, an unbounded
  allocation or unbounded recursion.
- `src/lib.rs` turns off `unused` warnings and all of Clippy for the whole crate, so dead code and
  lint findings here are not reported by the workspace gates.
- The history of every Mudcrab change is `git log -- vendor/project-wormhole-nif`. To compare with
  the published crate, download it from crates.io (check the SHA-256 above), run `rustfmt` on both
  trees and diff them with `--strip-trailing-cr`: the reformatting otherwise hides the real changes.

## Updating or removing this copy

There is no upstream release to move to: the fork has diverged, so it stays vendored. Moving back to
a registry release would need each Mudcrab change upstreamed first. After any change here, run
`cargo nextest run -p converter -p dummy-content --all-targets`.
