//! Item-family typed decoding and real publication from independently authored fixtures.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{inhouse_items as items, layout};
use std::{fs, path::PathBuf};

/// A dummy-content world and a full-slot dependent plugin with nonidentity indices.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    generated: Vec<u8>,
    paths: Vec<PathBuf>,
}
impl Fixture {
    /// Generate a genuine world/assets rather than mocking pipeline consumers.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        layout::prepare_directory(&data, false).unwrap();
        layout::generate(
            &data,
            layout::DEFAULT_SEED,
            layout::Formats::parse("dds,nif,pex,esm").unwrap(),
        )
        .unwrap();
        let base = data.join("Skyrim.esm");
        let generated = fs::read(&base).unwrap();
        let filler = data.join("Filler.esm");
        fs::write(
            &filler,
            items::dependent_plugin(&generated, &["Skyrim.esm"], &[]),
        )
        .unwrap();
        Self {
            directory,
            data,
            generated,
            paths: vec![base, filler],
        }
    }
    /// Append a plugin with an independently chosen master list.
    fn add(&mut self, name: &str, masters: &[&str], records: &[Vec<u8>]) {
        let path = self.data.join(name);
        fs::write(
            &path,
            items::dependent_plugin(&self.generated, masters, records),
        )
        .unwrap();
        self.paths.push(path);
    }
    /// Exercise the public typed frontend through its current load-order owner.
    fn read(&self) -> ReadResult {
        let order = LoadOrder::read(&self.paths).unwrap();
        read_plugins(&self.paths, &order).unwrap()
    }
}

/// Read a matched signature while preserving repeated subrecord occurrences.
fn field<'a>(record: &'a DecodedRecord, tag: &[u8; 4]) -> &'a Value {
    &record
        .fields
        .iter()
        .find(|field| &field.signature == tag)
        .unwrap()
        .value
}
/// Read one exact authored struct member, including signed values and padding.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// All fifteen families are supported and packed native scalar layouts remain distinct.
#[test]
fn all_item_families_decode_packed_values_and_header_metadata() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Items.esp",
        &["Skyrim.esm"],
        &items::minimal_family_records(0x0100_0900),
    );
    let decoded = fixture.read();
    for index in 0..15 {
        let record = &decoded.records[&(0x0200_0900 + index)];
        assert!(record.supported);
        assert!(
            record.rejected_fields.is_empty(),
            "{:?}",
            record.rejected_fields
        );
        assert_eq!(record.version_control, 0x1941_9626);
        assert_eq!(record.form_version, 44);
        assert_eq!(record.header_unknown, 0x07a9);
    }
    let weapon = &decoded.records[&0x0200_0900];
    assert_eq!(
        member(field(weapon, b"DATA"), "damage"),
        &Value::Unsigned(43)
    );
    assert_eq!(
        member(field(weapon, b"DNAM"), "flags"),
        &Value::Unsigned(0x89)
    );
    assert_eq!(
        member(field(weapon, b"DNAM"), "flags_2"),
        &Value::Unsigned(0x2031)
    );
    assert_eq!(
        member(field(weapon, b"DNAM"), "range_min"),
        &Value::Float(37.25)
    );
    assert_eq!(
        member(field(weapon, b"DNAM"), "range_max"),
        &Value::Float(809.5)
    );
    assert_eq!(member(field(weapon, b"DNAM"), "skill"), &Value::Signed(-13));
    assert_eq!(
        member(field(weapon, b"CRDT"), "unused_09"),
        &Value::Bytes(vec![0xa7, 0x51, 0xd2, 0x38, 0xc4, 0x19, 0x96])
    );
    assert_eq!(
        member(field(&decoded.records[&0x0200_0901], b"DATA"), "value"),
        &Value::Signed(-123)
    );
    assert_eq!(
        field(&decoded.records[&0x0200_0901], b"DNAM"),
        &Value::Signed(731)
    );
    assert_eq!(
        member(
            field(&decoded.records[&0x0200_0902], b"DNAM"),
            "female_priority"
        ),
        &Value::Unsigned(37)
    );
    assert_eq!(
        member(field(&decoded.records[&0x0200_0903], b"DATA"), "weight"),
        &Value::Float(0.625)
    );
    assert_eq!(
        member(field(&decoded.records[&0x0200_0904], b"DATA"), "teaches"),
        &Value::Signed(-17)
    );
    assert_eq!(
        member(
            field(&decoded.records[&0x0200_0907], b"ENIT"),
            "ingredient_value"
        ),
        &Value::Signed(-271)
    );
    assert_eq!(
        member(
            field(&decoded.records[&0x0200_0908], b"ENIT"),
            "addiction_chance"
        ),
        &Value::Float(0.375)
    );
    assert_eq!(
        field(&decoded.records[&0x0200_0909], b"SOUL"),
        &Value::Unsigned(3)
    );
    assert_eq!(
        field(&decoded.records[&0x0200_090b], b"QUAL"),
        &Value::Signed(3)
    );
    assert_eq!(
        member(field(&decoded.records[&0x0200_090c], b"DATA"), "weight"),
        &Value::Float(13.625)
    );
    assert_eq!(
        field(&decoded.records[&0x0200_090d], b"NAM1"),
        &Value::Unsigned(7)
    );
    assert_eq!(
        field(&decoded.records[&0x0200_090e], b"DATA"),
        &Value::Unsigned(1)
    );
    assert_eq!(decoded.diagnostics["items.esp"].skipped_records, 0);
}

/// Teaching spell links resolve with an asymmetric master order; skills stay signed.
#[test]
fn book_teaching_context_selects_spell_without_remapping_signed_skills() {
    let mut fixture = Fixture::new();
    let records = vec![
        items::record(
            b"SPEL",
            0x0100_0830,
            &[(b"EDID", items::text("SyntheticSpell"))],
        ),
        items::book(0x0100_0900, 1, (-13_i32) as u32),
        items::book(0x0100_0901, 4, 0x0100_0830),
        items::book(0x0100_0902, 5, 0x0100_0830),
    ];
    fixture.add("Items.esp", &["Skyrim.esm"], &records);
    let decoded = fixture.read();
    assert_eq!(
        member(field(&decoded.records[&0x0200_0900], b"DATA"), "teaches"),
        &Value::Signed(-13)
    );
    for id in [0x0200_0901, 0x0200_0902] {
        let book = &decoded.records[&id];
        assert_eq!(
            member(field(book, b"DATA"), "teaches"),
            &Value::FormId(0x0200_0830)
        );
        let bytes = &book
            .fields
            .iter()
            .find(|field| field.signature == *b"DATA")
            .unwrap()
            .canonical_bytes;
        assert_eq!(&bytes[2..4], &[0xa7, 0x51]);
        assert_eq!(&bytes[4..8], &0x0200_0830_u32.to_le_bytes());
    }
}

/// Absent whole native tails are allowed; partially truncated tails reject their candidate.
#[test]
fn ammo_and_legacy_body_template_optional_tails_preserve_authored_presence() {
    let mut fixture = Fixture::new();
    let mut legacy = 0x1234_5678_u32.to_le_bytes().to_vec();
    legacy.extend([0x11, 0xa7, 0x51, 0xd2]);
    let mut complete = legacy.clone();
    complete.extend(2_u32.to_le_bytes());
    let mut ammo = vec![0; 16];
    ammo[8..12].copy_from_slice(&17.625_f32.to_le_bytes());
    fixture.add(
        "Items.esp",
        &["Skyrim.esm"],
        &[
            items::record(b"ARMO", 0x0100_0900, &[(b"BODT", legacy)]),
            items::record(b"ARMO", 0x0100_0901, &[(b"BODT", complete)]),
            items::record(b"AMMO", 0x0100_0902, &[(b"DATA", ammo.clone())]),
            items::record(
                b"AMMO",
                0x0100_0903,
                &[(b"DATA", {
                    ammo.push(0xa7);
                    ammo
                })],
            ),
        ],
    );
    let decoded = fixture.read();
    let Value::Struct(short) = field(&decoded.records[&0x0200_0900], b"BODT") else {
        panic!()
    };
    assert!(!short.iter().any(|(name, _)| name == "armor_type"));
    assert_eq!(
        member(field(&decoded.records[&0x0200_0901], b"BODT"), "armor_type"),
        &Value::Unsigned(2)
    );
    let Value::Struct(short) = field(&decoded.records[&0x0200_0902], b"DATA") else {
        panic!()
    };
    assert!(!short.iter().any(|(name, _)| name == "weight"));
    assert!(!decoded.records.contains_key(&0x0200_0903));
    assert!(decoded.diagnostics["items.esp"].skipped_records > 0);
}

/// A malformed present BOOK DATA leaves its usable predecessor and publishes valid neighbors.
#[tokio::test]
async fn malformed_item_override_preserves_prior_winner_and_publishes_neighbors() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Items.esp",
        &["Skyrim.esm"],
        &[
            items::book(0x0100_0900, 1, (-13_i32) as u32),
            items::record(
                b"MISC",
                0x0100_0901,
                &[
                    (b"EDID", items::text("PriorNeighbor")),
                    (b"DATA", vec![0; 8]),
                ],
            ),
        ],
    );
    fixture.add(
        "BadItems.esp",
        &["Skyrim.esm", "Items.esp"],
        &[
            items::record(
                b"BOOK",
                0x0100_0900,
                &[
                    (b"EDID", items::text("BrokenBook")),
                    (b"DATA", vec![0xa7; 15]),
                ],
            ),
            items::record(
                b"APPA",
                0x0200_0930,
                &[
                    (b"EDID", items::text("GoodNewNeighbor")),
                    (b"QUAL", 3_i32.to_le_bytes().to_vec()),
                    (b"DATA", vec![0; 8]),
                ],
            ),
        ],
    );
    let list = fixture.directory.path().join("plugins.txt");
    fs::write(&list, "Skyrim.esm\nFiller.esm\n*Items.esp\n*BadItems.esp\n").unwrap();
    let output = fixture.directory.path().join("published");
    let mut config = PipelineConfig::new(&fixture.data, &output);
    config.record_reader = RecordReader::Inhouse;
    config.plugins_file = Some(list);
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x0200_0900_u32, 0x0200_0901, 0x0300_0930] {
        assert_eq!(
            database
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE form_id=?1",
                    [id],
                    |row| row.get::<_, u32>(0)
                )
                .unwrap(),
            1
        );
    }
    let blob: Vec<u8> = database
        .query_row(
            "SELECT data FROM records WHERE form_id=?1",
            [0x0200_0900_u32],
            |row| row.get(0),
        )
        .unwrap();
    let data =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&blob)
            .unwrap();
    let book = data
        .subrecords
        .iter()
        .find(|field| field.tag == *b"DATA")
        .unwrap();
    assert_eq!(book.data.len(), 16);
    assert_eq!(&book.data[4..8], &(-13_i32).to_le_bytes());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert!(
        diagnostics["decoder"]["baditems.esp"]["skipped_records"]
            .as_u64()
            .unwrap()
            > 0
    );
}

/// Gendered model paths remain separate from repeated armor-addon and race links.
#[test]
fn armor_models_and_same_tag_links_keep_their_native_context() {
    let mut fixture = Fixture::new();
    let body = [0x1234_5678_u32.to_le_bytes(), 2_u32.to_le_bytes()].concat();
    let addon = items::record(
        b"ARMA",
        0x0100_0901,
        &[
            (b"BOD2", body.clone()),
            (b"MOD2", items::text("male_biped.nif")),
            (b"MOD3", items::text("female_biped.nif")),
            (b"MOD4", items::text("male_first.nif")),
            (b"MOD5", items::text("female_first.nif")),
            (b"MODL", 0x0100_0840_u32.to_le_bytes().to_vec()),
        ],
    );
    let armor = items::record(
        b"ARMO",
        0x0100_0900,
        &[
            (b"MOD2", items::text("male_world.nif")),
            (b"MOD4", items::text("female_world.nif")),
            (b"BOD2", body),
            (b"MODL", 0x0100_0901_u32.to_le_bytes().to_vec()),
        ],
    );
    fixture.add(
        "Items.esp",
        &["Skyrim.esm"],
        &[
            items::record(
                b"RACE",
                0x0100_0840,
                &[(b"EDID", items::text("SyntheticRace"))],
            ),
            armor,
            addon,
        ],
    );
    let decoded = fixture.read();
    let armor = &decoded.records[&0x0200_0900];
    let addon = &decoded.records[&0x0200_0901];
    assert_eq!(
        field(armor, b"MOD2"),
        &Value::String("male_world.nif".into())
    );
    assert_eq!(
        field(armor, b"MOD4"),
        &Value::String("female_world.nif".into())
    );
    assert_eq!(field(armor, b"MODL"), &Value::FormId(0x0200_0901));
    assert_eq!(field(addon, b"MODL"), &Value::FormId(0x0200_0840));
    for (tag, path) in [
        (b"MOD2", "male_biped.nif"),
        (b"MOD3", "female_biped.nif"),
        (b"MOD4", "male_first.nif"),
        (b"MOD5", "female_first.nif"),
    ] {
        assert_eq!(field(addon, tag), &Value::String(path.into()));
    }
}

/// Repeated item extras resolve owner-dependent unions after the winning catalog exists.
#[test]
fn inventory_ownership_uses_npc_global_faction_rank_and_opaque_null_alternatives() {
    let mut fixture = Fixture::new();
    let extras = [
        (0x0100_0830_u32, 0x0100_0840_u32.to_le_bytes()),
        (0x0100_0831_u32, (-7_i32).to_le_bytes()),
        (0_u32, [0xa7, 0x51, 0xd2, 0x38]),
        (0x0100_0830_u32, 0_u32.to_le_bytes()),
    ];
    let mut fields: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"COCT", 4_u32.to_le_bytes().to_vec())];
    for (owner, condition) in extras {
        fields.push((
            b"CNTO",
            [0x0100_0850_u32.to_le_bytes(), (-13_i32).to_le_bytes()].concat(),
        ));
        fields.push((
            b"COED",
            [owner.to_le_bytes(), condition, 0.375_f32.to_le_bytes()].concat(),
        ));
    }
    fields.push((
        b"DATA",
        [vec![5], 13.625_f32.to_le_bytes().to_vec()].concat(),
    ));
    fixture.add(
        "Items.esp",
        &["Skyrim.esm"],
        &[
            items::record(
                b"NPC_",
                0x0100_0830,
                &[(b"EDID", items::text("SyntheticOwner"))],
            ),
            items::record(
                b"FACT",
                0x0100_0831,
                &[(b"EDID", items::text("SyntheticFaction"))],
            ),
            items::record(
                b"GLOB",
                0x0100_0840,
                &[
                    (b"EDID", items::text("SyntheticGlobal")),
                    (b"FNAM", vec![b'f']),
                    (b"FLTV", 0.25_f32.to_le_bytes().to_vec()),
                ],
            ),
            items::record(
                b"MISC",
                0x0100_0850,
                &[
                    (b"EDID", items::text("SyntheticInventoryItem")),
                    (b"DATA", vec![0; 8]),
                ],
            ),
            items::record(b"CONT", 0x0100_0900, &fields),
        ],
    );
    let decoded = fixture.read();
    let container = &decoded.records[&0x0200_0900];
    let extras: Vec<_> = container
        .fields
        .iter()
        .filter(|field| field.signature == *b"COED")
        .collect();
    assert_eq!(extras.len(), 4);
    assert_eq!(
        member(&extras[0].value, "global_or_rank"),
        &Value::FormId(0x0200_0840)
    );
    assert_eq!(
        member(&extras[1].value, "global_or_rank"),
        &Value::Signed(-7)
    );
    assert_eq!(
        member(&extras[2].value, "global_or_rank"),
        &Value::Bytes(vec![0xa7, 0x51, 0xd2, 0x38])
    );
    assert_eq!(member(&extras[0].value, "condition"), &Value::Float(0.375));
    assert_eq!(
        member(&extras[3].value, "global_or_rank"),
        &Value::FormId(0)
    );
}

/// Light owner indices differ from global slots, and native APPA models remain supported.
#[test]
fn light_plugin_book_links_and_apparatus_models_preserve_native_fields() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Earlier.esl",
        &["Skyrim.esm"],
        &[items::record(
            b"SPEL",
            0x0100_0830,
            &[(b"EDID", items::text("SyntheticLightSpell"))],
        )],
    );
    let earlier_path = fixture.paths.last().unwrap();
    let mut earlier = fs::read(earlier_path).unwrap();
    earlier[8..12].copy_from_slice(&0x200_u32.to_le_bytes());
    fs::write(earlier_path, earlier).unwrap();
    fixture.add(
        "Items.esl",
        &["Skyrim.esm", "Earlier.esl"],
        &[
            items::book(0x0200_0900, 4, 0x0100_0830),
            items::record(
                b"APPA",
                0x0200_0901,
                &[
                    (b"EDID", items::text("SyntheticModelledApparatus")),
                    (b"MODL", items::text("native_apparatus.nif")),
                    (b"QUAL", 3_i32.to_le_bytes().to_vec()),
                    (
                        b"DATA",
                        [173_u32.to_le_bytes(), 4.375_f32.to_le_bytes()].concat(),
                    ),
                ],
            ),
        ],
    );
    let items_path = fixture.paths.last().unwrap();
    let mut plugin = fs::read(items_path).unwrap();
    plugin[8..12].copy_from_slice(&0x200_u32.to_le_bytes());
    fs::write(items_path, plugin).unwrap();
    let decoded = fixture.read();
    let book = &decoded.records[&0xfe00_1900];
    assert_eq!(
        member(field(book, b"DATA"), "teaches"),
        &Value::FormId(0xfe00_0830)
    );
    let apparatus = &decoded.records[&0xfe00_1901];
    assert_eq!(
        field(apparatus, b"MODL"),
        &Value::String("native_apparatus.nif".into())
    );
    assert_eq!(field(apparatus, b"QUAL"), &Value::Signed(3));
    assert_eq!(
        member(field(apparatus, b"DATA"), "weight"),
        &Value::Float(4.375)
    );
}

/// Every assigned family tolerates a corrupt compressed override during real publication.
#[tokio::test]
async fn every_item_family_preserves_prior_winners_after_corrupt_compressed_overrides() {
    let mut fixture = Fixture::new();
    let signatures = [
        *b"WEAP", *b"ARMO", *b"ARMA", *b"AMMO", *b"BOOK", *b"MISC", *b"KEYM", *b"INGR", *b"ALCH",
        *b"SLGM", *b"SCRL", *b"APPA", *b"CONT", *b"COBJ", *b"EQUP",
    ];
    let mut prior = items::minimal_family_records(0x0100_0900);
    prior.push(items::record(
        b"MISC",
        0x0100_0b00,
        &[
            (b"EDID", items::text("PriorCompressedNeighbor")),
            (b"DATA", vec![0; 8]),
        ],
    ));
    fixture.add("Items.esp", &["Skyrim.esm"], &prior);
    let before_patch = fixture.read();
    let mut patch = Vec::new();
    for (index, signature) in signatures.iter().enumerate() {
        let mut bad = items::record(signature, 0x0100_0900 + index as u32, &[]);
        // Complete record boundary, invalid zlib body; never guess the next record.
        let payload = [64_u32.to_le_bytes().to_vec(), vec![0x78, 0x9c, 0xff]].concat();
        bad[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        bad[8..12].copy_from_slice(&0x0004_0000_u32.to_le_bytes());
        bad.extend(payload);
        patch.push(bad);
    }
    patch.push(items::record(
        b"APPA",
        0x0200_0b01,
        &[
            (b"EDID", items::text("NewCompressedNeighbor")),
            (b"QUAL", 3_i32.to_le_bytes().to_vec()),
            (b"DATA", vec![0; 8]),
        ],
    ));
    fixture.add("BadItems.esp", &["Skyrim.esm", "Items.esp"], &patch);
    let list = fixture.directory.path().join("plugins.txt");
    fs::write(&list, "Skyrim.esm\nFiller.esm\n*Items.esp\n*BadItems.esp\n").unwrap();
    let output = fixture.directory.path().join("published-compressed");
    let mut config = PipelineConfig::new(&fixture.data, &output);
    config.record_reader = RecordReader::Inhouse;
    config.plugins_file = Some(list);
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    for (index, signature) in signatures.iter().enumerate() {
        let id = 0x0200_0900_u32 + index as u32;
        let (order, kind, payload): (u32, String, Vec<u8>) = database
            .query_row(
                "SELECT load_order,record_type,payload FROM inhouse_source_records WHERE form_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(order, 2);
        assert_eq!(kind.as_bytes(), signature);
        assert_eq!(payload, prior[index][24..]);
        let blob: Vec<u8> = database
            .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let canonical = rkyv::from_bytes::<
            converter::esm::types::ArchivedRecordData,
            rkyv::rancor::Error,
        >(&blob)
        .unwrap();
        let expected = before_patch.records[&id].to_raw_record().subrecords;
        assert_eq!(canonical.subrecords.len(), expected.len());
        for (actual, (tag, bytes)) in canonical.subrecords.iter().zip(expected) {
            assert_eq!(actual.tag.as_slice(), tag.as_slice());
            assert_eq!(actual.data, bytes);
        }
    }
    for (id, order) in [(0x0200_0b00_u32, 2_u32), (0x0300_0b01, 3)] {
        let found: u32 = database
            .query_row(
                "SELECT load_order FROM inhouse_source_records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(found, order);
        let present: u32 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(present, 1);
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["baditems.esp"]["skipped_records"],
        15
    );
    assert!(diagnostics["decoder"]["baditems.esp"]["first_example"].is_string());
}
