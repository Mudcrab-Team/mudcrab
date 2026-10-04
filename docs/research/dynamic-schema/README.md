# Newest-SE schema source inventory kickoff

Status: provisional P0 research, 2026-10-04. This begins [DS1](../../roadmap/dynamic-schema-issue-proposals.md#ds1--catalog-and-target-pin-issue-body); it does not accept a schema, corpus or phase. The [initiative](../../roadmap/dynamic-schema-initiative.md) owns delivery order and the 100% newest-SE interpretation gate.

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

Source-reference pins are xEdit `9fb016884bec138ea6c7b872cec831537d464c3e` and Mutagen `4f533562ee0c70347d47c1979d5464d42b06ee6b`. Both temporary checkouts were clean and matched their upstream branches at inspection. These commits are distinct from any qualified executable/package versions, including the RE workbench's Mutagen `0.54.4` oracle.

The extraction reads active `wbRecord`, `wbRefRecord` and `ReferenceRecord(SIG, ...)` calls inside xEdit's `DefineTES5`, and root-level record objects in Mutagen's major-record XML. The `ReferenceRecord` helper delegates to `wbRefRecord`; its eight placed-trap calls must remain included. This is declaration scraping, not execution of Pascal callbacks, code generation or a full layout/field miner.

Reproduce with the standard-library-only [extractor](extract_candidate_inventory.py), supplying clean checkouts at the exact pins:

```sh
python3 docs/research/dynamic-schema/extract_candidate_inventory.py \
  --xedit-root /path/to/pinned/xedit \
  --mutagen-root /path/to/pinned/mutagen \
  --output /tmp/mudcrab-sse-candidate-inventory.json
```

The script rejects mismatched commits and dirty trees. Generated time and local checkout/branch/upstream metadata describe the run; candidate entries should match across equivalent pinned inputs. It does not fetch, build or execute either upstream project.

Verification on 2026-10-04: regeneration matched the retained JSON after excluding its timestamp; all 134 row identities/counts were consistent, and 293 declaration/gate source anchors were checked. The eight helper-derived placed-trap rows are shared candidates with one xEdit declaration each. Documentation links, fences and whitespace checks passed. These checks validate the inventory artifact, not the game format or interpretation.

Review limits:

- xEdit's `wbIsSkyrimSE` includes SSE, VR and EnderalSE. Mutagen's root release list spans LE, SE, GOG, VR and Enderal. Shared declaration placement does not prove newest-SE applicability.
- Names, record headers, registration joins and source availability do not prove a complete field/reference interpretation. Nested/custom codecs and conditional fields need inspection.
- Save-related, unused and compatibility declarations need separation from the supported plugin catalog. Empty Mutagen group joins do not establish missing registration for nested cell models.
- This kickoff did not read retail plugins, execute xEdit/Mutagen, measure conversion, or validate native behavior. Agreement is candidate evidence, not independent field-level proof.

The parent inspected the RE lock and oracle source separately: Steam SE/AE `1.7.104.0`, build `24914197`, executable hash `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`; static importer-native `1.6.1170.0`; existing Mutagen `0.54.4` placed-record pilot. The inventory preserves its original delegate-reported provenance. The full official Data/Creation manifest remains unpinned; RE T18 is still open.

Next work is to pin that manifest, reconcile source-candidate applicability, expand the signature ledger into variants/fields/ranges, qualify xEdit, and extend the existing Mutagen oracle. Newest-SE-native disagreements become build-tagged research items. Earlier SE/VR, LE and other-game implementation remains deferred.
