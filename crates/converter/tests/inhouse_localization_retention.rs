//! Original localized IDs survive publication separately from safe runtime text.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::{inhouse, types::ArchivedRecordData},
};
use dummy_content::{esm, inhouse_dialogue as native, layout};
use rusqlite::Connection;
use std::{fs, path::Path};

/// Generate a small native world without depending on authored record schemas.
fn world() -> Vec<u8> {
    esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap()
}

/// STRINGS offsets are relative to their data region; malformed entries stay bounded.
fn strings(entries: &[(u32, &str)], malformed_id: Option<u32>, prefixed: bool) -> Vec<u8> {
    let mut directory = Vec::new();
    let mut payload = Vec::new();
    if let Some(id) = malformed_id {
        directory.extend(id.to_le_bytes());
        if prefixed {
            directory.extend(0u32.to_le_bytes());
            payload.extend(u32::MAX.to_le_bytes());
        } else {
            directory.extend(u32::MAX.to_le_bytes());
        }
    }
    for &(id, text) in entries {
        directory.extend(id.to_le_bytes());
        directory.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
        // These fixtures use ASCII/Latin-1 plus Euro, all representable in CP1252.
        // Keep expected Unicode text unchanged while writing the native English encoding.
        let mut encoded: Vec<u8> = text
            .chars()
            .map(|character| {
                if character == '\u{20AC}' {
                    return 0x80;
                }
                let codepoint = u32::from(character);
                assert!(codepoint < 0x80 || (0xA0..=0xFF).contains(&codepoint));
                u8::try_from(codepoint).unwrap()
            })
            .collect();
        encoded.push(0);
        if prefixed {
            payload.extend(u32::try_from(encoded.len()).unwrap().to_le_bytes());
        }
        payload.extend(encoded);
    }
    [
        u32::try_from(entries.len() + usize::from(malformed_id.is_some()))
            .unwrap()
            .to_le_bytes()
            .to_vec(),
        u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec(),
        directory,
        payload,
    ]
    .concat()
}

/// A selected plugin's loose table must win over the previous record owner's bank.
fn write_strings(data: &Path, plugin: &str, bank: &str, bytes: &[u8]) {
    let folder = data.join("Strings");
    fs::create_dir_all(&folder).unwrap();
    let stem = Path::new(plugin).file_stem().unwrap().to_str().unwrap();
    fs::write(folder.join(format!("{stem}_english.{bank}")), bytes).unwrap();
}

/// One independently framed subtitle with an unrelated editor ID preceding it.
fn sound(id: u32, string_id: u32) -> Vec<u8> {
    native::named(
        b"SNDR",
        id,
        &[native::subrecord(b"FNAM", &string_id.to_le_bytes())],
    )
}

/// Read the established runtime archive instead of accepting only diagnostic typed data.
fn published(database: &Connection, id: u32) -> ArchivedRecordData {
    let bytes: Vec<u8> = database
        .query_row("SELECT data FROM records WHERE form_id=?", [id], |row| {
            row.get(0)
        })
        .unwrap();
    rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap()
}

/// Keep repeated native signatures in physical order, including safe empty text leaves.
fn runtime_fields(database: &Connection, id: u32, tag: &[u8; 4]) -> Vec<Vec<u8>> {
    published(database, id)
        .subrecords
        .into_iter()
        .filter(|field| &field.tag == tag)
        .map(|field| field.data)
        .collect()
}

type LocalizedRow = (u32, String, String, String, u32, Option<String>, String);

/// Publication records every original key and distinguishes missing from resolved-empty.
fn localized(database: &Connection, id: u32) -> Vec<LocalizedRow> {
    database
        .prepare(
            "SELECT field_index,signature,field_name,string_table,string_id,resolved_text,status \
             FROM inhouse_localized_fields WHERE form_id=? ORDER BY field_index",
        )
        .unwrap()
        .query_map([id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Compare public metadata with independently chosen native values and matched roles.
fn expected(
    index: u32,
    signature: &str,
    field: &str,
    bank: &str,
    id: u32,
    text: Option<&str>,
    status: &str,
) -> (u32, String, String, String, u32, Option<String>, String) {
    (
        index,
        signature.to_owned(),
        field.to_owned(),
        bank.to_owned(),
        id,
        text.map(str::to_owned),
        status.to_owned(),
    )
}

/// Retain source bytes and source winning priority independently of global FormID slots.
fn source(database: &Connection, id: u32) -> (u32, u32, Vec<u8>) {
    database
        .query_row(
            "SELECT source_form_id,load_order,payload FROM inhouse_source_records WHERE form_id=?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

/// Fast actual publication preserves missing, resolved and repeated fields and original IDs.
#[test]
fn localized_fields_preserve_zero_missing_resolved_and_repeated_keys() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    fs::create_dir(&data).unwrap();
    let generated = world();
    fs::write(data.join("Skyrim.esm"), &generated).unwrap();
    // Missing17 is first so the pre-fix RED is a canonical-field assertion,
    // before any query against the newly introduced provenance table.
    let ids = [17, 0, 18, 100, 101, 42, 100];
    let mut records = Vec::new();
    for (index, string_id) in ids.into_iter().enumerate() {
        records.extend(sound(
            0x0100_3500 + u32::try_from(index).unwrap(),
            string_id,
        ));
    }
    let mut response_fields = Vec::new();
    for (index, string_id) in [17u32, 100, 0].into_iter().enumerate() {
        let mut header = [0u8; 24];
        header[12] = u8::try_from(index + 1).unwrap();
        response_fields.push(native::subrecord(b"TRDT", &header));
        response_fields.push(native::subrecord(b"NAM1", &string_id.to_le_bytes()));
    }
    response_fields.push(native::subrecord(b"RNAM", &18u32.to_le_bytes()));
    let info = native::named(b"INFO", 0x0100_3510, &response_fields);
    records.extend(&info);
    fs::write(
        data.join("LocalizedFields.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0x80, &records),
    )
    .unwrap();
    // Even a physical zero-key entry must never turn the null sentinel into invented text.
    write_strings(
        &data,
        "LocalizedFields.esp",
        "strings",
        &strings(
            &[
                (0, "Forbidden zero-key display text"),
                (100, "Resolved STRINGS résumé"),
                (101, ""),
            ],
            Some(42),
            false,
        ),
    );
    write_strings(
        &data,
        "LocalizedFields.esp",
        "ilstrings",
        &strings(&[(100, "Resolved ILSTRINGS €")], Some(17), true),
    );
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*LocalizedFields.esp\n").unwrap();
    let output = temp.path().join("bundle");
    inhouse::export_record_bundle_typed(&data, &plugins, &output, &[*b"SNDR", *b"INFO"]).unwrap();
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for (index, string_id) in ids.into_iter().enumerate() {
        let rid = 0x0100_3500 + u32::try_from(index).unwrap();
        let text = match string_id {
            100 => Some("Resolved STRINGS résumé"),
            101 => Some(""),
            _ => None,
        };
        let status = if string_id == 0 {
            "null_id"
        } else if text.is_some() {
            "resolved"
        } else {
            "missing"
        };
        let runtime = [text.unwrap_or_default().as_bytes(), &[0]].concat();
        assert_eq!(runtime_fields(&database, rid, b"FNAM"), [runtime]);
        assert_eq!(
            localized(&database, rid),
            [expected(
                1, "FNAM", "subtitle", "strings", string_id, text, status
            )]
        );
        let framed = sound(rid, string_id);
        assert_eq!(source(&database, rid), (rid, 1, framed[24..].to_vec()));
    }
    assert_eq!(
        runtime_fields(&database, 0x0100_3510, b"NAM1"),
        [
            b"\0".to_vec(),
            "Resolved ILSTRINGS €\0".as_bytes().to_vec(),
            b"\0".to_vec()
        ]
    );
    assert_eq!(
        runtime_fields(&database, 0x0100_3510, b"RNAM"),
        [b"\0".to_vec()]
    );
    assert_eq!(
        localized(&database, 0x0100_3510),
        [
            expected(2, "NAM1", "response_text", "ilstrings", 17, None, "missing"),
            expected(
                4,
                "NAM1",
                "response_text",
                "ilstrings",
                100,
                Some("Resolved ILSTRINGS €"),
                "resolved"
            ),
            expected(6, "NAM1", "response_text", "ilstrings", 0, None, "null_id"),
            expected(7, "RNAM", "prompt", "strings", 18, None, "missing"),
        ]
    );
    assert_eq!(source(&database, 0x0100_3510).2, info[24..]);
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["adapter"]["localizedfields.esp"][0], 5);
    assert_eq!(
        diagnostics["adapter"]["localizedfields.esp"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        diagnostics["adapter"]["localizedfields.esp"][1]
            .as_str()
            .unwrap()
            .len()
            < 1024
    );
}

/// English plugin/bank bytes use CP1252 even if they also form valid UTF-8; VMAD uses UTF-8.
#[test]
fn english_text_encoding_is_explicit_and_distinct_from_vmad() {
    let directory = tempfile::tempdir().unwrap().keep();
    let data = directory.join("Data");
    fs::create_dir(&data).unwrap();
    let generated = world();
    fs::write(data.join("Skyrim.esm"), &generated).unwrap();
    let vmad = [
        5u16.to_le_bytes().as_slice(),
        2u16.to_le_bytes().as_slice(),
        1u16.to_le_bytes().as_slice(),
        2u16.to_le_bytes().as_slice(),
        &[0xC3, 0xA9, 0],
        0u16.to_le_bytes().as_slice(),
    ]
    .concat();
    let inline = native::named(
        b"ACTI",
        0x0100_3F01,
        &[
            native::subrecord(b"VMAD", &vmad),
            native::subrecord(b"FULL", &[0xC3, 0xA9, 0]),
        ],
    );
    fs::write(
        data.join("InlineEncoding.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0, &inline),
    )
    .unwrap();
    fs::write(
        data.join("BankEncoding.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0x80, &sound(0x0100_3F02, 100)),
    )
    .unwrap();
    let bank = [
        1u32.to_le_bytes().as_slice(),
        3u32.to_le_bytes().as_slice(),
        100u32.to_le_bytes().as_slice(),
        0u32.to_le_bytes().as_slice(),
        &[0xC3, 0xA9, 0],
    ]
    .concat();
    write_strings(&data, "BankEncoding.esp", "strings", &bank);
    let list = directory.join("plugins.txt");
    fs::write(
        &list,
        "*Skyrim.esm\n*InlineEncoding.esp\n*BankEncoding.esp\n",
    )
    .unwrap();
    let output = directory.join("encoding-output");
    inhouse::export_record_bundle(&data, &list, &output).unwrap();
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let expected = "\u{00C3}\u{00A9}";
    assert_eq!(
        runtime_fields(&db, 0x0100_3F01, b"FULL"),
        vec![[expected.as_bytes(), &[0]].concat()]
    );
    assert_eq!(
        runtime_fields(&db, 0x0200_3F02, b"FNAM"),
        vec![[expected.as_bytes(), &[0]].concat()]
    );
    let resolved: String = db
        .query_row(
            "SELECT resolved_text FROM inhouse_localized_fields WHERE form_id=?",
            [0x0200_3F02u32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(resolved, expected);
    assert_eq!(runtime_fields(&db, 0x0100_3F01, b"VMAD"), vec![vmad]);
    let paths = converter::esm::read_plugins_txt(&list, &data).unwrap();
    let order = converter::esm::load_order::LoadOrder::read(&paths).unwrap();
    let decoded = converter::records::read_plugins(&paths, &order).unwrap();
    let record = &decoded.records[&0x0100_3F01];
    let script = record
        .fields
        .iter()
        .find(|field| field.signature == *b"VMAD")
        .unwrap();
    assert!(
        serde_json::to_string(&script.value)
            .unwrap()
            .contains("\"\u{00E9}\""),
        "VMAD script name uses its independent UTF-8 encoding"
    );
    let payload: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?",
            [0x0100_3F01u32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        payload,
        inline[24..],
        "original ambiguous bytes remain independently preserved"
    );
}

/// Actual parser/pipeline publication retains winning IDs despite missing text and light slots.
#[tokio::test]
async fn winning_full_and_light_banks_preserve_ids_and_bound_missing_text_warnings() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let generated = fs::read(data.join("Skyrim.esm")).unwrap();
    let earlier = [sound(0x0100_3500, 100), sound(0x0100_3501, 100)].concat();
    fs::write(
        data.join("EarlierFull.esm"),
        native::plugin(&generated, &["Skyrim.esm"], 0x81, &earlier),
    )
    .unwrap();
    fs::write(
        data.join("Filler.esm"),
        native::plugin(&generated, &["Skyrim.esm"], 1, &[]),
    )
    .unwrap();
    fs::write(
        data.join("FirstLight.esl"),
        native::plugin(
            &generated,
            &["Skyrim.esm"],
            0x280,
            &[sound(0x0100_0ABA, 100), sound(0x0100_0ABB, 0)].concat(),
        ),
    )
    .unwrap();
    let mut winning = [
        sound(0x0100_3500, 17),
        sound(0x0200_0ABA, 18),
        sound(0x0300_3501, 100),
        native::record(
            b"CELL",
            0x0300_3510,
            0,
            &[
                native::subrecord(b"FULL", &0u32.to_le_bytes()),
                native::subrecord(b"DATA", &1u16.to_le_bytes()),
            ]
            .concat(),
        ),
    ]
    .concat();
    for index in 0..128 {
        winning.extend(sound(0x0300_3600 + index, 17));
    }
    fs::write(
        data.join("WinnerFull.esp"),
        native::plugin(
            &generated,
            &["Skyrim.esm", "EarlierFull.esm", "FirstLight.esl"],
            0x80,
            &winning,
        ),
    )
    .unwrap();
    let second_light = [sound(0x0100_3500, 17), sound(0x0400_0ABA, 100)].concat();
    fs::write(
        data.join("SecondLight.esl"),
        native::plugin(
            &generated,
            &[
                "Skyrim.esm",
                "EarlierFull.esm",
                "FirstLight.esl",
                "WinnerFull.esp",
            ],
            0x280,
            &second_light,
        ),
    )
    .unwrap();
    write_strings(
        &data,
        "EarlierFull.esm",
        "strings",
        &strings(
            &[
                (17, "Stale full-owner text"),
                (100, "Earlier surviving neighbor"),
            ],
            None,
            false,
        ),
    );
    write_strings(
        &data,
        "FirstLight.esl",
        "strings",
        &strings(
            &[
                (0, "Forbidden light zero text"),
                (18, "Stale light-owner text"),
                (100, "Earlier losing light text"),
            ],
            None,
            false,
        ),
    );
    write_strings(
        &data,
        "WinnerFull.esp",
        "strings",
        &strings(&[(100, "Winning full text")], Some(18), false),
    );
    write_strings(
        &data,
        "SecondLight.esl",
        "strings",
        &strings(&[(100, "Distinct second-light text")], Some(17), false),
    );
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*EarlierFull.esm\n*Filler.esm\n*FirstLight.esl\n*WinnerFull.esp\n*SecondLight.esl\n").unwrap();
    let selected = converter::esm::read_plugins_txt(&plugins, &data).unwrap();
    let names = selected
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "Skyrim.esm",
            "EarlierFull.esm",
            "Filler.esm",
            "FirstLight.esl",
            "WinnerFull.esp",
            "SecondLight.esl"
        ],
        "fixture preserves deliberate full/light priorities after normalization"
    );
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
    for (rid, raw, priority, key, text, status) in [
        (0x0100_3500, 0x0100_3500, 5, 17, None, "missing"),
        (0xFE00_0ABA, 0x0200_0ABA, 4, 18, None, "missing"),
        (0xFE00_0ABB, 0x0100_0ABB, 3, 0, None, "null_id"),
        (
            0x0300_3501,
            0x0300_3501,
            4,
            100,
            Some("Winning full text"),
            "resolved",
        ),
        (
            0xFE00_1ABA,
            0x0400_0ABA,
            5,
            100,
            Some("Distinct second-light text"),
            "resolved",
        ),
        (
            0x0100_3501,
            0x0100_3501,
            1,
            100,
            Some("Earlier surviving neighbor"),
            "resolved",
        ),
    ] {
        assert_eq!(
            runtime_fields(&database, rid, b"FNAM"),
            [[text.unwrap_or_default().as_bytes(), &[0]].concat()]
        );
        assert_eq!(
            localized(&database, rid),
            [expected(
                1, "FNAM", "subtitle", "strings", key, text, status
            )]
        );
        let framed = sound(raw, key);
        assert_eq!(
            source(&database, rid),
            (raw, priority, framed[24..].to_vec())
        );
    }
    for index in 0..128 {
        let rid = 0x0300_3600 + index;
        assert_eq!(runtime_fields(&database, rid, b"FNAM"), [b"\0".to_vec()]);
        assert_eq!(
            localized(&database, rid),
            [expected(
                1, "FNAM", "subtitle", "strings", 17, None, "missing"
            )]
        );
    }
    // A retained empty wire field must still project an unresolved runtime name as SQL NULL.
    assert_eq!(
        runtime_fields(&database, 0x0300_3510, b"FULL"),
        [b"\0".to_vec()]
    );
    let interior_name: Option<String> = database
        .query_row(
            "SELECT interior_name FROM cells WHERE id=?",
            [0x0300_3510u32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(interior_name, None);
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    for (plugin, count) in [("winnerfull.esp", 129), ("secondlight.esl", 1)] {
        assert_eq!(diagnostics["adapter"][plugin][0], count);
        let summary = diagnostics["adapter"][plugin].as_array().unwrap();
        assert_eq!(
            summary.len(),
            2,
            "retain one bounded example, not every record"
        );
        assert!(!summary[1].as_str().unwrap().is_empty());
        assert!(summary[1].as_str().unwrap().len() < 1024);
    }
}
