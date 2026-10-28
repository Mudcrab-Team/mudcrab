# Converter performance without generated LOD

Fresh Skyrim conversion took 381.731 seconds wall time; a warm run took 186.137 seconds. The preceding implementation took 491.721 and 258.439 seconds respectively. These sequential observations used uncontrolled source caches and different host workloads, so they do not establish a controlled speedup.

The benchmarked source is preserved in commit `54a4780bf462623a3ca04d29ab12b9f44b8670fd`. This commit contains the performance changes on the original #204 reader head. The later integration of #204's `7dcf1344` torn-record fix and any subsequent CI repairs are outside these historical timing and pack-parity receipts. Current-head CI results belong in the PR description separately.

## Setup and clocks

Apple M1 Pro, 32 GB RAM, macOS, Rust 1.98.1, Metal GPU texture encoding at quality 2. Inputs contained 80 plugins and 93 archives. The relevant invocation was:

```sh
converter "$SKYRIM_DATA" "$OUTPUT_PACK" \
  --record-reader inhouse --texture-encoder gpu --gpu-quality 2 \
  --cpu-jobs 8 --io-jobs 4 --no-lod --ingestion-sync archive \
  --report-json "$REPORT_PATH"
```

The cold run used a fresh output/cache and added `--invalidate-cache`. The warm run reused that output/cache after complete cold validation. No generated LOD was built. The current cold run sampled an active engine and Cargo/Clippy jobs; the current warm samples contained no selected competing process. Samples do not cover every source of host load. Wall and monotonic clocks agreed within milliseconds; bounded power logs contained no paired sleep interval.

| Measurement | Previous cold | Current cold | Previous warm | Current warm |
| --- | ---: | ---: | ---: | ---: |
| Wall seconds | 491.721 | 381.731 | 258.439 | 186.137 |
| Pipeline seconds | 490.741 | 380.442 | 257.542 | 185.565 |
| Peak converter RSS, GiB | 13.75 | 13.16 | 14.06 | 0.527 |
| Archive-ingestion seconds | 175.864 | 42.720 | 59.746 | 56.457 |
| Fresh database-conversion seconds | 44.169 | 52.472 | 54.295 | 0 |
| Raw payload flush calls | 76,521 | 61,570 | 0 | 0 |

## Repeated work removed

Fresh ingestion selected 76,521 entries containing 26,383,892,386 uncompressed bytes. Hashing before writing produced 61,570 canonical payloads totaling 22,775,099,219 bytes, with no physical fallback copies recorded. This avoided 14,951 writes/flushes and 3,608,793,167 payload bytes. These are file-byte counters, not device block traffic.

Cold flush wall time was 4.962 seconds versus 119.438 previously. This difference exceeds the 19.5% reduction in flush calls; deduplication alone cannot explain it from these observations. Fresh extraction now includes direct VFS alias linking. Its timer is not pure decompression time, and the removed second linking pass reports zero.

The warm run restored six verified database artifacts, built zero new runtime winners, and skipped both terrain-cache generation steps. Database identity, restore, input recheck and validation totaled 6.567 seconds. Integration still used a private database copy. Historical producer timings remain separate from current restore timings. All 93 raw archives restored; zero fresh raw-write counters do not independently prove zero fallback copying.

## Content and storage checks

Both full published-pack checks passed. All 81 evidence gates passed, including equality of 76,213 canonical assets, 27 logical runtime SQL tables, retained source payloads, both terrain caches, integration results, 3,998 aliases, and runtime support. The selected functional suite passed 497 tests across eight targets, with 11 existing ignores and one case-sensitive fixture filtered on APFS.

The emitted database retains 1,240,167 runtime winners and corresponding source records, 561,261,840 source payload bytes, 109,395 localized fields, and 52,364 terrain source rows across 52,362 cells. Five localization issues and 211 cleared invalid links are unchanged. These checks establish preservation within the measured scope, not retail gameplay parity.

The raw cache contains 22,775,099,219 logical bytes. The clean database cache adds 3,003,308,666 logical bytes. The published pack contains 16,495,180,329 bytes after accounting for hard links; canonical runtime assets remain 13,446,629,386 bytes. APFS clones may share blocks, and inode/stat totals do not measure distinct clone extents.

Warm archive restoration, runtime publication and staging cleanup accounted for 118.196 seconds, or 63.7% of pipeline time. Publication included 29.579 seconds linking, 9.896 seconds hashing generated artifacts, 5.222 seconds deleting the old pack and 5 milliseconds syncing the ownership journal. These nested timers are already included in the publication phase.

## Evidence identity and remaining qualification

- Benchmarked source-file manifest SHA-256: `7831233e81b4f56be58d3eca5b3046c1cd6e57db92e98437f04f978b694dc09d`.
- Benchmarked converter SHA-256: `84e2db7e7eeb0fde5837bd32f00d9b6ca121fc45ae47aa725e92ec6413774eb8`.
- Local receipts preserve exact commands, build identity, process timelines, full-check results, SQL/output comparisons, and storage inventory. Source Skyrim files, output packs and raw private receipts are excluded from this contribution.

Repeated quiet-host timings, native platforms beyond macOS, and actual power-loss recovery remain unverified. The next performance target is reducing filesystem restoration and publication work, after a worker-count sweep and syscall profile identify the owning cost. #208 independently changes archive batching while adding GPU LOD defaults; archive/cache integration needs reconciliation when both contributions land.
