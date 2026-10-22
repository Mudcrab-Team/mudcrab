# Authored Skyrim HDR and image-space settings

## Which settings can we extract reliably now?

### Takeaway

The authored controls are already present in the local retail-derived database. They comprise nine HDR floats, three cinematic floats, tint amount plus RGB, and separate depth-of-field data. Extracting them does not determine their transformation into shader constants or the active values in a given frame.

### Cited Findings

- Both xEdit and CommonLibSSE-NG identify `IMGS.HNAM` as Eye Adapt Speed, Bloom Blur Radius, Bloom Threshold, Bloom Scale, Receive Bloom Threshold, White, Sunlight Scale, Sky Scale, and Eye Adapt Strength, in that order. CommonLib defines a 36-byte `HDR` structure at base offset `0x00`. `CNAM` holds saturation, brightness, contrast (12 bytes at `0x24`); `TNAM` holds amount then RGB (16 bytes at `0x30`). The combined base structure also has depth of field at `0x40`, total `0x50` bytes. — [CommonLib pinned header](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceData.h); [xEdit pinned schema, lines 5729–5795](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L5729)
- This branch's converter schema already describes the nine 32-bit `HNAM` fields, the `CNAM` triple, `TNAM` amount/RGB, and `WTHR.IMSP` sunrise/day/sunset/night links. Source schemas are facts about serialized layout, not shader arithmetic. — [Local visual schema](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/converter/src/records/families/visual_extras.json)
- Read-only SQLite inspection on 2026-10-15 found schema version 9, 304 IMGS records, 272 IMAD records, and 110 WTHR records in `modern_assets_204/skyrim_world.db`. The records contain the established rkyv `ArchivedRecordData { subrecords: Vec<ArchivedSubRecord { tag, data }> }` contract. I decoded that current blob layout, checked pointer/length bounds, then interpreted payloads with the independent schemas above. This is data inspection, not a test run or game capture. — [Database](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db); [Local archive types](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/converter/src/esm/types.rs); [Serialization](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/converter/src/esm/extractors.rs:100)
- The same source schema distinguishes legacy `ENAM` from the split HNAM/CNAM/TNAM layout. Legacy ENAM must not be decoded as the nine-float HNAM structure. — [xEdit source](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L5731)
- The pinned public files were fetched again this turn and matched the existing local receipts byte-for-byte: `ImageSpaceData.h` 3227 bytes / SHA256 `36403948765fc8a679a095cd50c4987b3b5d4d64525127039893f15197472a3a`; `ImageSpaceManager.h` 17275 / `f06948347b094fe03a51497f8af17da885c22bc5767834c5037b1a30d67fd210`; `TESImageSpaceModifier.h` 6962 / `4fab870f4a3fd9faccc2e0213577687444312be2da68e157ed5d8476d28fff43`; xEdit TES5 definitions 377868 / `d79665d0812c47ea9212296ed3436c9565453f5c046d6ef98c321c426a7f683d`. — [CommonLib data](https://raw.githubusercontent.com/CharmedBaryon/CommonLibSSE-NG/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceData.h); [Manager](https://raw.githubusercontent.com/CharmedBaryon/CommonLibSSE-NG/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceManager.h); [Modifier](https://raw.githubusercontent.com/CharmedBaryon/CommonLibSSE-NG/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESImageSpaceModifier.h); [xEdit](https://raw.githubusercontent.com/TES5Edit/TES5Edit/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas)

- The current database was hashed after read-only inspection: 1,300,541,440 bytes, SHA256 `5dd8984382c04bc34ed2d1225be1cf664e5abda14c0b32e97b090395d6d0e30d`. Size and nanosecond modification time were unchanged across the hash read; this binds the extracted values to this local file snapshot. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db)

### Inferences

- A new universal “Skyrim tone map settings” constant would lose authored variability. These controls need record identity and runtime selection alongside the recovered arithmetic.
- No additional asset conversion is necessary to inspect these authored settings in this local pack.

### Gaps

- I did not read the original ESM files this turn, so the observations below bind to current archived DB payloads and the DB's recorded plugin hashes. They do not independently revalidate the source plugin bytes against those hashes.

## What exact numeric values are verified, and do they identify the approved screenshot?

### Takeaway

The current data confirms the previously reported clear-day HDR values. The approved Photo Mode snapshot is substantially closer to the authored cloudy-day grade, but its saturation does not match that record, so the snapshot cannot identify active weather or inherited HDR settings.

### Cited Findings

- Winning `WTHR 0x0000081A SkyrimClear` has `IMSP = [0x000C8208, 0x00012F88, 0x000C8209, 0x000C820A]`, in sunrise/day/sunset/night order. Each record's DB `load_order` is 1, resolving to `Update.esm` through the plugins table. The form's origin in `formid_map` remains `skyrim.esm`; origin and winning override are different concepts. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db); [IMSP source schema](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/converter/src/records/families/visual_extras.json)
- For `IMGS 0x00012F88 ISSkyrimClearDAY`, the following values were independently decoded from the current archived subrecords. Decimal values in this table are readable roundings of stored IEEE754 binary32, not Photo Mode readings. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db)

| Block | Field | Clear day | Cloudy day, 0x00012F87 |
| --- | --- | ---: | ---: |
| HNAM | Eye Adapt Speed | 45 | 40 |
| HNAM | Bloom Blur Radius | 7 | 7 |
| HNAM | Bloom Threshold | 0.8 | 0.8 |
| HNAM | Bloom Scale | 4 | 3 |
| HNAM | Receive Bloom Threshold | 0.8 | 0.7 |
| HNAM | White | 1 | 1.05 |
| HNAM | Sunlight Scale | 2.8 | 3 |
| HNAM | Sky Scale | 0.05 | 0.16 |
| HNAM | Eye Adapt Strength | 5 | 5 |
| CNAM | Saturation | 1.5 | 1.6 |
| CNAM | Brightness | 1.135 | 1.15 |
| CNAM | Contrast | 1.4 | 1.5 |
| TNAM | Amount | 0.42 | 0.5625 |
| TNAM | RGB | 0.894117653, 0.839215696, 0.772549033 | 0.882352948, 0.807843149, 0.717647076 |

- Exact clear-day HNAM float32 decode: `[45.0, 7.0, 0.800000011920929, 4.0, 0.800000011920929, 1.0, 2.799999952316284, 0.05000000074505806, 5.0]`; exact clear-day CNAM: `[1.5, 1.1349999904632568, 1.399999976158142]`; exact TNAM: `[0.41999998688697815, 0.8941176533699036, 0.8392156958580017, 0.772549033164978]`. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db)
- Exact cloudy-day HNAM float32 decode: `[40.0, 7.0, 0.800000011920929, 3.0, 0.699999988079071, 1.0499999523162842, 3.0, 0.1599999964237213, 5.0]`. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db)
- `ISSkyrimClearDAWN` HNAM is `[35,7,0.6,4,0.35,0.98,2.5,0,8]`; dusk `[35,7,0.65,3,0.5,0.9,2.5,0,8]`; night `[45,7,0.6,4,0.45,0.925,2.6,0.02,8]` (readable roundings). This demonstrates variation within one weather across its authored time slots. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db)
- `WTHR 0x00012F89 SkyrimCloudy` day links to `IMGS 0x00012F87 ISSkyrimCloudyDAY`, also winning at DB load_order 1. The approved source code explicitly stores its snapshot as rounded Photo Mode UI values: saturation `1.66`, brightness `1.15`, contrast `1.50`, tint `.56`, RGB `.88/.81/.72`. The tuned sRGB brightness was subsequently raised to `1.38`. The original snapshot does not include HNAM. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db); [Approved grade provenance](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/engine/src/color_pipeline.rs:281)
- A scan of all 304 current IMGS records found no authored CNAM tuple with saturation in `(1.64,1.68)`, brightness in `(1.12,1.18)`, and contrast in `(1.48,1.52)`. This supports treating the snapshot as UI/effective state instead of an identified raw record. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db)
- The measured capture position `[21750,-44530,140]` is inside Tamriel exterior cell `(5,-11)`, DB cell `0x00009712 Riverwood02` (`worldspace_id=60`). Its archived record has no XCIM subrecord. This establishes only this cell's record contents, not which weather was active in Skyrim. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db); [Capture camera](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/data/visual-comparisons/brightness-plus20-20261014T162641Z/camera.json)

Record receipts (SHA256 of current rkyv record blob, not original TES record serialization):

| Record | SHA256 |
| --- | --- |
| SkyrimClear 0x81A | `4b10bb22208276378539727391d9d792e70d2d089cb26a2bb4f8454ba755f3ea` |
| ISSkyrimClearDAY 0x12F88 | `7a4c3f93e1fcd41cdba7e73f0e5cda0709d84c51ef0374d6f912f8ed65cab171` |
| ISSkyrimCloudyDAY 0x12F87 | `b52d1acbd9914f66c5faa7b19d74bc92420cdee51ed5440cad4b201530ac2777` |
| SkyrimCloudy 0x12F89 | `b177e49679cb69914f40b708cf31dabd89c19f6f69e1cb259de3fc121440650b` |
| Riverwood02 0x9712 | `c1bba55f343fa4969fd7e07fce67cc42b0e9196f5ff42102e9bdb35d417c28fd` |

Clear-day subrecord hashes: HNAM `78e9e52595a435df103c449261a5528817f6043450214ff8d7febcd494415094`; CNAM `2a08b4cac678a8671a0f49c70883eed55cfcbec2843a3752c42687d45ebc418e`; TNAM `ba58f6a45da3458cbc8845f7f8be0edf64589f82a62695c07327c97bb34b8044`. SkyrimClear IMSP payload hash `4089ec945e794da765d16b0a1633f725d1f1b274f13b18c8e82cb6c919c0a75b`. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db)

Recorded plugin checksums: Skyrim.esm `e198c3b85e5e48e0c92a6580d8f66e644256b68d812ee32b61735cf9b753df73`; Update.esm `b298b0f65fa0127fd13c1f5ef6bb30eaf9d21fd19f5e6f8f91f829c9007d3280`. Current local schema hash: `visual_extras.json` SHA256 `8dc3fa66f9a29a462a5f4d63ac2ead816da5c7871837ee7d2c92d781e9894099`. — [Current DB](/Users/taylor/Projects/mudcrab/modern_assets_204/skyrim_world.db); [Schema](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/converter/src/records/families/visual_extras.json)

### Inferences

- Cloudy-day similarity is a useful lead for a future runtime receipt, not proof of the screenshot's weather. Its inherited HNAM differs from clear-day HNAM, so selecting clear-day HDR merely because that was the earlier baseline could introduce another mismatch.
- The saturation discrepancy could arise from an edited UI state, interpolation, another override, or a different source; the data inspected here cannot choose between those explanations.

### Gaps

- No effective-weather receipt, weather-transition fraction, time-slot blend, active IMAD stack, current/override image-space dump, or adaptation-history values are bound to the existing Skyrim screenshot.
- Original Plugin checksum equality, game version equality, and conversion load order equality to the screenshot's Fiji installation remain unverified this turn.

## What separates authored extraction from a faithful native tone-map reconstruction?

### Takeaway

The remaining bridge is runtime selection and transformation of the authored values into the tone-program constant buffer, plus scene luminance/history and final transfer. The approved TonyMcMapface experiment deliberately leaves these HDR controls unapplied; this research does not modify that result.

### Cited Findings

- CommonLib exposes manager `currentBaseData` at `+0xA8`, `overrideBaseData` at `+0xB0`, `underwaterBaseData` at `+0xB8`, `consoleBaseData` at `+0xC0`, and combined `ImageSpaceData data` at `+0xC8`. `ImageSpaceData` has base data, mod data, and base/mod amounts. These public layouts give concrete REA anchors; field names alone do not prove live precedence for the pinned executable. — [Manager source](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceManager.h#L208); [Data source](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/ImageSpaceData.h#L124)
- IMAD is separate from IMGS. Its serialized schema contains animated multiplicative/additive interpolators for HDR and cinematic controls, including target luminance min/max; CommonLib models multiplier/addend pairs and min/max pairs. It is unsafe to treat an IMGS record as the whole effective frame state without tracing this machinery. — [xEdit IMAD schema](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L5797); [CommonLib modifier](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESImageSpaceModifier.h)
- CELL can serialize an `XCIM` Image Space FormID; WTHR serializes the four IMSP links above. These are record links, not a recovered algorithm for choosing cell/weather/override sources and blending them. — [xEdit CELL field](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L4297); [Converter CELL schema](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/converter/src/records/schema.json)
- Prior local static analysis recovered scoped luminance reduction/adaptation equations and a tone program which uses exposure ratio `avg.y / avg.x`, then bloom, saturation, tint, brightness and contrast around adaptation X, followed by saturation and reciprocal-gamma power. The audit explicitly says these are conditional package/native equations, not proof a particular route or constant set ran. This is valuable prior evidence, independently being rechecked by the native researcher for this task. — [Prior arithmetic report](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/image-space-arithmetic.md); [Prior audit](/Users/taylor/.t3/worktrees/mudcrab/t3-4d431c98/docs/research/skyrim-render-map/research-notes/note-0003/imagespace-audit.md)
- This approved branch explicitly documents its artistic pivot `0.18` as unrelated to a measured Skyrim adapted luminance and records the clear-day HNAM values as unapplied. — [Current implementation](/Users/taylor/.t3/worktrees/mudcrab/t3-a30beb65/crates/engine/src/color_pipeline.rs:239)

### Inferences

- A focused REA extraction should follow IMGS deserialization and current/override/IMAD/time/weather selection into the committed tone-program uniforms, retaining each transformation and address receipt. Then it should identify gamma/brightness settings, luminance/history resources, and the route actually used by a pinned screenshot.
- Reproducing the native shader arithmetic without reproducing selected values, adaptation state, scene linear-light scale and final transfer would still leave a major fidelity gap.
- Keep the approved Cool Gray/sRGB/TonyMcMapface configuration available while a separate native reconstruction is researched.

### Gaps

- No new native disassembly, shader extraction, game launch, Fiji change, implementation edit, test, build, staging, commit, or GitHub post was performed by this researcher.
- The exact offsets above come from a pinned public CommonLib revision and require native verification against the executable under study before being used as trusted read/write locations.
- Neither shader uniform vector naming nor HNAM field naming alone establishes the field-to-slot bridge; this task's native researcher owns that investigation.
