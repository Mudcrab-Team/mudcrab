# Formats: spud runtime caches and readers

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Spud `src/formats` and `docs/formats`: chunk containers, forms database, converted mesh and world caches, inspection tool.

## Summary
- Spud reads Skyrim assets and plugins, then writes versioned runtime caches with a shared chunk container.
- Measured scope: `src/formats` contains 55 files and 10,413 C++ lines; `docs/formats` has 24 specifications.
- The format layer is broad and active: typed readers and writers exist for most documented cache kinds.
- Maturity is mixed: code exists for the main formats, but test coverage was not found under format-specific test names.
- Five key features: bounded binary readers; versioned chunk containers; plugin-form database; converted mesh and world caches; detailed inspection CLI.
- The forms database memory maps its input and keeps views into mapped bytes; most other readers materialize vectors.
- Spud assumes a little-endian host, as enforced by `core/binary.hpp`.
- Largest liberty: the runtime consumes Spud's own converted formats instead of Skyrim's source formats.
- Other major gaps: some data is reduced to runtime-specific representations; native endian assumptions limit portability.
- Format documents are not always synchronized with header constants, including `.osm`, `.osr`, `.osw`, and `.osv`.
- Spud is MIT and Mudcrab is GPL-3.0-or-later; borrow techniques and concepts, not code.

## Feature map

| id | feature | what vanilla Skyrim does (1 line) | how spud does it (approach, algorithm, key types) | where (file:line, symbols) | evidence | liberties | worth borrowing |
|---|---|---|---|---|---|---|---|
| formats-container | Shared cache envelope | No corresponding game file; Bethesda source assets use their own formats. | `SPUD` header, kind FourCC, version, chunk count; 16-byte chunk headers, payload size, alignment padding, UTF-8 offset string table. | [`src/formats/container.hpp:20-109`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/container.hpp#L20-L109), `container.cpp`; `docs/formats/container.md` | impl | Invented envelope; internal only. | yes — bounded chunks and string-offset tables. |
| formats-binary | Binary read/write and byte order | Source formats use their documented field encodings. | Bounds-checked `BinaryReader`, growable `BinaryWriter`, POD memcpy; build-time assertion requires little-endian host. | [`src/core/binary.hpp:16-127`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/core/binary.hpp#L16-L127) | impl | Host-endian POD writes make cache portability dependent on little-endian. | maybe — bounds checks; make endian conversion explicit. |
| formats-forms | ESM/ESP/ESL form database | Skyrim loads ordered plugins and resolves overrides by FormID. | Importer parses plugin records into `.osf` FORM v3: metadata, schema-described fields, versions, winner, deletion state, plugin XXH64; mapped read-only database. | [`src/formats/forms.hpp:26-179`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/forms.hpp#L26-L179), [`forms.cpp:27-58`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/forms.cpp#L27-L58); `docs/formats/forms.md` | impl | Stores normalized indexed records instead of raw plugin stream; internal only. | yes — keep per-plugin versions and field schema for diagnostics. |
| formats-dds | DDS/TEX images | Skyrim loads DDS texture payloads and mipmaps. | DDS reader/writer model handles headers, formats, mip levels and image bytes for conversion/cache. | [`src/formats/dds.hpp:1-73`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/dds.hpp#L1-L73), `dds.cpp`; importer texture path | impl | Texture conversion/cache representation can normalize source details; specific cases need per-path audit. | maybe — precise format preservation and conversion metadata. |
| formats-native-family | Native cache files | Skyrim reads Bethesda assets and saves; these cache files are Spud-specific. | Per-kind typed serializers listed in Data and file formats. Most use the shared container, explicit kind/version, chunk arrays, and string tables. | `src/formats/*.hpp,*.cpp`; `docs/formats/*.md` | impl | New runtime data model replaces source structures; many formats accept only selected versions. | yes — explicit per-file versioning and documented chunk contracts. |
| formats-skytool | Format inspection and conversion CLI | Skyrim's launcher and Creation Engine do not expose this audit CLI. | Registered subcommands inspect archives, plugins, meshes, scripts, UI, audio, behavior and converted cache. | [`apps/skytool/main.cpp:25-41`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/apps/skytool/main.cpp#L25-L41); [`commands.hpp:40-60`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/apps/skytool/commands.hpp#L40-L60) | impl | Tool behavior is author-defined; it does not prove game parity. | yes — expose deterministic source inspection and import diagnostics. |

## Data and file formats

### Skyrim inputs read

The formats area is the cache serialization layer. Source readers live under `src/importer/legacy` and `src/importer/convert`; the following are confirmed by those source registrations and format references. Support means a parser or conversion path exists, not full compatibility with every variant.

| source format | use and implementation | evidence / limits | Mudcrab cross-reference |
|---|---|---|---|
| BSA / BA2 | Archive source via indexed BSA listing/extraction and `DataSource`; compressed payloads are decompressed before asset parsing. | `src/importer/legacy/bsa.cpp/.hpp`; `apps/skytool/cmd_bsa.cpp`; v104/v105 details need a separate importer audit. | `crates/converter` BSA readers feed the converted pack; `crates/dummy-content/src/bsa.rs` generates fixtures. |
| ESM / ESP / ESL | Plugin records and overrides feed `.osf`, typed record access and importer conversion. | `src/importer/legacy/esm.cpp/.hpp`; `formats/forms*`; light/master flags in `forms.hpp`. | Mudcrab converter reads plugins into SQLite `skyrim_world.db` and rkyv caches; see `docs/guides/converter.md`. |
| NIF | Mesh, node, collision, animation, scene and UI assets are parsed into converted runtime types. | `src/importer/legacy/nif.cpp/.hpp`; converter `*_import.cpp`; parser coverage varies by block type. | NIF-to-glTF pipeline and extras: `docs/specs/converters/nif-to-gltf.md`; NIF reader lives in `crates/converter`. |
| DDS | Textures, cubemaps and mip chains feed render assets and generated far textures. | `src/formats/dds.cpp/.hpp`; source files are also named in importer/docs. | Converted packs contain KTX2 textures, documented in `docs/guides/converter.md`. |
| HKX / Havok packfiles | Animation, behavior, skeleton and physics source data are decoded through registered Havok classes. | `src/importer/convert/hkx_import.cpp/.hpp`, `hkx_classes.inc`; not a general-purpose HKX implementation. | `crates/converter/src/animation/packfile.rs` and animation conversion modules. |
| STRINGS / DLSTRINGS / ILSTRINGS | Localized plugin strings are resolved to UTF-8 for records and UI conversion. | `src/importer/legacy/strings.cpp/.hpp`; `src/importer/convert/ui_strings.*`; docs `formats/forms.md`, `osu.md`. | Converter has localized string handling and plugin caches; see `crates/converter` record/string modules. |
| SEQ | Quest start / sequence metadata is read with plugin-related conversion. | Mentioned in importer paths; exact parser and all consumers not established in this pass. | No direct one-file equivalent established; Mudcrab plugin/quest conversion resides in `crates/converter`. |
| SWF | Interface movies become `.osu` UI movie data and related images / strings. | `src/importer/legacy/swf.cpp/.hpp`, `convert/ui_import.cpp`, `formats/osu.*`; supported tags/actions need separate audit. | Mudcrab has no SWF runtime equivalent found; its native UI is Rust/Bevy. |
| PEX | Compiled Papyrus and record VMAD references become `.oss` script tables. | `src/importer/convert/script_import.*`; `docs/formats/oss.md`; do not infer full VM compatibility. | Mudcrab has Luau script conversion/runtime paths in `crates/converter` and `crates/engine`. |
| WAV / xWMA / FUZ / LIP | WAV retained; xWMA or FUZ audio becomes Ogg; FUZ and loose LIP become `.osv`. | `docs/formats/audio.md`; `src/importer/convert/audio_codec.*`, `audio_import.*`; Media Foundation decoder is platform-specific. | Mudcrab's sound assets are packed in converted assets; no same `.osv` format found. |
| TEX / other | No separate Bethesda `.TEX` parser was found in the inspected registration. Other asset classes enter through generic `DataSource` or specific importer modules. | Unverified beyond listed handlers; inspect all `legacy` readers to close coverage. | Mudcrab converter supports the documented source asset subset; consult `docs/specs/converters/data-formats.md`. |

### Spud-native written formats

Each row gives the current header kind/version. All are Spud formats, not Skyrim-native files. The common container uses little-endian fields and chunks; payload details and compression vary. Header constants were checked in source, while format docs may report older versions.

| file / kind / version | stores and purpose; layout and notable encoding | versioning, compression, mapping, hashing | Mudcrab cross-reference |
|---|---|---|---|
| `.osf` / FORM / 3 | Indexed plugin load-order database: plugins, form metadata, schema fields, values, versions and winning record. | Chunked; mapped read; plugin content hash is XXH64. | SQLite `skyrim_world.db` and rkyv plugin/cell caches; `crates/converter/src/database_cache.rs`. |
| `.oda` / NANM / 2 | Node animation tracks and sampled or keyed node transforms. | Container chunks, explicit version; see `oda.md`. | Converted animation assets; glTF animation and animation packfile pipeline. |
| `.ofm` / FMOR / 1 | Face morph vertex deltas, morph-major `i16` vec4, LZ4 compressed payload. | Header retains raw byte size; chunked. | glTF morph targets where supported; `crates/converter/src/animation/glb.rs`. |
| `.okc` / KCAM / 1 | Kill-camera model/camera animation data. | Container; source camera keys are sampled for runtime. | No dedicated camera cache found; converted model path uses glTF. |
| `.osa` / ATMO / 4 | Weather, atmosphere, sky layers and lighting-related records. | Container; versioned typed chunks/string offsets. | World database atmosphere/weather tables and engine world assets. |
| `.osb` / BEHV / 1 | Behavior graph objects and typed values. | Container chunks and string offsets; inspectable with `skytool osb`. | Mudcrab animation behavior support lives in converter/engine animation crates; no format equivalence asserted. |
| `.osd` / SNDB / 1 | Sound descriptors, categories, music, file path tables and related metadata. | Container; filenames are offsets; audio payloads stored as separate files. | Converted pack sound assets and world DB; `docs/guides/converter.md`. |
| `.ose` / PSYS / 6 | Particle effects, scene graph, emitters, sequences, text keys and texture paths. | Versions 1–6 accepted; typed chunks; source parser has compatibility branches. | Bevy particle/effect conversion in `crates/converter` and `crates/engine`. |
| `.osg` / GWLD / 1 | Game-world metadata and forms used by runtime world loading. | Container, strict current-version reader. | SQLite world database and world asset tables. |
| `.osh` / FARV / 1 | Far view regions, landscape maps, tree impostors and proxy references. | Container; DDS payloads and `.osm` proxies are side files. | LOD chunks, textures and world DB; `docs/adr/0010-lod-chunk-payload-format.md`. |
| `.osk` / SKEL / 2 | Skeleton hierarchy and bind/rest data. | Container, typed arrays; strict current version. | Skeleton and skin data from `crates/converter` glTF path. |
| `.osl` / LSCR / 1 | Loading screens and related presentation tables. | Container, string offsets; strict version. | No matching loading-screen binary found; Bevy UI is custom. |
| `.osm` / MESH / 11 | Render mesh shapes, materials, vertex/index payloads and skinned shape data. | Chunked; current reader accepts versions 9–11; packed tangent frames; version 11 stores shape names. | glTF 2.0 output plus converted pack mesh assets; `docs/specs/converters/nif-to-gltf.md`. |
| `.osn` / CLIP / 1 or 2 | Animation clips; v1 spline blocks, v2 raw clip data. | Header exposes two current encodings; see `osn.hpp`. | Animation packfile / glTF animation conversion; no binary compatibility. |
| `.osp` / PHYS / 2 | Collision shapes tied to render meshes. | Reads versions 1 and 2; v2 adds rank; payload structure is typed. | Rapier collision extras in GLB; `docs/specs/converters/nif-to-gltf.md`. |
| `.osr` / RULE / 6 | Runtime rules, collision layers, camera, stairs and controller parameters. | Reader accepts versions 1–6; docs/header change log differs. | Mudcrab record-driven behavior and engine settings; not a direct format match. |
| `.oss` / SCRP / 2 | Compiled script tables, properties, attachments, fragments and Papyrus source associations. | Versioned chunked data and strings. | Luau scripts in converted assets, `crates/engine` script runtime. |
| `.ost` / SAVE / 3 | Spud save-state data, plugin hashes and save sections. | Container; stores 64-bit plugin hashes; optional section read. | No direct save-file equivalent established in this area. |
| `.osu` / UIMV, UITX, UIFM, UICT / 2 | Converted UI movie timeline, strings, font maps and control maps. | Shared version; tagged chunks, string offsets and UI-specific records. | Mudcrab Bevy UI has no SWF cache equivalent. |
| `.osv` / LIPS / 2 | Lip-sync frames, raw floats or LZ4 block compression. | Code comment says v2 retains v1 layout after rename; inspect format doc discrepancy. | Converted voice assets have no identified `.osv` twin. |
| `.osw` / WRLD / 16 | Compiled worldspace/cell data, landscape, references, water, flow maps and related runtime tables. | Large versioned format; typed chunks; source/header version 16. | SQLite world DB, cell cache and LOD in Mudcrab. |
| `.ots` / TSWP / 2 | Texture swaps attached to shape/material data. | Container, per-shape string offsets; strict current version. | Converted material and texture references in glTF/pack. |
| DDS output | Generated far maps/impostors and converted image payloads. | BC1/BC3/BC5 according to use; DDS writer in `dds.cpp`. | Mudcrab writes KTX2 textures in registered packs. |

Container details: `FileHeader` is 16 bytes: magic, kind, version and chunk count. `ChunkHeader` is 16 bytes: id, reserved word and 64-bit payload size. The writer pads chunks; the reader checks magic/kind and exposes spans. The string table stores deduplicated NUL-terminated UTF-8 indexed by byte offset. See [`src/formats/container.hpp:20-109`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/container.hpp#L20-L109) and `docs/formats/container.md`.

## Cross-area dependencies

- Formats depend on core binary I/O, file mapping, errors, hashing and path handling.
- Importer readers produce typed source models, then writers persist these cache formats.
- Runtime consumers include engine, renderer, animation, physics, audio, UI and world systems.
- `forms::Database` exposes typed lookups and retained record versions to importer and tools.
- `skytool` links format and importer code for direct inspection, conversion and diagnostics.
- `core` provides logging, platform paths, file I/O, mapping, hashing, random, sampling, units and parallel jobs.
- `core/log.hpp` and `log.cpp` expose severity logging and configurable sinks; `parallel.hpp/.cpp` provide job dispatch; `hash.hpp/.cpp` implement xxHash64 and file hashing; `file.hpp/.cpp` own mapped-file lifecycle. Math is mostly external GLM use in importer/runtime, not a dedicated `core/math` module.

## Tests, tooling and gates

- No format-specific test files were found by a filename search; this does not prove there are no format assertions in broader importer or engine tests.
- `skytool` commands are registered in [`apps/skytool/main.cpp:25-41`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/apps/skytool/main.cpp#L25-L41): `bsa`, `esm`, `nif`, `audio`, `import`, `osb`, `conditions`, `far`, `records`, `strings`, `mo2`, `script`, `ui`, and `fx`.
- `bsa`: list, extract, and find archive entries (`cmd_bsa.cpp`).
- `esm`: inspect plugin headers, records, winners, fields and child groups (`cmd_esm.cpp`). `records`: check/import/show/list/stats/dump the forms database; `strings`: inspect localized tables (`cmd_records.cpp`).
- `nif`: inspect blocks, shapes, textures, transforms and related asset structure (`cmd_nif.cpp`).
- `audio`: decode/encode single audio files, inspect lip data and RNG details (`cmd_audio.cpp`).
- `import` and `far`: convert install assets or generate far views (`cmd_import.cpp`).
- `osb`, `script`, `ui`, and `fx`: behavior dump, Papyrus import, UI convert/dump/check/import, and effect convert/dump/sim/check.
- `mo2`: inspect instances, profiles and virtualized files; global options support `--game`, `--cache`, `--mo2`, `--profile` (`commands.hpp`, `apps/skytool/main.cpp`).
- No build or command execution was performed. This is a read-only source review.

## Liberties and deviations

1. **High — Skyrim does not use Spud's cache files.** All `.os*` formats and the `SPUD` envelope are new, so import is a semantic translation layer. Evidence: [`container.hpp:20-42`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/container.hpp#L20-L42) and each `formats/*.hpp`; affects gameplay correctness if conversion omits source semantics.
2. **High — selected source representations are normalized or sampled.** `.okc` sampling, converted mesh/material data, audio re-encoding, and compact world tables can change precision or edge behavior. Evidence: `docs/formats/okc.md`, `docs/formats/audio.md`, `docs/formats/osm.md`; player impact depends on each consumer.
3. **Medium — little-endian host assumption.** Native POD serialization and a static assertion reject big-endian hosts. Evidence: [`src/core/binary.hpp:16-19`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/core/binary.hpp#L16-L19); portability only.
4. **Medium — caches are version-gated.** Several readers reject old/new versions rather than migrate them; compatibility depends on reimport. Evidence: `src/formats/*.cpp` reader checks; internal/update behavior.
5. **Medium — compressed audio is lossy on conversion.** xWMA is decoded and encoded to Ogg/Vorbis. Evidence: [`docs/formats/audio.md:10-13`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/audio.md#L10-L13); audible but not necessarily gameplay-affecting.

## Claims versus code

- Format docs explicitly say headers are the source of truth, but constants diverge: docs title `.osm` says version 10 while `osm.hpp` says 11; `.osr` docs say 5 while header says 6; `.osw` docs say 12 while header says 16; `.osv` says version 2 in both, with code comment describing a renamed v1 layout. Verify each affected version-history section before using docs as a compatibility promise.
- `.okc.md` has a malformed sentence in its intro; code still defines a KCAM reader/writer.
- `skytool` help lists operational command families, but help alone does not establish supported source variants. The implementation needs per-parser coverage before claiming full Skyrim compatibility.
- This area has many implemented readers/writers, but this pass found no format-focused test files. Tests may exist under broader names and remain unverified.
- Source types confirmed in import code include BSA, ESM/ESP/ESL, NIF, DDS, HKX, localized strings, SWF, Papyrus PEX, audio WAV/xWMA/FUZ/LIP, and SEQ references. A complete inventory of every asset extension remains open.

## Open questions

- Which source file types have actual importer readers versus opaque copies? Cheapest next step: inventory `src/importer/legacy` and every `data.read/list` call, grouped by suffix and tests.
- Which native formats have round-trip, malformed-input and version-compatibility tests? Cheapest next step: search all `tests/` and CMake test registration for `formats::read/write` call sites.
- How do `.osf`, cache hashes and invalidation interact with source changes? Cheapest next step: trace import manifest/cache key creation and `forms::Database` consumers.
- Which version discrepancies are stale docs versus intentional backward compatibility? Cheapest next step: compare each header version to its writer/reader and doc version table.
- Is binary serialization portable across compiler ABIs? Cheapest next step: inspect struct padding, static size assertions and cross-platform CI; current code asserts little endian but raw POD serialization can still encode ABI padding.
