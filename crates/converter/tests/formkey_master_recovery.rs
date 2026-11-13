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

/// Preserve old typed publication without changing native ownership or usable neighbors.
#[test]
fn typed_optional_projection_preserves_null_after_native_self_resolution() {
    use converter::esm::form_keys::FormKeyOrder;
    use shared::source_form_id::ZeroPolicy;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    plugin(
        &data,
        "Synthetic.esm",
        &[],
        [
            record(b"STAT", 0x3f, 0, &sub(b"EDID", b"valid_target\0")),
            record(b"STAT", 0x811, 0, &sub(b"EDID", b"before\0")),
            record(
                b"NAVM",
                0x822,
                0,
                &sub(b"ONAM", &0x3f00_003fu32.to_le_bytes()),
            ),
            record(b"STAT", 0x833, 0, &sub(b"EDID", b"after\0")),
        ]
        .concat(),
    );
    let order = FormKeyOrder::read(&[data.join("Synthetic.esm")]).unwrap();
    let key = order
        .resolve(0, 0x3f00_003f, ZeroPolicy::Null)
        .unwrap()
        .unwrap();
    assert_eq!(key.local(), 0x3f);
    assert_eq!(key.plugin(), order.table().id("Synthetic.esm").unwrap());
    let list = data.join("plugins.txt");
    fs::write(&list, "*Synthetic.esm\n").unwrap();
    let output = root.path().join("published");
    inhouse::export_record_bundle(&data, &list, &output).unwrap();
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let bytes: Vec<u8> = conn
        .query_row("SELECT data FROM records WHERE form_id=2082", [], |row| {
            row.get(0)
        })
        .unwrap();
    let archived =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&bytes)
            .unwrap();
    assert_eq!(archived.subrecords.len(), 1);
    assert_eq!(archived.subrecords[0].tag, *b"ONAM");
    assert_eq!(
        archived.subrecords[0].data, [0; 4],
        "legacy typed writer keeps NULL even when self:00003F exists"
    );
    let count: u32 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 4);
    let event: (u32, String) = conn
        .query_row(
            "SELECT link_value,reason FROM record_diagnostics WHERE category='link'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        event,
        (
            0x3f00_003f,
            "optional FormID 3F00003F exceeds master list".into()
        )
    );
    let paths = [data.join("Synthetic.esm")];
    let legacy_order = converter::esm::load_order::LoadOrder::read(&paths).unwrap();
    converter::records::read_plugins(&paths, &legacy_order).unwrap();
    let identities = legacy_order.form_key_context().unwrap();
    assert_eq!(
        identities.projection_warnings(),
        [
            "warning: synthetic.esm: 1 FormID occurrence(s) past the original master list resolved to self (first: 3F00003F)"
        ]
    );
    assert_eq!(
        identities.legacy_projection_warnings(),
        [
            "warning: synthetic.esm: LegacyTypedReaderProjection kept NULL for 1 native self FormKey link occurrence(s) (first source: 3F00003F)"
        ]
    );
}

/// Checked light-width projection preserves old diagnostic rows and valid neighbors.
#[test]
fn typed_light_width_projection_retains_old_diagnostics_and_neighbors() {
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
        "Light.esl",
        &["Base.esm"],
        [
            record(b"STAT", 0x0100_0822, 0, &sub(b"EDID", b"before\0")),
            record(
                b"CELL",
                0x0100_0833,
                0,
                &[
                    sub(b"DATA", &[1, 0]),
                    sub(b"XCWT", &0x0100_1234u32.to_le_bytes()),
                ]
                .concat(),
            ),
            record(b"STAT", 0x0100_1235, 0, &sub(b"EDID", b"wide\0")),
            record(b"STAT", 0x0100_0844, 0, &sub(b"EDID", b"after\0")),
        ]
        .concat(),
    );
    let list = data.join("plugins.txt");
    fs::write(&list, "*Base.esm\n*Light.esl\n").unwrap();
    let output = root.path().join("published");
    inhouse::export_record_bundle(&data, &list, &output).unwrap();
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let reasons = conn
        .prepare("SELECT reason FROM record_diagnostics ORDER BY diagnostic_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        reasons
            .iter()
            .any(|r| r == "optional light FormID 01001234 is wider than 12 bits"),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("record light FormID 01001235 is wider than 12 bits")),
        "{reasons:?}"
    );
    let count: u32 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);
    let paths = [data.join("Base.esm"), data.join("Light.esl")];
    let order = converter::esm::load_order::LoadOrder::read(&paths).unwrap();
    converter::records::read_plugins(&paths, &order).unwrap();
    let identities = order.form_key_context().unwrap();
    assert!(
        identities
            .resolve(1, 0x0100_1235, shared::source_form_id::ZeroPolicy::Identity)
            .unwrap()
            .is_some()
    );
    let warnings = identities.legacy_projection_warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("cleared wide-light optional link for 1"))
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("rejected wide-light required identity for 1"))
    );
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
