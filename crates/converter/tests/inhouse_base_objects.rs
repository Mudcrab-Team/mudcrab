//! Native base-object fields, asymmetric identities, and local publication recovery.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{inhouse_perks as native, layout};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

const OWNERS: [[u8; 4]; 6] = [*b"ACTI", *b"FURN", *b"MSTT", *b"DOOR", *b"TREE", *b"FLOR"];
const FIRST_ID: u32 = 0x6A00;
type Fields = Vec<([u8; 4], Vec<u8>)>;

/// Distinct native links are supplied explicitly instead of consulting the schema.
#[derive(Clone, Copy, Default)]
struct Links {
    sound: u32,
    keyword: u32,
    light_keyword: u32,
    water: u32,
    ingredient: u32,
    explosion: u32,
    debris: u32,
    spell: u32,
    destination: u32,
}

/// Frame independently authored subrecords with the ordinary native six-byte header.
fn frame(fields: &Fields) -> Vec<u8> {
    fields
        .iter()
        .flat_map(|(tag, raw)| native::subrecord(tag, raw))
        .collect()
}

/// Keep native field order and payloads without confusing them with archive encoding.
fn raw_fields(fields: &Fields) -> Vec<(Vec<u8>, Vec<u8>)> {
    fields
        .iter()
        .map(|(tag, raw)| (tag.to_vec(), raw.clone()))
        .collect()
}

/// The database stores an archive of the independently expected canonical field list.
fn archive(fields: &Fields) -> Vec<u8> {
    converter::esm::extractors::serialize_subrecords(&raw_fields(fields))
}

/// Construct native fields from manually checked widths, ordered groups and target roles.
fn object_fields(owner: &[u8; 4], links: Links, changed: bool) -> Fields {
    let mut fields = vec![(*b"EDID", native::text("NativeBaseObject"))];
    fields.push((
        *b"OBND",
        [-31i16, -17, -3, 47, 79, 113]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect(),
    ));
    if owner != b"TREE" {
        fields.push((
            *b"FULL",
            native::text(if changed {
                "Overridden native object"
            } else {
                "Original native object"
            }),
        ));
    }
    fields.push((*b"MODL", native::text("meshes\\native\\object.nif")));
    fields.push((*b"MODT", vec![0x91, 0xC7, 0x53, 0x2B]));
    if matches!(owner, b"ACTI" | b"FURN" | b"MSTT" | b"DOOR" | b"FLOR") {
        let mut header = (-173i32).to_le_bytes().to_vec();
        header.extend([2, 1, 0xA7, 0x53]);
        fields.push((*b"DEST", header));
        for (index, percent) in [(3u8, 71u8), (7, 29)] {
            let mut stage = vec![percent, index, index + 1, 0xA5];
            stage.extend((-39i32).to_le_bytes());
            stage.extend(links.explosion.to_le_bytes());
            stage.extend(links.debris.to_le_bytes());
            stage.extend(17i32.to_le_bytes());
            fields.push((*b"DSTD", stage));
            fields.push((*b"DMDL", native::text("meshes\\native\\damaged.nif")));
            fields.push((*b"DMDT", vec![0x19, 0xC3, index]));
            fields.push((*b"DSTF", vec![]));
        }
    }
    if matches!(owner, b"ACTI" | b"FURN" | b"FLOR") {
        fields.push((*b"KSIZ", 2u32.to_le_bytes().to_vec()));
        fields.push((
            *b"KWDA",
            [
                links.keyword.to_le_bytes(),
                links.light_keyword.to_le_bytes(),
            ]
            .concat(),
        ));
        // This opaque/color word resembles a FormID and must remain untouched.
        fields.push((*b"PNAM", vec![0x31, 0x09, 0x00, 0x01]));
    }
    match owner {
        b"ACTI" => {
            fields.push((*b"SNAM", links.sound.to_le_bytes().to_vec()));
            fields.push((*b"VNAM", links.sound.to_le_bytes().to_vec()));
            fields.push((*b"WNAM", links.water.to_le_bytes().to_vec()));
            fields.push((*b"RNAM", native::text("Distinct activation prompt")));
            fields.push((*b"FNAM", 0xA503u16.to_le_bytes().to_vec()));
            fields.push((*b"KNAM", links.light_keyword.to_le_bytes().to_vec()));
        }
        b"FURN" => {
            fields.push((*b"FNAM", 0xC102u16.to_le_bytes().to_vec()));
            fields.push((*b"KNAM", links.keyword.to_le_bytes().to_vec()));
            fields.push((*b"MNAM", 0x0100_0931u32.to_le_bytes().to_vec()));
            fields.push((*b"WBDT", vec![7, 0xF9]));
            fields.push((*b"NAM1", links.spell.to_le_bytes().to_vec()));
            for index in [3u32, 11] {
                fields.push((*b"ENAM", index.to_le_bytes().to_vec()));
                fields.push((*b"NAM0", vec![0xA7, 0x53, 0x05, 0x80]));
                fields.push((*b"FNMK", links.light_keyword.to_le_bytes().to_vec()));
            }
            fields.push((*b"FNPR", vec![3, 0, 0x15, 0x80]));
            fields.push((*b"FNPR", vec![1, 0, 0x09, 0x40]));
            fields.push((*b"XMRK", native::text("meshes\\native\\marker.nif")));
        }
        b"MSTT" => {
            fields.push((*b"DATA", vec![0x85]));
            fields.push((*b"SNAM", links.sound.to_le_bytes().to_vec()));
        }
        b"DOOR" => {
            fields.push((*b"SNAM", links.sound.to_le_bytes().to_vec()));
            fields.push((*b"ANAM", links.sound.to_le_bytes().to_vec()));
            fields.push((*b"BNAM", links.sound.to_le_bytes().to_vec()));
            fields.push((*b"FNAM", vec![0xB6]));
            fields.push((*b"TNAM", links.destination.to_le_bytes().to_vec()));
            fields.push((*b"TNAM", links.destination.to_le_bytes().to_vec()));
        }
        b"TREE" => {
            fields.push((*b"PFIG", links.ingredient.to_le_bytes().to_vec()));
            fields.push((*b"SNAM", links.sound.to_le_bytes().to_vec()));
            fields.push((*b"PFPC", vec![11, 37, 59, 83]));
            fields.push((*b"FULL", native::text("Independent harvested tree")));
            fields.push((
                *b"CNAM",
                [
                    -3.25f32, 1.875, 7.125, -11.5, 0.625, 13.75, 0.375, -17.25, 19.5, 2.125,
                    -23.75, 29.125,
                ]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
            ));
        }
        b"FLOR" => {
            fields.push((*b"RNAM", native::text("Harvest independent ingredient")));
            fields.push((*b"FNAM", vec![0xA7, 0x53]));
            fields.push((*b"PFIG", links.ingredient.to_le_bytes().to_vec()));
            fields.push((*b"SNAM", links.sound.to_le_bytes().to_vec()));
            fields.push((*b"PFPC", vec![17, 43, 61, 97]));
        }
        _ => unreachable!(),
    }
    fields
}

/// A real world and assets support both direct reading and publication tests.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Add all six native base records after the standard synthetic world.
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
        let path = data.join("Skyrim.esm");
        let mut bytes = fs::read(&path).unwrap();
        let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let header = bytes[24..24 + size].to_vec();
        for (index, owner) in OWNERS.iter().enumerate() {
            bytes.extend(native::record(
                owner,
                FIRST_ID + index as u32,
                0,
                &frame(&object_fields(owner, Links::default(), false)),
            ));
        }
        fs::write(&path, bytes).unwrap();
        Self {
            directory,
            data,
            header,
            paths: vec![path],
        }
    }

    /// Explicit master order allows local full/light words to differ from global words.
    fn plugin(&mut self, name: &str, flags: u32, masters: &[&str], records: &[u8]) {
        let mut header = self.header.clone();
        for master in masters {
            header.extend(native::subrecord(b"MAST", &native::text(master)));
            header.extend(native::subrecord(b"DATA", &[0xA5; 8]));
        }
        let mut bytes = native::record(b"TES4", 0, flags, &header);
        bytes.extend(records);
        let path = self.data.join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Supply accepted target kinds without borrowing fixture schemas for their layout.
    fn targets(&mut self) {
        // Master priority keeps this deliberate full-slot spacer ahead of Objects.esm
        // under both direct reading and the public plugins.txt normalization path.
        self.plugin("Unrelated.esm", 0, &["Skyrim.esm"], &[]);
        let mut full = vec![];
        for (owner, id) in [
            (b"SNDR", 0x911u32),
            (b"KYWD", 0x916),
            (b"WATR", 0x917),
            (b"EXPL", 0x918),
            (b"SPEL", 0x919),
            (b"WRLD", 0x91A),
        ] {
            full.extend(native::record(
                owner,
                0x0100_0000 | id,
                0,
                &native::subrecord(b"EDID", &native::text("NativeFullTarget")),
            ));
        }
        self.plugin("Objects.esm", 0, &["Skyrim.esm"], &full);
        let mut light = vec![];
        for (owner, id) in [(b"KYWD", 0x931u32), (b"INGR", 0x933), (b"DEBR", 0x934)] {
            light.extend(native::record(
                owner,
                0x0100_0000 | id,
                0,
                &native::subrecord(b"EDID", &native::text("NativeLightTarget")),
            ));
        }
        self.plugin("ObjectsLight.esl", 0x200, &["Skyrim.esm"], &light);
    }

    /// Exercise the actual accepted-winner catalogue and post-winner target validation.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// File master1 is global full2; file master0 is global light0.
fn source_links() -> Links {
    Links {
        sound: 0x0100_0911,
        keyword: 0x0100_0916,
        light_keyword: 0x0000_0931,
        water: 0x0100_0917,
        ingredient: 0x0000_0933,
        explosion: 0x0100_0918,
        debris: 0x0000_0934,
        spell: 0x0100_0919,
        destination: 0x0100_091A,
    }
}

/// Canonical expected words are independent explicit constants, never decoded from output.
fn canonical_links() -> Links {
    Links {
        sound: 0x0200_0911,
        keyword: 0x0200_0916,
        light_keyword: 0xFE00_0931,
        water: 0x0200_0917,
        ingredient: 0xFE00_0933,
        explosion: 0x0200_0918,
        debris: 0xFE00_0934,
        spell: 0x0200_0919,
        destination: 0x0200_091A,
    }
}

/// Find one physical field by signature without assuming schema field names.
fn field<'a>(record: &'a DecodedRecord, tag: &[u8; 4]) -> &'a Value {
    &record
        .fields
        .iter()
        .find(|field| &field.signature == tag)
        .unwrap()
        .value
}

/// Read a typed member while keeping its signed/unsigned/float envelope observable.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// Every native occurrence and byte survives; repeated destruction and marker groups stay ordered.
#[test]
fn six_base_object_layouts_preserve_every_native_field() {
    let fixture = Fixture::new();
    let result = fixture.read();
    for (index, owner) in OWNERS.iter().enumerate() {
        let record = &result.records[&(FIRST_ID + index as u32)];
        let expected = object_fields(owner, Links::default(), false);
        assert!(record.rejected_fields.is_empty(), "{owner:?}");
        assert_eq!(
            record.to_raw_record().subrecords,
            raw_fields(&expected),
            "lost native fields for {owner:?}"
        );
    }
    let furniture = &result.records[&(FIRST_ID + 1)];
    assert_eq!(
        member(field(furniture, b"WBDT"), "bench_type"),
        &Value::Unsigned(7)
    );
    assert_eq!(
        member(field(furniture, b"WBDT"), "uses_skill"),
        &Value::Signed(-7)
    );
    assert_eq!(
        field(furniture, b"PNAM"),
        &Value::Bytes(vec![0x31, 0x09, 0, 1])
    );
    assert_eq!(
        member(field(furniture, b"NAM0"), "unused"),
        &Value::Bytes(vec![0xA7, 0x53])
    );
    assert_eq!(
        member(
            field(&result.records[&(FIRST_ID + 4)], b"CNAM"),
            "trunk_flexibility"
        ),
        &Value::Float(-3.25)
    );
    assert_eq!(
        field(&result.records[&(FIRST_ID + 5)], b"FNAM"),
        &Value::Bytes(vec![0xA7, 0x53])
    );
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
    assert_eq!(result.diagnostics["skyrim.esm"].unexpected_subrecords, 0);
}

/// Localized native words remain typed IDs, including zero, without fabricated text.
#[test]
fn localized_base_object_words_remain_ids() {
    let mut fixture = Fixture::new();
    let mut records = vec![];
    for (index, owner) in OWNERS.iter().enumerate() {
        let mut fields = object_fields(owner, Links::default(), true);
        for (tag, raw) in &mut fields {
            if tag == b"FULL" {
                *raw = 0u32.to_le_bytes().to_vec();
            }
            if tag == b"RNAM" {
                *raw = 404u32.to_le_bytes().to_vec();
            }
        }
        records.extend(native::record(
            owner,
            0x0100_0000 | (FIRST_ID + index as u32),
            0,
            &frame(&fields),
        ));
    }
    fixture.plugin("LocalizedObjects.esp", 0x80, &["Skyrim.esm"], &records);
    let result = fixture.read();
    for (index, owner) in OWNERS.iter().enumerate() {
        let record = &result.records[&(0x0100_0000 | (FIRST_ID + index as u32))];
        assert_eq!(field(record, b"FULL"), &Value::LocalizedString(0));
        if matches!(owner, b"ACTI" | b"FLOR") {
            assert_eq!(field(record, b"RNAM"), &Value::LocalizedString(404));
        }
    }
}

/// Full/light remapping, damaged known fields and compressed overrides remain local in publication.
#[tokio::test]
async fn asymmetric_base_overrides_and_malformed_neighbors_publish_exact_bytes() {
    let mut fixture = Fixture::new();
    fixture.targets();
    let mut records = vec![];
    for (index, owner) in OWNERS.iter().enumerate() {
        let mut fields = object_fields(owner, source_links(), true);
        if owner == b"ACTI" {
            fields.iter_mut().find(|(tag, _)| tag == b"FNAM").unwrap().1 = vec![0xA7];
        }
        if owner == b"MSTT" {
            records.extend(native::record(
                owner,
                0x0200_0000 | (FIRST_ID + index as u32),
                0x40000,
                &[24, 0, 0, 0, 99],
            ));
        } else {
            records.extend(native::record(
                owner,
                0x0200_0000 | (FIRST_ID + index as u32),
                0,
                &frame(&fields),
            ));
        }
    }
    let neighbor_id = 0x0300_6A06;
    records.extend(native::record(
        b"ACTI",
        neighbor_id,
        0,
        &frame(&object_fields(b"ACTI", source_links(), true)),
    ));
    fixture.plugin(
        "NativeObjectPatch.esp",
        0,
        &["ObjectsLight.esl", "Objects.esm", "Skyrim.esm"],
        &records,
    );
    let result = fixture.read();
    let warnings = &result.diagnostics["nativeobjectpatch.esp"];
    assert_eq!(warnings.skipped_records, 1);
    assert_eq!(warnings.skipped_fields, 1);
    assert_eq!(warnings.invalid_links, 0);
    assert!(warnings.first_by_category["field"].contains("FNAM"));
    let mut expected_records = vec![];
    for (index, owner) in OWNERS.iter().enumerate() {
        let id = FIRST_ID + index as u32;
        let record = &result.records[&id];
        let mut expected = object_fields(
            owner,
            if owner == b"MSTT" {
                Links::default()
            } else {
                canonical_links()
            },
            owner != b"MSTT",
        );
        if owner == b"ACTI" {
            expected.retain(|(tag, _)| tag != b"FNAM");
        }
        assert_eq!(record.load_order, if owner == b"MSTT" { 0 } else { 4 });
        assert_eq!(
            record.to_raw_record().subrecords,
            raw_fields(&expected),
            "wrong canonical {owner:?}"
        );
        expected_records.push((
            id,
            archive(&expected),
            record.raw_payload.clone(),
            record.load_order,
        ));
    }
    let neighbor = &result.records[&neighbor_id];
    expected_records.push((
        neighbor_id,
        archive(&object_fields(b"ACTI", canonical_links(), true)),
        neighbor.raw_payload.clone(),
        4,
    ));
    let plugins = fixture.directory.path().join("plugins.txt");
    fs::write(
        &plugins,
        "*Skyrim.esm\n*Unrelated.esm\n*Objects.esm\n*ObjectsLight.esl\n*NativeObjectPatch.esp\n",
    )
    .unwrap();
    assert_eq!(
        converter::esm::read_plugins_txt(&plugins, &fixture.data).unwrap(),
        fixture.paths,
        "public publication order must retain the independently expected full/light slots"
    );
    let output = fixture.directory.path().join("pack");
    let mut config = PipelineConfig::new(&fixture.data, &output);
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
    for (id, canonical, source, priority) in expected_records {
        let (published,raw,load_order):(Vec<u8>,Vec<u8>,u32) = database.query_row(
            "SELECT records.data,source.payload,source.load_order FROM records JOIN inhouse_source_records source ON source.form_id=records.form_id WHERE records.form_id=?",
            [id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).unwrap();
        assert_eq!(published, canonical, "lost canonical neighbor {id:08X}");
        assert_eq!(raw, source, "source witness changed {id:08X}");
        assert_eq!(load_order, priority);
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["nativeobjectpatch.esp"]["skipped_records"],
        1
    );
    assert_eq!(
        diagnostics["decoder"]["nativeobjectpatch.esp"]["skipped_fields"],
        1
    );
}
