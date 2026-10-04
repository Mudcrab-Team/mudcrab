# P0 installed corpus observation

The standard-library tool in [`scripts/schema/corpus_manifest.py`](../../../scripts/schema/corpus_manifest.py) records a versioned JSON manifest outside the game install. Its source digests and TES4 observations come from the same verified file descriptor. Runtime mismatch, missing required input, malformed TES4 metadata, duplicate plugin names, unresolved masters, or detected source drift prevents a successful pin.

The read checks file identity, size, modification time and change time before and after hashing. It is not an immutable snapshot: concurrent same-size rewrites can retain both timestamps on this host. The manifest records the bytes read; immutable source retention remains separate P1 work.

The 2026-10-04 observation targeted Steam Skyrim SE/AE `1.7.104.0`, build `24914197`, from `/home/dev/skyrim/Skyrim Special Edition`. `SkyrimSE.exe` matched the target SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` and expected size `37,910,440` bytes.

The Data scan found 80 plugin files. All 80 had a bounded TES4 header that decoded, the dependency closure had no missing or ambiguous masters, and all five required base masters were present. The ordered `Skyrim.ccc` list had 75 names, all matching Data plugin filenames. That list is recorded as a declared content list; it does not establish active load order, official status, or entitlement.

No loose `.strings`, `.dlstrings`, or `.ilstrings` files were present. The version-2 scan hashes all 93 installed BSA/BA2 archives (20,336,627,877 bytes). No scan issue was reported. Bundled table payloads, active load order, locale, official provenance and an accepted corpus digest set remain unresolved; the verdict stays incomplete. Whole-archive hashes do not inspect entries or establish localized text resolution.

A separate read-only `BSArch64.exe -list` observation enumerated all 93 archive file lists, with 181,654 entries and 2,160 string-table names across 76 archives. This identifies bundled table candidates without extracting game assets or interpreting table bytes. The manifest itself still reports bundled tables uninspected. See [the delivery notes](p0-next-delivery.md) for tool pins, local artifacts and limits.

The optional `--corpus-evidence` contract records hash-bound profile, locale and ordered active/optional plugin declarations. It validates file identity and dependency ordering; it does not independently verify the semantic claims or accept the supplied corpus. [The tool README](../../../scripts/schema/README.md) defines the strict input fields.

The current version-2 local output is `/home/dev/.cache/mudcrab-schema-p0-next-evidence/corpus.json` (300,001 bytes; SHA-256 `eda27707f636fbe7793054d58f2b63be57ff99d81b9957feca22f6d31899bc66`). The prior version-1 observation remains at `/tmp/mudcrab-schema-p0-corpus.json` (297,635 bytes; SHA-256 `dce890a64ac05cd6065275e6c9055589c2ffcdf61b02a99289f3fd8cefc16042`). It contains source names, paths, file hashes/sizes, and TES4 metadata; proprietary file bytes and localization/record dumps remain outside the repository. This observation closes only the installed-file-manifest tooling slice. The broader P0 catalog, validator, archive/localization, and baseline gates remain open.

Reproduce the scan with:

```sh
python3 scripts/schema/corpus_manifest.py \
  --game-root '/home/dev/skyrim/Skyrim Special Edition' \
  --data-dir '/home/dev/skyrim/Skyrim Special Edition/Data' \
  --executable '/home/dev/skyrim/Skyrim Special Edition/SkyrimSE.exe' \
  --output /tmp/mudcrab-schema-p0-corpus-v2.json
```
