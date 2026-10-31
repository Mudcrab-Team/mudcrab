//! RV-A real-transform invariants and private synthetic-schema publication recovery.
use super::*;
use crate::{
    esm::{extractors::SubrecordView, types::ArchivedRecordData},
    records::{DecodedRecord, ReadResult, schema_format::Schema},
};
use serde_json::Value as Json;

const MOVEMENT: u32 = 0x0003_580d;
const WALK_SETTING: u32 = 0x0001_ec72;
const JUMP_SETTING: u32 = 0x000a_bef6;
const RACE_OVERRIDE: u32 = 0x900;
const RACE_MISSING_TEXT: u32 = 0x0200_0901;
const RACE_NULL_TEXT: u32 = 0x0200_0902;
const TERRAIN: u32 = 0x8a1;
const HEIGHTLESS_TERRAIN: u32 = 0x0200_0911;
const SELF_TEXTURE: u32 = 0x0200_0a20;

/// Frame native subrecords independently of the schema-driven decoder.
fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    framed.extend(bytes);
    framed
}

/// Supply distinct header sentinels and exact, independently framed payload bytes.
fn native_record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    framed.extend(flags.to_le_bytes());
    framed.extend(id.to_le_bytes());
    framed.extend(0x1937_5badu32.to_le_bytes());
    framed.extend(44u16.to_le_bytes());
    framed.extend(0x2759u16.to_le_bytes());
    framed.extend(payload);
    framed
}

/// Preserve native world/cell labels without inferring them from record identities.
fn native_group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = b"GRUP".to_vec();
    framed.extend((24 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
    framed.extend(label.to_le_bytes());
    framed.extend(kind.to_le_bytes());
    framed.extend(0x7319_5ba7u32.to_le_bytes());
    framed.extend(0x2971u16.to_le_bytes());
    framed.extend(0x3957u16.to_le_bytes());
    framed.extend(payload);
    framed
}

/// Retain original Windows-1252 bytes and deliberate ASCII source padding.
fn text(bytes: &[u8]) -> Vec<u8> {
    [bytes, &[0]].concat()
}

/// Construct one checked terrain layer with nonzero source padding.
fn layer(texture: u32, quadrant: u8, number: u16) -> Vec<u8> {
    [
        texture.to_le_bytes().as_slice(),
        &[quadrant, 0x97],
        number.to_le_bytes().as_slice(),
    ]
    .concat()
}

/// Use asymmetric vertices/opacities and preserve the two unused native bytes.
fn weights(vertex: u16, opacity: f32) -> Vec<u8> {
    [
        vertex.to_le_bytes().as_slice(),
        &[0x37, 0xa9],
        opacity.to_le_bytes().as_slice(),
    ]
    .concat()
}

/// Zero deltas make the independently expected runtime height exactly eight times the offset.
fn heights(offset: f32) -> Vec<u8> {
    let mut bytes = offset.to_le_bytes().to_vec();
    bytes.extend(vec![0; 33 * 33]);
    bytes.extend([0x19, 0x73, 0xb5]);
    bytes
}

/// Frame exterior ownership with distinguishable grid coordinates.
fn exterior_cell(id: u32) -> Vec<u8> {
    let grid = [17i32.to_le_bytes(), (-29i32).to_le_bytes(), [0; 4]].concat();
    native_record(
        b"CELL",
        id,
        0,
        &[sub(b"DATA", &[0]), sub(b"XCLC", &grid)].concat(),
    )
}

/// Small authored files only; no game files, asset conversion, or LOD generation are involved.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Keep the scoped owner alive through every SQL/cache/source assertion.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        fs::create_dir(&data).unwrap();
        Self {
            directory,
            data,
            paths: Vec::new(),
        }
    }

    /// Add a complete TES4 header, including independent master-list indices.
    fn add(&mut self, name: &str, masters: &[&str], flags: u32, body: &[u8]) {
        let header = [
            1.7f32.to_le_bytes(),
            32u32.to_le_bytes(),
            0x800u32.to_le_bytes(),
        ]
        .concat();
        let mut payload = sub(b"HEDR", &header);
        for master in masters {
            payload.extend(sub(b"MAST", &text(master.as_bytes())));
            payload.extend(sub(b"DATA", &[0; 8]));
        }
        let mut bytes = native_record(b"TES4", 0, flags, &payload);
        bytes.extend(body);
        let path = self.data.join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Exercise the actual adapter, SQL export, typed projection and both cache writers.
    fn publish(
        &self,
        schema: Option<&Schema>,
        signatures: &[[u8; 4]],
    ) -> (HashMap<u32, RawRecord>, Connection, PathBuf) {
        let output = self.directory.path().join("published");
        fs::create_dir(&output).unwrap();
        let db_path = output.join("skyrim_world.db");
        let records = convert_plugins_with_dump_impl(
            &self.paths,
            &db_path,
            &self.data,
            Some((&output, signatures)),
            RecordBundleMode::Full,
            None,
            schema,
        )
        .unwrap();
        write_terrain_caches(&records, &db_path, &output).unwrap();
        (records, Connection::open(db_path).unwrap(), output)
    }
}

/// Read published canonical bytes through the established rkyv SQL contract.
fn canonical(db: &Connection, id: u32) -> ArchivedRecordData {
    let bytes: Vec<u8> = db
        .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap();
    rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap()
}

/// Select by native signature so fixture assertions do not depend on decoder role names.
fn field<'a>(record: &'a DecodedRecord, tag: &[u8; 4]) -> &'a crate::records::DecodedField {
    record
        .fields
        .iter()
        .find(|field| &field.signature == tag)
        .unwrap()
}

/// Decode a four-byte native/global identity without sharing remapping calculations.
fn word(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

/// Legacy flags must survive typed/runtime publication even in a localized plugin with no bank.
#[test]
fn r10_sndr_fnam_flags_publish_without_localization() {
    let mut fixture = Fixture::new();
    let flags = [0x11u32, 0x12, 0x8000_0014];
    let mut body = Vec::new();
    for (index, bits) in flags.into_iter().enumerate() {
        let mut bytes = native_record(
            b"SNDR",
            0x900 + index as u32,
            0,
            &sub(b"FNAM", &bits.to_le_bytes()),
        );
        bytes[20..22].copy_from_slice(&34u16.to_le_bytes());
        body.extend(bytes);
    }
    fixture.add("Flags.esm", &[], 0x81, &body);
    let order = LoadOrder::read(&fixture.paths).unwrap();
    let result = records::read_plugins(&fixture.paths, &order).unwrap();
    assert_eq!(result.diagnostics["flags.esm"].skipped_fields, 0);
    for (index, bits) in flags.into_iter().enumerate() {
        let decoded = &result.records[&(0x900 + index as u32)];
        let value = field(decoded, b"FNAM");
        assert_eq!(value.name, "legacy_flags");
        assert_eq!(value.value, records::Value::Unsigned(u64::from(bits)));
        assert_eq!(value.canonical_bytes, bits.to_le_bytes());
        assert!(value.localized_bank.is_none());
        assert_eq!(decoded.raw_payload, sub(b"FNAM", &bits.to_le_bytes()));
    }
    let (_, db, output) = fixture.publish(None, &[*b"SNDR"]);
    let localized: u32 = db
        .query_row("SELECT count(*) FROM inhouse_localized_fields", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(localized, 0);
    let typed = fs::read_to_string(output.join("typed-records.jsonl")).unwrap();
    let rows: Vec<Json> = typed
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (index, bits) in flags.into_iter().enumerate() {
        let id = 0x900 + index as u32;
        let runtime = canonical(&db, id);
        assert_eq!(runtime.subrecords[0].tag, *b"FNAM");
        assert_eq!(runtime.subrecords[0].data, bits.to_le_bytes());
        let row = rows.iter().find(|row| row["form_id"] == id).unwrap();
        assert_eq!(row["fields"][0]["value"]["Unsigned"], bits);
        assert_eq!(row["localization"], serde_json::json!({}));
    }
}

/// Unrestricted still means a live link: retain REFR remapping and diagnose missing targets.
#[test]
fn r10_navm_onam_reference_remaps_and_preserves_source() {
    let mut fixture = Fixture::new();
    fixture.add("Base.esm", &[], 1, &[]);
    fixture.add("Gap.esm", &[], 1, &[]);
    let source_link = 0x0100_0920u32;
    let payload = sub(b"ONAM", &source_link.to_le_bytes());
    let mut body = native_record(b"REFR", source_link, 0, &sub(b"EDID", b"LiveReference\0"));
    body.extend(native_record(b"NAVM", 0x0100_0930, 0, &payload));
    body.extend(native_record(
        b"NAVM",
        0x0100_0931,
        0,
        &sub(b"ONAM", &0x0100_0999u32.to_le_bytes()),
    ));
    fixture.add("Navigation.esp", &["Base.esm"], 0, &body);
    let result =
        records::read_plugins(&fixture.paths, &LoadOrder::read(&fixture.paths).unwrap()).unwrap();
    let decoded = &result.records[&0x0200_0930];
    let onam = field(decoded, b"ONAM");
    assert_eq!(
        onam.value.array_element(0),
        Some(records::Value::FormId(0x0200_0920))
    );
    assert_eq!(onam.canonical_bytes, 0x0200_0920u32.to_le_bytes());
    assert_eq!(decoded.raw_payload, payload);
    assert_eq!(
        field(&result.records[&0x0200_0931], b"ONAM").canonical_bytes,
        [0; 4]
    );
    assert_eq!(result.diagnostics["navigation.esp"].invalid_links, 1);
    let (_, db, _) = fixture.publish(None, &[*b"NAVM"]);
    let runtime = canonical(&db, 0x0200_0930);
    assert_eq!(runtime.subrecords[0].tag, *b"ONAM");
    assert_eq!(runtime.subrecords[0].data, 0x0200_0920u32.to_le_bytes());
    let source: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
            [0x0200_0930],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source, payload);
}

/// Correct primitive/target contracts do not accept malformed widths or invalid master indices.
#[test]
fn r10_malformed_flags_and_onam_still_diagnose() {
    let mut fixture = Fixture::new();
    let body = [
        native_record(b"SNDR", 0x900, 0, &sub(b"FNAM", &[0x11, 0, 0])),
        native_record(b"NAVM", 0x901, 0, &sub(b"ONAM", &[0x11, 0, 0])),
        native_record(
            b"NAVM",
            0x902,
            0,
            &sub(b"ONAM", &0x0500_0920u32.to_le_bytes()),
        ),
    ]
    .concat();
    fixture.add("Malformed.esm", &[], 1, &body);
    let result =
        records::read_plugins(&fixture.paths, &LoadOrder::read(&fixture.paths).unwrap()).unwrap();
    let diagnostics = &result.diagnostics["malformed.esm"];
    assert_eq!(diagnostics.skipped_fields, 2);
    assert!(diagnostics.invalid_links >= 1);
    assert_eq!(result.records[&0x900].rejected_fields, [*b"FNAM"]);
    assert_eq!(result.records[&0x901].rejected_fields, [*b"ONAM"]);
    assert_eq!(
        field(&result.records[&0x902], b"ONAM").canonical_bytes,
        [0; 4]
    );
}

/// Keep a missing base, missing/wrong-kind overlays and native null distinct before target clearing.
fn invariant_fixture(speed_count: usize) -> Fixture {
    let mut fixture = Fixture::new();
    let speeds = |start: f32| {
        (0..speed_count)
            .flat_map(|index| (start + index as f32 * 17.0).to_le_bytes())
            .collect::<Vec<_>>()
    };
    let movement = |start: f32| {
        native_record(
            b"MOVT",
            MOVEMENT,
            0,
            &[
                sub(b"EDID", b"NPC_Default_MT\0\0"),
                sub(b"MNAM", b"Marche\xe9\0"),
                sub(b"SPED", &speeds(start)),
            ]
            .concat(),
        )
    };
    let setting = |id: u32, name: &[u8], value: f32| {
        native_record(
            b"GMST",
            id,
            0,
            &[
                sub(b"EDID", &text(name)),
                sub(b"DATA", &value.to_le_bytes()),
            ]
            .concat(),
        )
    };
    let mut baseline = native_record(b"WRLD", 1, 0, &sub(b"EDID", b"InvariantWorld\0"));
    baseline.extend(native_record(
        b"LTEX",
        0x880,
        0,
        &sub(b"EDID", b"ExistingTexture\0"),
    ));
    baseline.extend(native_record(
        b"STAT",
        0x881,
        0,
        &sub(b"EDID", b"WrongTargetKind\0"),
    ));
    baseline.extend(movement(5.0));
    baseline.extend(setting(WALK_SETTING, b"fMoveCharWalkBase", 7.0));
    baseline.extend(setting(JUMP_SETTING, b"fJumpHeightMin", 11.0));
    baseline.extend(native_record(
        b"RACE",
        RACE_OVERRIDE,
        0,
        &[
            sub(b"EDID", b"EarlierRace\0"),
            sub(b"WKMV", &MOVEMENT.to_le_bytes()),
            sub(b"RNMV", &MOVEMENT.to_le_bytes()),
        ]
        .concat(),
    ));
    let mut old_cell = exterior_cell(0x8a0);
    old_cell.extend(native_group(
        8,
        0x8a0,
        &native_record(b"LAND", TERRAIN, 0, &sub(b"VHGT", &heights(7.0))),
    ));
    baseline.extend(native_group(1, 1, &old_cell));
    fixture.add("Skyrim.esm", &[], 1, &baseline);
    fixture.add("Gap.esm", &[], 1, &[]);

    let mut patch = movement(13.0);
    patch.extend(setting(WALK_SETTING, b"fMoveCharWalkBase", 17.25));
    patch.extend(setting(JUMP_SETTING, b"fJumpHeightMin", -23.5));
    patch.extend(native_record(
        b"LTEX",
        0x0100_0a20,
        0,
        &sub(b"EDID", b"SelfTexture\0"),
    ));
    patch.extend(native_record(
        b"RACE",
        RACE_OVERRIDE,
        0,
        &[
            sub(b"EDID", b"Caf\xe9\0"),
            sub(b"FULL", &42u32.to_le_bytes()),
            sub(b"WKMV", &0x89bu32.to_le_bytes()),
            sub(b"RNMV", &0x881u32.to_le_bytes()),
        ]
        .concat(),
    ));
    patch.extend(native_record(
        b"RACE",
        0x0100_0901,
        0,
        &[
            sub(b"EDID", b"MissingName\0"),
            sub(b"FULL", &43u32.to_le_bytes()),
            sub(b"WKMV", &[0; 4]),
            sub(b"RNMV", &[0; 4]),
        ]
        .concat(),
    ));
    patch.extend(native_record(
        b"RACE",
        0x0100_0902,
        0,
        &[
            sub(b"EDID", b"NullNameAndAbsentLinks\0"),
            sub(b"FULL", &[0; 4]),
        ]
        .concat(),
    ));
    let mut terrain = [
        sub(b"DATA", &7u32.to_le_bytes()),
        sub(b"VNML", &[1, 2, 126].repeat(33 * 33)),
        sub(b"VHGT", &heights(13.5)),
        sub(b"VCLR", &[19, 83, 151].repeat(33 * 33)),
        sub(b"BTXT", &layer(0x899, 0, 0)),
    ]
    .concat();
    for (texture, slot, vertex, opacity) in [
        (0x0100_0a20, 1, 2, 0.25),
        (0x89a, 2, 9, 0.75),
        (0x881, 3, 17, 0.375),
        (0, 4, 23, 0.625),
    ] {
        terrain.extend(sub(b"ATXT", &layer(texture, 0, slot)));
        terrain.extend(sub(b"VTXT", &weights(vertex, opacity)));
    }
    terrain.extend(sub(
        b"VTEX",
        &[0x880u32, 0x0100_0a20, 0x89a, 0x881, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
    ));
    let mut cell = exterior_cell(0x8a0);
    cell.extend(native_group(
        8,
        0x8a0,
        &native_record(b"LAND", TERRAIN, 0, &terrain),
    ));
    // A genuine heightless record also exercises native-null removal, implicit base
    // insertion and capacity normalization; every source opacity remains valid.
    let mut heightless = sub(b"BTXT", &layer(0, 2, 0));
    for slot in 1..=8 {
        heightless.extend(sub(b"ATXT", &layer(0x0100_0a20, 2, slot)));
        heightless.extend(sub(b"VTXT", &weights(slot * 3, f32::from(slot) / 16.0)));
    }
    cell.extend(exterior_cell(0x0100_0910));
    cell.extend(native_group(
        8,
        0x0100_0910,
        &native_record(b"LAND", 0x0100_0911, 0, &heightless),
    ));
    patch.extend(native_group(1, 1, &cell));
    fixture.add("LocalizedPatch.esp", &["Skyrim.esm"], 0x80, &patch);

    let strings = fixture.data.join("strings");
    fs::create_dir(&strings).unwrap();
    let resolved = b"Carte\xe9\0";
    let invalid_null = b"Null key must not resolve\0";
    let contents = [resolved.as_slice(), invalid_null.as_slice()].concat();
    let mut bank = 2u32.to_le_bytes().to_vec();
    bank.extend(u32::try_from(contents.len()).unwrap().to_le_bytes());
    bank.extend(42u32.to_le_bytes());
    bank.extend(0u32.to_le_bytes());
    bank.extend(0u32.to_le_bytes());
    bank.extend(u32::try_from(resolved.len()).unwrap().to_le_bytes());
    bank.extend(contents);
    fs::write(strings.join("LocalizedPatch_english.strings"), bank).unwrap();
    fixture
}

/// Each real validator accepts before and after actual final-catalogue target clearing.
#[test]
fn rv_a_runtime_invariant_actual_target_nulling_preserves_candidate_validation() {
    let fixture = invariant_fixture(10);
    let order = LoadOrder::read(&fixture.paths).unwrap();
    let schemas = records::addons::RuntimeSchemas::from_addon_bytes(&[]).unwrap();
    let prepared = records::prepare_runtime_schema(&schemas);
    let mut tables = HashMap::new();
    let mut validate = |record: &records::DecodedRecord| {
        validate_candidate(
            record,
            &prepared,
            &fixture.paths,
            &fixture.data,
            &mut tables,
        )
    };
    let mut before = HashMap::new();
    let result = records::read_plugins_with_validation_and_observer(
        &fixture.paths,
        &order,
        &mut validate,
        |accepted: &ReadResult| before.clone_from(&accepted.records),
    )
    .unwrap();
    for id in [
        MOVEMENT,
        WALK_SETTING,
        JUMP_SETTING,
        RACE_OVERRIDE,
        RACE_MISSING_TEXT,
        RACE_NULL_TEXT,
        TERRAIN,
        HEIGHTLESS_TERRAIN,
    ] {
        let original = &before[&id];
        let nulled = &result.records[&id];
        validate(original).unwrap();
        validate(nulled).unwrap();
        assert_eq!(original.load_order, 2);
        assert_eq!(nulled.load_order, 2);
        assert_eq!(original.raw_payload, nulled.raw_payload);
        assert_eq!(original.source_record_offset, nulled.source_record_offset);
    }
    assert_eq!(
        word(&field(&before[&RACE_OVERRIDE], b"WKMV").canonical_bytes),
        0x89b
    );
    assert_eq!(
        word(&field(&before[&RACE_OVERRIDE], b"RNMV").canonical_bytes),
        0x881
    );
    for tag in [b"WKMV", b"RNMV"] {
        assert_eq!(
            field(&result.records[&RACE_OVERRIDE], tag).canonical_bytes,
            [0; 4]
        );
        assert_eq!(
            field(&result.records[&RACE_MISSING_TEXT], tag).canonical_bytes,
            [0; 4]
        );
        assert!(
            !result.records[&RACE_NULL_TEXT]
                .fields
                .iter()
                .any(|field| &field.signature == tag)
        );
    }
    let original = &before[&TERRAIN];
    let nulled = &result.records[&TERRAIN];
    assert_eq!(word(&field(original, b"BTXT").canonical_bytes), 0x899);
    assert_eq!(word(&field(nulled, b"BTXT").canonical_bytes), 0);
    let overlays = |record: &DecodedRecord| {
        record
            .fields
            .iter()
            .filter(|field| field.signature == *b"ATXT")
            .map(|field| word(&field.canonical_bytes))
            .collect::<Vec<_>>()
    };
    assert_eq!(overlays(original), [SELF_TEXTURE, 0x89a, 0x881, 0]);
    assert_eq!(overlays(nulled), [SELF_TEXTURE, 0, 0, 0]);
    for tag in [b"VHGT", b"VNML", b"VCLR", b"VTXT"] {
        let selected = |record: &DecodedRecord| {
            record
                .fields
                .iter()
                .filter(|field| &field.signature == tag)
                .map(|field| field.canonical_bytes.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(selected(original), selected(nulled));
    }
    let vtex = &field(nulled, b"VTEX").canonical_bytes;
    assert_eq!(vtex.len(), 20);
    assert_eq!(
        vtex.as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| word(bytes))
            .collect::<Vec<_>>(),
        [0x880, SELF_TEXTURE, 0, 0, 0]
    );
    assert_eq!(result.diagnostics["localizedpatch.esp"].skipped_records, 0);
    assert!(result.diagnostics["localizedpatch.esp"].invalid_links >= 6);
}

/// Actual text/localization export preserves valid overrides and both native/runtime layer views.
#[test]
fn rv_a_runtime_invariant_actual_text_localization_and_cache_publication() {
    let fixture = invariant_fixture(11);
    let (records, db, output) = fixture.publish(None, &[*b"RACE", *b"MOVT", *b"GMST", *b"LAND"]);
    let race = &records[&RACE_OVERRIDE];
    let view = SubrecordView::new(&race.subrecords);
    assert_eq!(view.find(b"EDID").unwrap(), "Caf\u{e9}\0".as_bytes());
    assert_eq!(view.find(b"FULL").unwrap(), "Carte\u{e9}\0".as_bytes());
    for id in [RACE_MISSING_TEXT, RACE_NULL_TEXT] {
        assert_eq!(
            SubrecordView::new(&records[&id].subrecords).find(b"FULL"),
            Some(&[0][..])
        );
    }
    let movement = SubrecordView::new(&records[&MOVEMENT].subrecords);
    assert_eq!(movement.find(b"EDID"), Some(&b"NPC_Default_MT\0"[..]));
    assert_eq!(movement.find(b"MNAM"), Some("Marche\u{e9}\0".as_bytes()));
    for raw in records
        .values()
        .filter(|record| matches!(&record.record_type, b"LAND" | b"MOVT" | b"GMST" | b"RACE"))
    {
        validate_inhouse_record(raw).unwrap();
        if raw.record_type == *b"LAND" {
            validate_inhouse_land(raw).unwrap();
        }
        assert_eq!(raw.load_order, 2);
        let supplier: u32 = db
            .query_row(
                "SELECT load_order FROM records WHERE form_id=?1",
                [raw.form_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(supplier, 2);
    }
    assert_eq!(
        db.query_row(
            "SELECT forward_walk,forward_run,load_order FROM movement_types WHERE id=?1",
            [MOVEMENT],
            |row| Ok((
                row.get::<_, f32>(0)?,
                row.get::<_, f32>(1)?,
                row.get::<_, u32>(2)?
            ))
        )
        .unwrap(),
        (81.0, 98.0, 2)
    );
    for (name, expected) in [
        ("fMoveCharWalkBase", 17.25f32),
        ("fJumpHeightMin", -23.5f32),
    ] {
        assert_eq!(
            db.query_row(
                "SELECT value,load_order FROM movement_game_settings WHERE editor_id=?1",
                [name],
                |row| Ok((row.get::<_, f32>(0)?, row.get::<_, u32>(1)?))
            )
            .unwrap(),
            (expected, 2)
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT walk_movt_id,run_movt_id,load_order FROM race_movement_links WHERE id=?1",
            [RACE_OVERRIDE],
            |row| Ok((
                row.get::<_, Option<u32>>(0)?,
                row.get::<_, Option<u32>>(1)?,
                row.get::<_, u32>(2)?
            ))
        )
        .unwrap(),
        (None, None, 2)
    );
    let source: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
            [RACE_OVERRIDE],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(&source[..11], &sub(b"EDID", b"Caf\xe9\0"));
    let states: Vec<(u32, String, Option<String>)> = db.prepare("SELECT plugin_id,status,reason FROM record_overrides WHERE form_id=?1 ORDER BY chain_position").unwrap().query_map([RACE_OVERRIDE], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        states,
        [(0, "accepted".into(), None), (2, "accepted".into(), None)]
    );
    let localized: Vec<(u32, String, u32)> = db.prepare("SELECT form_id,status,load_order FROM inhouse_localized_fields WHERE form_id IN (?1,?2,?3) ORDER BY form_id").unwrap().query_map([RACE_OVERRIDE, RACE_MISSING_TEXT, RACE_NULL_TEXT], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        localized,
        [
            (RACE_OVERRIDE, "resolved".into(), 2),
            (RACE_MISSING_TEXT, "missing".into(), 2),
            (RACE_NULL_TEXT, "null_id".into(), 2)
        ]
    );
    let read_cache = |name| {
        rkyv::from_bytes::<shared::CellCache, rkyv::rancor::Error>(
            &fs::read(output.join(name)).unwrap(),
        )
        .unwrap()
    };
    let runtime = read_cache("cell_cache.rkyv");
    let preserved = read_cache("cell_cache_preserved.rkyv");
    assert_eq!(runtime.cells.len(), 2);
    let runtime_land = runtime
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x8a0)
        .unwrap();
    let preserved_land = preserved
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x8a0)
        .unwrap();
    assert_eq!(runtime_land.heights, vec![108.0; 33 * 33]);
    assert_eq!(runtime_land.vertex_colors, [19, 83, 151].repeat(33 * 33));
    assert_eq!(runtime_land.normals, preserved_land.normals);
    assert_eq!(
        preserved_land
            .layers
            .iter()
            .map(|layer| layer.texture_form_id)
            .collect::<Vec<_>>(),
        [0x899, SELF_TEXTURE, 0x89a, 0x881]
    );
    assert_eq!(
        runtime_land
            .layers
            .iter()
            .map(|layer| layer.texture_form_id)
            .collect::<Vec<_>>(),
        [0, SELF_TEXTURE, 0, 0]
    );
    assert_eq!(
        runtime_land
            .layers
            .iter()
            .map(|layer| layer.weights.clone())
            .collect::<Vec<_>>(),
        preserved_land
            .layers
            .iter()
            .map(|layer| layer.weights.clone())
            .collect::<Vec<_>>()
    );
    assert!(!preserved_land.layers.iter().any(|layer| layer.layer == 4));
    // R4 preserves every normalized overlay; only runtime applies six-layer capacity.
    for (cache, expected_slots) in [
        (&runtime, &[0_u16, 4, 5, 6, 7, 8][..]),
        (&preserved, &[0_u16, 1, 2, 3, 4, 5, 6, 7, 8][..]),
    ] {
        let heightless = cache
            .cells
            .iter()
            .find(|cell| cell.cell_id == 0x0200_0910)
            .unwrap();
        assert_eq!(heightless.heights, vec![0.0; 33 * 33]);
        assert_eq!(heightless.layers.len(), expected_slots.len());
        assert!(heightless.layers[0].is_base);
        assert_eq!(heightless.layers[0].texture_form_id, 0);
        assert!(heightless.layers[0].weights.is_empty());
        assert_eq!(
            heightless.layers[1..]
                .iter()
                .map(|layer| layer.layer)
                .collect::<Vec<_>>(),
            expected_slots[1..]
        );
        assert_eq!(heightless.layers[0].layer, 0);
        assert!(heightless.layers.iter().all(|layer| layer.quadrant == 2));
        for (&slot, layer) in expected_slots[1..].iter().zip(&heightless.layers[1..]) {
            assert!(!layer.is_base);
            assert_eq!(layer.texture_form_id, SELF_TEXTURE);
            assert_eq!(
                layer
                    .weights
                    .iter()
                    .map(|weight| (weight.vertex, weight.opacity.to_bits()))
                    .collect::<Vec<_>>(),
                [(slot * 3, (f32::from(slot) / 16.0).to_bits())]
            );
        }
    }
    let typed = fs::read_to_string(output.join("typed-records.jsonl")).unwrap();
    let rows = typed
        .lines()
        .map(|line| serde_json::from_str::<Json>(line).unwrap())
        .collect::<Vec<_>>();
    let typed_race = rows
        .iter()
        .find(|row| row["form_id"] == RACE_OVERRIDE)
        .unwrap();
    assert_eq!(typed_race["load_order"], 2);
    assert_eq!(typed_race["localization"]["1"]["status"], "resolved");
    assert_eq!(typed_race["localization"]["1"]["text"], "Carte\u{e9}");
    assert!(!typed.contains("\"Deferred\""));
}

/// A native EDID suffix can hide a required setting until the actual text transform runs.
#[test]
fn rv_a_runtime_late_validation_text_normalization_preserves_predecessor() {
    let mut fixture = Fixture::new();
    let prior_setting = [
        sub(b"EDID", b"fMoveCharWalkBase\0"),
        sub(b"DATA", &37.25f32.to_le_bytes()),
    ]
    .concat();
    let mut baseline = native_record(b"GMST", WALK_SETTING, 0, &prior_setting);
    baseline.extend(native_record(
        b"LAND",
        0x8a1,
        0,
        &sub(b"VHGT", &heights(7.0)),
    ));
    fixture.add("Skyrim.esm", &[], 1, &baseline);
    fixture.add("Gap.esm", &[], 1, &[]);
    let mut patch = native_record(
        b"GMST",
        WALK_SETTING,
        0,
        &sub(b"EDID", b"fMoveCharWalkBase\0suffix"),
    );
    patch.extend(native_record(
        b"GMST",
        0x0100_0903,
        0,
        &[
            sub(b"EDID", b"fJumpHeightMin\0valid_suffix"),
            sub(b"DATA", &19.5f32.to_le_bytes()),
        ]
        .concat(),
    ));
    patch.extend(native_record(
        b"STAT",
        0x0100_0901,
        0,
        &sub(b"EDID", b"LateValidationNeighbor\0"),
    ));
    fixture.add("Patch.esp", &["Skyrim.esm"], 0, &patch);
    let (records, db, output) = fixture.publish(None, &[*b"GMST"]);
    let count: u32 = db
        .query_row(
            "SELECT count(*) FROM records WHERE form_id=?1",
            [WALK_SETTING],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "late failure must retain the earlier usable setting"
    );
    let winner = &records[&WALK_SETTING];
    assert_eq!(winner.load_order, 0);
    assert_eq!(
        SubrecordView::new(&winner.subrecords).find(b"DATA"),
        Some(37.25f32.to_le_bytes().as_slice())
    );
    assert!(records.contains_key(&0x0200_0901));
    assert_eq!(db.query_row("SELECT value,load_order FROM movement_game_settings WHERE editor_id='fMoveCharWalkBase'", [], |row| Ok((row.get::<_, f32>(0)?, row.get::<_, u32>(1)?))).unwrap(), (37.25, 0));
    assert_eq!(
        db.query_row(
            "SELECT value,load_order FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| Ok((row.get::<_, f32>(0)?, row.get::<_, u32>(1)?))
        )
        .unwrap(),
        (19.5, 2)
    );
    let (supplier, source): (u32, Vec<u8>) = db
        .query_row(
            "SELECT load_order,payload FROM inhouse_source_records WHERE form_id=?1",
            [WALK_SETTING],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((supplier, source), (0, prior_setting));
    let states: Vec<(u32, String, Option<String>)> = db.prepare("SELECT plugin_id,status,reason FROM record_overrides WHERE form_id=?1 ORDER BY chain_position").unwrap().query_map([WALK_SETTING], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(states.len(), 2);
    assert_eq!((states[0].0, states[0].1.as_str()), (0, "accepted"));
    assert_eq!((states[1].0, states[1].1.as_str()), (2, "skipped"));
    assert!(states[1].2.as_deref().unwrap().contains("DATA"));
    let typed = fs::read_to_string(output.join("typed-records.jsonl")).unwrap();
    let rows = typed
        .lines()
        .map(|line| serde_json::from_str::<Json>(line).unwrap())
        .collect::<Vec<_>>();
    let row = rows
        .iter()
        .find(|row| row["form_id"] == WALK_SETTING)
        .unwrap();
    assert_eq!(row["form_id"], WALK_SETTING);
    assert_eq!(row["load_order"], 0);
    for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
        let cache = rkyv::from_bytes::<shared::CellCache, rkyv::rancor::Error>(
            &fs::read(output.join(name)).unwrap(),
        )
        .unwrap();
        let neighbor = cache
            .cells
            .iter()
            .find(|cell| cell.cell_id == 0x8a1)
            .unwrap();
        assert_eq!(neighbor.heights, vec![56.0; 33 * 33]);
    }
}

/// Missing DATA in an activated first-definition selector is skipped in history as well as output.
#[test]
fn rv_a_runtime_late_validation_text_normalization_skips_first_definition() {
    let mut fixture = Fixture::new();
    fixture.add("Skyrim.esm", &[], 1, &[]);
    fixture.add("Gap.esm", &[], 1, &[]);
    let mut body = native_record(
        b"GMST",
        0x0100_0900,
        0,
        &sub(b"EDID", b"fMoveCharWalkBase\0suffix"),
    );
    body.extend(native_record(
        b"STAT",
        0x0100_0901,
        0,
        &sub(b"EDID", b"FirstDefinitionNeighbor\0"),
    ));
    fixture.add("Patch.esp", &["Skyrim.esm"], 0, &body);
    let (records, db, output) = fixture.publish(None, &[*b"GMST"]);
    assert!(!records.contains_key(&0x0200_0900));
    assert!(records.contains_key(&0x0200_0901));
    let (status, reason): (String, Option<String>) = db
        .query_row(
            "SELECT status,reason FROM record_overrides WHERE source_form_id=?1 AND plugin_id=2",
            [0x0100_0900u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        status, "skipped",
        "an unusable first definition cannot remain an accepted physical winner"
    );
    assert!(reason.as_deref().unwrap().contains("DATA"));
    assert!(
        fs::read_to_string(output.join("typed-records.jsonl"))
            .unwrap()
            .trim()
            .is_empty()
    );
}

/// The altered schema makes the actual replay decoder fail; it is not a retail-schema repro.
fn replay_fixture(with_predecessor: bool) -> Fixture {
    let mut fixture = Fixture::new();
    let mut baseline = native_record(b"FACT", 0x880, 0, &sub(b"EDID", b"ReplayOwner\0"));
    if with_predecessor {
        baseline.extend(native_record(
            b"TEST",
            0x900,
            0,
            &sub(b"EDID", b"EarlierVersion\0"),
        ));
    }
    baseline.extend(native_record(
        b"LAND",
        0x8a1,
        0,
        &sub(b"VHGT", &heights(7.0)),
    ));
    fixture.add("Skyrim.esm", &[], 1, &baseline);
    fixture.add("Gap.esm", &[], 1, &[]);
    let coed = [
        0x880u32.to_le_bytes(),
        0x1379_2468u32.to_le_bytes(),
        0.375f32.to_le_bytes(),
    ]
    .concat();
    let mut patch = native_record(
        b"TEST",
        if with_predecessor { 0x900 } else { 0x0100_0900 },
        0,
        &[
            sub(b"EDID", b"FailedDeferredVersion\0"),
            sub(b"COED", &coed),
        ]
        .concat(),
    );
    patch.extend(native_record(
        b"STAT",
        0x0100_0901,
        0,
        &sub(b"EDID", b"UsableReplayNeighbor\0"),
    ));
    fixture.add("Patch.esp", &["Skyrim.esm"], 0, &patch);
    fixture
}

/// A replay-rejected override must publish its earlier complete owner and never Deferred bytes.
#[test]
fn rv_a_deferred_replay_failure_publishes_predecessor_and_neighbor_with_private_schema() {
    let fixture = replay_fixture(true);
    let schema = records::review_recovery_tests::replay_failure_schema();
    let (records, db, output) = fixture.publish(Some(&schema), &[*b"TEST"]);
    let winner = &records[&0x900];
    assert_eq!(winner.load_order, 0);
    assert_eq!(
        SubrecordView::new(&winner.subrecords).find(b"EDID"),
        Some(&b"EarlierVersion\0"[..])
    );
    assert!(
        winner
            .subrecords
            .iter()
            .all(|(tag, _)| tag.as_slice() != b"COED")
    );
    assert!(
        canonical(&db, 0x900)
            .subrecords
            .iter()
            .all(|field| field.tag != *b"COED")
    );
    assert!(records.contains_key(&0x0200_0901));
    let (supplier, source): (u32, Vec<u8>) = db
        .query_row(
            "SELECT load_order,payload FROM inhouse_source_records WHERE form_id=2304",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(supplier, 0);
    assert_eq!(source, sub(b"EDID", b"EarlierVersion\0"));
    let states: Vec<(u32, String, Option<String>)> = db.prepare("SELECT plugin_id,status,reason FROM record_overrides WHERE form_id=2304 ORDER BY chain_position").unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(states.len(), 2);
    assert_eq!((states[0].0, states[0].1.as_str()), (0, "accepted"));
    assert_eq!((states[1].0, states[1].1.as_str()), (2, "skipped"));
    assert!(states[1].2.as_deref().unwrap().contains("deferred"));
    assert!(states[1].2.as_deref().unwrap().contains("i32"));
    let typed = fs::read_to_string(output.join("typed-records.jsonl")).unwrap();
    assert!(!typed.contains("\"Deferred\""));
    let row: Json = serde_json::from_str(typed.trim()).unwrap();
    assert_eq!(row["load_order"], 0);
    for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
        let cache = rkyv::from_bytes::<shared::CellCache, rkyv::rancor::Error>(
            &fs::read(output.join(name)).unwrap(),
        )
        .unwrap();
        let neighbor = cache
            .cells
            .iter()
            .find(|cell| cell.cell_id == 0x8a1)
            .unwrap();
        assert_eq!(neighbor.heights, vec![56.0; 33 * 33]);
    }
}

/// A first definition retains safe fields and archival source but omits its unresolved whole field.
#[test]
fn rv_a_deferred_replay_failure_publishes_safe_first_definition_with_private_schema() {
    let fixture = replay_fixture(false);
    let schema = records::review_recovery_tests::replay_failure_schema();
    let (records, db, output) = fixture.publish(Some(&schema), &[*b"TEST"]);
    let id = 0x0200_0900;
    let winner = &records[&id];
    assert_eq!(winner.load_order, 2);
    assert_eq!(winner.form_id, id);
    assert_eq!(
        SubrecordView::new(&winner.subrecords).find(b"EDID"),
        Some(&b"FailedDeferredVersion\0"[..])
    );
    assert!(
        winner
            .subrecords
            .iter()
            .all(|(tag, _)| tag.as_slice() != b"COED")
    );
    assert!(
        canonical(&db, id)
            .subrecords
            .iter()
            .all(|field| field.tag != *b"COED")
    );
    let (source_id, supplier, source): (u32, u32, Vec<u8>) = db
        .query_row(
            "SELECT source_form_id,load_order,payload FROM inhouse_source_records WHERE form_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((source_id, supplier), (0x0100_0900, 2));
    assert!(
        source.windows(4).any(|bytes| bytes == b"COED"),
        "archival native bytes remain separate from runtime fields"
    );
    assert!(records.contains_key(&0x0200_0901));
    let (status, reason): (String, Option<String>) = db
        .query_row(
            "SELECT status,reason FROM record_overrides WHERE form_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "accepted");
    assert!(reason.as_deref().unwrap().contains("deferred"));
    let typed = fs::read_to_string(output.join("typed-records.jsonl")).unwrap();
    assert!(!typed.contains("\"Deferred\""));
    let row: Json = serde_json::from_str(typed.trim()).unwrap();
    assert_eq!(row["form_id"], id);
    assert_eq!(row["source_form_id"], 0x0100_0900u32);
    assert_eq!(row["load_order"], 2);
}

/// Ordinary embedded-schema COED damage stays field-local and completes real publication.
#[test]
fn rv_a_deferred_builtin_damaged_optional_coed_publishes_safe_neighbors() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Skyrim.esm",
        &[],
        1,
        &native_record(b"MISC", 0x880, 0, &sub(b"EDID", b"InventoryItem\0")),
    );
    fixture.add("Gap.esm", &[], 1, &[]);
    let inventory = [0x880u32.to_le_bytes(), 3i32.to_le_bytes()].concat();
    let mut body = native_record(
        b"CONT",
        0x0100_0900,
        0,
        &[
            sub(b"EDID", b"BrokenOptionalInventory\0"),
            sub(b"CNTO", &inventory),
            sub(b"COED", &[0x79; 11]),
        ]
        .concat(),
    );
    body.extend(native_record(
        b"STAT",
        0x0100_0901,
        0,
        &sub(b"EDID", b"UsableCOEDNeighbor\0"),
    ));
    fixture.add("Patch.esp", &["Skyrim.esm"], 0, &body);
    let (records, db, output) = fixture.publish(None, &[*b"CONT"]);
    let winner = &records[&0x0200_0900];
    let view = SubrecordView::new(&winner.subrecords);
    assert_eq!(view.find(b"EDID"), Some(&b"BrokenOptionalInventory\0"[..]));
    assert!(view.find(b"COED").is_none());
    assert!(
        canonical(&db, winner.form_id)
            .subrecords
            .iter()
            .all(|field| field.tag != *b"COED")
    );
    assert!(records.contains_key(&0x0200_0901));
    let count: u32 = db
        .query_row(
            "SELECT count(*) FROM statics WHERE id IN (?1,?2)",
            [0x0200_0900u32, 0x0200_0901],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let diagnostics: Json =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["patch.esp"]["skipped_records"], 0);
    assert_eq!(diagnostics["decoder"]["patch.esp"]["skipped_fields"], 1);
    assert!(
        !fs::read_to_string(output.join("typed-records.jsonl"))
            .unwrap()
            .contains("\"Deferred\"")
    );
}

/// Both readers must archive identical canonical links when a patch's local
/// master indices differ from normal/light load-order identities.
#[test]
fn race_base_movement_links_match_legacy_and_inhouse_publication() {
    let tags = [b"WKMV", b"RNMV", b"SWMV", b"FLMV", b"SNMV", b"SPMV"];
    for light in [false, true] {
        for first_id in [0x340u32, 0x440] {
            let mut fixture = Fixture::new();
            fixture.add("Skyrim.esm", &[], 1, &[]);
            fixture.add("Gap.esm", &[], 1, &[]);
            let mut body = Vec::new();
            let mut race = sub(b"EDID", b"MovementRace\0");
            for (index, tag) in tags.iter().enumerate() {
                let id = 0x0100_0000 | (first_id + index as u32);
                race.extend(sub(tag, &id.to_le_bytes()));
                body.extend(native_record(
                    b"MOVT",
                    id,
                    0,
                    &[
                        sub(b"EDID", format!("Movement{index}\0").as_bytes()),
                        sub(b"MNAM", format!("Movement{index}\0").as_bytes()),
                        sub(b"SPED", &[100.0f32.to_le_bytes(); 11].concat()),
                    ]
                    .concat(),
                ));
            }
            // Override a base-owned race. Only the link targets are patch-owned.
            body.extend(native_record(b"RACE", 0x900, 0, &race));
            fixture.add(
                "Movement.esp",
                &["Skyrim.esm"],
                if light { 0x200 } else { 0 },
                &body,
            );
            let (inhouse, db, _) = fixture.publish(None, &[*b"RACE", *b"MOVT"]);
            let legacy = crate::esm::EsmParser::merge_plugins(&fixture.paths).unwrap();
            let legacy_db = Connection::open_in_memory().unwrap();
            create_tables(&legacy_db).unwrap();
            crate::esm::exporter::export_to_db(&legacy_db, &legacy).unwrap();
            let inhouse_stored = canonical(&db, 0x900);
            let legacy_stored = canonical(&legacy_db, 0x900);
            for (index, tag) in tags.iter().enumerate() {
                let expected =
                    (if light { 0xFE00_0000 } else { 0x0200_0000 }) | (first_id + index as u32);
                for records in [&inhouse, &legacy] {
                    let bytes = SubrecordView::new(&records[&0x900].subrecords);
                    assert_eq!(word(bytes.find(tag).unwrap()), expected);
                }
                for stored in [&inhouse_stored, &legacy_stored] {
                    let field = stored
                        .subrecords
                        .iter()
                        .find(|field| &field.tag == *tag)
                        .unwrap();
                    assert_eq!(word(&field.data), expected);
                }
            }
            assert_eq!(inhouse[&0x900].load_order, 2);
            assert_eq!(legacy[&0x900].load_order, 2);
        }
    }
}
