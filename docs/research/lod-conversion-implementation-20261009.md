# LOD conversion implementation and validation

These changes build on [PR #167](https://github.com/Mudcrab-Team/mudcrab/pull/167)
at `9ef353c1ef24959e21ec231532c14fbe4791c030`, adopted locally after the
[October 8 analysis](lod-conversion-performance-20261008.md). They address
repeated LOD work and archive extraction, and add a GPU terrain atlas
encoder. The measured revision's release build, 581 standard tests, Clippy and three explicit Metal
checks pass. All six release conversions pass ordinary asset checks and
every-chunk LOD audits, including exact warm reuse. The protected source pack's
manifest is unchanged. Matched kernel measurements and observed release times
are recorded separately below and in the
[machine-readable receipt](lod-conversion-implementation-20261009.json).

The follow-up makes GPU LOD the default for new CLI runs and configs and adopts
the subsequent PR #167 cache-safety fixes. The measurements below and their
machine-readable receipt retain the exact earlier source and binary identities;
they do not claim to benchmark this follow-up revision. Current revision checks
are recorded separately in the PR-readiness addendum.

## Implemented behavior

The PR baseline removes the discarded atlas encode, reads the cell cache once
per LOD stage, shares decoded diffuse images across worlds and FormIDs, retains
three atlas mips, caches identical UASTC blocks within an atlas, and reuses
unchanged chunks only when their inputs and payloads verify. Chunk compilation
uses bounded batches. It does not share encoded atlases across different chunks.

Archive ingestion now selects the formats consumed by the converter: `dds`,
`nif`, `pex`, `lod`, `strings`, `ilstrings` and `dlstrings`. It validates archive
paths and collisions before filtering. The selection is recorded as
`converter-inputs-v1`; source archive hashes and compatible extraction recipes
govern reuse. Ordinary texture setting changes preserve valid ingestion caches.
`--invalidate-cache` forces extraction and conversion again. Source archive
inodes must stay unchanged while mapped. The initial hash covers the decoded
mapping; a changed or unreadable path invalidates newly sealed packs, including
on failure or cancellation, while retaining earlier verified checkpoints.

CPU decompression uses `cpu_jobs`; pack and VFS writers use `io_jobs`. Ordinary
batches contain at most 256 files or 16 MiB of decoded payload and share a
64 MiB payload budget. A larger single entry can raise that budget, subject to
the 1 GiB entry limit. Archive mappings, indexes and other converter caches are
additional memory. Separate conversions need separate output and cache roots;
default cache roots belong to their output. Custom shared cache roots have no
cross-output writer lock.

Completed batches become immutable, hashed packs under
`.ingestion-cache/batches/<archive hash>/<recipe>/`. Pack bytes are flushed
before publication and containing directories are flushed on Unix. Per-file
blobs and VFS files are derived copies rather than individual durability
checkpoints. Resume recovers verified sealed packs, including batches from an
archive interrupted before completion. A separate journal records completed
archive inventories. Damaged or missing payloads are recovered from verified
packs or blobs, or decoded again. Batched ingestion verifies reused bytes even
with `--no-verify-cache`.
Persistent cache publication repairs damaged copies and flushes packs again.
Windows pack files are flushed; portable directory flushing remains a platform
limit.

The new LOD GPU route receives the CPU-authored RGBA mip chain. LAND blending,
two-pixel gutters, transparent padding and linear-light sRGB mip generation
remain in the bake. Both encoders receive exactly three mips. The GPU uses the
existing three-slot upload, dispatch and readback pipeline, serializes shared
encoder calls, and returns slots for subsequent batches. Every post-map error
unmaps or cancels the pending readback before its slot returns to rotation. Device allocation
waits for the first chunk miss; a fully reused build needs no GPU allocation.
LOD atlases use no Zstandard supercompression on either backend.

CPU and GPU chunk proofs are separate. GPU quality changes invalidate GPU
proofs; GPU batch size changes scheduling only. The LOD build identity includes
the requested atlas recipe. Initialization failures, encode failures and invalid
GPU output layouts fall back to the retained CPU mips. Those chunks omit reuse
proof so a later healthy GPU retries them. `lod_gpu_chunks` and
`lod_cpu_fallback_chunks` count newly encoded, accepted chunks; reused chunks
are reported separately by `lod_cache_hits`. World rollback discards its counts.

New CLI runs and configs default to GPU LOD encoding. `--lod-encoder cpu` selects
the CPU recipe; old serialized configs without this field retain CPU encoding.
A GPU-less host retries fallback chunks on subsequent runs because they have no
GPU reuse proof. Explicit CPU selection enables CPU reuse on that host.
LOD GPU slots cap their source budget at
64 MiB each; smaller batch settings are available. Ordinary textures use a
separate GPU instance with the requested batch budget. LOD GPU post-processing
and validation split their worker budget for `cpu_jobs >= 2`.

## CLI

Normal conversion uses the new ingestion path and GPU terrain atlases:

```sh
cargo run --release -p converter -- "<Skyrim Data>" "<output>" \
  --cpu-jobs 8 --io-jobs 4 --report-json target/conversion-gpu.json
```

Select the retained CPU LOD recipe explicitly:

```sh
cargo run --release -p converter -- "<Skyrim Data>" "<output>" \
  --cpu-jobs 8 --io-jobs 4 --lod-encoder cpu \
  --report-json target/conversion-cpu.json
```

`--texture-encoder gpu` controls ordinary source textures independently.
When both GPU routes are enabled, the CLI's GPU quality and batch flags apply
to both. `--resume-staging <directory>` retains the selected encoder settings
in the printed resume command.

## Matched performance measurements

The host is an Apple M1 Pro with 10 CPU cores and 32 GiB RAM. These paired
kernel tests use the quick profile, four CPU workers unless stated, GPU quality
2 and a 16 MiB GPU batch budget. No named competing build, converter or engine
was sampled during these tests. Inputs and output hashes are recorded under
`target/lod-profile/matched/`; OS page-cache state was not controlled. These
measurements predate hardware SHA and the final failure-cleanup patches. The
compression recipes are unchanged; final release validation is recorded below.

| Workload | CPU / old path | GPU / batched path | Result |
| --- | ---: | ---: | --- |
| Extract 2,048 entries, mean of two pairs | 10.237 s | 0.732 s | 13.99 times faster |
| Land atlas path, 21 chunks, four workers, mean of two pairs | 24.141 s | 2.664 s | 9.06 times faster |
| Land atlas path, eight workers | 13.291 s | 2.255 s | 5.89 times faster |
| Ocean atlas path, four workers | 0.245 s | 0.284 s | GPU adds 40 ms |

The extraction test rewrites the first 2,048 real Misc archive entries into an
uncompressed disposable fixture with 2,326,291 decoded bytes. Both paths
extract all entries; filtering supplies none of this speedup. The new path
seals eight packs, verifies identical inventories and repairs corrupted derived
files. Warm extraction averages 1.353 s; corruption recovery averages 1.367 s.
Whole-probe system CPU exceeds user CPU substantially. This result points to
filesystem calls and durability checkpoints, rather than sustained disk
bandwidth, as the cost of the old small-file path.

The land test selects 256 Tamriel cells (`x=0..15`, `y=-16..-1`). Each atlas and
its three mips are authored once, then passed to both encoders. The comparable
path includes geometry and atlas bake, mip generation, encoding and GLB
assembly. It excludes database/cache loading, input fingerprints, GPU device
initialization, quality checks and file/database publication. Four-worker CPU
encoding alone averages 23.256 s versus 1.779 s for GPU encoding and readback:
13.07 times faster. CPU encoding accounts for 96.3% of the measured CPU path;
eight workers reduce CPU encoding time by a factor of 1.83.

All 63 land mips meet the quality gate. The lowest GPU PSNR is 44.42 dB and the
largest CPU-to-GPU PSNR loss is 1.03 dB. Alpha, padding and sampled gutters pass.
Payloads are identical across repeats and four/eight worker counts within each
backend. The Solstheim ocean test (`x/y=96..111`) is lossless on both encoders;
GPU scheduling overhead outweighs its compression savings. GPU LOD was opt-in
in the measured revision; the subsequent default change retains explicit CPU selection.

A late CPU-run stack sample also found serial chunk publication and SHA-256
compression while worker threads were mostly idle. An isolated release probe
compares identical one-core SHA workloads: software takes 3.068 s and the
Apple ARM hardware path takes 0.515 s, a factor of 5.96. Known-answer and
independent Python reference hashes match. The dependency feature is scoped to
Apple ARM; no digest, proof format or cache recipe changes.

## Complete release observations

All runs use eight CPU workers, four I/O writers and GPU quality 2. Ordinary
textures use the GPU recipe; LOD switches between CPU and GPU as shown. The
requested GPU batch budget is 256 MiB, capped at 64 MiB for LOD. Metadata runs
verify and retain ordinary assets; the fresh run also exercises archive
ingestion and ordinary conversion. Normal warm conversion uses that isolated
fresh output and its persistent cache.

| Release run | Command elapsed, monotonic | Explicit LOD timer | LOD hits | New GPU chunks |
| --- | ---: | ---: | ---: | ---: |
| CPU metadata cold | 1,102.43 s | 937.95 s | 0 | 0 |
| CPU metadata warm | 251.08 s | 55.53 s | 4,673 | 0 |
| GPU metadata cold | 422.73 s | 262.62 s | 0 | 4,673 |
| GPU metadata warm | 318.94 s | 113.16 s | 4,673 | 0 |
| Full GPU fresh | 904.23 s | 224.37 s | 0 | 4,673 |
| Full GPU warm | 819.49 s | 36.51 s | 4,673 | 0 |

Command durations include CPU, GPU and filesystem waits, startup, cache
persistence, pruning and cleanup. Independent post-run audits and output-size
walks are outside them. GPU metadata warm spans 1,863.95 s in UTC versus
318.94 s monotonic; full warm spans 1,785.97 s UTC versus 819.49 s monotonic.
The causes of these clock or suspend discontinuities are unknown. Retain both
clocks rather than treating the shorter duration as experienced wall time.

The first CPU cold metadata rebuild completes with 4,673 newly compiled chunks,
zero LOD hits and zero fallback chunks. Wall time is 18 min 22 s; the explicit
LOD timer is 15 min 38 s. It predates the hardware SHA feature. The full ordinary
asset check and every-chunk LOD graph/hash/coverage audit pass.

The original audit rejected all LOD warnings. The corrected gate compares the
entire worldspace/origin, exterior LAND and plugin-checksum inventory to the
protected source pack. Exactly 38 worlds lack authored origins in both packs.
All 14 LAND worlds with origins have indexed LOD. The metadata route also
omits `MarketplaceTextures.bsa`, whose unchanged source hash and lack of a
matching source-package plugin are verified. Any additional or changed omission
fails the audit; full extraction expects no archive-omission warning.

The GPU cold metadata rebuild passes with all 4,673 chunks GPU encoded, zero
cache hits and zero CPU fallbacks. Its explicit LOD timer is 262.62 s and
converter-reported elapsed time is 421.67 s. Every chunk has unchanged geometry,
bounds, cell coverage and atlas dimensions relative to CPU cold. GPU warm reuse
preserves all payloads and input proofs exactly. Fresh GPU conversion also
matches all 4,673 GPU metadata payloads and proofs, with zero fallbacks; normal
warm conversion preserves them again. All six reports are complete with zero
skipped items. The full runs omit no source archives.

Full warm spends only 36.51 s in LOD, but still takes 13 min 39 s monotonic
command time. Package publication alone takes 197.92 s, compared with 55.32 s
fresh. Restoration, publication and verification limit the warm result despite
zero newly converted items or GPU-encoded textures and chunks. The report's
`converted` and `cache_hits` counters combine extracted archive entries and
ordinary asset items; they do not count unique runtime files.

Other converters, builds and engines ran during these measurements. CPU cold
also predates hardware SHA acceleration. These observations cannot isolate an
end-to-end GPU scaling factor. Global disk and GPU counters include other
processes. Keep cache persistence, pruning and cleanup separate from package
publication; label an event span when no explicit phase timer exists.

## Correctness gates for the measured revision

- **Build, tests and lint: pass.** The final release converter builds; all 581
  standard tests pass under the quick profile. Clippy passes with warnings
  denied, excluding the existing native codec linker warning.
- **Archive recovery: pass on fixtures.** Tests cover BSA/BA2 decoding, selected string tables,
  path/collision checks, damaged packs/blobs, interrupted batches, explicit
  invalidation and producer/writer failure handling. Fixture recovery is not an
  actual power-loss test.
- **CPU LOD regression: pass.** CPU cold metadata-pack hashes, graph, geometry, bounds,
  coverage and three-mip sRGB UASTC layouts pass. Matched kernels preserve
  alpha and gutters and produce deterministic payloads across worker counts.
- **Cache and fallback: pass on fixtures.** Tests verify CPU-to-GPU misses, GPU warm hits,
  quality-change misses, return-to-CPU payloads, and failed GPU initialization
  or encoding without GPU reuse proof.
- **Hardware quality and slots: pass on Metal.** The bounded quality, concurrent
  slot and GPU cache-transition tests pass. The synthetic three-mip atlas
  measures GPU PSNR of 60.45, 51.01 and 45.22 dB against its source (CPU: 60.17,
  53.41 and 46.80 dB). Alpha, padding and sampled gutters pass. These correctness
  checks overlapped the release build and supply no performance result.
- **Full fresh and warm pack checks: pass.** Both ordinary `check --full` and
  the independent LOD audit pass. All 4,673 payloads and proofs match the GPU
  metadata rebuild exactly, then survive normal warm conversion. The audit
  checks database integrity, indexed geometry, bounds, cell coverage and
  three-mip sRGB UASTC layouts. These are pack checks; no engine visual
  acceptance or controlled complete-conversion speedup is claimed.

An initial hardware quality test crashed in the test-only Basis RGBA32 decoder:
its Rust wrapper supplied a block-based pixel row pitch. The helper now passes
the correct pitch and full allocation through the native API, with a CPU-only
regression test. Corrected hardware validation passes; the crash itself yielded no
quality score or benchmark result. Evidence is in
`target/lod-profile/atlas-quality-decoder-crash.txt`.

Warm CPU reuse completes with all 4,673 cache hits and a 55.53 s LOD timer.
The run takes 247.32 s of converter-reported elapsed time; unchanged payloads and proofs
compare exactly to CPU cold. A two-second sample during database export places
all 177 main-thread observations in SQLite integrity traversal of the newly
rebuilt staging database; 176 reach `pread`. Nearby sampled CPU use is 0.36
cores. This identifies a local database verification I/O wait. Hardware SHA and
GPU compression do not remove that gate, and these observations do not prove
physical disk saturation. Evidence is in
`target/lod-profile/warm-validation-bottlenecks.json`.

Measure serial chunk publication after hardware SHA acceleration next.
The captured main-thread slice spends 85 of 175 samples hashing and 59 in file
open/write/close; these are local observations, not whole-conversion phase shares.
Publication blocks preparation of the next batch, repeats an original-GLB hash,
and computes an unused embedded-texture digest. Its spatial-row deletion and
`MAX(id)+1` allocation also scan the growing R-tree for every chunk. Preserve
readback and structural validation while measuring these costs. Source references
and read-only query plans are in `target/lod-profile/publication-bottlenecks.json`.

The fresh run also exposes serial ingestion-cache persistence after package
publication. In a two-second sample, all 179 main-thread observations are in
`persist_ingestion_cache`; 171 reach hard-link creation in `linkat`. Nearby CPU
use averages 0.39 cores and global GPU counters are 1–3%. This identifies
filesystem metadata work in that slice, rather than GPU encoding. It does not
prove disk bandwidth saturation. `publication_elapsed_ms` stops before this
cache walk, pruning and staging cleanup; the Publishing 100% event also precedes
them. Reducing derived-file materialization or using bounded writers here is a
follow-up target. Pack flushes and recovery guarantees must remain intact.

Warm archive restoration has bounded writers already, but filesystem work
still applies backpressure. In a separate two-second sample, producers mostly
wait while writers create hard links, rename files and check paths. The active
producer frames verify cached bytes; none decompresses archive content. The
archive coordinator also rechecks the source checksum. Nearby converter CPU
use is 1.65 cores, while another converter uses about 5.54 cores. These local
observations support a restoration and integrity-verification cost, without
establishing a disk bandwidth limit. Evidence is in
`target/lod-profile/warm-ingestion-bottlenecks.json`.

Implementation references: [archive batches](../../crates/converter/src/archive/batched.rs),
[cache journal](../../crates/converter/src/cache.rs),
[LOD pipeline](../../crates/converter/src/pipeline.rs),
[terrain preparation and recipes](../../crates/converter/src/lod/terrain.rs),
[atlas batching and quality tests](../../crates/converter/src/lod/albedo.rs), and
[GPU encoder](../../crates/converter/src/texture_gpu/mod.rs).

## PR-readiness addendum: GPU default and latest cache checks

The follow-up adopts PR #167 at `2983e96f25dd488858feec1f5b0cd22058e20ca1`.
New CLI runs and `PipelineConfig::new` use GPU LOD at quality 2 with the requested
256 MiB batch setting, capped at 64 MiB per terrain slot. Ordinary textures keep
their independent CPU default. Explicit CPU selection survives the printed
resume command; omitted legacy serialized LOD encoder fields retain CPU behavior.

Checked cached-GLB references and committed per-world refusal/totals notices are
merged with GPU batching. A further KTX guard rejects impossible mip counts
before shifting dimensions, with regressions for rejection and valid array,
cubemap and volume layouts. The launcher report fixture includes both new counters,
and the engine option scan excludes converter-only profiling tools while retaining
its existing engine-script checks.

The final code tree passes 1,149 workspace tests across 49 suites; 24 tests are
ignored. Workspace formatting and all-target, all-feature Clippy pass. Clippy
denies warnings except the existing macOS native-codec linker warning. The release
converter builds on macOS 26.6.2/arm64 with Rust 1.98.1.

Seven native CLI legs pass using nine synthetic LAND cells and six LOD chunks:

| CLI leg | LOD hits | New GPU chunks | CPU fallback chunks |
|---|---:|---:|---:|
| Default cold, no encoder flags | 0 | 6 | 0 |
| Default warm | 6 | 0 | 0 |
| Explicit GPU cold | 0 | 6 | 0 |
| Forced unavailable backend | 0 | 0 | 6 |
| Healthy retry | 0 | 6 | 0 |
| Retry warm | 6 | 0 | 0 |
| `--no-lod` with unavailable backend | 0 | 0 | 0 |

The GPU is Apple M1 Pro/Metal. Every leg passes the full ordinary-package check.
LOD audits verify graph, geometry, bounds, source-cell coverage and three-mip sRGB
UASTC layout. Default and explicit GPU payloads/proofs match exactly, as do both
warm runs. Fallback omits all six GPU proofs and retries on the healthy run.
Warm and disabled runs emit no GPU initialization notice; disabled output has no
LOD payloads or indexed chunks. This is behavior validation, not a benchmark.

Local performance acceptance is incomplete: three release CPU budgets pass, but
the unchanged dummy-content file-generation budget fails twice at 66.923 ms and
72.393 ms against its 50 ms limit. All 15 tracked dummy-content files match the
PR #167 baseline; no local baseline performance run or causal attribution is
claimed. The limit and file flushes are retained. Linux CI, the dependency audit
and human code-owner approval remain merge gates. A PR stacked on
`fix/lod-build-performance` needs manual CI dispatch or retargeting after #167
merges because the workflow only triggers automatically for `main` and `develop`.

The protected source manifest remains
`ff5743feff7b2db3dc77fa2cac8b5741191e7e7c3e2069daddbde5f362ee7d09`.
Current source/binary identities and validation receipts are in the
[readiness receipt](lod-conversion-pr-readiness-20261009.json). The six full-pack
measurements above remain pinned to their earlier binaries.
