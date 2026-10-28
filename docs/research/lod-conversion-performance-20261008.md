# LOD conversion performance analysis

Analysis of upstream `main` at `c39449b5a8b6a62c7c3c43bb60164e8ba6911839`
and [PR #167](https://github.com/Mudcrab-Team/mudcrab/pull/167) at
`9ef353c1ef24959e21ec231532c14fbe4791c030`. Both were fetched on October 8.
Measurements use the installed Skyrim pack on this Apple M1 Pro MacBook Pro
(eight performance cores, two efficiency cores, 32 GiB RAM).

The main LOD compiler is CPU bound. Its ordinary-texture `gpu` option does not
accelerate terrain LOD: terrain blending and atlas UASTC compression use CPU
workers. Fresh archive extraction is a separate major cost, with per-file
durability flushes and unnecessary extracted inputs. The existing LOD
performance PR already removes several repeated operations; further work
should build on it.

## Where the completed conversion spent its time

The saved October 8 full conversion used `--texture-encoder gpu --cpu-jobs 8
--io-jobs 4`. It completed with 4,673 LOD chunks.

| Work | Recorded active time | Share of 43m 54s |
| --- | ---: | ---: |
| Terrain LOD | 1,227.659 s | 46.6% |
| Archive extraction, event span | 990.882 s | 37.6% |
| Mesh conversion, event span | 101.865 s | 3.9% |
| Ordinary textures, event span | 82.196 s | 3.1% |
| Other work and gaps between events | 231.206 s | 8.8% |

LOD and extraction account for 84.2% of recorded active time. Optimizing
ordinary GPU texture conversion has a much smaller ceiling for this run.

Stage events are not complete timers. The database has a zero first-to-last
event span but roughly 34.5 seconds elapse before mesh work starts. The exact
publication counter is 26.154 seconds; publishing-to-complete spans 158.187
seconds because cache persistence, pruning and staging cleanup follow package
publication. Those later operations need separate timers.

The wrapper's UTC timestamps span 85m 35s. Power logs show an idle-sleep period
with two brief dark wakes, totaling approximately 41m 49s asleep. This explains
the roughly 41m 42s difference from the converter's active elapsed timer; it is
not additional conversion work. The new probes use transient `caffeinate -i`.

## Measured LOD costs

The isolated probe compiles exact current Rust LOD and texture sources with
phase timers at optimization level 3, using cached dependency/native codec
libraries. It loads the real published database and cell cache read-only and
uses 66 source DDS files recovered from the ingestion cache. Every DDS hash
matches the published LOD manifest. It does not run a second full conversion
or modify the published pack.

Direct compilation excludes pipeline publication, input fingerprinting and
unchanged-chunk reuse. Comparisons with the PR measure fresh chunk compilation
on fixed, warm/in-memory inputs, including the existing published LAND
projection.

The land workload is Tamriel cells `x=0..15`, `y=-16..-1`: 256 distinct material
inputs, producing 21 chunks. The ocean workload is Solstheim cells
`x=96..111`, `y=96..111`: 256 cells without texture layers or vertex colors,
also producing 21 chunks. These workloads exercise different costs and should
not be extrapolated interchangeably to a whole installation.

Results and scaling are recorded in the accompanying JSON evidence. The
single-worker land probe took 206.841 seconds to compile. Aggregate worker
timers recorded 204.560 seconds encoding, including 0.859 seconds generating
mips, versus 2.226 seconds baking and 0.041 seconds assembling GLBs. The
discarded template encoding alone took 105.970 seconds, 51.2% of compile wall
time. A bounded stack sample independently identifies Basis UASTC compression
and its ETC1 transcode hints. Compile-phase block I/O counters were zero;
the inputs had already been loaded into memory.

| Fixed 256-cell workload | Workers | Compile wall time | CPU time |
| --- | ---: | ---: | ---: |
| Main, land | 1 | 206.841 s | 183.484 s |
| Main, land | 4 | 57.443 s; repeat 51.337 s | 186.601 s; 180.668 s |
| Main, land | 8 | 30.231 s | 187.794 s |
| PR #167, land | 4 | 22.174 s; repeat 21.870 s | 78.640 s; 78.800 s |
| Main, ocean | 4 | 0.552 s | 1.874 s |
| PR #167, ocean | 4 | 0.230 s | 0.737 s |

The main land workload scales to 3.6–4.0 times its single-worker speed with
four workers, and 6.84 times with eight. Average compile CPU occupancy rises
from 0.887 to 3.25–3.52 to 6.21 cores. This small 21-job workload has scheduling
tails; it does not establish the best worker count for all worlds. Identical
chunk digests across main runs confirm scheduling did not change output.

The paired four-worker land means are 54.390 seconds on main and 22.022
seconds on the PR, a 59.5% wall-time reduction. CPU work falls by 57.1%.
Encoding still accounts for 97.4% of the PR's summed bake/encode/GLB timers;
its mip generation is included in encoding and is only about 0.25 worker
seconds. The PR repeats also produce identical chunk digests. The implementations use
different KTX2 container writers and produce different digests; visual and
decoder equivalence between implementations was not tested.

These are diagnostic microbenchmarks, not release acceptance or a controlled
whole-game speedup. Other worktrees had builds or an engine running during
parts of the measurement; their processes were preserved. CPU time, phase
timers, input identities and competing-process snapshots are retained so
scheduling effects can be distinguished from reduced work.

## Measured extraction costs

A separate probe used the current BSA reader and temporary-write/rename
sequence on 2,048 real entries from `Skyrim - Misc.bsa`. These uncompressed
small files total 2,326,291 bytes. Every run produced the same output digest;
only the per-file flush was changed in disposable diagnostic outputs.

| Workers | Per-file `sync_all()` | No per-file flush |
| --- | ---: | ---: |
| 1 | 10.696 s | 0.274 s |
| 4, mean of two runs | 9.161 s | 0.167 s |
| 8 | 7.631 s | 0.192 s |

The single-worker flush timer recorded 10.114 seconds, 94.6% of wall time,
while the whole extraction used only 0.598 CPU seconds. More workers did not
remove the flush wait. This establishes a small-file I/O latency bottleneck,
not an SSD bandwidth limit or a large-texture decompression measurement. The
39–55-fold difference does not predict whole-conversion speedup, and the
production flush behavior was left intact.

## Repeated work already addressed by PR #167

The current main encoder compresses the three supplied atlas mips, then
compresses the base and a generated pyramid again solely to obtain a KTX2
template (`texture.rs:148-175`). The PR builds the container without that
disposable compression and caches exact repeated RGBA blocks within each
atlas while retaining the upstream UASTC routine.

Main also reads and validates the entire 490,733,344-byte cell cache for each
producing world (`lod/terrain.rs:1024`). Fourteen producing worlds imply
6.870 GB of logical reads and repeated validation; this need not be physical
disk I/O when the OS cache is warm. The PR loads one immutable indexed
snapshot, shares decoded diffuse images across worlds/FormIDs, avoids repeated
decoding of identical source mips, and removes the base atlas clone.

The PR adds verified unchanged-chunk reuse. Its committed Fiji evidence,
pinned to implementation `c9894ed`, records 1,531.532 seconds cold and 172.750
seconds warm with all 4,673 chunks reused: an 88.7% LOD-stage reduction for
that pair. This is existing historical evidence on another machine, not a
measurement of the current integration head or this laptop.

## Additional optimization priorities

1. **Expose the existing GPU encoder to prepared terrain RGBA mips.**
   `GpuUastc` already uploads RGBA input and overlaps upload, dispatch and
   readback through three slots. Offloading atlas compression is a smaller
   change than moving LAND blending to a compute shader. Preserve exactly
   three mips, gutters and sRGB metadata; give GPU output its own recipe
   identity, check retail quality and retain CPU fallback. Byte identity with
   CPU Basis level 2 is not assumed. Measure GPU dispatch and readback before
   deciding whether GPU LAND baking is worthwhile.

2. **Filter unused archive entries before decompression.** The real manifest
   records 181,654 extracted entries totaling 30.626 GB. Of those, 105,373
   entries, 4.250 GB, have extensions outside `dds`, `nif`, `pex` and `lod`.
   The current conversion stages ignore them; loose-input overlay already
   filters to these four kinds. Filtering avoids 58.0% of file operations and
   13.9% of decoded bytes in this installation, plus their hashes, links,
   verification and cleanup. Bind the filter to the ingestion recipe so
   future consumers can expand it without silently reusing incomplete caches;
   forthcoming localized-string readers and audio/animation support require
   their own inputs.

3. **Separate decompression from bounded I/O scheduling.** Extraction uses
   Rayon parallelism inside each archive, while archives run in sequence to
   preserve override order (`archive/mod.rs:258`, `pipeline.rs:477-565`).
   `io_jobs` is accepted and validated but never controls this work. Wire it
   to a bounded writer queue with a byte budget, and measure 1/4/8 writers.
   Do not concurrently overlay archives into the same VFS; resolve winners
   first or use separate extraction namespaces followed by ordered overlay.
   Every temporary extraction file currently calls `sync_all()` before rename
   (`archive/mod.rs:471`). Batch durability at verified archive checkpoints
   while retaining atomic replacement and hash-verified resume.

4. **Keep unrelated caches valid when tuning textures.** One global
   configuration hash contains encoder mode/quality, Zstd level and script
   ABI (`cache.rs:278-301`). A mismatch discards the whole previous manifest
   (`pipeline.rs:308-314`), causing archive, mesh and script work to repeat
   after texture-only changes. Use separate ingestion and producer identities.
   An interrupted first conversion also lacks an ingestion journal for its
   already-extracted archives; resume can repeat extraction despite staged
   blobs. Final artifact validation was only about 9.6 seconds in the recorded
   full run, so redundant ordinary-output hashing is a lower priority.

5. **Overlap bounded chunk compilation and publication.** Main already runs
   chunk jobs in parallel under `cpu_jobs`; it is not a serial compiler. The
   PR bounds memory using batches of `cpu_jobs` jobs, then waits for the slowest
   job and publishes the batch serially (`pipeline.rs:2591-2625` on the PR).
   A bounded queue feeding one SQLite publisher can keep workers busy while
   files are written and validated. Bound bytes, preserve world rollback and
   cancellation, and sort final identity inputs deterministically.

6. **Share whole material atlases across geometry chunks.** In the current
   Solstheim pack, 32,727 of 34,383 terrain cells have neither layers nor VCLR;
   2,729 of 2,956 chunks have constant-white material input. Tamriel and
   Solstheim together have 2,867 repeated atlas input signatures among 3,964
   jobs. Their repeated layouts account for 2.912 billion of 4.039 billion
   allocated atlas pixels, or 72.1%. These are reuse candidates, not a measured
   speedup. Atlas colors do not depend on heights or chunk position, so keep
   geometry independent and share baking/mips/encoded texture bytes. Include
   ordered materials, VCLR, DDS hashes, tier/tile size, tile occupancy,
   dimensions and encoder recipe in the key. A constant-white fast path is
   the smallest first case; fill actual tiles and retain transparent unused
   atlas padding.
   PR #167's block cache is local to one atlas and does not remove this work
   across chunks.

   The measured ocean sample took only 0.552 seconds on main and 0.230 seconds
   on the PR for 21 chunks. That is roughly 100 times cheaper than the land
   sample. High duplicate counts therefore do not establish a large time
   saving; keep this behind the measured textured-compression and extraction
   costs. Use a single-flight cache to prevent concurrent identical misses.

7. **Reduce warm-path readback and hash repetition.** Current LOD payloads
   total 8,925,619,804 bytes. A reused payload is read, hashed and structurally
   validated, then written, read, hashed and validated again during publication.
   Pass verified results forward and parallelize independent file checks;
   preserve validation of published bytes. Spatial-index maintenance also
   scans auxiliary R-tree keys and `MAX(id)` per chunk. A read-only query probe
   measured 2.405 ms for auxiliary-key lookup versus 0.0075 ms by ID, so an
   indexed key-to-ID map and transaction-local ID allocation are secondary
   warm-run targets.

The atlas inner loop has smaller exact-output opportunities too: resolve
layer images once, skip zero-weight samples after validating authored inputs,
prepare opacity grids once per cell, and copy gutters from interior edges.
Copying gutters removes 357,568,832 repeated pixel evaluations, 8.11% of the
current pack's 4.409 billion base bake samples. Approximate sRGB lookup tables
or lower compression quality need a separate quality/error decision; they are
not required for the first optimizations.

## Evidence and reproduction

The accompanying `lod-conversion-performance-20261008.json` retains measured
results and source identities. Local raw evidence is under
`target/lod-profile/`: exact-source probe builders, build commands, instrumented
modules, input provenance, worker timings, CPU sample, extraction probes,
material statistics and power-log excerpts. No production code was changed.

Run the local probes from the repository root:

```sh
python3 target/lod-profile/build_lod_probe.py
python3 target/lod-profile/build_pr_probe.py
caffeinate -i target/lod-profile/lod-probe 4 16
caffeinate -i target/lod-profile/pr167/lod-probe 4 16
caffeinate -i target/lod-profile/lod-probe 4 16 ocean
caffeinate -i target/lod-profile/pr167/lod-probe 4 16 ocean
```

The probe builders require the cached local dependencies recorded in their
provenance. Run one timed workload at a time. Confirm chunk digest equality
across thread counts for the same implementation; compare phase and CPU time
alongside wall time. A final isolated full cold/warm run is still needed before
claiming end-to-end acceptance for any further implementation changes.
