//! Typed audio layouts, repeated children and corrupt-mod publication regressions.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{inhouse_audio_extras as audio, inhouse_items as framing, layout};
use std::{fs, path::PathBuf};

/// A real dummy-content world, filler slot and independently framed dependent plugins.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    generated: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Create source assets and reserve a nonidentity full-plugin load-order index.
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
            framing::dependent_plugin(&generated, &["Skyrim.esm"], &[]),
        )
        .unwrap();
        Self {
            directory,
            data,
            generated,
            paths: vec![base, filler],
        }
    }

    /// Add a full plugin using its explicit author-selected master ordering.
    fn add(&mut self, name: &str, masters: &[&str], records: &[Vec<u8>]) {
        let path = self.data.join(name);
        fs::write(
            &path,
            framing::dependent_plugin(&self.generated, masters, records),
        )
        .unwrap();
        self.paths.push(path);
    }

    /// Read through the public typed frontend and current load-order authority.
    fn read(&self) -> ReadResult {
        let order = LoadOrder::read(&self.paths).unwrap();
        read_plugins(&self.paths, &order).unwrap()
    }
}

/// Locate an exact named occurrence without conflating AVIF's two CNAM roles.
fn named<'a>(record: &'a DecodedRecord, name: &str) -> &'a Value {
    &record
        .fields
        .iter()
        .find(|field| field.name == name)
        .unwrap()
        .value
}

/// Locate a primitive inside a decoded native byte structure.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// Every type decodes while signed widths, source padding and role-specific links survive.
#[test]
fn all_audio_misc_types_preserve_packed_layouts_and_nonidentity_links() {
    let mut fixture = Fixture::new();
    fixture.add("Audio.esp", &["Skyrim.esm"], &audio::records(0x0100_5900));
    let result = fixture.read();
    for (index, signature) in audio::SIGNATURES.iter().enumerate() {
        let record = &result.records[&(0x0200_5900 + index as u32)];
        assert!(record.supported, "{:?}", signature);
        assert!(
            record.rejected_fields.is_empty(),
            "{:?}: {:?}",
            signature,
            record.rejected_fields
        );
        assert_eq!(record.form_version, 44);
        assert_eq!(record.header_unknown, 0x07a9);
    }
    assert_eq!(
        named(&result.records[&0x0200_5900], "descriptor"),
        &Value::FormId(0x0200_5901)
    );
    let descriptor = &result.records[&0x0200_5901];
    assert_eq!(named(descriptor, "category"), &Value::FormId(0x0200_5903));
    assert_eq!(
        named(descriptor, "output_model"),
        &Value::FormId(0x0200_5902)
    );
    assert_eq!(
        member(named(descriptor, "variation"), "frequency_shift"),
        &Value::Signed(-13)
    );
    assert_eq!(
        member(named(descriptor, "variation"), "attenuation_hundredths_db"),
        &Value::Unsigned(309)
    );
    assert_eq!(
        member(
            named(&result.records[&0x0200_5907], "reverb"),
            "room_filter"
        ),
        &Value::Signed(-13)
    );
    assert_eq!(
        named(&result.records[&0x0200_590c], "node_index"),
        &Value::Signed(-193)
    );
    let color = named(&result.records[&0x0200_590e], "color");
    assert_eq!(member(color, "red"), &Value::Unsigned(17));
    assert_eq!(member(color, "blue"), &Value::Unsigned(211));
    let rotation = named(&result.records[&0x0200_5912], "initial_rotation_degrees");
    assert_eq!(member(rotation, "x"), &Value::Signed(-73));
    assert_eq!(member(rotation, "z"), &Value::Signed(137));
    let Value::Array(objects) = named(&result.records[&0x0200_590a], "default_objects") else {
        panic!("objects must be an array")
    };
    assert_eq!(
        member(&objects[0], "use_code"),
        &Value::Unsigned(u64::from(u32::from_le_bytes(*b"ZZ17")))
    );
    assert_eq!(member(&objects[0], "object"), &Value::FormId(0x0200_5907));
    assert_eq!(member(&objects[1], "object"), &Value::FormId(0x0200_5903));
}

/// All three MUST variants retain physical reordered fields and contextual palette links.
#[test]
fn music_variants_reordered_fields_and_repeated_debris_perk_nodes_are_distinct() {
    let mut fixture = Fixture::new();
    let mut records = audio::records(0x0100_5900);
    records.push(audio::music_track(0x0100_5a00, 0x23f6_78c3, 0x0100_5905));
    records.push(audio::music_track(0x0100_5a01, 0xa1a9_c4d5, 0));
    fixture.add("Audio.esp", &["Skyrim.esm"], &records);
    let result = fixture.read();
    let single = &result.records[&0x0200_5905];
    assert_eq!(named(single, "track_kind"), &Value::Unsigned(0x6ed7_e048));
    let Value::Array(points) = named(single, "cue_points") else {
        panic!("cue array")
    };
    assert_eq!(&points[1], &Value::Float(7.625));
    assert_eq!(member(named(single, "loop"), "count"), &Value::Unsigned(7));
    let palette = &result.records[&0x0200_5a00];
    let Value::Array(tracks) = named(palette, "palette_tracks") else {
        panic!("palette array")
    };
    assert_eq!(&tracks[0], &Value::FormId(0x0200_5905));
    assert_eq!(&tracks[1], &Value::FormId(0));
    assert_eq!(
        named(&result.records[&0x0200_5a01], "duration"),
        &Value::Float(11.625)
    );
    for id in [0x0200_5905, 0x0200_5a00, 0x0200_5a01] {
        assert!(result.records[&id].rejected_fields.is_empty());
    }
    let debris = &result.records[&0x0200_590b];
    let models: Vec<_> = debris
        .fields
        .iter()
        .filter(|field| field.name == "debris_model")
        .collect();
    assert_eq!(models.len(), 2);
    assert_eq!(member(&models[0].value, "percentage"), &Value::Unsigned(37));
    assert_eq!(
        member(&models[1].value, "model_filename"),
        &Value::String("Meshes\\much_longer_model.nif".into())
    );
    assert_eq!(member(&models[0].value, "flags"), &Value::Unsigned(1));
    let avif = &result.records[&0x0200_590d];
    assert_eq!(
        named(avif, "unknown_actor_value_data"),
        &Value::Bytes(vec![0x51, 0xa7, 0x13, 0x97])
    );
    let connections: Vec<_> = avif
        .fields
        .iter()
        .filter(|field| field.name == "connection_indices")
        .collect();
    assert_eq!(
        connections
            .iter()
            .map(|field| &field.value)
            .collect::<Vec<_>>(),
        vec![
            &Value::Unsigned(18),
            &Value::Unsigned(19),
            &Value::Unsigned(94),
            &Value::Unsigned(95)
        ]
    );
    assert_eq!(
        avif.fields
            .iter()
            .filter(|field| field.name == "associated_skill")
            .map(|field| &field.value)
            .collect::<Vec<_>>(),
        vec![&Value::FormId(0x0200_590d); 2]
    );
}

/// A broken variable filename drops only that field; the framed tail remains decodable.
#[test]
fn malformed_debris_and_invalid_optional_sound_link_are_local_diagnostics() {
    let mut fixture = Fixture::new();
    fixture.add(
        "BadAudio.esp",
        &["Skyrim.esm"],
        &[
            framing::record(
                b"DEBR",
                0x0100_5b00,
                &[
                    (b"EDID", framing::text("BrokenFilename")),
                    (b"DATA", vec![37, 65, 66]),
                    (b"MODT", vec![0x91]),
                ],
            ),
            framing::record(
                b"SOUN",
                0x0100_5b01,
                &[
                    (b"EDID", framing::text("BadMasterSound")),
                    (b"SDSC", 0x0700_1234u32.to_le_bytes().to_vec()),
                ],
            ),
            framing::record(
                b"REVB",
                0x0100_5b02,
                &[
                    (b"EDID", framing::text("UsableNeighbor")),
                    (b"DATA", vec![0; 14]),
                ],
            ),
        ],
    );
    let result = fixture.read();
    assert_eq!(result.records[&0x0200_5b00].rejected_fields, vec![*b"DATA"]);
    assert!(
        result.records[&0x0200_5b00]
            .fields
            .iter()
            .any(|field| field.signature == *b"MODT")
    );
    assert_eq!(
        named(&result.records[&0x0200_5b01], "descriptor"),
        &Value::FormId(0)
    );
    assert!(result.records.contains_key(&0x0200_5b02));
    let diagnostic = &result.diagnostics["badaudio.esp"];
    assert_eq!(diagnostic.skipped_fields, 1);
    assert_eq!(diagnostic.invalid_links, 1);
    assert!(diagnostic.first_example.is_some());
}

/// Real conversion publishes all earlier family winners and both valid neighbours.
#[tokio::test]
async fn corrupt_compressed_overrides_for_every_family_preserve_real_publication() {
    let mut fixture = Fixture::new();
    let mut prior = audio::records(0x0100_5900);
    prior.push(framing::record(
        b"REVB",
        0x0100_5c00,
        &[
            (b"EDID", framing::text("PriorNeighbor")),
            (b"DATA", vec![0; 14]),
        ],
    ));
    fixture.add("Audio.esp", &["Skyrim.esm"], &prior);
    let before = fixture.read();
    let mut patch = Vec::new();
    for (index, tag) in audio::SIGNATURES.iter().enumerate() {
        let mut bad = framing::record(tag, 0x0100_5900 + index as u32, &[]);
        let payload = [64u32.to_le_bytes().to_vec(), vec![0x78, 0x9c, 0xff]].concat();
        bad[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        bad[8..12].copy_from_slice(&0x0004_0000u32.to_le_bytes());
        bad.extend(payload);
        patch.push(bad);
    }
    patch.push(framing::record(
        b"DEBR",
        0x0200_5c01,
        &[
            (b"EDID", framing::text("BrokenVariableNeighbor")),
            (b"DATA", vec![73, 65, 66]),
            (b"MODT", vec![0x53]),
        ],
    ));
    patch.push(framing::record(
        b"REVB",
        0x0200_5c02,
        &[
            (b"EDID", framing::text("NewNeighbor")),
            (b"DATA", vec![0; 14]),
        ],
    ));
    fixture.add("BadAudio.esp", &["Skyrim.esm", "Audio.esp"], &patch);
    let list = fixture.directory.path().join("plugins.txt");
    fs::write(&list, "Skyrim.esm\nFiller.esm\n*Audio.esp\n*BadAudio.esp\n").unwrap();
    let output = fixture.directory.path().join("published-audio");
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
    for (index, tag) in audio::SIGNATURES.iter().enumerate() {
        let id = 0x0200_5900 + index as u32;
        let (order, kind, payload): (u32, String, Vec<u8>) = database.query_row("SELECT load_order,record_type,payload FROM inhouse_source_records WHERE form_id=?1", [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(order, 2);
        assert_eq!(kind.as_bytes(), tag);
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
        let expected = before.records[&id].to_raw_record().subrecords;
        assert_eq!(canonical.subrecords.len(), expected.len());
        for (actual, (signature, bytes)) in canonical.subrecords.iter().zip(expected) {
            assert_eq!(actual.tag.as_slice(), signature.as_slice());
            assert_eq!(actual.data, bytes);
        }
    }
    for (id, order) in [(0x0200_5c00u32, 2u32), (0x0300_5c01, 3), (0x0300_5c02, 3)] {
        let actual: u32 = database
            .query_row(
                "SELECT load_order FROM inhouse_source_records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, order);
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["badaudio.esp"]["skipped_records"],
        19
    );
    assert_eq!(diagnostics["decoder"]["badaudio.esp"]["skipped_fields"], 1);
    assert!(diagnostics["decoder"]["badaudio.esp"]["first_example"].is_string());
}
