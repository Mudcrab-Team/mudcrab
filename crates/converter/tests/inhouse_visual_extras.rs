//! Native visual-family variants and actual publication with corrupt mod data.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, inhouse_visual_extras as visual, layout};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

/// A nested generated world plus independently authored family records.
struct Fixture {
    directory: tempfile::TempDir,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}
impl Fixture {
    /// Append native family records to a valid base plugin.
    fn new(extra: &[u8]) -> Self {
        let mut bytes = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let header = bytes[24..24 + size].to_vec();
        bytes.extend(extra);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, bytes).unwrap();
        Self {
            directory,
            header,
            paths: vec![path],
        }
    }
    /// Link an override patch to the base with distinct owner and global namespaces.
    fn patch(&mut self, name: &str, extra: &[u8]) {
        let mut header = self.header.clone();
        header.extend(visual::subrecord(b"MAST", b"Skyrim.esm\0"));
        header.extend(visual::subrecord(b"DATA", &[0xA5; 8]));
        let mut bytes = visual::record(b"TES4", 0, 0, &header);
        bytes.extend(extra);
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }
    /// Exercise public load-order decoding and final target resolution.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Select a named canonical occurrence without reducing repeated tags to a map.
fn named<'a>(record: &'a DecodedRecord, name: &str) -> &'a Value {
    &record.fields.iter().find(|f| f.name == name).unwrap().value
}
/// Select a native member of the decoded struct.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct {value:?}")
    };
    &members.iter().find(|(n, _)| n == name).unwrap().1
}

/// Binary signatures, contextual curves, shortened colors and duplicate roles decode.
#[test]
fn every_visual_type_preserves_native_values_and_context() {
    let fixture = Fixture::new(&visual::records());
    let result = fixture.read();
    for (index, sig) in visual::SIGNATURES.iter().enumerate() {
        let r = &result.records[&(visual::FIRST_ID + index as u32)];
        assert_eq!(&r.record_type, sig);
        assert!(r.supported);
        assert!(
            r.rejected_fields.is_empty(),
            "{sig:?}: {:?}",
            r.rejected_fields
        );
        assert!(r.payload_complete);
    }
    assert_eq!(result.diagnostics["skyrim.esm"].unexpected_subrecords, 0);
    let weather = &result.records[&visual::FIRST_ID];
    assert_eq!(
        named(weather, "cloud_texture_10"),
        &Value::String("independent/cloud.dds".into())
    );
    let colors = named(weather, "weather_colors");
    assert!(matches!(member(colors, "sky_statics"), Value::Struct(_)));
    let Value::Struct(colors) = colors else {
        unreachable!()
    };
    assert!(!colors.iter().any(|(n, _)| n == "water_multiplier"));
    let animation = &result.records[&(visual::FIRST_ID + 3)];
    let Value::Array(keys) = named(animation, "eye_adaptation_speed_multiply") else {
        panic!("curve lost")
    };
    assert_eq!(keys.len(), 2);
    assert_eq!(member(&keys[0], "time"), &Value::Float(-1.875));
    assert_eq!(member(&keys[0], "value"), &Value::Float(-1.5));
    let counts = named(animation, "animation_counts");
    assert_eq!(member(counts, "radial_center_x"), &Value::Float(0.3125));
    assert_eq!(member(counts, "radial_center_y"), &Value::Float(-0.8125));
    assert_eq!(
        member(counts, "radial_flags"),
        &Value::Unsigned(0x8000_0001)
    );
    let light = &result.records[&(visual::FIRST_ID + 4)];
    assert_eq!(
        member(named(light, "lighting"), "directional_xy"),
        &Value::Signed(-37)
    );
    let lens = &result.records[&(visual::FIRST_ID + 12)];
    let ids = lens
        .fields
        .iter()
        .filter(|f| f.name == "sprite_id")
        .map(|f| f.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            Value::String("first".into()),
            Value::String("second".into())
        ]
    );
    assert_eq!(named(lens, "fade_radius_scale"), &Value::Float(1.8125));
}

/// Count mismatch drops only that curve and keeps an unrelated adjacent curve.
#[test]
fn malformed_animation_count_recovers_fields_and_remaps_patch_identity() {
    let mut fixture = Fixture::new(&visual::records());
    fixture.patch("VisualPatch.esp", &visual::image_modifier(0x0100_2900, 3));
    let result = fixture.read();
    let record = &result.records[&0x0100_2900];
    assert_eq!(record.rejected_fields, vec![*b"BNAM"]);
    assert!(!record.fields.iter().any(|f| f.signature == *b"BNAM"));
    assert!(record.fields.iter().any(|f| f.signature == *b"VNAM"));
    assert_eq!(result.diagnostics["visualpatch.esp"].skipped_fields, 1);
    assert_eq!(result.diagnostics["visualpatch.esp"].skipped_records, 0);
}

/// Form-version15 ENAM retains ten named values and unknown trailing source bytes.
#[test]
fn legacy_image_version_controls_enam_semantics() {
    let mut data = Vec::new();
    for i in 0..10 {
        data.extend((0.1875 + i as f32 * 0.375).to_le_bytes());
    }
    data.extend([0xA5; 16]);
    let payload = visual::subrecord(b"ENAM", &data);
    let mut old = visual::record(b"IMGS", 0x29A0, 0, &payload);
    old[20..22].copy_from_slice(&15u16.to_le_bytes());
    let modern = visual::record(b"IMGS", 0x29A1, 0, &payload);
    old.extend(&modern);
    let fixture = Fixture::new(&old);
    let result = fixture.read();
    assert_eq!(
        member(
            named(&result.records[&0x29A0], "legacy_image_settings"),
            "eye_adaptation_speed"
        ),
        &Value::Float(0.1875)
    );
    assert_eq!(
        named(&result.records[&0x29A1], "legacy_image_settings"),
        &Value::Bytes(data)
    );
}

/// A28-byte legacy MATO override is a complete projection-vector prefix.
#[test]
fn legacy_material_prefix_can_override_a_full_sse_material() {
    let mut fixture = Fixture::new(&visual::records());
    let mut values = Vec::new();
    for i in 0..7 {
        values.extend((0.4375 + i as f32 * 0.375).to_le_bytes());
    }
    let payload = visual::subrecord(b"DATA", &values);
    fixture.patch(
        "LegacyMaterial.esp",
        &visual::record(b"MATO", visual::FIRST_ID + 5, 0, &payload),
    );
    let decoded = fixture.read();
    let material = &decoded.records[&(visual::FIRST_ID + 5)];
    assert_eq!(material.load_order, 1);
    assert!(material.rejected_fields.is_empty());
    let Value::Struct(members) = named(material, "directional_material") else {
        panic!("material prefix lost its typed shape")
    };
    assert_eq!(members.len(), 7);
    assert_eq!(members[6].0, "projection_z");
    assert_eq!(members[6].1, Value::Float(2.6875));
    assert!(!members.iter().any(|(name, _)| name == "normal_dampener"));
}

/// Actual publication retains all prior winners and both neighbors around15 bad streams.
#[tokio::test]
async fn broken_visual_mod_publishes_neighbors_prior_winners_and_safe_fields() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base = data.join("Skyrim.esm");
    let mut bytes = fs::read(&base).unwrap();
    let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let mut header = bytes[24..24 + size].to_vec();
    header.extend(visual::subrecord(b"MAST", b"Skyrim.esm\0"));
    header.extend(visual::subrecord(b"DATA", &[0xA5; 8]));
    bytes.extend(visual::records());
    fs::write(&base, bytes).unwrap();
    let earlier = read_plugins(
        std::slice::from_ref(&base),
        &LoadOrder::read(std::slice::from_ref(&base)).unwrap(),
    )
    .unwrap();
    let mut patch = visual::record(b"TES4", 0, 0, &header);
    let mut neighbors = Vec::new();
    for (index, signature) in visual::SIGNATURES.iter().enumerate() {
        let index = index as u32;
        let before = 0x0100_2B00 + index * 2;
        let after = before + 1;
        neighbors.extend([before, after]);
        patch.extend(visual::image_modifier(before, 2));
        patch.extend(visual::record(
            signature,
            visual::FIRST_ID + index,
            0x40000,
            &[24, 0, 0, 0, 99],
        ));
        patch.extend(visual::image_modifier(after, 2));
    }
    patch.extend(visual::image_modifier(0x0100_2C00, 3));
    let patch_path = data.join("DamagedVisual.esp");
    fs::write(&patch_path, patch).unwrap();
    let paths = [base, patch_path];
    let recovered = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let diag = &recovered.diagnostics["damagedvisual.esp"];
    assert_eq!(diag.skipped_records, 15);
    assert_eq!(diag.skipped_fields, 1);
    assert_eq!(diag.first_by_category.len(), 2);
    for (index, _) in visual::SIGNATURES.iter().enumerate() {
        let id = visual::FIRST_ID + index as u32;
        assert_eq!(
            recovered.records[&id].raw_payload,
            earlier.records[&id].raw_payload
        );
        assert_eq!(recovered.records[&id].load_order, 0);
        assert!(!recovered.overrides.contains_key(&id));
    }
    let order = temp.path().join("plugins.txt");
    fs::write(&order, "*Skyrim.esm\n*DamagedVisual.esp\n").unwrap();
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(order);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in neighbors.into_iter().chain([0x0100_2C00]) {
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM records WHERE form_id=?", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
    for (index, _) in visual::SIGNATURES.iter().enumerate() {
        let id = visual::FIRST_ID + index as u32;
        let (load, payload): (i64, Vec<u8>) = db
            .query_row(
                "SELECT load_order,payload FROM inhouse_source_records WHERE form_id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(load, 0);
        assert_eq!(payload, earlier.records[&id].raw_payload);
    }
    let diag: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diag["decoder"]["damagedvisual.esp"]["skipped_records"], 15);
    assert_eq!(diag["decoder"]["damagedvisual.esp"]["skipped_fields"], 1);
}
