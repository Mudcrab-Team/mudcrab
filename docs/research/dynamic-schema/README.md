# Newest-SE schema source inventory kickoff

Status: provisional P0 research, 2026-10-04. This supports filed issue [#157](https://github.com/Mudcrab-Team/mudcrab/issues/157); it does not accept a schema, corpus or phase. The [initiative](../../roadmap/dynamic-schema-initiative.md) owns delivery order and the 100% newest-SE interpretation gate.

Open the [schema explorer](schema-explorer.html) in a browser to browse the human record names, current SQL tables/columns, selected field evidence, and known unknowns. The single HTML file works offline, with embedded data and no backend, fonts, framework or runtime MCP connection. Source links open pinned public files; RE ledger/store references are labelled local-only.

The catalog retains all 134 source entries: 133 plugin-record candidates and the separate `TES4` header. The builder derives the table view by executing the pinned `create_tables` SQL in an empty in-memory SQLite database: 26 logical tables, including two RTree virtual tables, with six implementation shadow tables excluded. Column facts preserve composite PK order, declared `NOT NULL`, SQL defaults and indexes. All 26 foreign-key lists are empty; code and semantic relationships are labelled separately. This is DDL evidence, not a populated database browser or an accepted field schema.

Use `/` or `Ctrl K` to focus the current view's search. Browse catalog links with the keyboard; the arrow keys move focus within the list. Tables have a separate column search. Hash links retain the selected item or column, for example `#record/REFR`, `#evidence/REFR.XESP` and `#table/references/enable_parent_flags`. The Guide distinguishes the current partial winning-record projection from the planned immutable archive and canonical registry.

The [builder](build_schema_explorer.py) uses Python's standard library and a fixed converter-source evidence pin, `44499b22274e3e951f8aa5e630c833e39acf88cb`. It reads the source inventory, current source files, [curated annotations](schema-explorer-annotations.json), and a [historical read-only MCP brief](schema-explorer-re-evidence.json). The brief records actual `mudcrab-re` calls and separate repository/build/tool identities from 2026-10-05. It contains factual labels and limits, with no decompiled bodies or proprietary payload bytes. The brief, field/native ledger and code comparison report retain their original capture/source pins and bytes. Historical finding access does not rerun or qualify the historical producer.

The bounded 2026-10-06 source refresh reviews master-priority normalization for explicit plugins.txt input and optional FormID layouts. The remapper requires exactly eight bytes for XESP across 11 placed kinds; invalid optional IDs become zero, malformed optional subrecords are removed, and valid XESP bytes 4-7 stay unchanged. XEMI on placed kinds and LTMP on CELL/WRLD are remapped without new typed SQL columns. The tag/payload projection remains lossy. SQL DDL, exporter paths, versions, catalog counts and historical native/MCP observations are unchanged.

```sh
python3 -B docs/research/dynamic-schema/build_schema_explorer.py
python3 -B docs/research/dynamic-schema/build_schema_explorer.py --check
python3 -B -m unittest discover -s docs/research/dynamic-schema -p 'test_*.py'
```

Generation is deterministic. The builder hashes strict UTF-8 text after converting only CRLF to LF; it preserves lone CR, Unicode and all other whitespace. The displayed text digests and the pinned LF source identities use that convention. Equivalent LF/CRLF checkouts produce identical embedded data and HTML. Other source drift fails closed and requires reviewed pins/annotations before regeneration. The regressions also exercise actual SQL null/default/key behavior, the full catalog denominator, safe script embedding and generated-output drift. The [historical browser smoke report](schema-explorer-browser-check.json) records 18 passing checks in Chromium 138 on Linux: search/filters, columns, deep links/back navigation, keyboard/skip focus, 390px layout, offline loading with HTTP/HTTPS blocked, and literal rendering of hostile labels. The 82 schema-tool and 22 research-tool tests passed; native Windows CI remains a separate check.

The [focused refresh browser report](schema-explorer-refresh-browser-check.json) records three passing Chromium checks for the updated XESP evidence, data flow and provenance pages. It identifies the refreshed HTML by SHA-256 and keeps the earlier 18-check report separate.

The [candidate inventory](candidate-inventory.json) records source declarations, paths/lines, pins, duplicate model declarations, release gates and extraction limits. It contains no proprietary game data or copied record-definition bodies.

| Source comparison | Result |
| --- | --- |
| xEdit active candidate signatures | 133 excluding the `TES4` file header |
| Mutagen major-record wire signatures | 127; 135 XML objects |
| Shared candidates | 127 |
| xEdit-only candidates | `CLDC`, `PLYR`, `PWAT`, `RGDL`, `SCOL`, `SCPT` |
| Mutagen-only candidates | None in this extraction |
| Retained rows | 134 including a separate `TES4` header row |

The six differences are research candidates, not six established missing retail-SE records. Review declaration purpose, active SSE registration, official occurrences and valid synthetic cases before deciding applicability. Multiple Mutagen `GLOB` and `GMST` model objects share one physical signature; object counts are not record-type counts.

Source-reference pins are xEdit `9fb016884bec138ea6c7b872cec831537d464c3e` and Mutagen `4f533562ee0c70347d47c1979d5464d42b06ee6b`. Both available temporary checkouts were clean and matched their configured upstream branches (`0` ahead, `0` behind) when rechecked on 2026-10-05. These commits are distinct from any qualified executable/package versions, including the RE workbench's Mutagen `0.54.4` oracle.

The extractor records factual signatures, declared names, source paths, and line anchors from those pins. It does not copy implementation source or record-definition bodies, evaluate either implementation, or decide reuse rights. The pinned xEdit [`LICENSE.txt`](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/LICENSE.txt) identifies MPL-2.0; Mutagen's pinned [`LICENSE.txt`](https://github.com/Mutagen-Modding/Mutagen/blob/4f533562ee0c70347d47c1979d5464d42b06ee6b/LICENSE.txt) identifies GPL-3.0. These are source license-file facts only, not a legal assessment or reuse decision.

The extraction reads active `wbRecord`, `wbRefRecord` and `ReferenceRecord(SIG, ...)` calls inside xEdit's `DefineTES5`, and root-level record objects in Mutagen's major-record XML. The `ReferenceRecord` helper delegates to `wbRefRecord`; its eight placed-trap calls must remain included. This is declaration scraping, not execution of Pascal callbacks, code generation or a full layout/field miner.

Reproduce with the standard-library-only [extractor](extract_candidate_inventory.py), supplying clean checkouts at the exact pins:

```sh
python3 docs/research/dynamic-schema/extract_candidate_inventory.py \
  --xedit-root /path/to/pinned/xedit \
  --mutagen-root /path/to/pinned/mutagen \
  --output /tmp/mudcrab-sse-candidate-inventory.json
```

The script rejects mismatched commits and dirty trees. Generated time and explicit checkout/branch/upstream metadata describe the run; candidate entries should match across equivalent pinned inputs. The parent-reported RE context is historical metadata, with paths relative to the separate local-only `mudcrab-reverse-engineering` checkout; those paths are not extractor defaults. The script does not fetch, build or execute either upstream project.

Portable parser checks use only Python's standard library and do not need either upstream checkout:

```sh
python3 -B -m unittest discover -s docs/research/dynamic-schema -p 'test_*.py'
```

Verification on 2026-10-05: regeneration from the verified source pins retained all 134 row identities/counts and 293 declaration/gate anchors. The eight helper-derived placed-trap rows are shared candidates with one xEdit declaration each. Parent workbench paths in the generated provenance were normalized to repository-relative local-only references; source checkout paths, generated time, and branch metadata remain run-specific. These checks validate the inventory artifact, not the game format or interpretation.

Review limits:

- xEdit's `wbIsSkyrimSE` includes SSE, VR and EnderalSE. Mutagen's root release list spans LE, SE, GOG, VR and Enderal. Shared declaration placement does not prove newest-SE applicability.
- Names, record headers, registration joins and source availability do not prove a complete field/reference interpretation. Nested/custom codecs and conditional fields need inspection.
- Save-related, unused and compatibility declarations need separation from the supported plugin catalog. Empty Mutagen group joins do not establish missing registration for nested cell models.
- This declaration extraction did not read retail plugins, execute xEdit/Mutagen, measure conversion, or validate native behavior. Agreement is candidate evidence, not independent field-level proof.

The parent inspected the RE lock and oracle source separately: Steam SE/AE `1.7.104.0`, build `24914197`, executable hash `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`; static importer-native `1.6.1170.0`; existing Mutagen `0.54.4` placed-record pilot. The inventory preserves its original delegate-reported provenance. The full official Data/Creation manifest remains unpinned; RE T18 is still open.

The [installed corpus observation](p0-corpus-manifest.md) now records hashes and bounded TES4 metadata for 80 installed plugins, the five base masters and 75 CCC names. Its runtime hash matches the target. The historical version-2 verdict remains incomplete: all 93 archives now have byte hashes, while bundled table payloads, active load order, locale, official-content provenance and an accepted corpus pin set remain unresolved. Separate BSArch name observations found 2,160 string-table candidates in 76 archives; they do not establish table interpretation. TES4 metadata decoding does not establish whole-plugin structural validity.

The [validator starter tooling](../../../scripts/schema/README.md) extends a verified temporary copy of the existing Mutagen oracle and keeps synthetic inputs/build outputs outside the active RE checkout and store. CI is configured to run the Python contract tests; local .NET execution is a separate qualification step requiring the pinned oracle source and SDK. A pinned official xEdit console release now runs through isolated Wine. Its synthetic capability probe retains schema diagnostics, incomplete light-plugin execution and unqualified malformed-input outcomes. It has not become a qualified independent validator, and its binary pin is separate from the mined source pin. The native runtime VM execution gate remains open.

The [historical selected field and variant ledger](native-field-ledger.md) records 2026-10-05 base/main converter projection, SQL, load-order identity, and terrain-cache anchors, plus targeted static loader observations for `REFR`, `CELL`, and `STAT`. Its task base and observed main differ in exporter/identity sources; a separate bounded comparison of that main with review branch `2ce3977` matches all five inspected converter files. Its source pins and [code comparison report](pilot-code-evidence.json) are separate from the explorer's refreshed converter-source pin. This remains a sample: target-corpus field lengths/occurrences, full catalog coverage, downstream runtime consumers, and runtime behavior remain open. Reproduce and check that historical source report with:

```sh
python3 docs/research/dynamic-schema/pilot_code_evidence.py \
  --repo . \
  --base 97ddf6966310061c859f9eb3db84e279121d23b4 \
  --current db3a1dc3ed80795149e3a15a5047f4d313dd3e74 \
  --output docs/research/dynamic-schema/pilot-code-evidence.json
python3 docs/research/dynamic-schema/pilot_code_evidence.py \
  --repo . \
  --base 97ddf6966310061c859f9eb3db84e279121d23b4 \
  --current db3a1dc3ed80795149e3a15a5047f4d313dd3e74 \
  --output docs/research/dynamic-schema/pilot-code-evidence.json \
  --check
```

The historical parent starter verification on 2026-10-04 passed all 35 offline tests and the separate Mutagen `0.54.4` / SDK `9.0.318` qualification: six positive hand-encoded fixtures, four managed malformed-input rejections, and the original `placed`/`placed-lo` commands. The build reported zero warnings and errors. Positive `HEDR` counts include major records and group headers, with independent physical-count assertions. The full fixture SHA-256 is `2b6a56869dfb17fb41e1fc5faa58f529afd98818515b1c632b072492fb0f50c8`.

The parent starter evidence remains at the author-host local-only path `/tmp/mudcrab-p0-mutagen-bde01061228f4ace8ad28f3bd5691688/qualification.json` (6,808 bytes; SHA-256 `47ef80c1e99271edbdce609240a15698a4416406f48949a14cb6f3d0352e9e70`). Its localized ARMO observation independently exposes Mutagen's `StringsKey` as `305419896`, matching fixture wire ID `0x12345678`; printable Name and translated text remain unavailable without a string table. Physical offsets/order, opaque payloads, complete source-occurrence coverage, retail/native behavior and full-catalog acceptance remain unavailable. Workflow lint passed with actionlint `1.7.12`; no local Rust or engine runtime tests ran for this tooling slice.

This starter work is part of [#157](https://github.com/Mudcrab-Team/mudcrab/issues/157) and [#158](https://github.com/Mudcrab-Team/mudcrab/issues/158), with root SPEC T41/T56 still in progress. Remaining P0 work includes accepted official-content/archive/localization/load-order pins, source-candidate applicability, the complete variant/field/range ledger, current-game output/time/RSS baselines, archive contracts and qualified independent validators. Newest-SE-native disagreements become build-tagged research items. Earlier SE/VR, LE and other-game implementation remains deferred.

The [next delivery notes](p0-next-delivery.md) record the newer corpus inputs, bounded xEdit probe, current pilot field/native ledger, validation fixes and remaining P0 gates. Exact runner-source hashes now accompany executable observations. T41/T56 remain in progress.

The 2026-10-05 parent repair passed 40 offline schema tests, two portable extractor tests and 41 engine configuration tests on Linux. Three already-built CLI tests also passed; engine sources and build inputs are unchanged since that binary was built. The next repair passed 76 offline schema tests and the same two extractor tests. Native Windows checks remain a CI gate; these checks do not refresh historical external-tool qualification or accept P0.

The follow-up Windows repair passed 45 offline schema tests and two extractor
tests on Linux. Parent aliases and `..` are tested as separate native paths;
Data-relative issue keys use POSIX separators on every host. Timeout cleanup
now bounds the final drain, attempts Windows process-tree termination, and
avoids closing pipes held by Windows reader threads. A real detached POSIX child
and simulated Windows cleanup failures verify bounded incomplete outcomes.
The parent's standalone runner saves undecodable partial timeout streams with
raw bytes and availability metadata. The next layer keeps its binary supervisor
and strict decoder; both Mutagen and xEdit save decode-failure byte artifacts
and metadata. The Mutagen regression covers checked, fixture, legacy and
SDK-startup commands.
If Windows tree termination fails, escaped child processes and reader threads
can remain until those children exit; the runner still returns an incomplete
timeout. Fresh native Windows CI remains required. Earlier external-tool
qualification retains its original runner pins.
