# Record reader divergence: legacy vs in-house

Status: research, 2026-10-16. Follows the #269 triage, which found that every new projection
needed extra rules to give identical rows from `--record-reader legacy` and the default in-house
reader. This note lists why the two readers differ, where it shows up, how to measure it on real
data, and the options. Nothing here has been measured on real Skyrim data yet; see "Measuring on
real data".

## Summary

- The two readers do not produce the same canonical records. `records.data` (and the `cells`,
  `references` and `waters` blobs, which are the same subrecords) differ in ten known ways,
  M1–M10 below. Every runtime projection reads those records, so each projection inherits the
  differences unless it adds its own guards. The #269 PRs did this, but per projection.
- The largest gap is FormID remapping. The in-house reader remaps every FormID its schema types:
  653 (record type, subrecord) pairs carry one. The legacy reader remaps an explicit list covering
  172 of them, so **481 pairs, in 103 record types, keep plugin-local FormIDs in legacy output**.
  Examples are `MODS` alternate textures (all record types but `GRAS`), `KWDA` keywords,
  `CNTO` container items, most `NPC_`/`RACE` links, and `REGN`/`CLMT` weather lists. The full list
  is in the appendix.
- The in-house decoder is stricter. It drops a field it cannot decode (wrong size, non-finite
  float, out of schema order), nulls links to absent or wrong-type targets, rejects a record whose
  mandatory field is malformed (the earlier winner stays), and decodes text to UTF-8. The legacy
  reader does none of this.
- `crates/converter/tests/reader_divergence.rs` reproduces eight of the mechanisms with one
  synthetic case each. `reader-diff` (below) classifies every difference between two real
  databases.

## Where each reader's records come from

| Step | Legacy (`esm/mod.rs`) | In-house (`esm/inhouse.rs`, `records/decoder.rs`) |
|---|---|---|
| Parse | hand-written subrecord split | schema-driven decode of every field |
| Merge | last plugin wins; GMST by EditorID | same winner rules, but a candidate that fails validation never replaces an earlier winner |
| FormIDs | `remap_record_form_ids`: `form_id_layout` table, placed `NAME`, `GRAS.MODS`, LAND textures, VMAD | every typed `form_id` member (`decoder::remap`), then target validation (`validate_targets_with_prepared`) |
| Text | raw bytes | zstrings Windows-1252 → UTF-8; lstrings resolved through string tables |
| Canonical record | the merged subrecords | `take_canonical_record`: decoded fields' `canonical_bytes`, text replaced |
| Hidden from projections | nothing | opaque and unresolved fields (`runtime_projection`) |

Both then run the same `export_records`, so every difference below reaches the runtime tables
unless a projection compensates.

## Mechanisms

| # | Mechanism | In-house | Legacy | Test case (`reader_divergence.rs`) |
|---|---|---|---|---|
| M1 | Field fails to decode: wrong size, non-finite `f32`, bad union | field dropped from the record (`rejected_fields`) | kept | `CELL XCLW` (3 bytes), `LIGH FNAM` (NaN) |
| M2 | Known subrecord out of schema order (`allow_unordered: false`) | dropped (`match_field` miss) | kept | `STAT` with `MODL` before `EDID` |
| M3 | Subrecord unknown to the schema, in a supported record | kept in `records.data`, hidden from projections (opaque) | kept, visible | (no blob difference; projections differ) |
| M4 | Typed link to an absent or wrong-type record | set to 0 (IDs below 0x800 exempt) | kept | `REFR XEMI` → missing record |
| M5 | Typed FormID the legacy layout table lacks | remapped to load-order numbering | plugin-local | `WTHR IMSP` in a slot-2 mod |
| M6 | Windows-1252 text in a zstring | UTF-8 | raw bytes; projections read them with `from_utf8_lossy` (U+FFFD) | `STAT EDID` "Café" |
| M7 | Localized string (`lstring` in a localized plugin) | resolved text, or empty and unresolved | 4-byte string ID | `ACTI FULL` |
| M8 | Candidate with a malformed mandatory field (`reject_record_on_error`, 70 fields) | rejected; the earlier winner stays | the candidate wins | `CONT DATA` 4 bytes in an override |
| M9 | Scripts | scalar `script_properties` tables | JSON `scripts` projection | by design (`db-schema.md` §9) |
| M10 | Merge rules outside the decoder (`DOBJ` key-by-key merge in #246, INFO topic owners) | per PR | per PR | — |

M3 explains a subtle case found while writing the test: `STAT` has no `FULL` in the schema, so
an in-house `STAT FULL` stays raw (opaque) and agrees with legacy. The same subrecord on an
`ACTI` is decoded and differs (M7).

## Where it shows up today (`main`)

Projections on `main` that read affected fields, from `exporter.rs`:

| Table / column | Mechanisms |
|---|---|
| `statics.editor_id`, `model_path` | M1/M2 (field dropped → NULL in-house), M6, M8 (other winner) |
| `cells.interior_name` | M6 |
| `npcs.full_name` | M6, M7 (#195) |
| `texture_sets.*_path`, `grass_types.model_path`, `waters.flow_normal_path` | M6, M1 |
| `waters` colours (`DNAM`), `lights` (`DATA`, `FNAM`), `landscape_textures` (`HNAM`) | M1 (in-house checks sizes; legacy reads what is there, e.g. `lights` needs only 20 of 48 `DATA` bytes) |
| `references.radius_override`, `enable_parent_*` | M1 (sizes) |
| any table joined on a FormID column | M4 (legacy keeps dangling IDs), M5 (only for fields outside the legacy table) |
| `scripts` vs `script_properties` | M9 (by design) |
| PR #128 `door_links.destination_name` | M6, M7 |

The #269 PRs (#252, #270, #271, #276, #277, #278) guard their own new columns against M1, M4,
M5 and M7 with existence/type checks, schema-size checks and added legacy remaps. Each guard
covers one field at a time.

## Measuring on real data

`reader-diff` compares two databases built from the same plugins:

```sh
# Build both (records only, no asset conversion) and compare:
cargo run --release -p converter --bin reader-diff -- \
  build "<Skyrim>/Data" "<plugins.txt>" /tmp/reader-diff --json /tmp/reader-diff.json

# Or compare two existing packs' databases:
cargo run --release -p converter --bin reader-diff -- \
  compare <legacy>/skyrim_world.db <inhouse>/skyrim_world.db --json report.json
```

It prints one line per (class, record type) for records, and per (class, record type and
signature) for fields, largest first, plus every table whose rows differ. The JSON report keeps
up to `--samples` examples per bucket (FormID and both values). Classes: `omitted_by_inhouse`,
`only_inhouse`, `link_cleared`, `form_id_remap`, `link_cleared_and_remap`, `text_encoding`,
`localized_string`, `order`, `other_length`, `other_bytes`; for records, `only_legacy`,
`only_inhouse`, `different_winner`, `record_type`, `cell_or_worldspace`.

To run:
1. the 5 official masters plus Creation Club (the 80-plugin set #204 measured);
2. a large mod list. `other_*` buckets are the ones to read first: they are differences no
   known mechanism explains.

On the synthetic `dummy-content` corpus, all 15 records and every shared table are identical, as
expected: those plugins are well-formed, ASCII-only and non-localized.

## Options

1. **Keep per-projection guards** (what #269's PRs do). Every new column re-derives in-house's
   rules: link existence and type, field sizes, legacy remap entries. It works, but it is
   whack-a-mole: 481 FormID subrecords and every typed size rule are candidates, and missing one
   only shows up in a two-reader test.
2. **Canonicalize legacy output through the in-house decoder.** Keep the legacy scanner and merge,
   then run each winning record through the schema decoder (`records::decoder`) to apply
   M1/M2/M4/M5/M6/M7 the same way. Projections then see one canonical form. Remaining differences
   would be merge and recovery behaviour (M8, M10), which is what a fallback reader is for. Cost:
   the legacy path becomes "legacy scanner + in-house decoder", and decoder changes now affect
   both readers.
3. **Make legacy a diagnostic-only reader.** In-house is already the default and the contract
   (`db-schema.md`). Stop requiring new projections to match legacy row for row: keep
   `reader-diff` and `reader_divergence.rs` as the record of how they differ, and remove
   `--record-reader legacy` once a real-data `reader-diff` on the official set and a large mod
   list shows no unexplained (`other_*`) differences and no in-house regressions.

Recommendation: **3**, gated on the real-data run. If a supported fallback reader is still
wanted after that run, 2 is the way to keep one canonical form. Either way, the guards already
in the #269 PRs (target validation, size checks) are sound defensive projection rules and can
stay.

## Appendix: FormID subrecords the legacy reader does not remap

From the in-house schema (`records/schema.json`, `records/families/*.json`, with definitions
expanded) minus the legacy coverage in `esm/mod.rs` (`form_id_layout`, placed `NAME`,
`GRAS.MODS`, LAND `BTXT`/`ATXT`/`VTEX`, VMAD), on `main` 27d4851. This is a schema-level list:
a subrecord appears if any of its members is a FormID, whether or not real data uses it.

```text
ACHR INAM TNAM XMBR XMRC
ACTI DMDS DSTD KNAM KWDA MODS SNAM VNAM WNAM
ADDN MODS SNAM
ALCH DMDS DSTD EFID ENIT ETYP KWDA MODS YNAM ZNAM
AMMO DATA DMDS DSTD KWDA MODS YNAM ZNAM
ANIO MODS
APPA DMDS DSTD MODS YNAM ZNAM
ARMA MO2S MO3S MO4S MO5S MODL NAM0 NAM1 NAM2 NAM3 ONAM RNAM SNDD
ARMO BAMT BIDS DMDS DSTD EITM ETYP KWDA MO2S MO4S MODL RNAM TNAM YNAM ZNAM
ARTO MODS
ASPC BNAM RDAT SNAM
AVIF PNAM SNAM
BOOK DATA DMDS DSTD INAM KWDA MODS YNAM ZNAM
BPTD BPND MODS
CAMS MNAM MODS
CELL XILL
CLMT MODS WLST
COBJ BNAM CNAM CNTO COED
COLL CNAM
CONT CNTO COED DMDS DSTD MODS QNAM SNAM
CPTH ANAM SNAM
DIAL BNAM QNAM
DLBR QNAM SNAM
DLVW BNAM QNAM TNAM
DOBJ DNAM
DOOR ANAM BNAM DMDS DSTD MODS SNAM TNAM
DUAL DATA
ECZN DATA
EFSH DATA
ENCH EFID ENIT
EQUP PNAM
EXPL DATA EITM MNAM MODS
FACT CRGR JAIL JOUT PLCN PLVD STOL VENC VEND WAIT XNAM
FLOR DMDS DSTD KWDA MODS PFIG SNAM
FLST LNAM
FSTP DATA
FSTS DATA
FURN DMDS DSTD FNMK KNAM KWDA MODS NAM1
HAZD DATA MNAM MODS
HDPT CNAM HNAM MODS RNAM TNAM
IDLE ANAM
IDLM IDLA MODS
INFO ANAM CTDA DNAM LNAM ONAM PNAM QNAM SNAM TCLT TPIC TRDT TWAT
INGR DMDS DSTD EFID ETYP KWDA MODS YNAM ZNAM
IPCT DNAM ENAM MODS NAM1 NAM2 SNAM
IPDS PNAM
KEYM DMDS DSTD KWDA MODS YNAM ZNAM
LCTN ACEC ACEP ACID ACPR ACSR ACUN FNAM KWDA LCEC LCEP LCID LCPR LCSR LCUN MNAM NAM0 NAM1 PNAM RCEC RCPR RCSR RCUN
LIGH LNAM MODS SNAM
LSCR NNAM
LVLI COED LVLG LVLO
LVLN COED LVLG LVLO MODS
LVSP LVLO
MATO MODS
MATT HNAM PNAM
MESG CTDA INAM QNAM
MGEF DATA ESCE KWDA MDOB SNDD
MISC DMDS DSTD KWDA MODS YNAM ZNAM
MSTT DMDS DSTD MODS SNAM
MUSC TNAM
MUST SNAM
NAVI NVSI
NAVM ONAM
NPC_ ANAM ATKD ATKR CNTO COED CRIF CSCR CSDI DMDS DOFT DPLT DSTD ECOR FTST GNAM GWOR HCLF KWDA OCOR PKID PNAM PRKR SOFT SPLO SPOR TPLT VTCK WNAM ZNAM
OTFT INAM
PACK CNAM IDLA INAM PDTO PKCU PLDT PTDA QNAM
PARW XMBR XPWR
PBAR XMBR XPWR
PBEA XMBR XPWR
PCON XMBR XPWR
PERK DATA EPFD NNAM
PFLA XMBR XPWR
PGRE XMBR XPWR
PHZD XMBR XPWR
PLYR PLYR
PMIS XMBR XPWR
PROJ DATA DMDS DSTD MODS
QUST ALCO ALDN ALEQ ALFC ALFL ALFR ALPC ALRT ALSP ALUA CNTO COED CTDA ECOR GWOR KNAM KWDA NAM0 OCOR QSTA QTGL SPOR VTCK
RACE AHCF AHCM ATKD ATKR DFTF DFTM DNAM ENAM FLMV FTSF FTSM GNAM HCLF HEAD HNAM KWDA LNAM MODS MTYP NAM4 NAM5 NAM7 NAM8 ONAM QNAM RNAM RPRF RPRM SNMV SPLO SPMV SWMV TINC TIND UNES VTCK WNAM
REFR XATR XCZC XCZR XLIB XLTW XMBR XORD XPWR XSPC XTNM
REGN RDGS RDMO RDOT RDSA RDWT WNAM
RELA DATA
RFCT DATA
SCEN CTDA DATA PNAM
SCOL MODS ONAM
SCRL DMDS DSTD EFID ETYP KWDA MDOB MODS SPIT YNAM ZNAM
SHOU MDOB SNAM
SLGM DMDS DSTD KWDA MODS NAM0 YNAM ZNAM
SMBN CTDA PNAM SNAM
SMEN CTDA PNAM SNAM
SMQN CTDA NNAM PNAM SNAM
SNCT PNAM
SNDR GNAM ONAM SNAM
SOUN SDSC
SPEL EFID ETYP KWDA MDOB SPIT
STAT DNAM MODS
TACT DMDS DSTD KWDA MODS SNAM VNAM
TES4 ONAM
TREE MODS
WATR INAM SNAM TNAM XNAM
WEAP BAMT BIDS CNAM CRDT DMDS DSTD EITM ETYP INAM KWDA MO3S MODS NAM7 NAM8 NAM9 SNAM TNAM UNAM WNAM XNAM YNAM ZNAM
WRLD MODS ZNAM
WTHR GNAM HNAM IMSP MNAM MODS NNAM SNAM TNAM
```
