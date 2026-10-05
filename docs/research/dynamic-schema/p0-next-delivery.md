# P0 corpus, probe and pilot evidence delivery

This slice hashes installed archive bytes, adds strict candidate-evidence inputs,
probes a pinned xEdit console binary, and refreshes selected converter/native
field evidence. It does not accept P0. SPEC T35/T50 remain in progress, and no
P1/P2 ingestion implementation is introduced.

## Inputs and observations

The target remains Steam Skyrim SE/AE `1.7.104.0`, build `24914197`, executable
SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
The read-only version-2 manifest observed 80 plugins, five required base masters,
75 CCC names, a clean master closure and 93 hashed archives (20,336,627,877
bytes), with no reported scan issue. These are installed-file observations;
CCC membership and matching hashes do not establish official-content provenance.

A separate `BSArch64.exe -list` run enumerated 181,654 entries across all 93
archives. It found 2,160 string-table names in 76 archives without extracting
payloads. BSArch came from the official `xedit-4.1.5f` release archive:

| Artifact | Size / SHA-256 |
| --- | --- |
| `xEdit.4.1.5f.7z` | 31,345,110 bytes; `54c014da621f83f06a64fd92ddb8e32ed3082d1c65f543dc1c4e432130dced08` |
| `BSArch64.exe` | 4,893,184 bytes; `5a8f1fd36adb183fcf3eec04e092f61f2afa5e9a869ab181f81bd65a55e5b267` |
| `xDump64.exe` | 20,510,720 bytes; `30c085b8a20dc02bf5abae2cb6610870c9bb9eea50330e0fe5ade98e3f89efe6` |

The official release URL is
<https://github.com/TES5Edit/TES5Edit/releases/download/xedit-4.1.5f/xEdit.4.1.5f.7z>.
Its tag points to `f5c00f3fa3ee39511185515802647246c807f759`; a reproducible
source-to-binary build relationship has not been established. The mined xEdit
source pin is separately `9fb016884bec138ea6c7b872cec831537d464c3e`.
Both console observations used Wine `11.0` at
`/nix/store/cirj08cjmc9asn8kflycp2r5mchfyhvy-wine-wow64-11.0/bin/wine`.
Each archive was rehashed against the manifest before listing, with stat checks
around the observation. This does not create an immutable snapshot or prove
string-table payloads, locale applicability or resolution.

The candidate input contract is documented in [the schema tool README](../../../scripts/schema/README.md).
It checks strict JSON, exact target metadata, artifact hashes and dependency
order. Supplied locale/order/profile claims stay unverified; accepted pins and
provenance cannot be supplied as acceptance booleans. Output aliases and
ancestors of evidence paths are rejected before writes.

## Executed validator proof

At tooling commit `252bbca`, all 64 offline tests passed. New regressions cover
deep evidence JSON, output/evidence aliases, every registered Mudcrab checkout,
failed worktree discovery, custom SDK/Wine directories, protected process temp
settings and detached children holding captured pipes. The detached-pipe
regression waited 60.1 seconds before
the fix; final output collection now has a deadline. The supervisor signals its
original process group, but an escaped descendant can survive that signal. It
cannot keep the verdict path waiting indefinitely.

The fresh Mutagen `0.54.4` / SDK `9.0.318` run passed six synthetic positive
cases, four managed malformed-input rejections and both original commands
(`placed`, `placed-lo`), with zero build warnings/errors. Localized string ID,
optional typed ID and unavailable translated text remain distinct.

The fresh xEdit capability probe returned exit 2 and `qualified: false`:

| Cases | Outcome |
| --- | --- |
| Full, base, override, localized, unknown/repeated fixtures | Five known-value observations with schema diagnostics; fixture schema validity remains unestablished |
| Light plugin, invalid subrecord length | Two incomplete observations |
| Truncated tail/header, invalid TES4 length | Three unqualified negatives; dumping malformed input does not establish managed rejection |

Physical offsets, opaque payload completeness, complete source occurrences,
full-catalog correctness and native-runtime compatibility remain unavailable.
The tool reports retain raw outputs and exact Python runner/helper/fixture
source hashes separately from executable/package pins. The final reports' source
hashes were checked against the frozen tooling files. Documentation and native
ledger files are outside those runner-source hash sets.

## Current-game consumer smokes

On 2026-10-05 UTC, three existing opt-in tests ran at `2cbc99f` in the isolated
CI worktree. Its converter and workspace dependency sources match this branch
and observed main `5579f007`. The supplied order was `Skyrim.esm`, `Update.esm`,
`Dawnguard.esm`, `HearthFires.esm`, `Dragonborn.esm`; active load order remains
unresolved. All five plugin hashes/sizes (363,401,747 bytes total) and the
plugin-list digest matched before and after the tests.

Each command ran exactly one test and exited zero, using the same Rust/kache
settings as the CI regression proof below, with a task-local temporary directory:

```sh
cargo test -p converter --lib esm::exporter::tests::lights_of_the_real_plugin_decode_through_export -- --ignored --exact --nocapture
cargo test -p converter --test plugin_references full_local_load_order_merges -- --ignored --exact --nocapture
cargo test -p converter --lib esm::cell_cache::tests::local_load_order_terrain_cache_passes_validation -- --ignored --exact --nocapture
```

| Check | Observation and scope |
| --- | --- |
| Selected SQLite fields | Parsed 435 `LIGH` records with 48-byte `DATA`; 10,810/12,148 light references had `XRDS`. Assertions export four selected records through in-memory SQLite. |
| Base-master merge | Five plugins produced 1,168,387 effective records; the test also requires a `GRAS` record. |
| Terrain cache | Validated 52,181 terrain cells from five plugins in a temporary cache. |

The merge warned about `Skyrim.esm` GMST `0123C00E` and `Dawnguard.esm` ACTI
`0307B5B9`: each names a master index past its plugin's declared list. The
existing converter treats them as plugin-owned. These observations do not
independently validate that identity policy; native resolution remains open.

The test-profile timings include build/test overhead and are not comparable
release-mode cold/warm time or RSS measurements. The tests remove temporary
outputs and do not retain a complete on-disk SQL/cache baseline. Exact argv,
exit codes, counts and log hashes are in the local run summary and artifact
manifest below. Stat-checked input reads do not create an immutable snapshot.

## Local artifacts

Artifacts remain local; game bytes, table payloads and proprietary decompiled
source are not committed. Paths below are relative to
`/home/dev/.cache/mudcrab-schema-p0-next-evidence/`.

| File | Bytes / SHA-256 |
| --- | --- |
| `corpus.json` | 300,001; `eda27707f636fbe7793054d58f2b63be57ff99d81b9957feca22f6d31899bc66` |
| `bsarch/observations.json` | 165,369; `5c5167a2ceb77030720c9a575e8aa458c82631c4c47a4bdd973c66daad106e3d` |
| `mutagen-252bbca/qualification.json` | 7,632; `d1d85546aaf901613d1fbaeb94e66429e780e2f3b21552da762792ee1c974501` |
| `xedit-252bbca/probe-results.json` | 19,005; `3c242dd51597c9b648807ceb480dcae740a93045c8d20269f01e1ccb8c3523b2` |
| `ci-fix/focused-validation.log` | 8,315; `556abe83364bfe0a92eeb38d87386d306e4ee4edc7c7e882f4649e7c5ce3224a` |
| `consumer-smokes/inputs.after.json` | 1,596; `11aa3c165a39591cd716012d238124d73527250fdf14c8b68a9b7bd69ce340d9` |
| `consumer-smokes/run-2026-10-05T00-48-05Z/summary.json` | 6,746; `666c6949bc7e51e317c8f1a8a8619970c3165f0be29685a712396737070bfaf6` |
| `consumer-smokes/run-2026-10-05T00-48-05Z/artifact_manifest.json` | 2,822; `448190823390615511cc943dfd2bc9f742865fa5216fc699608013c1506db4d7` |

The [corpus notes](p0-corpus-manifest.md) reproduce the manifest scan; the tool
README reproduces both synthetic probes. BSArch listing uses the copied binary
with an isolated Wine prefix: `wine BSArch64.exe 'Z:\path\to\archive.bsa' -list`.
Its raw local lists and partial/final JSON were retained; this is a name
observation, not a qualified localization adapter.

## Pilot ledger and remaining gates

The [field/native ledger](native-field-ledger.md) examines selected `REFR`,
`CELL` and `STAT` fields/variants, current SQL/identity/cache source behavior,
and static native-loader dispatch. Its generator compares exact converter
source/function hashes between task base `97ddf69` and observed main `5579f007`.
These selected files match. The ledger retains disputes about flag widths,
length/version branches, defaults and unresolved runtime consumers. It covers
three source-catalog signatures; it is not field-complete for any record type.

P0 still requires accepted official-content, locale/load-order and corpus pins;
string-table payload/resolution evidence; full signature/variant/field/range
coverage; qualified independent validators; and current-game SQL/cache/runtime
output plus comparable cold/warm plugin time/RSS baselines. Static source hashes
are not an executed output baseline. The parent CI failure was an engine-only
script-option audit treating schema argparse/.NET flags as engine options;
its narrow regression fix at `2cbc99f` is tracked with SPEC V105/B62 and merged
into this branch. Local verification passed 37 configuration tests and three
CLI tests, the doctest command exited successfully with zero doctests, and
formatting passed. The configuration audit also passed with the stacked
tool-option set. These checks used `devenv shell`, Rust/Cargo `1.98.1`, opt-in
kache, two jobs, `RUSTFLAGS=-C debuginfo=0` and the isolated CI worktree's own
target. The local log is `ci-fix/focused-validation.log`; broad CI and engine
runtime testing remain separate gates.

The source-retention contract stays in [the approved phase plan](../../roadmap/dynamic-schema-initiative.md#immutable-structural-authority):
a converter-owned immutable plugin archive separate from runtime output, staging
and evictable caches; scanning and indexes bound to the retained blob; exact
archive-reopened no-op output. Its implementation and acceptance remain P1 work
after the P0 gate. Earlier SE/VR, LE and other games remain deferred.
