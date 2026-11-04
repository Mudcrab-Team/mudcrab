# Running the engine

The engine streams a converted asset pack (see [Converting game data](converter.md)). The
launcher's **Play** button starts it on the launcher's Output folder; this guide covers starting it
directly and what to expect once you are in the world.

## Start the engine

```bash
cargo run --release -p engine --bin engine -- --assets modern_assets
```

With no options the engine reads `modern_assets` and starts in Skyrim's Tamriel worldspace
(`0x3c`) at cell (0, 0). Use `--worldspace <id>`, `--grid-x <cell>` and `--grid-y <cell>` to start
elsewhere, and `--help` for streaming, benchmark and profiling options.

The engine is slow in a debug build. For trying a change, `cargo build --profile quick --bin engine`
builds an optimised binary into `target/quick/` much faster than `--release`; use `--release` for
anything you measure. See [`CONTRIBUTING.md`](../../CONTRIBUTING.md#5-fast-optimised-builds).

## Cluster geometry

A pack converted with `--lod-mode cluster` (see [Cluster geometry](converter.md#cluster-geometry))
records `geometry_mode` and `cluster_package` in `conversion-manifest.json`. At startup the engine
reads them and draws objects and terrain from that package; no extra option is needed. If
`--clusters` is also given, it must name the same package (by label, settings id or path), or
startup fails. For a pack converted with `baked` or `auto`, `--clusters [label|settings-id|path]`
draws a derived overlay of `--assets` instead; bare `--clusters` means the only overlay. In both
cases the package must contain terrain and trees.

```bash
cargo run --release -p engine --bin engine -- --assets modern_assets --clusters
```

- `--cluster-error-pixels <px>` and `--cluster-terrain-error-pixels <px>` set the object and
  terrain cluster error thresholds (default 1; above 0, at most 16). Both are refused without
  `--clusters` on the command line, including for a pack whose manifest selects the package.

The adapter must expose the `INDIRECT_FIRST_INSTANCE` feature and allow at least 8 storage buffers
per shader stage, 5 bind groups and 65,535 compute workgroups per dimension; its storage buffer and
buffer size limits must also hold the largest cluster pool of the loaded scene.
`MULTI_DRAW_INDIRECT_COUNT` is not required.

An adapter that falls short, or a package that fails validation, does not fall back to the stock
models. The engine logs the first reason at error level (for example `cluster package failed;
stopping before world readiness`) and exits with an error status; later failures are not reported.
A loading cover stays up until cluster pipelines, materials and batches are ready, and a teleport
whose destination does not become ready before the loading deadline ends the same way with
`cluster teleport refused: ...`.

To return to the ordinary path, start without `--clusters` on a `baked` or `auto` pack. A pack
converted with `--lod-mode cluster` has no baked products; reconvert it with `--lod-mode baked`
(the default). The payload format is in
[ADR-0013](../adr/0013-cluster-lod-payload-format.md).

## Interactive player startup

Interactive world sessions start in first-person **WALK** with the player capsule
active. During startup and WALK teleports, movement and gravity wait for an enabled
Rapier world-collision hit, followed by the usual safe capsule placement check. A
validated local terrain sample bounds the startup query for elevated `--start-shot`
poses and sampled `coe`/`coc` destinations; it does not prove collision is loaded.
If placement is blocked, or collision does not appear within 30 seconds of fixed
simulation time, the session falls back to **NOCLIP** and shows the reason in the
overlay. Press **V** or run **tcl** to cancel a hold, toggle modes, or retry WALK.
NOCLIP teleports stay camera-only. Ordinary airborne WALK retains its existing
behavior, and door-crossing movement and hold release remain guarded through the crossing.

The asset-free `--physics-fixture` still starts in NOCLIP. Automated camera paths
are unchanged. Full-world validation should check delayed terrain/static collision,
blocked spawn, camera orientation, and V/tcl round trips with a Skyrim package;
primitive regression tests do not establish Skyrim collision coverage.
