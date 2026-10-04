# P0 installed corpus observation

The standard-library tool in [`scripts/schema/corpus_manifest.py`](../../../scripts/schema/corpus_manifest.py) records a versioned JSON manifest outside the game install. Its source digests and TES4 observations come from the same verified file descriptor. Runtime mismatch, missing required input, malformed TES4 metadata, duplicate plugin names, unresolved masters, or source drift prevents a successful pin.

The 2026-10-04 observation targeted Steam Skyrim SE/AE `1.7.104.0`, build `24914197`, from `/home/dev/skyrim/Skyrim Special Edition`. `SkyrimSE.exe` matched the target SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` and expected size `37,910,440` bytes.

The Data scan found 80 plugin files. All 80 had a bounded TES4 header that decoded, the dependency closure had no missing or ambiguous masters, and all five required base masters were present. The ordered `Skyrim.ccc` list had 75 names, all matching Data plugin filenames. That list is recorded as a declared content list; it does not establish active load order, official status, or entitlement.

No loose `.strings`, `.dlstrings`, or `.ilstrings` files were present. The tool observed 93 BSA/BA2 filenames and sizes without hashing archive contents. Bundled string tables, active load order, and locale therefore remain unresolved. Plugin and loose-table hashes are first observations; no prior accepted corpus digest set was supplied. These gaps block `complete` and `successful_pin` even when the runtime pin and observed file metadata validate.

The full local output is `/tmp/mudcrab-schema-p0-corpus.json` (297,635 bytes; SHA-256 `dce890a64ac05cd6065275e6c9055589c2ffcdf61b02a99289f3fd8cefc16042`). It contains source names, paths, file hashes/sizes, and TES4 metadata; proprietary file bytes and localization/record dumps remain outside the repository. This observation closes only the installed-file-manifest tooling slice. The broader P0 catalog, validator, archive/localization, and baseline gates remain open.

Reproduce the scan with:

```sh
python3 scripts/schema/corpus_manifest.py \
  --game-root '/home/dev/skyrim/Skyrim Special Edition' \
  --data-dir '/home/dev/skyrim/Skyrim Special Edition/Data' \
  --executable '/home/dev/skyrim/Skyrim Special Edition/SkyrimSE.exe' \
  --output /tmp/mudcrab-schema-p0-corpus.json
```
