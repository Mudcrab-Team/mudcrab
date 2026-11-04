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
16-byte instance layout and bounded output allocation. It reports the median of
three generation samples per cell at spacings 64, 32 and the MudCrab default 20
(4,096, 16,384 and 41,944 candidates). The stress fixture explicitly allows four
resolved species per texture with configured cap four, exercising the boundary beyond the default cap two.
Exact species IDs and replay batch counts are asserted, so a truncated `zip`
cannot conceal a missing species.

The mapped converted-fixture generator has no committed VCLR; it uses the white
fallback and therefore still exercises the uniform luminance/range law. A
companion module-level budget test exercises the private committed-surface path
with a nonuniform LAND colour ramp, encoded post-jitter sampling and eight species:

```text
cargo test --locked --release -p engine --lib grass::generation::tests::performance_coloured_terrain_generation -- --ignored --test-threads=1 --nocapture
```

It uses the same candidate counts, latency ceilings and allocation bound, and
charges its retained colour plane separately. It is a synthetic worker test;
the converted-fixture test above remains the converter integration check.
Neither test requires a GPU.

The respective 10 ms, 50 ms and 100 ms per-cell ceilings have headroom for shared CI
runners. They are worker-latency regression limits, not frame-time guarantees.
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

Independent species each have the listed candidate count. The eight-species
fixture's total output bound is eight times that per-species count; allocation
remains bounded by 32 bytes per potential instance plus 32 KiB of cell overhead.
The existing 10/50/100 ms worker ceilings are unchanged. Measured times must come
from the current release tests, not the earlier shared-population implementation.
