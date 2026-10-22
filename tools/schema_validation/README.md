# Standalone schema evidence tools

These tools preserve the reusable validation work from [#161](https://github.com/Mudcrab-Team/mudcrab/pull/161)
and [#163](https://github.com/Mudcrab-Team/mudcrab/pull/163) after the
[old schema execution plan was retired](https://github.com/Mudcrab-Team/mudcrab/issues/156#issuecomment-6094962291).
The retained source comes from #163 at
[`ff066139ff648556f67be4e606601b0b6c266946`](https://github.com/Mudcrab-Team/mudcrab/tree/ff066139ff648556f67be4e606601b0b6c266946/scripts/schema).
The original fixture bytes, external-tool pins and wire/report formats remain intact.
Paths and runner provenance now refer to this directory; artifact protection uses
configured roots instead of another workstation's `/home/dev` paths.

[#204](https://github.com/Mudcrab-Team/mudcrab/pull/204) owns the active in-house
reader and its record comparisons. This directory supplies optional input
inventory and synthetic external-tool observations. It does not change the
reader, duplicate its record-family schemas, or require #204 to merge first.
The retired SPEC/roadmap plan, mined declaration inventory and source-dependent
explorer are not part of this delivery. Their historical source remains on the
old PR branches.

## Offline checks

Python 3.11 or newer and the standard library suffice:

```sh
python3 -B -m unittest discover -s tools/schema_validation -p 'test_*.py' -v
```

CI runs the suite on Linux/Python 3.11, macOS/Python 3.11 and Windows/Python 3.13.
The `schema_validation` matrix feeds the required `tests_pass` gate. These tests
use synthetic inputs and mocked external-tool results; they do not execute
Mutagen, xEdit, Wine, the converter or the game. POSIX process-group checks skip
on Windows, where the mocked cleanup tests do not prove real process-tree cleanup.
Historical `p0` filenames and test identifiers identify the original pilot,
not an approved schema milestone.

## Corpus inventory

```sh
python3 tools/schema_validation/corpus_manifest.py \
  --game-root /path/to/Skyrim \
  --data-dir /path/to/Skyrim/Data \
  --executable /path/to/Skyrim/SkyrimSE.exe \
  --output /path/outside/game/corpus-manifest.json
```

The retained version-3 manifest hashes observed plugin, string-table and archive
bytes, reads TES4 metadata, and records runtime identity and missing inputs.
Its fixed reference profile is the original Steam `1.7.104.0`, build `24914197`
with executable SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
This is a historical comparison pin, not a claim about the newest release or
the required runtime for #204. A different executable is recorded as a mismatch.
The tool returns status 2 for incomplete evidence and writes the candidate
manifest when it can; a manifest never certifies corpus or schema acceptance.

Optional `--corpus-evidence` accepts version-1 JSON with `schema_version`,
`corpus_profile`, `locale` and `load_order`. The profile has `id`, `target` and
`evidence`; locale has `value` and `evidence`; load order has `active_plugins`,
`unloaded_optional_plugins` and `evidence`. Every evidence item names a `path`
and lowercase `sha256`; relative paths resolve beside the descriptor. The
target must match the historical game/version/build above. Every installed
plugin must be classified and active masters must precede dependents. Matching
hashes verify supplied artifact bytes, not the truth of a locale or profile claim.

Use a stable source tree. The reader rejects observed directory symlinks and
final-component symlinks, but does not pin ancestor directories against later
replacement. Hashes identify the bytes actually read; paths do not prove
unchanged ancestry. This limit remains explicit in the manifest.

## External-tool probes

Both probes require caller-supplied tools. Set `MCRAB_RE_PROJECT_ROOT` and
`MCRAB_STORE` to absolute paths when those directories need protection, or pass
repeated `--protected-root /path/to/source-or-store` options. Every registered
Mudcrab checkout, the supplied tool/oracle source, and the resolved SDK/Wine
directory are protected automatically. Configured roots, descendants, ancestors
and symlink aliases are rejected as artifact destinations before artifact writes.
Unset environment variables add no guessed host-specific roots; empty or
relative configured environment values fail closed.

The Mutagen pilot needs .NET SDK `9.0.318`, Mutagen `0.54.4` and a separately
supplied oracle source matching the hashes in `run_mutagen_p0.py`. That oracle is
not included here, so its execution cannot be reproduced from this repository
alone. The command restores dependencies in an isolated artifact directory.

```sh
python3 tools/schema_validation/run_mutagen_p0.py \
  --dotnet /path/to/dotnet \
  --oracle-source /path/to/oracles/records \
  --protected-root /path/to/game-store \
  --artifact-dir /path/to/new-mutagen-artifacts
```

The xEdit capability probe uses caller-supplied Wine and the pinned
`xedit-4.1.5f` `xDump64.exe` (20,510,720 bytes, SHA-256
`30c085b8a20dc02bf5abae2cb6610870c9bb9eea50330e0fe5ade98e3f89efe6`).
It downloads no binary and opens only generated synthetic plugins.

```sh
python3 tools/schema_validation/run_xedit_p0.py \
  --xdump /path/to/xDump64.exe --wine /path/to/wine \
  --protected-root /path/to/game-store \
  --artifact-dir /path/to/new-xedit-artifacts
```

Both runners retain invocation, input/runner hashes, partial output, timeout
and cleanup diagnostics. An escaped descendant can survive process-group
signals; inherited pipes cannot keep the timeout verdict waiting indefinitely.
The xEdit probe remains deliberately unqualified (status 2). A completed dump,
typed traversal or matching synthetic value proves neither full source
occurrence coverage nor independent full-corpus, native-runtime, visual,
performance or lossless archive acceptance.
