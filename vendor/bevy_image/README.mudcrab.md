# Vendored `bevy_image` 0.19.0

Source: the published [`bevy_image` 0.19.0 crate](https://crates.io/crates/bevy_image/0.19.0),
SHA-256 `37bc41f69a0c6ade2e7961602517545b39ab8e42bc8c4a729bd24ffee3bcd904`. Upstream provenance is
kept in `.cargo_vcs_info.json` (Bevy commit `c6f634ca9f406d68ba5109d921247b654cb42c10`,
`crates/bevy_image`), and the unmodified published manifest is `Cargo.toml.orig`.

All upstream Rust source and both license files are unchanged and remain `MIT OR Apache-2.0`;
see `LICENSE-MIT` and `LICENSE-APACHE`. The normalized `Cargo.toml` differs from the published
crate only by a comment naming the advisories and by the optional `zstd` requirement, `0.13.3` to
`0.14.3`. The crate's own lockfile is omitted so Mudcrab's root `Cargo.lock` controls resolution.

## Why

Bevy 0.19 requires `zstd ^0.13.3`, which cannot resolve the fixes for `RUSTSEC-2026-0391`,
`RUSTSEC-2026-0398`, `RUSTSEC-2026-0403` and `RUSTSEC-2026-0410`. The widened requirement
resolves `zstd 0.14.3`, `zstd-safe 8.2.0` and `zstd-sys 2.1.2+zstd.1.5.7` for the engine while
preserving Bevy's KTX2 support, and adds no `cargo audit` exceptions. `crates/converter` uses the
same `zstd 0.14` line, so one fixed decoder serves both.

None of the four is reachable here: -0391 needs `Encoder::with_context` or `Decoder::with_context`,
which nothing calls; -0398 needs the `seekable` feature; -0410 is `wasm32`-only and no wasm target
is built; and -0403 affects the streaming *write* decoder, while this crate's only call is
`zstd::decode_all`, which the advisory lists as unaffected. Patching is still preferred over
adding audit ignores, because it removes the affected versions from the graph outright.

## Updating or removing this copy

Replace or drop this directory together with the `[patch.crates-io]` entry in the root
`Cargo.toml` when the Bevy image release Mudcrab uses accepts the fixed Zstd dependencies.
After any change, run the converter texture tests and
`cargo test --locked -p engine --test native_ktx2_loads_in_bevy`, and confirm
`cargo audit` reports no Zstd advisories and `Cargo.lock` still names the patched versions.
