# LOD build performance

Implementation: `3ffcd9c23121da81df3364d30e9d47fea9124ffd` on
`db3a1dc3ed80795149e3a15a5047f4d313dd3e74`.
Converter schema 24 and world schema 7 are unchanged. Terrain recipe revision 3
invalidates earlier LOD payloads without invalidating ordinary assets.

The change removes a second, disposable atlas mip encoding; shares one validated
cell snapshot and decoded terrain textures across worlds; publishes bounded chunk
batches; and reuses unchanged chunks only after current inputs and stored payloads
verify. Each chunk completes geometry, atlas baking, linear-light mips and encoding
in one worker. Ordinary textures already finish their conversion and mip work in
one operation. LOD combines multiple resolved cells and winning diffuse sources,
so it runs after world extraction rather than inside an individual NIF conversion.

An atlas-local cache additionally reuses exact 64-byte RGBA input blocks. It calls
the same upstream level-2 UASTC routine with all transcode hints retained, caps the
cache at 65,536 entries, and releases it after the atlas. It does not lower quality
or depend on a particular GPU format. The KTX2 writer is shared with the GPU
encoder, retaining the existing ordinary GPU writer metadata.

## Baseline and measurement method

The prior Fiji run at `9de099e2cb429cf6c143c384e5cf2baa17bf6318` spent 3,302.352 s
(55m 2s) in LOD and generated 4,673 chunks. Its complete fresh conversion took
5,581.549 s. A warm `--no-lod` run took 504.657 s. The baseline binary SHA256 was
`4e87f2a513fa25a61b3c1d1ba3d210a86da6a0cb65466fbf96bafbf926b39b81`.

The benchmark ran on `fiji-desktop`, in an isolated detached worktree,
with six CPU workers, four I/O workers, Nice 5, CPUQuota 600%, MemoryHigh 20 GiB,
and MemoryMax 24 GiB. A disk guard interrupts the verified owned converter PID
below 15 GiB free space. The unchanged installation contains 93 BSAs and 80
plugins; names and sizes match the baseline. Game inputs remain on Fiji.

The cold LOD run starts with seeded ordinary/archive caches and no prior LOD.
The warm acceptance run must reuse all 4,673 chunks. The driver requires complete reports,
zero reconversions/skips, passing integration, unchanged manifest byte proofs for all
76,213 ordinary entries, equal cold/warm chunk-index digests and build identities,
and a successful full ordinary-output hash check. Warm reuse additionally verifies
each LOD payload hash and terrain structure before republishing it. The seed proof digest is
`d1d790983c95bd87160644d40444e46259877a522e3c03e076d92b2faf19e985`.

Cold LOD and the historical fresh conversion have different ordinary-cache and
OS-cache conditions. Stage timings are comparable evidence under the same worker
and resource limits; they do not isolate the contribution of each optimization.

Final converter SHA256:
`974c831383d902a0423bb2e2047082d2c84aad758887f127c44a6d82a3fc1d98`.
Remote evidence root:
`/home/taylor/Projects/mudcrab-lod-performance/target/lod-performance/fiji-final-3ffcd9c`.
The driver and transient service survive SSH/T3 restarts. A Fiji machine reboot
stopped the `0518df8` run at 28m 6.8s, before publication; its last sample showed
2,636 staged chunks and the journal recorded a 17.1 GiB memory peak. This attempt
supplies no completed timing result. Its logs and reboot record are preserved.
A fresh current-head attempt started at `2026-10-05T04:24:36Z` from the same unmodified seed. Setup verified
that the dirty primary checkout's HEAD, status and index hash were unchanged.
The original benchmark package and its manifest were preserved.

## Completed cold result and pending warm acceptance

| Run | Converter elapsed | LOD stage | Publication | Chunks | Verified chunk hits |
| --- | ---: | ---: | ---: | ---: | ---: |
| Historical fresh full conversion | 5,581.549 s | 3,302.352 s | 1.356 s | 4,673 | 0 |
| Current cold LOD, ordinary/archive caches seeded | 2,303.725 s | 1,680.650 s | 157.716 s | 4,673 | 0 |

The completed current cold run exited zero, passed integration, converted zero
ordinary inputs and skipped zero inputs. All 76,213 ordinary manifest entries
retained the seed configuration and byte proof. Its driver wall time was
2,315.148 s. The observed LOD stage fell from 55m 02s to 28m 01s, a 49.1%
reduction. The differing cache conditions prevent treating the end-to-end
intervals as a controlled speedup. Publication also differs: the baseline used a
new destination, while this run replaced and sealed an existing package.

Both runs generated 4,673 chunks. Current integration recorded 52,362 terrain/cache
cells, zero missing or invalid models and zero missing textures. It retained the
baseline's coverage limits: 28 models without static bounds, 128 unavailable model
sources, 16 unavailable texture sources and 38 LOD warnings for worldspaces without
origins. Published meshes omit 527 texture references absent from the game data.
This is output/integration evidence, not new runtime rendering acceptance.

Cold report SHA256:
`a342437b28380c1df42973412de3956b6bfd48509494c36eabafc865aedbe74f`.
Cold chunk-index digest:
`91e92656179452b16d25abe2ba7714b228a6e6ffc2b0870e265389245a7e1467`.
Cold LOD build identity:
`aab4fe611b329a74e10482ff66f4a6e9cc71f22756456413f2e0f42c593009e5`.

The warm attempt hit the disk guard at 692.776 s, with 4,571 staged chunks and
16,087,994,368 bytes free, below the 16,106,127,360-byte threshold. The converter
stopped and retained private staging; the completed cold package remains the
published source. The driver exited 1 and left its measurement status as
`running`, so that field alone does not establish liveness or completion. This
attempt supplies no completed warm timing or reuse verdict. The full ordinary
hash check did not run. Fiji subsequently went offline in Tailscale and stopped
answering SSH; disk recovery and a fresh warm attempt remain pending.

Cold metadata-only evidence is retained locally under
`target/lod-performance/fiji-final-3ffcd9c-evidence/`, including the report, log,
samples, provenance and native release regression results. The warm failure is
recorded in `warm-interruption-observation.json` from the live SSH read. No game
inputs or converted payloads were transferred. T41 remains open until warm reuse
and the full ordinary-output hash check finish.

## Remaining compression cost

A 15-second user CPU profile of the superseded `efeedfe` cold run captured 7,137
samples at 99 Hz with no lost samples. `compute_etc1_hints` accounted for 40.47%;
`etc_block::get_block_colors` for 13.89%; `evaluate_solution` for 13.54%; and
`compile_chunk` for 3.50%. This identifies UASTC compression as the sampled CPU
cost; it does not establish an I/O bottleneck or account for the whole run.
That intermediate run was deliberately interrupted with SIGINT after profiling,
before publication, so it supplies no completed timing result. Its logs, samples,
profile and interruption record remain in the sibling `fiji-full` evidence root.
Only its verified owned partial staging was removed; its unmodified ordinary seed
and immutable ingestion cache were moved into the final benchmark root.

The upstream faster ETC1-hint flags narrow the encoder's search, and the low-level
`basis_compress` API masks those flags out. Bevy supports ETC2 fallback as well as
ASTC, BC7 and RGBA, so weakening ETC1 hints would change a supported decode target.
The implementation instead reuses only byte-identical inputs at the unchanged
encoding level. Regression tests compare exact payloads and format descriptors,
including alpha, repeated inputs and partially filled edge blocks.

Pinned source evidence:

- [basis-universal-sys 0.3.1 UASTC encoder](https://docs.rs/crate/basis-universal-sys/0.3.1/source/vendor/basis_universal/encoder/basisu_uastc_enc.cpp): `compute_etc1_hints` and `encode_uastc`.
- [basis-universal-sys 0.3.1 compressor](https://docs.rs/crate/basis-universal-sys/0.3.1/source/vendor/basis_universal/encoder/basisu_comp.cpp): `encode_slices_to_uastc` extracts clamped 4x4 blocks and invokes `encode_uastc`; `basis_compress` keeps only the UASTC level mask.
- [Bevy image 0.19.0 KTX2 loader](https://docs.rs/crate/bevy_image/0.19.0/source/src/ktx2.rs): RGB/RGBA transcode priority ASTC, BC7, ETC2, then RGBA.

A bounded release-mode encoder comparison on Fiji used a 512x512 synthetic image
per case. It requires exact encoded payload equality for each case. Millisecond
values are rounded; zero means below one millisecond. This is an encoder test,
not an end-to-end LOD speedup claim.

| Input | Upstream | Exact block reuse | Distinct blocks |
| --- | ---: | ---: | ---: |
| Solid | 30 ms | <1 ms | 1 |
| Repeating 32x32 pattern | 1,564 ms | 6 ms | 64 |
| Nonrepeating input | 1,868 ms | 1,855 ms | 16,384 |

## Verification

Current-head CI (`3ffcd9c23121da81df3364d30e9d47fea9124ffd`) ran 1,083
workspace/all-target tests: 1,083 passed, 19 skipped. Formatting, strict Clippy,
security and the CI performance checks pass. The cold measurement completed;
warm acceptance and the full hash check remain pending. CI performance checks
do not substitute for these LOD measurements.
CodeRabbit completed at this head without actionable findings; no inline review
threads were open when checked.

The first workspace run hit the inherited
`extraction_does_not_wait_for_a_front_end_that_stopped_reading` 60-second timeout.
Extraction code was unchanged. The test passed in isolation in 27.56 s and passed in the four-thread workspace
rerun. That rerun was stopped only after exact-head CI completed all targets,
while the inherited streaming-interior fixture was still running locally.
The initial failure and intentional-interruption logs are retained; no extraction
or streaming code was changed. A Clippy fixed-size-chunk lint was corrected in
`3ffcd9c`, using `as_chunks::<4>()` for the same alpha scan.

Regressions cover single-pass mips and exact block payloads, sRGB lookup bit
identity, shared authored-mip decode, immutable cell snapshots, consumed-input
fingerprints, source-derived bounds, byte-identical warm reuse, damaged-chunk and
legacy-proof misses, affected-tier rebuilds, origin/diffuse invalidation, removed
cells, metadata-source preservation and later-batch world rollback.

This work does not supply new runtime rendering captures or complete deferred T40.
