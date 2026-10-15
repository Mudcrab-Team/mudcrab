//! Case-paired Windows-1252 plugin names use one namespace across selection and replay.
use converter::{
    esm::{EsmParser, load_order::LoadOrder, read_plugins_txt_with_options},
    records::history_source::{SourceQuery, visit_source_history},
};
use rusqlite::{Connection, params};
use std::{
    fs,
    path::{Path, PathBuf},
};

const UPPER: &str = "MiXeDÀÅÄÉÖÜŠŒŽŸ.ESM";
const LOWER: &str = "mixedàåäéöüšœžÿ.esm";
const LEGACY_OWNER: &str = "mixedÀÅÄÉÖÜŠŒŽŸ.esm";
const UPPER_BYTES: &[u8] = b"MiXeD\xc0\xc5\xc4\xc9\xd6\xdc\x8a\x8c\x8e\x9f.ESM";
const LOWER_BYTES: &[u8] = b"mixed\xe0\xe5\xe4\xe9\xf6\xfc\x9a\x9c\x9e\xff.esm";

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

fn frame(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &0u32.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        payload,
    ]
    .concat()
}

/// MAST is independently authored Windows-1252, while filesystem names are Unicode.
fn plugin(data: &Path, name: &str, masters: &[&[u8]], records: &[u8]) -> PathBuf {
    let mut header = sub(b"HEDR", &[0; 12]);
    for master in masters {
        let mut bytes = master.to_vec();
        bytes.push(0);
        header.extend(sub(b"MAST", &bytes));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let mut bytes = frame(b"TES4", 0, &header);
    bytes.extend(records);
    let path = data.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn fixture(root: &Path) -> (PathBuf, PathBuf, Vec<PathBuf>) {
    fixture_with_spelling(root, LOWER_BYTES)
}

/// Choose independently encoded native/list spellings before publishing the original files.
fn fixture_with_spelling(root: &Path, owner_bytes: &[u8]) -> (PathBuf, PathBuf, Vec<PathBuf>) {
    let data = root.join("Data");
    fs::create_dir(&data).unwrap();
    let leading = plugin(&data, "Leading.esm", &[], &[]);
    let owner = plugin(
        &data,
        UPPER,
        &[],
        &frame(b"STAT", 0x1800, &sub(b"EDID", b"original\0")),
    );
    let mut patch_records = frame(b"STAT", 0x1800, &sub(b"EDID", b"override\0"));
    patch_records.extend(frame(
        b"REFR",
        0x0100_1801,
        &sub(b"NAME", &0x1800u32.to_le_bytes()),
    ));
    let patch = plugin(&data, "Patch.esp", &[owner_bytes], &patch_records);
    let list = root.join("plugins.txt");
    let mut listed = b"*Leading.esm\n*".to_vec();
    listed.extend(owner_bytes);
    listed.extend(b"\n*Patch.esp\n");
    fs::write(&list, listed).unwrap();
    fs::write(root.join("Skyrim.ccc"), []).unwrap();
    (data, list, vec![leading, owner, patch])
}

#[test]
fn player_selection_master_remap_and_stable_owner_agree_on_case_pairs() {
    let root = tempfile::tempdir().unwrap();
    let (data, list, expected) = fixture(root.path());
    let paths =
        read_plugins_txt_with_options(&list, &data, Some(&root.path().join("Skyrim.ccc")), None)
            .unwrap();
    assert_eq!(paths, expected);
    let order = LoadOrder::read(&paths).unwrap();
    assert_eq!(order.names, ["leading.esm", LOWER, "patch.esp"]);
    assert_eq!(order.identity(0x0100_1800).unwrap().plugin, LOWER);
    let legacy = EsmParser::merge_plugins(&paths).unwrap();
    assert_eq!(legacy[&0x0100_1800].load_order, 2);
    let reference = &legacy[&0x0200_1801];
    let base = reference
        .subrecords
        .iter()
        .find(|(tag, _)| tag.as_slice() == b"NAME")
        .unwrap();
    assert_eq!(base.1, 0x0100_1800u32.to_le_bytes());

    let output = root.path().join("pack");
    converter::esm::inhouse::export_record_bundle_typed(
        &data,
        &list,
        &output,
        &[*b"STAT", *b"REFR"],
    )
    .unwrap();
    let history =
        converter::records::history::read_history(&output, Some(&data), 0x0100_1800).unwrap();
    assert_eq!(history.versions.len(), 2);
    assert!(
        history
            .versions
            .iter()
            .all(|version| version.decode_status == "decoded")
    );
    assert_eq!(history.versions[1].plugin, "Patch.esp");
    assert!(history.versions[1].winner);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let owner: String = db
        .query_row(
            "SELECT original_owner FROM record_overrides WHERE record_type='STAT' AND plugin_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner, LOWER);
    let linked: u32 = db
        .query_row(
            "SELECT base_form_id FROM \"references\" WHERE id=?1",
            [0x0200_1801u32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked, 0x0100_1800);
    drop(db);

    for query_owner in [UPPER, LOWER] {
        let mut ids = Vec::new();
        let summary = visit_source_history(
            &output,
            Some(&data),
            SourceQuery::Source {
                original_owner: query_owner.into(),
                full_local: 0x1800,
            },
            |version| {
                assert_eq!(version.identity.original_owner.as_deref(), Some(LOWER));
                assert!(version.raw.as_ref().unwrap().stored_payload.is_some());
                assert_eq!(version.latest_source_candidate, version.plugin_id == 2);
                assert_eq!(
                    version.latest_reader_admitted_candidate,
                    version.plugin_id == 2
                );
                ids.push(version.plugin_id);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(ids, [1, 2]);
        assert!(summary.fully_raw_available);
    }
}

#[test]
fn formerly_admissible_ascii_folded_owner_remains_raw_queryable_with_unicode_keys() {
    let root = tempfile::tempdir().unwrap();
    // The old namespace required matching Unicode case in the filename, MAST and list.
    // ASCII letters still varied under its ordinary lowercase key.
    let (data, list, paths) = fixture_with_spelling(root.path(), UPPER_BYTES);
    let order = LoadOrder::read(&paths).unwrap();
    assert_eq!(order.metadata[2].masters, [UPPER]);
    assert_ne!(LEGACY_OWNER, UPPER);
    assert_ne!(LEGACY_OWNER, LOWER);
    let output = root.path().join("pack");
    converter::esm::inhouse::export_record_bundle_typed(
        &data,
        &list,
        &output,
        &[*b"STAT", *b"REFR"],
    )
    .unwrap();
    // Restore the exact saved key of the former producer, preserving native bytes/checksums.
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    db.execute(
        "UPDATE record_overrides SET original_owner=?1 WHERE original_owner=?2",
        params![LEGACY_OWNER, LOWER],
    )
    .unwrap();
    drop(db);
    for query_owner in [UPPER, LOWER] {
        let mut count = 0;
        let summary = visit_source_history(
            &output,
            Some(&data),
            SourceQuery::Source {
                original_owner: query_owner.into(),
                full_local: 0x1800,
            },
            |version| {
                assert_eq!(version.identity.original_owner.as_deref(), Some(LOWER));
                assert_eq!(version.latest_source_candidate, version.plugin_id == 2);
                assert_eq!(
                    version.latest_reader_admitted_candidate,
                    version.plugin_id == 2
                );
                assert!(version.raw.as_ref().unwrap().stored_payload.is_some());
                count += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(count, 2);
        assert!(summary.fully_raw_available);
    }
}

#[test]
fn unicode_case_duplicates_share_last_checkbox_and_metadata_rejects_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let (data, list, _) = fixture(root.path());
    let mut listed = b"*Leading.esm\n*".to_vec();
    // Checked uppercase spelling followed by unchecked lowercase spelling disables one plugin.
    listed.extend(UPPER_BYTES);
    listed.extend(b"\n");
    listed.extend(LOWER_BYTES);
    listed.extend(b"\n*Patch.esp\n");
    fs::write(&list, listed).unwrap();
    let paths =
        read_plugins_txt_with_options(&list, &data, Some(&root.path().join("Skyrim.ccc")), None)
            .unwrap();
    assert_eq!(paths, [data.join("Leading.esm")]); // Disabled master also excludes its dependent.
    let duplicate = LoadOrder::from_metadata(
        vec![UPPER.into(), LOWER.into()],
        vec![Default::default(), Default::default()],
    )
    .err()
    .unwrap();
    assert!(duplicate.to_string().contains("duplicate plugin"));
}

#[test]
fn ccc_and_ini_case_pairs_select_installed_names_once() {
    let root = tempfile::tempdir().unwrap();
    let (data, list, _) = fixture(root.path());
    fs::write(&list, b"*Leading.esm\n*Patch.esp\n").unwrap();
    let ccc = root.path().join("Skyrim.ccc");
    let ini = root.path().join("Skyrim.ini");
    fs::write(&ccc, [LOWER_BYTES, b"\n"].concat()).unwrap();
    fs::write(
        &ini,
        [b"[General]\nsTestFile1=".as_slice(), LOWER_BYTES, b"\n"].concat(),
    )
    .unwrap();
    let paths = read_plugins_txt_with_options(&list, &data, Some(&ccc), Some(&ini)).unwrap();
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.file_name().unwrap() == UPPER)
            .count(),
        1
    );
    let order = LoadOrder::read(&paths).unwrap();
    assert!(order.names.contains(&LOWER.to_owned()));
    assert!(order.names.contains(&"patch.esp".to_owned()));
}
