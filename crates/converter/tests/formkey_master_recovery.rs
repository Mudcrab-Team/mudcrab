//! Publication uses one supplier-self rule for every out-of-range source selector.
use converter::esm::{EsmParser, inhouse};
use rusqlite::Connection;
use std::{fs, path::Path};

/// Frame one synthetic native subrecord.
fn sub(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u16).to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Frame a native record with a deliberately asymmetric source ID.
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        payload,
    ]
    .concat()
}

/// Write a self-contained plugin whose source master selectors remain file-local.
fn plugin(root: &Path, name: &str, masters: &[&str], body: Vec<u8>) {
    let mut header = sub(
        b"HEDR",
        &[1.71f32.to_le_bytes(), [0; 4], 0x800u32.to_le_bytes()].concat(),
    );
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    fs::write(
        root.join(name),
        [record(b"TES4", 0, 0, &header), body].concat(),
    )
    .unwrap();
}

/// Exercise unified header ownership while retaining collision protection and valid neighbors.
#[test]
fn past_master_selectors_resolve_to_self_in_both_writers() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    plugin(
        &data,
        "Base.esm",
        &[],
        record(b"STAT", 0x811, 0, &sub(b"EDID", b"base\0")),
    );
    plugin(
        &data,
        "Patch.esp",
        &["Base.esm"],
        [
            record(b"STAT", 0x0100_0822, 0, &sub(b"EDID", b"before\0")),
            record(b"STAT", 0x0200_0833, 0, &sub(b"EDID", b"invalid\0")),
            record(b"STAT", 0x0100_0844, 0, &sub(b"EDID", b"retained\0")),
            record(b"STAT", 0x0200_0844, 0x20, &[]),
            record(b"STAT", 0x0100_0855, 0, &sub(b"EDID", b"after\0")),
            record(
                b"CELL",
                0x0200_0866,
                0,
                &[sub(b"EDID", b"SelfCell\0"), sub(b"DATA", &[1, 0])].concat(),
            ),
        ]
        .concat(),
    );
    let paths = [data.join("Base.esm"), data.join("Patch.esp")];
    let legacy = root.path().join("legacy.db");
    EsmParser::convert_plugins(&paths, &legacy).unwrap();
    let list = data.join("plugins.txt");
    fs::write(&list, "*Base.esm\n*Patch.esp\n").unwrap();
    let output = root.path().join("inhouse");
    inhouse::export_record_bundle(&data, &list, &output).unwrap();
    for path in [legacy, output.join("skyrim_world.db")] {
        let conn = Connection::open(path).unwrap();
        let ids = conn
            .prepare("SELECT form_id FROM records ORDER BY form_id")
            .unwrap()
            .query_map([], |row| row.get::<_, u32>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            ids,
            [
                0x811,
                0x0100_0822,
                0x0100_0833,
                0x0100_0844,
                0x0100_0855,
                0x0100_0866
            ],
            "out-of-range selectors use self; a colliding candidate cannot erase its valid neighbor"
        );
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["patch.esp"]["skipped_records"], 1);
}

/// The native path admits every full/light owner and refuses only legacy publication.
#[test]
fn extended_native_conversion_preserves_all_owners_and_unrelated_output() {
    use converter::esm::form_key_merge::LegacyWriterOutcome;
    let root = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for (count, extension) in [(255, "esm"), (4097, "esl")] {
        for index in 0..count {
            let name = format!("P{index:04}.{extension}");
            plugin(
                root.path(),
                &name,
                &[],
                record(b"STAT", 0xabc, 0, &sub(b"EDID", b"extended\0")),
            );
            paths.push(root.path().join(name));
        }
    }
    let unrelated = root.path().join("existing-asset.bin");
    fs::write(&unrelated, b"retained unrelated output").unwrap();
    let database = root.path().join("unavailable.db");
    let list = root.path().join("plugins.txt");
    fs::write(
        &list,
        paths
            .iter()
            .map(|path| format!("*{}\n", path.file_name().unwrap().to_string_lossy()))
            .collect::<String>(),
    )
    .unwrap();
    let (converted, outcome) =
        EsmParser::convert_selected_form_key_plugins(root.path(), &list, None, None, &database)
            .unwrap();
    assert_eq!(converted.records.len(), 4352);
    assert!(converted.warnings.is_empty(), "{:?}", converted.warnings);
    assert!(
        matches!(outcome, LegacyWriterOutcome::Unavailable { ref notice }
        if notice.contains("255 full / 4097 light") && notice.contains("FormKey identities remain available"))
    );
    assert_eq!(converted.notices.len(), 1);
    assert!(!database.exists());
    assert_eq!(fs::read(unrelated).unwrap(), b"retained unrelated output");
    let full = converted.order.table().id("P0254.esm").unwrap();
    let light = converted.order.table().id("P4096.esl").unwrap();
    assert!(
        converted
            .records
            .keys()
            .any(|key| key.plugin() == full && key.local() == 0xabc)
    );
    assert!(
        converted
            .records
            .keys()
            .any(|key| key.plugin() == light && key.local() == 0xabc)
    );
}
