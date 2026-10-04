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

At tooling commit `a386361`, all 62 offline tests passed. New regressions cover
deep evidence JSON, output/evidence aliases, every registered Mudcrab checkout,
failed worktree discovery, protected process temp settings and detached children
holding captured pipes. The detached-pipe regression waited 60.1 seconds before
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
hashes were checked against the frozen tooling files; later documentation and
ledger integration do not change those files.

## Local artifacts

Artifacts remain local; game bytes, table payloads and proprietary decompiled
source are not committed. Paths below are relative to
`/home/dev/.cache/mudcrab-schema-p0-next-evidence/`.

| File | Bytes / SHA-256 |
| --- | --- |
| `corpus.json` | 300,001; `eda27707f636fbe7793054d58f2b63be57ff99d81b9957feca22f6d31899bc66` |
| `bsarch/observations.json` | 165,369; `5c5167a2ceb77030720c9a575e8aa458c82631c4c47a4bdd973c66daad106e3d` |
| `mutagen-a386361/qualification.json` | 7,632; `ac9ab5d07998aeef29addd072d24228c2c11b099a9ac86d7516c8ee98976039a` |
| `xedit-a386361/probe-results.json` | 19,005; `89a0bb300a96d7d7484b67007686486065e6aae7704d7d2aff1bc3c91fddbb6b` |

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
its narrow regression fix is tracked with SPEC V105/B62.

The source-retention contract stays in [the approved phase plan](../../roadmap/dynamic-schema-initiative.md#immutable-structural-authority):
a converter-owned immutable plugin archive separate from runtime output, staging
and evictable caches; scanning and indexes bound to the retained blob; exact
archive-reopened no-op output. Its implementation and acceptance remain P1 work
after the P0 gate. Earlier SE/VR, LE and other games remain deferred.
