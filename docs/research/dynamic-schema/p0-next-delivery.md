# P0 corpus, probe and pilot evidence delivery

This slice hashes installed archive bytes, adds strict candidate-evidence inputs,
probes a pinned xEdit console binary, and refreshes selected converter/native
field evidence. It does not accept P0. SPEC T58/T56 remain in progress, and no
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
Both console observations used Wine `11.0`. Its absolute executable path,
`/nix/store/cirj08cjmc9asn8kflycp2r5mchfyhvy-wine-wow64-11.0/bin/wine`, is a
local-only host detail.
Each archive was rehashed against the manifest before listing, with stat checks
around the observation. This does not create an immutable snapshot or prove
string-table payloads, locale applicability or resolution.

The candidate input contract is documented in [the schema tool README](../../../scripts/schema/README.md).
It checks strict JSON, exact target metadata, artifact hashes and dependency
order. Supplied locale/order/profile claims stay unverified; accepted pins and
provenance cannot be supplied as acceptance booleans. Output aliases and
ancestors of evidence paths are rejected before writes.

## Source and runner refresh at `2ce3977`

The bounded source report now compares task base `97ddf6966310061c859f9eb3db84e279121d23b4`
with main `db3a1dc3ed80795149e3a15a5047f4d313dd3e74`. `exporter.rs` and
`esm/mod.rs` differ; four selected function bodies changed: `create_tables`,
`export_records`, `insert_reference`, and `remap_record_form_ids`. Review branch
`2ce397734fa59c1dd8749928376b439a99edff39` matches main byte-for-byte across all
five inspected converter files. The tracked report is 18,092 bytes with SHA-256
`6da7df0e559d5f2b594d1d3ff1648488e76f79340d7aef85064a30e60d555e0d`; its
generator `pilot_code_evidence.py` at that refresh has SHA-256
`9a3d6bf25df64022daaa1af220e4293084d1181492f0136dc32ec8d36ea58ef9`.
Supplemental branch comparisons and their hashes appear in the refreshed local
artifact table below. These reports cover selected source anchors only.

The external-tool observations below use runner sources from commit
`2ce397734fa59c1dd8749928376b439a99edff39`. The Mutagen runner SHA-256 is
`14bd6541db162b4f97d5e65e4bf92b28987cc39b513c799ad38689950eb8ee7c`; the xEdit
runner SHA-256 is
`d7474f1cc80a043bae2ab860332ae44b1bc9b57f9c9df926b9de9ba4c02c0508`. The
shared source hashes for these attempts are `p0_tools.py` SHA-256
`9cd36680933f20b2167ade2818a190abcea7864ca120d19cd0454eb2cad1288a`,
`corpus_manifest.py` SHA-256
`c4c609a77d1cf56b2d99615293402300fab3506890e90d682277f28a95086ce3`, and
`p0_fixtures.py` SHA-256
`fc442f973609f0baf8cfe8bba6801364e29c6125dc6049f98e7ab30e708a922b`.

The fresh Mutagen attempt used the local-only oracle source
`/home/dev/.t3/projects/mudcrab-reverse-engineering/oracles/records` and local-only
SDK path `/home/dev/mcrab-store/tools/dotnet-sdk-9/bin/dotnet` (pinned to
`9.0.318`).
The runner stopped at its 15-second `dotnet --version` timeout, before restore,
build, or any fixture command. The captured stdout/stderr are empty; the
206-byte failure summary records `pilot_status: failed` and an unavailable
verdict. This is an incomplete environment/tool start, not a Mutagen case result.

The fresh xEdit attempt used the pinned 20,510,720-byte `xDump64.exe`
(`30c085b8a20dc02bf5abae2cb6610870c9bb9eea50330e0fe5ade98e3f89efe6`) and Wine
`11.0`. Its bounded help startup timed out after 60 seconds. The runner returned
exit 2 with `qualified: false`, `completed_verdict_saved: false`, and zero fixture
cases. This is an incomplete probe; no schema or malformed-input observations
were obtained in this refresh.

The earlier completed Mutagen report at `mutagen-252bbca/qualification.json`
was generated by runner SHA-256
`527106847b9086c9f80f11fa29833339c441602983d75dddf7d11ae6b17eb532`, not the
current runner. It passed six synthetic positives, four managed malformed
rejections, and the `placed`/`placed-lo` command smokes with zero build
warnings/errors. Its localized string ID, optional typed ID, and unavailable
translated text remain distinct. The earlier xEdit report at
`xedit-252bbca/probe-results.json` was generated by runner SHA-256
`1dd3789789c8d13e6e301c766bb93de9b916e13642d43e8ecbf16ba4ce57565f`; it
recorded ten cases and remained unqualified with exit 2. These are historical
runner results. They do not replace the incomplete attempts against the current
runner sources at `2ce3977`.

The later Windows repair changes the Python sources below. The source report
still matches its generator and retains the same 18,092-byte hash; lifetime and
character-literal parsing regressions do not change the selected source spans.
All five inspected converter files still match main `db3a1dc`. These source pins
identify the repair code only. No external-tool qualification was rerun against
them, so the startup attempts and earlier successful reports above retain their
original commits and hashes.

| Repair source | SHA-256 |
| --- | --- |
| `pilot_code_evidence.py` | `f718cabc93c96800306d212126cc28aebbf38a07fbaff93b05d732f93fdd7322` |
| `run_mutagen_p0.py` | `8b0c0e9d39f77258f4d32c698b20fdf8b6cf3bee5aef3c9d25cf0f389e0d557a` |
| `run_xedit_p0.py` | `796de43b47653f2c0fe7baef1e9e7f5d226cc1058bb44a726b1b2b2e40625fb1` |
| `p0_tools.py` | `e2b6eba29a28083e1cd38ef0e66a51d793b495e0967963af08a6f1b94953b2fc` |
| `corpus_manifest.py` | `3746e7c9025be97ef6da0e3606363d08421496e15643ea829f69336fad8494f6` |
| `p0_fixtures.py` | `fc442f973609f0baf8cfe8bba6801364e29c6125dc6049f98e7ab30e708a922b` |

The parent repair passed 45 schema tests and two extractor tests on Linux. The
next repair passed 89 schema tests and two extractor tests. They cover native
path fixtures, canonical report keys, UTF-8 artifact writes, bounded descendant
cleanup and source-span lifetimes. Mutagen and xEdit decode failures preserve
raw stream artifacts, hashes and explicit unavailable text; the Mutagen cases
cover checked, fixture, legacy and SDK-startup commands. Native Windows CI
remains required; these
offline checks do not accept P0.

At tooling commit `252bbca`, all 64 offline tests passed. Regressions there cover
deep evidence JSON, output/evidence aliases, every registered Mudcrab checkout,
failed worktree discovery, custom SDK/Wine directories, protected process temp
settings and detached children holding captured pipes. The detached-pipe
regression waited 60.1 seconds before its fix; final output collection now has a
deadline. An escaped descendant can survive signaling of the original process
group, but cannot keep the verdict path waiting indefinitely. These tests are
historical to that tooling commit.

Physical offsets, opaque payload completeness, complete source occurrences,
full-catalog correctness, qualified independent validators and native-runtime
compatibility remain unavailable.

## Historical current-game consumer smokes

On 2026-10-05 UTC, three existing opt-in tests ran at `2cbc99f` in the isolated
CI worktree against the then-observed main `5579f007`. They are historical for
this refresh: current main `db3a1dc` and review branch `2ce3977` add reference
header-flag/XESP projection and remapping changes absent from that smoke run.
The supplied order was `Skyrim.esm`, `Update.esm`, `Dawnguard.esm`,
`HearthFires.esm`, `Dragonborn.esm`; active load order remains unresolved. All
five plugin hashes/sizes (363,401,747 bytes total) and the plugin-list digest
matched before and after those tests.

Each historical command ran exactly one test and exited zero, using the same
Rust/kache settings as the CI regression proof below, with a task-local temporary
directory. This evidence refresh did not run Rust builds or converter smokes:

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

The historical merge warned about `Skyrim.esm` GMST `0123C00E` and `Dawnguard.esm` ACTI
`0307B5B9`: each names a master index past its plugin's declared list. The
existing converter treats them as plugin-owned. These observations do not
independently validate that identity policy; native resolution remains open.

The historical test-profile timings include build/test overhead and are not comparable
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

The refresh artifacts below are also local-only. Paths are relative to
`/home/dev/.cache/mudcrab-schema-p0-review-evidence/run-2ce3977-20261005T063537Z/`.

| File | Bytes / SHA-256 | Result scope |
| --- | --- | --- |
| `source/task-base-to-review-branch.json` | 18,092; `983ee5fcae2df7b0e2141a356c6023d293cacf596c707fab6f824782b0d26ec5` | Task base `97ddf69` vs review branch `2ce3977`; two source files differ, four selected function bodies differ. |
| `source/main-to-review-branch.json` | 18,086; `6fbd8772a4559430bfdf95540b5dfcd494898bfe828141f6092cd773d3c6dd56` | Main `db3a1dc` vs review branch `2ce3977`; all five inspected converter files match. |
| `mutagen/qualification.json` | 206; `b37ba51f8dc83c4745beee0a3def04ce41d4c7adaa46cfc54c00358d4301759f` | Failure summary; 15-second SDK version timeout before restore/build/cases. |
| `mutagen/logs/dotnet-version.timeout.json` | 64; `777223f72a08a3954812ff02908d8b8d4ce5ebfa1b6764aaeacf631034daf7c0` | Bounded timeout metadata; no completed verdict. |
| `xedit/probe-results.json` | 1,783; `6daea1749717dbebdc9fc75915ceafa8aa43145907aacba225190da9fc0cf24d` | Help startup timed out at 60 seconds; exit 2, unqualified, zero fixture cases. |
| `refresh-summary.json` | 8,240; `c61af98a4081f4e3806d9f56773aa60bcfabc1d1a956f951c68e61c2384078e0` | Source report, current runner/source pins, fresh incomplete attempts, and historical proof metadata. |

The [corpus notes](p0-corpus-manifest.md) reproduce the manifest scan; the tool
README reproduces both synthetic probes. BSArch listing uses the copied binary
with an isolated Wine prefix: `wine BSArch64.exe 'Z:\path\to\archive.bsa' -list`.
Its raw local lists and partial/final JSON were retained; this is a name
observation, not a qualified localization adapter.

## Pilot ledger and remaining gates

The [field/native ledger](native-field-ledger.md) examines selected `REFR`,
`CELL` and `STAT` fields/variants, current SQL/identity/cache source behavior,
and static native-loader dispatch. Its tracked report compares task base
`97ddf696` with main `db3a1dc3`; two selected files and four selected function
bodies differ. Review branch `2ce3977` matches main across all five inspected
converter files. The current main/branch projection stores raw reference header
flags and selected XESP bytes, and remaps XESP parent IDs for five placed-record
types. The ledger retains disputes about flag widths, length/version branches,
defaults and unresolved runtime consumers. It covers three source-catalog
signatures; it is not field-complete for any record type.

P0 still requires accepted official-content, locale/load-order and corpus pins;
string-table payload/resolution evidence; full signature/variant/field/range
coverage; qualified independent validators; and current-game SQL/cache/runtime
output plus comparable cold/warm plugin time/RSS baselines. Static source hashes
are not an executed output baseline. The parent CI failure was an engine-only
script-option audit treating schema argparse/.NET flags as engine options;
its narrow regression fix at `2cbc99f` is tracked with SPEC V149/B97 and merged
into this branch. The historical local verification for that patch passed 37
configuration tests and three CLI tests; doctest exited successfully with zero
doctests, formatting passed, and the configuration audit passed with the
stacked tool-option set. Those checks used `devenv shell`, Rust/Cargo `1.98.1`,
opt-in kache, two jobs, `RUSTFLAGS=-C debuginfo=0` and the isolated CI
worktree's own target. The local log is `ci-fix/focused-validation.log`;
broad CI and engine runtime testing remain separate gates. The 2cbc99 current-game
smokes are historical and do not provide a current-main SQL/cache/output or
timing baseline.

The source-retention contract stays in [the approved phase plan](../../roadmap/dynamic-schema-initiative.md#immutable-structural-authority):
a converter-owned immutable plugin archive separate from runtime output, staging
and evictable caches; scanning and indexes bound to the retained blob; exact
archive-reopened no-op output. Its implementation and acceptance remain P1 work
after the P0 gate. Earlier SE/VR, LE and other games remain deferred.

The next-layer Rust CI run `37283916889` at `3853f91` passed 1,072 of 1,073 tests. Its engine utility audit rejected a source-evidence unit-test fixture containing `--repo`. The tests now sit beside their responsible script in this directory, which the existing portable CI discovery already covers. The strict engine audit and parser are unchanged. Local verification passed 82 schema tests and nine research-tool tests, preserving the prior total of 91; the source report check also matched. A local Cargo attempt stalled while loading cached/toolchain files and stopped before tests ran. Fresh full Rust CI remains a separate gate.
