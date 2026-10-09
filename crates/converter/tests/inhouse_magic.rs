//! Native magic layouts, contextual alternatives and local corruption recovery.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedField, DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, inhouse_magic as magic, layout};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

/// A real nested base plugin plus independently framed supplemental family records.
struct Fixture {
    directory: tempfile::TempDir,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Add independent family records to the existing generated world.
    fn new(records: &[u8]) -> Self {
        let mut bytes = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let header = bytes[24..24 + length].to_vec();
        bytes.extend(records);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, bytes).unwrap();
        Self {
            directory,
            header,
            paths: vec![path],
        }
    }

    /// File-relative master links differ from this patch's own global namespace.
    fn patch(&mut self, name: &str, records: &[u8]) {
        let mut header = self.header.clone();
        header.extend(magic::subrecord(b"MAST", b"Skyrim.esm\0"));
        header.extend(magic::subrecord(b"DATA", &[0xA5; 8]));
        let mut bytes = magic::record(b"TES4", 0, 0, &header);
        bytes.extend(records);
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Use the public load-order and reader APIs, including final target validation.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Select a typed occurrence without collapsing ordered repeat signatures.
fn field<'a>(record: &'a DecodedRecord, signature: &[u8; 4]) -> &'a DecodedField {
    record
        .fields
        .iter()
        .find(|field| &field.signature == signature)
        .unwrap()
}

/// Pick a named member from a struct or selected union struct.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(member, _)| member == name).unwrap().1
}

/// Every assigned family decodes, keeping padding, arrays and effect ordering.
#[test]
fn all_magic_families_keep_native_values_and_order() {
    let fixture = Fixture::new(&magic::records());
    let result = fixture.read();
    for (index, signature) in magic::SIGNATURES.iter().enumerate() {
        let record = &result.records[&(magic::FIRST_ID + index as u32)];
        assert_eq!(&record.record_type, signature);
        assert!(record.supported);
        assert!(record.rejected_fields.is_empty(), "{signature:?}");
        assert_eq!(record.version_control, 0x1234_5678);
        assert_eq!(record.header_unknown, 0xA573);
    }
    let spell = &result.records[&magic::FIRST_ID];
    let data = &field(spell, b"SPIT").value;
    assert_eq!(member(data, "base_cost"), &Value::Unsigned(93));
    assert_eq!(member(data, "casting_duration"), &Value::Float(3.75));
    let effects = spell
        .fields
        .iter()
        .filter(|field| field.signature == *b"EFIT")
        .collect::<Vec<_>>();
    assert_eq!(effects.len(), 2);
    assert_eq!(member(&effects[0].value, "magnitude"), &Value::Float(3.25));
    assert_eq!(member(&effects[1].value, "magnitude"), &Value::Float(-8.5));
    assert_eq!(
        spell
            .fields
            .iter()
            .filter(|field| field.signature == *b"CTDA")
            .count(),
        2
    );
    assert_eq!(
        field(&result.records[&(magic::FIRST_ID + 1)], b"SNDD").value,
        Value::Array(vec![])
    );
    assert_eq!(
        field(&result.records[&(magic::FIRST_ID + 10)], b"FLTV").value,
        Value::Float(13.75)
    );
    assert_eq!(
        field(&result.records[&(magic::FIRST_ID + 11)], b"DATA").value,
        Value::Unsigned(2)
    );
    let entry = field(&result.records[&(magic::FIRST_ID + 5)], b"LVLO");
    assert_eq!(&entry.canonical_bytes[2..4], &[0xA7, 0x53]);
    assert_eq!(&entry.canonical_bytes[10..12], &[0xC1, 0x39]);
    let links = result.records[&(magic::FIRST_ID + 8)]
        .fields
        .iter()
        .filter(|field| field.signature == *b"LNAM")
        .map(|field| field.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        links,
        [
            Value::FormId(magic::FIRST_ID + 4),
            Value::FormId(magic::FIRST_ID)
        ]
    );
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 0);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
    assert_eq!(
        member(
            &field(&result.records[&(magic::FIRST_ID + 9)], b"CNAM").value,
            "alpha"
        ),
        &Value::Unsigned(0xA5)
    );
    let dual = &field(&result.records[&(magic::FIRST_ID + 17)], b"DATA").value;
    assert_eq!(
        member(dual, "projectile"),
        &Value::FormId(magic::FIRST_ID + 13)
    );
    assert_eq!(
        member(dual, "explosion"),
        &Value::FormId(magic::FIRST_ID + 12)
    );
    assert_eq!(member(dual, "shader"), &Value::FormId(magic::FIRST_ID + 16));
    assert_eq!(
        member(dual, "hit_art"),
        &Value::FormId(magic::FIRST_ID + 15)
    );
    assert_eq!(member(dual, "scale_flags"), &Value::Unsigned(5));
}

/// Archetype changes select same-sized alternatives rather than guessing from the slot.
#[test]
fn associated_magic_slot_uses_parent_archetype_and_remaps_only_links() {
    let mut fixture = Fixture::new(&magic::records());
    fixture.patch(
        "Archetypes.esp",
        &[
            magic::effect(0x0100_1900, 35, magic::FIRST_ID),
            magic::effect(0x0100_1901, 0, 0xFEAB_CDEF),
            magic::effect(0x0100_1902, 12, magic::FIRST_ID),
        ]
        .concat(),
    );
    let result = fixture.read();
    let linked = field(&result.records[&0x0100_1900], b"DATA");
    assert_eq!(
        member(&linked.value, "associated_form"),
        &Value::FormId(magic::FIRST_ID)
    );
    let unused = field(&result.records[&0x0100_1901], b"DATA");
    assert_eq!(
        member(&unused.value, "associated_form"),
        &Value::Bytes(0xFEAB_CDEFu32.to_le_bytes().to_vec())
    );
    assert_eq!(
        &unused.canonical_bytes[8..12],
        &0xFEAB_CDEFu32.to_le_bytes()
    );
    let invalid = field(&result.records[&0x0100_1902], b"DATA");
    assert_eq!(member(&invalid.value, "associated_form"), &Value::FormId(0));
    assert!(result.diagnostics["archetypes.esp"].invalid_links > 0);
}

/// Valid whole optional tails stay absent; partial members invalidate only that candidate.
#[test]
fn native_optional_tails_and_empty_shader_are_distinguished_from_truncation() {
    let mut records = magic::records();
    let cases = [
        (*b"EXPL", vec![40, 44, 48, 52]),
        (*b"PROJ", vec![84, 88, 92]),
        (*b"EFSH", vec![0, 4, 308, 312, 344, 396, 400]),
    ];
    let mut accepted = Vec::new();
    let mut next = 0x1920;
    for (signature, lengths) in cases {
        for length in lengths {
            records.extend(magic::data_record(&signature, next, &vec![0; length]));
            accepted.push((next, length));
            next += 1;
        }
    }
    records.extend(magic::data_record(b"EFSH", 0x1990, &[0; 309]));
    let result = Fixture::new(&records).read();
    for (id, length) in accepted {
        assert_eq!(
            field(&result.records[&id], b"DATA").canonical_bytes.len(),
            length
        );
    }
    assert!(!result.records.contains_key(&0x1990));
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 1);
}

/// Owner identity selects faction ranks, NPC globals and unused null-owner bytes.
#[test]
fn leveled_owner_rank_global_and_unused_bytes_resolve_after_winners() {
    let mut records = magic::records();
    records.extend(magic::record(
        b"NPC_",
        0x19C0,
        0,
        &magic::subrecord(b"EDID", b"OwnerNPC\0"),
    ));
    records.extend(magic::record(
        b"FACT",
        0x19C1,
        0,
        &magic::subrecord(b"EDID", b"OwnerFaction\0"),
    ));
    let mut payload = magic::subrecord(b"EDID", b"OwnerUnionList\0");
    payload.extend(magic::subrecord(b"LLCT", &[3]));
    for (owner, extra, condition) in [
        (0x19C0u32, magic::FIRST_ID + 10, 0.375f32),
        (0x19C1, (-3i32) as u32, 0.8125),
        (0, 0xFEAB_CDEF, 0.625),
    ] {
        let mut entry = vec![0; 12];
        entry[4..8].copy_from_slice(&0x18F1u32.to_le_bytes());
        payload.extend(magic::subrecord(b"LVLO", &entry));
        let mut data = owner.to_le_bytes().to_vec();
        data.extend(extra.to_le_bytes());
        data.extend(condition.to_le_bytes());
        payload.extend(magic::subrecord(b"COED", &data));
    }
    records.extend(magic::record(b"LVLI", 0x19C2, 0, &payload));
    let result = Fixture::new(&records).read();
    let extras = result.records[&0x19C2]
        .fields
        .iter()
        .filter(|field| field.signature == *b"COED")
        .collect::<Vec<_>>();
    assert_eq!(extras.len(), 3);
    assert_eq!(
        member(&extras[0].value, "global_or_rank"),
        &Value::FormId(magic::FIRST_ID + 10)
    );
    assert_eq!(
        member(&extras[1].value, "global_or_rank"),
        &Value::Signed(-3)
    );
    assert_eq!(
        member(&extras[2].value, "global_or_rank"),
        &Value::Bytes(0xFEAB_CDEFu32.to_le_bytes().to_vec())
    );
    assert_eq!(member(&extras[1].value, "condition"), &Value::Float(0.8125));
    assert_eq!(result.diagnostics["skyrim.esm"].invalid_links, 0);
}

/// A malformed present SPIT cannot replace a good winner; genuine deletion still applies.
#[test]
fn malformed_override_preserves_old_spell_and_neighbors_before_real_deletion() {
    let mut fixture = Fixture::new(&magic::records());
    let bad = magic::record(
        b"SPEL",
        magic::FIRST_ID,
        0,
        &[
            magic::subrecord(b"EDID", b"BadOverride\0"),
            magic::subrecord(b"SPIT", &[0; 35]),
        ]
        .concat(),
    );
    fixture.patch(
        "DamagedMagic.esp",
        &[
            magic::spell(0x0100_19A0, 47),
            bad,
            magic::spell(0x0100_19A1, 83),
        ]
        .concat(),
    );
    let result = fixture.read();
    assert_eq!(
        member(
            &field(&result.records[&magic::FIRST_ID], b"SPIT").value,
            "base_cost"
        ),
        &Value::Unsigned(93)
    );
    assert!(result.records.contains_key(&0x0100_19A0));
    assert!(result.records.contains_key(&0x0100_19A1));
    assert_eq!(result.diagnostics["damagedmagic.esp"].skipped_records, 1);
    assert!(
        result.diagnostics["damagedmagic.esp"]
            .first_example
            .as_ref()
            .unwrap()
            .contains("spell_data")
    );
    assert!(!result.overrides.contains_key(&magic::FIRST_ID));
    fixture.patch(
        "DeletedMagic.esp",
        &magic::record(b"SPEL", magic::FIRST_ID, 0x20, &[]),
    );
    assert!(!fixture.read().records.contains_key(&magic::FIRST_ID));
}

/// Conversion publishes usable neighbors despite a safely framed malformed magic record.
#[tokio::test]
async fn broken_magic_mod_publishes_usable_neighbors_and_bounded_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let path = data.join("Skyrim.esm");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend(magic::records());
    bytes.extend(magic::record(
        b"SPEL",
        0x19B0,
        0,
        &magic::subrecord(b"SPIT", &[0; 35]),
    ));
    bytes.extend(magic::spell(0x19B1, 71));
    fs::write(path, bytes).unwrap();
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [magic::FIRST_ID, 0x19B1] {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    let rejected: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM records WHERE form_id=0x19B0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rejected, 0);
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["skyrim.esm"]["skipped_records"], 1);
}

/// Each assigned family preserves a prior winner and both neighbors after unusable compression.
#[tokio::test]
async fn all_magic_families_publish_prior_winners_and_neighbors_after_bad_compression() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base_path = data.join("Skyrim.esm");
    let mut base_bytes = fs::read(&base_path).unwrap();
    let header_size = u32::from_le_bytes(base_bytes[4..8].try_into().unwrap()) as usize;
    let mut patch_header = base_bytes[24..24 + header_size].to_vec();
    patch_header.extend(magic::subrecord(b"MAST", b"Skyrim.esm\0"));
    patch_header.extend(magic::subrecord(b"DATA", &[0xA5; 8]));
    base_bytes.extend(magic::records());
    fs::write(&base_path, base_bytes).unwrap();
    let earlier = read_plugins(
        std::slice::from_ref(&base_path),
        &LoadOrder::read(std::slice::from_ref(&base_path)).unwrap(),
    )
    .unwrap();
    let mut patch_bytes = magic::record(b"TES4", 0, 0, &patch_header);
    let mut neighbors = Vec::new();
    for (index, signature) in magic::SIGNATURES.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        let before = 0x0100_1A00 + index * 2;
        let after = before + 1;
        neighbors.extend([before, after]);
        patch_bytes.extend(magic::spell(before, 43 + index));
        // Framing stays valid, but this declared24-byte output has no complete zlib stream.
        patch_bytes.extend(magic::record(
            signature,
            magic::FIRST_ID + index,
            0x40000,
            &[24, 0, 0, 0, 99],
        ));
        patch_bytes.extend(magic::spell(after, 79 + index));
    }
    let patch_path = data.join("DamagedMagic.esp");
    fs::write(&patch_path, patch_bytes).unwrap();
    let paths = [base_path, patch_path];
    let recovered = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let warnings = &recovered.diagnostics["damagedmagic.esp"];
    assert_eq!(warnings.skipped_records, 18);
    assert_eq!(warnings.skipped_fields, 0);
    assert_eq!(warnings.first_by_category.len(), 1);
    assert!(warnings.first_by_category["record"].starts_with("SPEL 00001800:"));
    for (index, signature) in magic::SIGNATURES.iter().enumerate() {
        let id = magic::FIRST_ID + u32::try_from(index).unwrap();
        let original = &earlier.records[&id];
        let retained = &recovered.records[&id];
        assert_eq!(&retained.record_type, signature);
        assert_eq!(retained.load_order, 0);
        assert_eq!(retained.raw_payload, original.raw_payload);
        assert_eq!(retained.fields.len(), original.fields.len());
        for (actual, expected) in retained.fields.iter().zip(&original.fields) {
            assert_eq!(actual.signature, expected.signature);
            assert_eq!(actual.value, expected.value);
            assert_eq!(actual.canonical_bytes, expected.canonical_bytes);
        }
        assert!(!recovered.overrides.contains_key(&id));
    }
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*DamagedMagic.esp\n").unwrap();
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(plugins);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for (index, _) in magic::SIGNATURES.iter().enumerate() {
        let id = magic::FIRST_ID + u32::try_from(index).unwrap();
        let (load_order, payload): (i64, Vec<u8>) = database
            .query_row(
                "SELECT source.load_order,source.payload FROM inhouse_source_records source JOIN records ON records.form_id=source.form_id WHERE source.form_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(load_order, 0);
        assert_eq!(payload, earlier.records[&id].raw_payload);
    }
    for id in neighbors {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "missing usable neighbor {id:08X}");
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["damagedmagic.esp"]["skipped_records"],
        18
    );
    assert_eq!(
        diagnostics["decoder"]["damagedmagic.esp"]["skipped_fields"],
        0
    );
}
