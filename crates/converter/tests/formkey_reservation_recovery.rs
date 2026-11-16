//! Regression fixtures are generated during the test; no installed game inputs.
use converter::esm::EsmParser;
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

/// Check both native winners and handwritten SQL after rejected candidate recovery.
fn check(body: Vec<u8>, expected: &[u32]) {
    let root = tempfile::tempdir().unwrap();
    plugin(root.path(), "Broken.esp", &[], body);
    let paths = [root.path().join("Broken.esp")];
    let native = EsmParser::merge_plugins_form_keys(&paths).unwrap();
    let mut ids: Vec<_> = native.records.keys().map(|key| key.local()).collect();
    ids.sort_unstable();
    let native_ids = ids;
    let native_neighbor = native.records.values().any(|record| {
        record
            .source
            .subrecords
            .iter()
            .any(|(tag, bytes)| tag.as_slice() == b"EDID" && bytes.as_slice() == b"neighbor\0")
    });
    assert!(
        native
            .warnings
            .iter()
            .any(|line| line.contains("unusable record candidate")),
        "rejected data must have a skipped-record diagnostic: {:?}",
        native.warnings
    );
    let db = root.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    let ids = conn
        .prepare("SELECT form_id FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |row| row.get::<_, u32>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        (native_ids.as_slice(), ids.as_slice()),
        (expected, expected),
        "native merge and handwritten publication must preserve valid neighbors"
    );
    let blobs = conn
        .prepare("SELECT data FROM records")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let published_neighbor = blobs.iter().any(|blob| {
        let record = rkyv::from_bytes::<
            converter::esm::types::ArchivedRecordData,
            rkyv::rancor::Error,
        >(blob)
        .unwrap();
        record
            .subrecords
            .iter()
            .any(|field| field.tag == *b"EDID" && field.data.as_slice() == b"neighbor\0")
    });
    assert!(
        native_neighbor && published_neighbor,
        "the same-key valid successor must replace the preceding payload"
    );
}

/// Reject missing-EDID GMST without reserving its projected source key.
#[test]
fn missing_edid_does_not_reserve_rejected_source() {
    check(
        [
            record(b"GMST", 0x0100_0800, 0, &[]),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x800],
    );
}

/// An unknown setting tombstone is rejected before reserving its source key.
#[test]
fn unknown_deleted_gmst_does_not_reserve_rejected_source() {
    check(
        [
            record(b"GMST", 0x0100_0800, 0x20, &[]),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x800],
    );
}

/// GMST alias rejection retains the accepted setting and an independent neighbor.
#[test]
fn gmst_alias_rejections_preserve_neighbors() {
    check(
        [
            record(b"GMST", 0x800, 0, &sub(b"EDID", b"iExample\0")),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"bad_alias\0")),
            record(b"GMST", 0x800, 0, &sub(b"EDID", b"iExample\0")),
            record(b"GMST", 0x800, 0, &sub(b"EDID", b"iDifferent\0")),
            record(b"GMST", 0x800, 0, &sub(b"EDID", b"iExample\0")),
            record(b"STAT", 0x801, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x800, 0x801],
    );
}

/// Invalid explicit filenames skip only themselves and preserve valid plugin slots.
#[test]
fn invalid_plugin_name_skips_item_and_preserves_neighbors() {
    let root = tempfile::tempdir().unwrap();
    for (name, id) in [
        ("Before.esp", 0x811),
        (" Invalid.esp", 0x822),
        ("After.esp", 0x833),
    ] {
        plugin(
            root.path(),
            name,
            &[],
            record(b"STAT", id, 0, &sub(b"EDID", b"valid\0")),
        );
    }
    let paths = ["Before.esp", " Invalid.esp", "After.esp"].map(|name| root.path().join(name));
    let native = EsmParser::merge_plugins_form_keys(&paths);
    assert!(
        native.is_ok(),
        "invalid input must be skipped locally: {native:?}"
    );
    let native = native.unwrap();
    assert_eq!(native.records.len(), 2);
    assert!(
        native
            .warnings
            .iter()
            .any(|text| text.contains("plugin name"))
    );
    let db = root.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    let ids = conn
        .prepare("SELECT form_id FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |row| row.get::<_, u32>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(ids, [0x811, 0x0100_0833]);
}
/// A compatibility event is persisted with the resolved ID even without later replay.
#[test]
fn projected_header_diagnostic_has_runtime_id() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    plugin(
        &data,
        "Base.esm",
        &[],
        record(b"STAT", 0x0200_0880, 0, &sub(b"EDID", b"ProjectedHeader\0")),
    );
    let list = data.join("plugins.txt");
    fs::write(&list, "*Base.esm\n").unwrap();
    let output = root.path().join("typed");
    converter::esm::inhouse::export_record_bundle(&data, &list, &output).unwrap();
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let event: Option<u32> = conn.query_row("SELECT form_id FROM record_diagnostics WHERE category='compatibility' AND source_form_id=33556608", [], |r| r.get(0)).unwrap();
    assert_eq!(event, Some(0x880));
}

/// The public typed reader skips an invalid explicit path before constructing identities.
#[test]
fn typed_invalid_plugin_name_keeps_valid_neighbors() {
    let root = tempfile::tempdir().unwrap();
    for (name, id) in [
        ("Before.esp", 0x811),
        (" Invalid.esp", 0x822),
        ("After.esp", 0x833),
    ] {
        plugin(
            root.path(),
            name,
            &[],
            record(b"STAT", id, 0, &sub(b"EDID", b"valid\0")),
        );
    }
    let paths = ["Before.esp", " Invalid.esp", "After.esp"].map(|name| root.path().join(name));
    let order = converter::esm::load_order::LoadOrder::read(&paths).unwrap();
    let result = converter::records::read_plugins(&paths, &order);
    assert!(
        result.is_ok(),
        "typed reader must preserve usable neighbors: {result:?}"
    );
    let result = result.unwrap();
    assert!(
        result
            .diagnostics
            .get("<input>")
            .is_some_and(|diagnostics| diagnostics
                .first_by_category
                .get("input")
                .is_some_and(|warning| warning.contains("1 invalid plugin name"))),
        "skipped inputs must remain visible through the existing diagnostics map"
    );

    let mut ids: Vec<_> = result.records.keys().copied().collect();
    ids.sort_unstable();
    assert_eq!(ids, [0x811, 0x0100_0833]);
    assert_eq!(result.load_order, ["before.esp", "after.esp"]);
    for record in result.records.values() {
        assert_eq!(
            result.load_order[record.load_order as usize],
            if record.form_id == 0x811 {
                "before.esp"
            } else {
                "after.esp"
            }
        );
    }
    for entry in result
        .history
        .iter()
        .filter(|entry| entry.record_type == *b"STAT")
    {
        assert_eq!(
            result.load_order[entry.plugin_index as usize],
            if entry.source_form_id == 0x811 {
                "before.esp"
            } else {
                "after.esp"
            }
        );
        assert!(entry.plugin_index < 2);
    }
}

/// Admission failure before identity reservation cannot suppress a same-local neighbor.
#[test]
fn malformed_payload_does_not_reserve_rejected_source() {
    check(
        [
            record(b"GRAS", 0x0100_0800, 0, &sub(b"MODS", &[1, 2])),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x800],
    );
}

/// A null header has no reservable identity; the following usable record still publishes.
#[test]
fn null_header_does_not_suppress_neighbor() {
    check(
        [
            record(b"STAT", 0, 0, &sub(b"EDID", b"null\0")),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x800],
    );
}

/// An admitted tombstone keeps its reservation and blocks a conflicting projected restoration.
#[test]
fn admitted_tombstone_keeps_source_reservation() {
    check(
        [
            record(b"STAT", 0x0100_0800, 0x20, &[]),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"collision\0")),
            record(b"STAT", 0x801, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x801],
    );
}

/// EDID aliases reserve the supplier's original key for both insertion and admitted deletion.
#[test]
fn gmst_canonicalization_reserves_source_key_before_insert_or_delete() {
    for deleted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let setting = |value: i32| {
            [
                sub(b"EDID", b"iShared\0"),
                sub(b"DATA", &value.to_le_bytes()),
            ]
            .concat()
        };
        plugin(
            root.path(),
            "Base.esm",
            &[],
            record(b"GMST", 0x800, 0, &setting(10)),
        );
        plugin(
            root.path(),
            "Patch.esp",
            &["Base.esm"],
            [
                record(
                    b"GMST",
                    0x0100_0810,
                    if deleted { 0x20 } else { 0 },
                    &setting(20),
                ),
                record(b"GMST", 0x0200_0810, 0, &setting(30)),
                record(b"STAT", 0x0100_0811, 0, &sub(b"EDID", b"neighbor\0")),
            ]
            .concat(),
        );
        let paths = [root.path().join("Base.esm"), root.path().join("Patch.esp")];
        let native = EsmParser::merge_plugins_form_keys(&paths).unwrap();
        assert!(
            native.warnings.iter().any(|line| line.contains("01000810")
                && line.contains("02000810")
                && line.contains("same key")),
            "{:?}",
            native.warnings
        );
        assert_eq!(native.records.len(), if deleted { 1 } else { 2 });
        let db = root.path().join("world.db");
        EsmParser::convert_plugins(&paths, &db).unwrap();
        let conn = Connection::open(db).unwrap();
        let count: u32 = conn
            .query_row("SELECT COUNT(*) FROM records WHERE form_id=2048", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count,
            u32::from(!deleted),
            "a projected spelling cannot restore a tombstone"
        );
        if !deleted {
            let bytes: Vec<u8> = conn
                .query_row("SELECT data FROM records WHERE form_id=2048", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let record = rkyv::from_bytes::<
                converter::esm::types::ArchivedRecordData,
                rkyv::rancor::Error,
            >(&bytes)
            .unwrap();
            assert_eq!(
                record
                    .subrecords
                    .iter()
                    .find(|field| field.tag == *b"DATA")
                    .unwrap()
                    .data,
                20i32.to_le_bytes()
            );
        }
        let neighbor: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=16779281",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(neighbor, 1);
    }
}

/// Invalid names remove their dependent closure while keeping unrelated supplier indices aligned.
#[test]
fn invalid_name_dependency_closure_keeps_neighbors() {
    let root = tempfile::tempdir().unwrap();
    for (name, masters, id) in [
        ("Before.esp", vec![], 0x811),
        (" Invalid.esp", vec![], 0x822),
        ("Dependent.esp", vec![" Invalid.esp"], 0x0100_0844),
        ("Transitive.esp", vec!["Dependent.esp"], 0x0100_0855),
        ("After.esp", vec![], 0x833),
    ] {
        plugin(
            root.path(),
            name,
            &masters,
            record(b"STAT", id, 0, &sub(b"EDID", b"valid\0")),
        );
    }
    let paths = [
        "Before.esp",
        " Invalid.esp",
        "Dependent.esp",
        "Transitive.esp",
        "After.esp",
    ]
    .map(|name| root.path().join(name));
    let native = EsmParser::merge_plugins_form_keys(&paths);
    assert!(
        native.is_ok(),
        "skip invalid input and its closure: {native:?}"
    );
    let native = native.unwrap();
    assert_eq!(native.records.len(), 2);
    assert_eq!(native.order.plugin_order().len(), 2);
    assert!(
        native
            .warnings
            .iter()
            .any(|line| line.contains("skipped 1 invalid plugin name")
                && line.contains("skipped 2 dependent plugins"))
    );
    let db = root.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    let ids = conn
        .prepare("SELECT form_id FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |r| r.get::<_, u32>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(ids, [0x811, 0x0100_0833]);
    let plugins: u32 = conn
        .query_row("SELECT COUNT(*) FROM plugins", [], |r| r.get(0))
        .unwrap();
    assert_eq!(plugins, 2);
    let order = converter::esm::load_order::LoadOrder::read(&paths).unwrap();
    let typed = converter::records::read_plugins(&paths, &order).unwrap();
    let mut ids: Vec<_> = typed.records.keys().copied().collect();
    ids.sort_unstable();
    assert_eq!(ids, [0x811, 0x0100_0833]);
}

/// A setting colliding with an accepted ordinary record leaves a same-key neighbor usable.
#[test]
fn gmst_type_collision_keeps_same_key_neighbor() {
    check(
        [
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"initial\0")),
            record(b"GMST", 0x800, 0, &sub(b"EDID", b"iRejected\0")),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x800],
    );
}

/// An accepted projected predecessor rejects only the conflicting spelling, not its successor.
#[test]
fn projected_collision_keeps_same_key_neighbor() {
    check(
        [
            record(b"STAT", 0x0100_0800, 0, &sub(b"EDID", b"initial\0")),
            record(b"STAT", 0x800, 0, &sub(b"EDID", b"rejected\0")),
            record(b"STAT", 0x0100_0800, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
        &[0x800],
    );
}

/// Framed but rejected/deleted targets have the same source identity during publication and history.
#[test]
fn history_uses_source_headers_for_deleted_and_rejected_targets() {
    for deleted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        fs::create_dir(&data).unwrap();
        plugin(
            &data,
            "Base.esm",
            &[],
            [
                record(
                    b"CELL",
                    0x811,
                    0,
                    &[
                        sub(b"DATA", &[1, 0]),
                        sub(b"XCWT", &0x0200_0830u32.to_le_bytes()),
                    ]
                    .concat(),
                ),
                record(
                    b"WATR",
                    0x830,
                    if deleted { 0x20 } else { 0x40000 },
                    if deleted { &[] } else { &[0, 0, 0, 0, 99] },
                ),
                record(b"STAT", 0x840, 0, &sub(b"EDID", b"neighbor\0")),
            ]
            .concat(),
        );
        let list = data.join("plugins.txt");
        fs::write(&list, "*Base.esm\n").unwrap();
        let output = root.path().join("typed");
        converter::esm::inhouse::export_record_bundle(&data, &list, &output).unwrap();
        let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
        let bytes: Vec<u8> = conn
            .query_row("SELECT data FROM records WHERE form_id=2065", [], |r| {
                r.get(0)
            })
            .unwrap();
        let record = rkyv::from_bytes::<
            converter::esm::types::ArchivedRecordData,
            rkyv::rancor::Error,
        >(&bytes)
        .unwrap();
        assert_eq!(
            record
                .subrecords
                .iter()
                .find(|field| field.tag == *b"XCWT")
                .unwrap()
                .data,
            [0; 4]
        );
        let neighbor: u32 = conn
            .query_row("SELECT COUNT(*) FROM records WHERE form_id=2112", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(neighbor, 1);
        let reasons = conn
            .prepare("SELECT reason FROM record_diagnostics WHERE category='link'")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("absent or has a disallowed target type")),
            "{reasons:?}"
        );
        assert!(
            !reasons
                .iter()
                .any(|reason| reason.contains("exceeds master list")),
            "{reasons:?}"
        );
        let history =
            converter::records::history::read_history(&output, Some(&data), 0x811).unwrap();
        let version = &history.versions[0];
        assert_eq!(version.decode_status, "decoded", "{history:?}");
        let field = version
            .record
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .find(|field| field.signature == *b"XCWT")
            .unwrap();
        assert_eq!(
            field.canonical_bytes,
            0x830u32.to_le_bytes(),
            "history decodes the source self link before target validation"
        );
        assert_eq!(version.diagnostics.invalid_links, 0);
    }
}

/// Deferred COED form-ID leaves retain the same source-header authority during lazy history.
#[test]
fn deferred_self_link_replay_and_history_share_source_headers() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    plugin(
        &data,
        "Base.esm",
        &[],
        [
            record(
                b"CONT",
                0x811,
                0,
                &[
                    sub(b"EDID", b"TestContainer\0"),
                    sub(
                        b"CNTO",
                        &[0x820u32.to_le_bytes(), 1u32.to_le_bytes()].concat(),
                    ),
                    sub(
                        b"COED",
                        &[
                            0x830u32.to_le_bytes(),
                            0x0200_0840u32.to_le_bytes(),
                            1f32.to_le_bytes(),
                        ]
                        .concat(),
                    ),
                ]
                .concat(),
            ),
            record(b"MISC", 0x820, 0, &sub(b"EDID", b"TestItem\0")),
            record(b"NPC_", 0x830, 0, &sub(b"EDID", b"TestOwner\0")),
            record(b"GLOB", 0x840, 0x20, &[]),
            record(b"STAT", 0x850, 0, &sub(b"EDID", b"neighbor\0")),
        ]
        .concat(),
    );
    let list = data.join("plugins.txt");
    fs::write(&list, "*Base.esm\n").unwrap();
    let output = root.path().join("typed");
    converter::esm::inhouse::export_record_bundle(&data, &list, &output).unwrap();
    let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
    let reasons = conn
        .prepare("SELECT reason FROM record_diagnostics WHERE category='link'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("absent or has a disallowed target type")),
        "{reasons:?}"
    );
    assert!(
        !reasons
            .iter()
            .any(|reason| reason.contains("exceeds master list")),
        "{reasons:?}"
    );
    let bytes: Vec<u8> = conn
        .query_row("SELECT data FROM records WHERE form_id=2065", [], |r| {
            r.get(0)
        })
        .unwrap();
    let record =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&bytes)
            .unwrap();
    assert_eq!(
        &record
            .subrecords
            .iter()
            .find(|field| field.tag == *b"COED")
            .unwrap()
            .data[4..8],
        &[0; 4]
    );
    let history = converter::records::history::read_history(&output, Some(&data), 0x811).unwrap();
    let version = &history.versions[0];
    assert_eq!(version.decode_status, "decoded", "{history:?}");
    let field = version
        .record
        .as_ref()
        .unwrap()
        .fields
        .iter()
        .find(|field| field.signature == *b"COED")
        .unwrap();
    assert_eq!(&field.canonical_bytes[4..8], &0x840u32.to_le_bytes());
    assert_eq!(version.diagnostics.invalid_links, 0);
    let neighbor: u32 = conn
        .query_row("SELECT COUNT(*) FROM records WHERE form_id=2128", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(neighbor, 1);
}

/// Invalid movement settings keep the earlier value and leave their source alias reusable.
#[test]
fn malformed_gmst_values_preserve_native_and_published_winners() {
    for bad_data in [vec![1], f32::NAN.to_le_bytes().to_vec(), Vec::new()] {
        let root = tempfile::tempdir().unwrap();
        let good = [
            sub(b"EDID", b"fJumpHeightMin\0"),
            sub(b"DATA", &76.0f32.to_le_bytes()),
        ]
        .concat();
        plugin(
            root.path(),
            "Base.esm",
            &[],
            record(b"GMST", 0x800, 0, &good),
        );
        let mut bad = sub(b"EDID", b"fJumpHeightMin\0");
        if !bad_data.is_empty() {
            bad.extend(sub(b"DATA", &bad_data));
        }
        plugin(
            root.path(),
            "Broken.esp",
            &["Base.esm"],
            [
                record(b"GMST", 0x0100_0810, 0, &bad),
                record(b"GMST", 0x0100_0811, 0, &bad),
                record(b"STAT", 0x0100_0810, 0, &sub(b"EDID", b"neighbor\0")),
            ]
            .concat(),
        );
        let paths = [root.path().join("Base.esm"), root.path().join("Broken.esp")];
        let db = root.path().join("world.db");
        let (native, outcome) = EsmParser::convert_form_key_plugins(&paths, &db).unwrap();
        assert_eq!(
            outcome,
            converter::esm::form_key_merge::LegacyWriterOutcome::Published
        );
        let winner = native
            .records
            .values()
            .find(|r| r.source.record_type == *b"GMST")
            .unwrap();
        assert_eq!(
            winner.source.load_order, 0,
            "invalid GMST override must retain the previous valid native winner"
        );
        assert!(
            winner
                .source
                .subrecords
                .iter()
                .any(|(tag, bytes)| tag == b"DATA" && bytes == &76.0f32.to_le_bytes())
        );
        assert_eq!(
            native.records.len(),
            2,
            "rejected GMST must leave its source alias available to the valid neighbor"
        );
        assert_eq!(
            native.warnings.len(),
            1,
            "one bounded warning per plugin: {:?}",
            native.warnings
        );
        assert!(
            native.warnings[0].contains("Broken.esp")
                && native.warnings[0].contains("2 unusable record candidate"),
            "{:?}",
            native.warnings
        );
        let conn = Connection::open(db).unwrap();
        let value: f64 = conn
            .query_row(
                "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, 76.0);
        let ids: Vec<u32> = conn
            .prepare("SELECT form_id FROM records ORDER BY form_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids, [0x800, 0x0100_0810]);
    }
}
