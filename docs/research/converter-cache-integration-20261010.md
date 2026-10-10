# Converter cache integration: PRs #208 and #209

This branch integrates #208 at `119f8995012c8f8e3596f7797e52b3dd0370baaa` and #209 at `a19b95be0af9b9984fb8fe64c1ff08a662dcbfd4`. It preserves their ancestry, including #167 and #204, and does not retarget or merge either original PR. Merge those dependencies before landing the integration.

## Contracts compared and reconciled

| Contract | #208 | #209 | Combined implementation |
| --- | --- | --- | --- |
| Selection | String recipes; supported extensions including all languages | Typed all/runtime-English coverage | One typed coverage model with explicit versioned recipes. All covers runtime inputs with every language bank, which cover runtime-English; reuse in the opposite direction misses. Unknown recipes miss. Both manifest fields constrain reuse. |
| Raw cache | SHA blobs derived from immutable sealed packs | SHA blobs with deduplicated writes | One batched backend, using the shared deduplicated blob writer and bounded spill verifier. |
| Pack format | Version 1 contiguous payloads | No pack format | Read version 1 and version 2; write version 2 with repeated payloads sharing one range within each pack. Duplicate bytes across packs/archives can still exist. |
| Recovery | Verified partial packs; completed-archive staging journal | Verified blob/session reuse | Both retained. Source hash, safe namespace, pack hash, index version/layout, payload size/hash and coverage are checked before reuse. |
| Synchronization | Durable batch packs; derived blobs need no individual flush | Per-file, archive, none | New CLI/config runs default to archive (durable pack batches). Per-file additionally flushes unique physical blob writes. None skips fresh extraction flushes and requires verified reuse. Legacy serialized configs retain per-file defaults. Published-cache/package durability remains separate. |
| Persistence | Allowlisted sealed packs/canonical blobs | Repair corrupt persistent blobs without mutating output links | Allowlisted objects, verified content addresses, atomic replacement and pack/directory synchronization. |
| Database cache | Conversion rebuilds database | Verified pristine bundle; private integration copy | #209 bundle identity, verification, input recheck and private integration copy retained. |
| Invalidation | Source hash and extraction recipe; separate LOD encoding identity | Source hash, selection, reader/plugin/localized-bank database identity | Both constraints retained. Explicit invalidation bypasses pack/inventory reuse. Producer changes invalidate affected derived outputs, rather than valid raw inputs. |
| Profiling | LOD timing and counters | Detailed ingestion/database/publication/persistence/pruning/cleanup spans | Both retained; pack flush counters (including verified restore flushes) are separate from raw blob flush counters. Worker flush durations overlap extraction and must not be added as wall-time phases. |

## Implementation and compatibility

`IngestionSelection` is the single coverage vocabulary. `ArchiveSelection` is an alias, rather than a second extraction contract. Existing #208 manifests deserialize with default `selection=all` but their recipe still limits reuse; existing #209 manifests deserialize with an empty recipe but their typed selection still limits reuse. Combined manifests write both fields. A restricted manifest can never become a broader proof just because one field is missing.

The pipeline uses bounded CPU decoding and separate I/O workers. Its raw session spans the ordered archive overlay. SHA-addressed writes and physical spill copies use #209's measurements and durability bookkeeping; #208's spill proof cache bounds hashing/handle retention. A pack provides recovery for missing or corrupt derived blobs, including interruption before the completed archive journal is recorded. Unsealed temporary objects are never published.

Old #208 readers reject version-2 packs and unknown combined recipes and can re-extract; old #209 readers cannot consume the new typed converter-input coverage. This is safe cache invalidation, not bidirectional binary-format compatibility.

The native reader retains #204's accepted LAND winners by plugin/native position. Legacy publication retains #208's ambiguity rejection. GPU LOD defaults and its independent encoder/quality identity remain intact; ordinary source textures keep their independent encoder.

## Regression coverage

New integration cases exercise both prior manifest shapes and conflicting/unknown coverage, English-only coverage expansion/narrowing, duplicate payload ranges, recovery of aliases with all derived blobs removed, version-1 pack compatibility, and none-to-archive synchronization changes. Verified restored packs are flushed under a durable policy even when no copy is needed. Inherited suites exercise malformed/corrupt packs, corrupt blobs and spill files, atomic cache repair and replacement of destination symlinks without modifying their targets, torn journals/receipts, source replacement, writer failure, partial batch recovery, cancellation and resumed conversion, reader/configuration changes, and database-bundle integrity.

## Accessible validation

PR #213 recorded the following Linux results at its own checkpoint with the repository-pinned Rust 1.98.1 toolchain. The current all-pending-PR integration results are in the [integration report](pending-pr-integration-20261010/README.md):

- `cargo +1.98.1 test --locked -j 2 -p converter --lib --bins --tests --no-fail-fast -- --test-threads=4`: 811 passed, 19 ignored across 57 targets. The final symlink replacement regression was added afterwards: its targeted library run passed (1 passed, 524 filtered out).
- `cargo +1.98.1 clippy --locked -j 2 -p converter --all-targets --all-features -- -D warnings`: passed. Cargo reports an existing future-compatibility notice for dependency `proc-macro-error2` 2.0.1.
- `python -m unittest discover -s scripts/lod-performance -p 'test_*.py'`: 12 passed.
- `cargo +1.98.1 fmt --all -- --check` and `git diff --check`: passed.

The all-pending-PR integration additionally routes verified warm materialization through the shared durability and copy accounting. A regression covers None/Archive/PerFile, existing/new cache roots, and hard-link/copy/spill routes. PerFile flushes previously unsynced physical payloads; ordinary warm hard links avoid a second full digest read and retain parallel per-hash ownership. Copy counters describe payload copies, excluding metadata and hash/read I/O.

Historical benchmark reports from either PR remain historical; none measures this combined implementation.

## Remaining acceptance work

Run the full workspace and required CI gates on the published integration commit; complete human/code-owner review. The original stacked draft required a manual workflow dispatch because its workflow filtered PR bases. The all-pending-PR integration repairs that filter and checks changes against the immutable PR-event base; these changes are recorded in the [integration report](pending-pr-integration-20261010/README.md). Verify native Metal plus other supported native platforms, including Windows flush/link-limit behavior. Fixture interruption tests do not establish actual power-loss recovery.

For performance evidence, build a pinned combined commit and record the binary hash, source inputs, configuration, worker counts, cache starting state and output validation. Run repeated matched cold/warm legs on a quiet host with equivalent source-cache conditions, both with and without generated LOD. Compare output manifests, logical database content, terrain proofs and assets. Report pack bytes as well as canonical blob writes: deduplication within a pack does not establish global pack-byte deduplication or device-I/O savings. Separate raw blob flushes, pack flushes, restore work, persistence, pruning, cleanup and nested publication timings. Do not sum overlapping worker/nested timers or reuse either PR's historical measurements as a combined speedup claim.
