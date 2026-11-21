# Grass verification

Run the release grass generation budgets:

```text
cargo test --locked --release -p engine --test grass_performance -- --ignored --test-threads=1 --nocapture
```

`performance_grass_generation` converts a generated plugin and authored crossed-plane
NIF through the ordinary asset pipeline. Its 5x5 terrain ring supports a terrain
stream radius of two. Nine central cells are measured with eight grass types,
two blended texture layers, texture alpha cutouts and placed-reference exclusions.
The converted GLB's attached material must use `MASK` with the authored cutoff
`96 / 255`; the test asserts both before measuring generation.
The test checks deterministic output, participation of all species, slot ordering,
16-byte instance layout and measured peak and scratch Rust heap allocation. It reports the median of
three generation samples per cell at spacings 64, 32 and the retail default 20 (`iMinGrassSize`)
(4,096, 16,384 and 36,864 candidates). A cell whose three-sample median misses its
budget is measured once more with five fresh samples and judged on the median of all eight.
This absorbs one-off runner noise; its limit is that a slowdown present in only four of the
eight samples can still pass, because the median of eight averages the fourth and fifth. The stress fixture explicitly allows four
resolved species per texture with configured cap four, exercising the boundary beyond the default cap two.
Exact species IDs and replay batch counts are asserted, so a truncated `zip`
cannot conceal a missing species.

The mapped converted-fixture generator has no committed VCLR; it uses the white
fallback and therefore still exercises the uniform luminance/range law. A
companion module-level budget test exercises the private committed-surface path
with a nonuniform cached LAND colour ramp, committed terrain heights and eight species:

```text
cargo test --locked --release -p engine --lib grass::generation::tests::performance_coloured_terrain_generation -- --ignored --test-threads=1 --nocapture
```

It uses the same candidate counts and latency ceilings, checks output size only, and
charges its retained colour plane separately. It is a synthetic worker test;
the converted-fixture test above remains the converter integration check.
Neither test requires a GPU.

CI runners vary; the strict budget applies to local and release runs.
The respective strict per-cell ceilings remain 6 ms, 25 ms and 60 ms. Both tests
apply a shared 1.5x time allowance only when the environment variable `CI` is
exactly `true` (as on GitHub Actions), giving effective ceilings of 9 ms, 37.5 ms
and 90 ms. Unset or other values use 1.0x, including local `--release` runs.
This CI-only allowance accounts for hosted-runner variability: identical grass
code and an unchanged companion benchmark can slow down between runners.
Each timing-budget line, including the re-measure line, prints the strict ceiling, allowance
and effective ceiling;
memory and heap limits remain unchanged.
These are worker-latency regression limits, not frame-time guarantees.
Inspect the printed measurements and the runtime profiler on the target machine.
The fixture does not establish performance for every authored mesh or grass mod.

To retain the converted fixture for a GPU run, set `MUDCRAB_GRASS_FIXTURE_OUTPUT`
to an empty output directory when running the test. Then run the release engine:

```text
engine --assets <fixture-output> --worldspace 1 --grid-x 0 --grid-y 0 --stream-radius 2 --grass
```

The runtime profiler records `grass/generate_cell` for completed worker jobs and
`grass/stream_frame` for main-thread scheduling/admission. `grass/cache_bytes`,
`grass/pending_cells` and `grass/uploads` expose retained CPU data and admission
pressure. The CPU cache budget excludes active worker scratch space and GPU
allocations; process and render metrics are needed to assess total memory.

For real-world comparisons, use identical converted inputs and exact camera poses
with grass disabled and enabled. Interactive walking verifies streaming while moving;
the grass-aware acceptance and multi-shot paths wait for generation, model
attachment and actual draws. Grass preparation errors prevent a settled capture
and fail the run. Headless regression tests cover these gates; actual GPU
captures remain necessary to verify the rendered result. Record the exact source
revision with every capture.

Texture cutout screenshots alone do not verify fractional vertex alpha. A separate
controlled GLB variant can assign every grass vertex alpha 0.25 and material cutoff
0.5, then compare density one against density zero with grass enabled in both runs.
With opaque texture and material alpha, grass must retain coverage in both color
and depth passes: vertex alpha weights native wind and does not
control grass opacity. A texture or material alpha below the cutoff must still
leave neither visible color nor prepass holes. Such a post-conversion variant must
be identified as an edited test asset. The converter preserves enabled authored
vertex alpha under the source-channel contract; tree and LOD shaders have separate
alpha semantics.

Keep converted game data, generated packs, captures, profiling output and binaries
outside the source repository. Report backend, GPU/driver, source state, asset
provenance and remaining limitations alongside measurements. A static capture
does not establish movement behavior or visual parity with retail Skyrim.

Independent species each have the listed candidate count. The integration binary
uses a counting global allocator around synchronous generation after conversion
has completed: peak incremental live heap is limited to 48 bytes per potential
instance plus 64 KiB, and retained heap to 16 bytes per potential instance plus
64 KiB. All eight species' vectors can retain growth capacity simultaneously before
compaction. The scratch allowance is therefore
`slots * (8 * (2 * staging_bytes - 16) + staging_bytes) + 64 KiB`: two staging
capacities per species minus final output, plus one buffer's growth/shrink overlap.
The unpublished instance Z lane stores the exact binary16 height until final
quantization, so staging stays within each 16-byte instance. This gives 144 bytes
per candidate slot plus 64 KiB. Each species vector is compacted to exact capacity
once at publication, before the stable slot sort; the overlap also covers that
sort's temporary buffer. Placement writes each instance directly into its vector,
and compaction happens outside the per-instance generation loop. These are explicit
fixture allocation limits, checked against the actual
counting allocator; they include Vec growth and scratch but exclude mapped input,
native allocations and GPU resources. The library test only checks
the generator's reported output size (32 bytes per potential instance plus 32 KiB).

Both ignored tests require the actual Cargo OPT_LEVEL to be 3 (including a
guard against custom profiles with lower optimization). CI runs both with --release
and the CI-only 1.5x time allowance described above. The strict 6/25/60 ms limits are
portable regression policy, not a measured Linux baseline or a guarantee that a
particular slowdown factor fails; runner measurements must be collected before
claiming that. Determinism covers same-cache replay and byte-identical caches from two independent conversions at CPU/IO job counts 2 and 1.
