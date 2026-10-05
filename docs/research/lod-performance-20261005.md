# LOD build performance

Corrected implementation: `c9894eded2e7c3dbe1732965b176f1a624b959ae` on
`db3a1dc3ed80795149e3a15a5047f4d313dd3e74`.
Converter schema 24 and world schema 7 are unchanged. Terrain recipe revision 4
invalidates earlier LOD payloads without invalidating ordinary assets.

The corrected Fiji cold/warm pair passed: cold generated 4,673 chunks with zero
chunk hits; warm reused **all 4,673 chunks**. Both runs completed, passed integration,
converted/skipped zero ordinary inputs and preserved all 76,213 ordinary entry
proofs. An independent pass verified every final ordinary output size/hash and
every LOD payload hash. T41 is complete; runtime captures remain deferred under T40.

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
and MemoryMax 24 GiB. The original disk guard interrupted the verified owned converter PID
below 15 GiB free space; corrected runs use a 14 GiB guard. Neither corrected run
triggered it. The installation contains 93 BSAs and 80 plugins; names and sizes
match the baseline. This does not establish historical input byte identity.
Game inputs and converted payloads remain on Fiji.

The original cold LOD run starts with seeded ordinary/archive caches and no prior LOD.
The corrected cold run starts from the revision-3 benchmark package: revision 4
rejects every prior chunk's compiler identity and rebuilds all 4,673 chunks.
The warm acceptance run must reuse all 4,673 chunks. The driver requires complete reports,
zero reconversions/skips, passing integration, unchanged manifest byte proofs for all
76,213 ordinary entries, equal cold/warm chunk-index digests and build identities,
and a successful full ordinary-output hash check. Warm reuse additionally verifies
each LOD payload hash and terrain structure before republishing it. The seed proof digest is
`d1d790983c95bd87160644d40444e46259877a522e3c03e076d92b2faf19e985`.

Cold LOD and the historical fresh conversion have different ordinary-cache and
OS-cache conditions. Stage timings are comparable evidence under the same worker
and resource limits; they do not isolate the contribution of each optimization.

Revision-3 converter SHA256:
`974c831383d902a0423bb2e2047082d2c84aad758887f127c44a6d82a3fc1d98`.
Remote evidence root:
`/home/taylor/Projects/mudcrab-lod-performance/target/lod-performance/fiji-final-3ffcd9c`.
The driver runs in a transient user service. After Fiji returned with user
lingering disabled, closing the last SSH session stopped an initial retry;
subsequent runs kept an SSH session open until service completion. A Fiji machine reboot
stopped the `0518df8` run at 28m 6.8s, before publication; its last sample showed
2,636 staged chunks and the journal recorded a 17.1 GiB memory peak. This attempt
supplies no completed timing result. Its logs and reboot record are preserved.
A fresh current-head attempt started at `2026-10-05T04:24:36Z` from the same unmodified seed. Setup verified
that the dirty primary checkout's HEAD, status and index hash were unchanged.
The original benchmark package and its manifest were preserved.

## Completed corrected cold/warm acceptance

The corrected pair ran from `2026-10-05T17:53:21Z` to `2026-10-05T18:39:08Z`,
including the final ordinary full hash check.

| Run | Converter elapsed | LOD stage | Publication | Chunks | Verified chunk hits |
| --- | ---: | ---: | ---: | ---: | ---: |
| Corrected cold LOD, `c9894ed` | 2,038.579 s | 1,531.532 s | 125.071 s | 4,673 | 0 |
| Corrected warm LOD, `c9894ed` | 686.320 s | 172.750 s | 126.806 s | 4,673 | **4,673** |

Driver wall times were 2,040.887 s cold and 691.461 s warm. The LOD stage fell
from **25m 31.532s** cold to **2m 52.750s** warm, an 88.7% reduction for this pair.
Corrected cold LOD was 53.6% below the historical 55m 02.352s stage. Different
ordinary/OS-cache conditions prevent a controlled end-to-end speedup claim or
isolating each optimization's contribution.

Both corrected reports record 257,867 ordinary cache hits, zero ordinary
conversions/skips and passing integration. Coverage remains 52,362 terrain/cache
cells, zero missing/invalid models and zero missing textures. Existing limits
remain: 28 unbounded models, 128 unavailable model sources, 16 unavailable texture
sources, 38 warnings for worlds without LOD origins, and 527 pruned texture
references absent from game data. Acceptance covers the same 4,673 produced chunks.

Implementation tree:
`24690d6e15004bf91359269273958feec922f0ac`.
Corrected converter SHA256:
`daf24867aa9aa4782b20712a6bcdafec45fcad6f4ee0d1fb2e1957af5a052f8a`.
Cold report SHA256:
`81253cd2ae76e2f4ecc34d7f9a5dc53689ffe82c1c26eedade644e9c15071c63`.
Warm report SHA256:
`a28540b59a4fb14670c8976d14216c0629414489575e074ce82d68055a70ae3a`.

The saved cold/warm index rows and all 4,673 consumed-input fingerprints are exactly
equal. Their shared chunk-index digest is
`91e92656179452b16d25abe2ba7714b228a6e6ffc2b0870e265389245a7e1467`;
their shared build identity is
`31c0165bf5e010fb35327a353ad7381078dfa37fb234607e821cecf86d6d9ddc`.
The final SQLite rows and LOD manifest match those saved proofs.

All 76,213 ordinary entry proofs retain the seed digest, schema and configuration.
The ordinary manifest itself is byte-identical to the original seed manifest:
`67f2e79fdbf9c6690b366cd962705effcd3aa098da17357bad0ec88f79e13297`.
`converter check assets --full` exited zero after warm publication and checked all
76,213 ordinary files in 13.7 s. That command covers ordinary manifest entries.
A separate read-only pass, holding the converter's package lock, hashed all
**76,213 ordinary files** (13,462,818,455 bytes) and **4,673 LOD files**
(8,924,734,348 bytes), with zero mismatches. Each final LOD payload matches the
recorded cold/warm hash. Warm reuse also validates every source payload's terrain
structure in `LodReuse::checked_chunk`; the final hash pass binds published bytes
to those validated payloads. Native binary and runtime hashes matched provenance.

[Committed summary](lod-acceptance-20261005/summary.json),
[read-only verifier](lod-acceptance-20261005/verify.py), and
[ordinary full-check log](lod-acceptance-20261005/check-full-v4.log) retain the proof.
The summary pins SHA256s for raw reports, measurement, provenance, driver and logs
on Fiji. The remote evidence root above retains `*-v4` files for the corrected
head despite its historical directory name. Metadata copies are retained in the
proof worktree's `target/lod167-evidence/`; no inputs or payloads were copied.

```sh
python3 docs/research/lod-acceptance-20261005/verify.py target/lod167-evidence
```

On Fiji, copy the verifier beside the raw evidence and add `--assets` pointing to
that root's `assets` directory to repeat every published-output hash check. A
summary is emitted only after all requested checks pass. Native acceptance stays
pinned to `c9894ed`; later implementation/base integration changes need their own
validation.

## Earlier revision-3 cold result and interruptions

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
answering SSH; task-owned staging cleanup recovered 10,522,361,856 bytes without changing
the published cold manifest. Retries recorded a 14 GiB harness disk guard;
the artifact acceptance assertions remained unchanged.

Cold metadata-only evidence is retained locally under
`target/lod-performance/fiji-final-3ffcd9c-evidence/`, including the report, log,
samples, provenance and native release regression results. The warm failure is
recorded in `warm-interruption-observation.json` from the live SSH read. No game
inputs or converted payloads were transferred. This incomplete attempt is superseded
by the completed corrected pair above.

## Earlier revision-3 warm acceptance failure and LAND selection

Two completed unchanged-input runs at `3ffcd9c` each reused 4,670 of 4,673
chunks, converted/skipped zero ordinary inputs and passed integration. Both
failed the driver's all-chunk reuse requirement. The first took 691.244 s
(LOD 181.758 s, publication 125.532 s); the diagnostic repeat took 664.559 s
(LOD 180.085 s, publication 126.069 s). No disk guard fired.

The diagnostic saved per-chunk input fingerprints and index rows before running.
Exactly three Tamriel chunks changed both fingerprints and payload hashes:
`lod/0000003c/4/cell_13_33.glb`, `lod/0000003c/8/cell_6_16.glb`, and
`lod/0000003c/16/cell_3_8.glb`. Their source-cell lists remained equal.
The build identity changed from
`8286276fd93567bee8bc4ca7ed081beeb3c7629feef534e9e86a7cf633824a62` to
`e8e356364e449f673aa5e0b73e6fbda3c14116b205755dbfa1ec3075db986c30`.
This is a reproduced stability failure, not completed warm acceptance.

Live record metadata identifies two overlapping LAND definitions per cell:
cell `000018DF` at (-44,37) has forms `000028DF` (priority 0) and
`00222222` (priority 2); cell `000018FD` at (-43,36) has forms
`000028FD` (priority 0) and `000179DB` (priority 2).
`write_cell_cache` overwrote LAND by cell through unordered `HashMap` traversal;
`export_records` overwrote the database LAND row in ascending FormID order.
The two projections therefore lacked one shared plugin-priority winner.

The correction selects the highest-plugin-priority LAND for each cell in both
projections, retains raw records, and rejects multiple candidates at the winning
priority before replacing outputs. Terrain compiler revision 4 invalidates prior
LOD packages. The completed corrected pair above establishes revision-4 acceptance;
these revision-3 diagnostic measurements remain failed attempts.

An independent `converter check assets --full` on the post-diagnostic package
exited zero in 13.527 s. This verifies all 76,213 ordinary manifest outputs;
it does not resolve the failed LOD identity/reuse gate. Metadata evidence is
retained as `diagnostic-baseline-v2.json`,
`warm-diagnostic-v3-measurement.json`, and
`check-full-post-diagnostic-v3.{json,log}`. Inputs and payloads remain on Fiji.

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

Corrected-head CI (`c9894eded2e7c3dbe1732965b176f1a624b959ae`) passed **1,085**
workspace/all-target tests with 19 skipped, plus formatting, strict Clippy, security
and performance checks. CodeRabbit completed and no review threads were open when
checked. Local corrected-head verification passed 382 converter library tests with
12 ignored; the LAND selection regression failed with the original projections
restored. The evidence follow-up changes preserve the measured implementation.

Earlier-head CI (`3ffcd9c23121da81df3364d30e9d47fea9124ffd`) ran 1,083
workspace/all-target tests: 1,083 passed, 19 skipped. Formatting, strict Clippy,
security and the CI performance checks pass. The cold measurement completed;
revision-3 warm acceptance failed as detailed above. An independent full ordinary-output
hash check passed after that diagnostic repeat. Corrected-head native acceptance
passed separately as recorded above. CI performance checks
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
