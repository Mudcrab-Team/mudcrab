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
