//! A source that changes after metadata admission does not reject usable neighbors.
use super::*;

/// Frame procedural native bytes with the independent 24-byte SE header layout.
fn frame(tag: &[u8; 4], id: u32, fields: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (signature, bytes) in fields {
        payload.extend(signature);
        payload.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
        payload.extend(bytes);
    }
    let mut bytes = tag.to_vec();
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Supply one valid header and keyword without using the converter's writer.
fn plugin(path: &Path, id: u32) {
    plugin_with_master(path, id, None);
}

/// A neighboring plugin may refer to the lost plugin's already-admitted master slot.
fn plugin_with_master(path: &Path, id: u32, master: Option<&str>) {
    let mut header = 1.7f32.to_le_bytes().to_vec();
    header.extend(1u32.to_le_bytes());
    header.extend(0x1000u32.to_le_bytes());
    let mut fields = vec![(*b"HEDR", header)];
    if let Some(master) = master {
        let mut name = master.as_bytes().to_vec();
        name.push(0);
        fields.push((*b"MAST", name));
        fields.push((*b"DATA", vec![0; 8]));
    }
    let mut bytes = frame(b"TES4", 0, &fields);
    bytes.extend(frame(
        b"KYWD",
        id,
        &[(*b"EDID", b"SnapshotNeighbor\0".to_vec())],
    ));
    std::fs::write(path, bytes).unwrap();
}

/// The changed source is renamed out of the selected path; no live source is truncated.
fn assert_missing_admitted_plugin_publishes(reader: crate::config::RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let paths =
        ["HealthyA.esm", "MutableMod.esp", "HealthyC.esp"].map(|name| directory.path().join(name));
    plugin(&paths[0], 0x811);
    plugin(&paths[1], 0x822);
    plugin_with_master(&paths[2], 0x0100_0933, Some("MutableMod.esp"));
    let unavailable = paths[1].clone();
    let retained = directory.path().join("removed-plugin.retained");
    // Reset even if publication panics, so another test on this thread cannot inherit the hook.
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            AFTER_PLUGIN_ADMISSION.with(|slot| *slot.borrow_mut() = None);
        }
    }
    let clear = ClearHook;
    AFTER_PLUGIN_ADMISSION.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(move || {
            std::fs::rename(unavailable, retained).unwrap();
        }));
    });
    let database = directory.path().join("skyrim_world.db");
    let result = EsmParser::convert_plugins_with_reader_and_schemas(
        &paths,
        &database,
        reader,
        directory.path(),
        crate::config::DEFAULT_LANGUAGE,
        None,
    );
    drop(clear);
    assert!(!paths[1].exists(), "the after-admission hook must have run");
    assert!(
        result.is_ok(),
        "an unavailable admitted plugin must not abort publication: {result:?}"
    );
    let conversion = result.unwrap();
    assert_eq!(conversion.skipped_inputs, BTreeSet::from([1]));
    assert!(conversion.records.contains_key(&0x0000_0811));
    assert!(conversion.records.contains_key(&0x0200_0933));
    assert!(!conversion.records.contains_key(&0x0100_0822));
    let warnings: Vec<_> = conversion
        .warnings
        .iter()
        .filter(|warning| warning.to_lowercase().contains("mutablemod.esp"))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "the unavailable input must warn exactly once: {warnings:?}"
    );
    let conn = Connection::open(database).unwrap();
    let rows = conn.prepare("SELECT id,name,priority,length(checksum),typeof(checksum) FROM plugins ORDER BY priority")
        .unwrap().query_map([], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,String>(1)?,
            row.get::<_,i64>(2)?, row.get::<_,i64>(3)?, row.get::<_,String>(4)?)))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert_eq!(rows.len(), 3);
    for (index, (id, name, priority, checksum_len, checksum_type)) in rows.iter().enumerate() {
        assert_eq!((*id, *priority), (index as i64, index as i64));
        assert_eq!(name, paths[index].file_name().unwrap().to_str().unwrap());
        assert_eq!(*checksum_len, if index == 1 { 0 } else { 32 });
        assert_eq!(checksum_type, "blob");
    }
    let (owner, local): (String, u32) = conn
        .query_row(
            "SELECT plugin_name,internal_id FROM formid_map WHERE form_id=?1",
            [0x0200_0933u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(owner.eq_ignore_ascii_case("HealthyC.esp"));
    assert_eq!(local, 0x933);
    if reader == crate::config::RecordReader::Inhouse {
        // Fidelity inspects retained SQL, so a missing source cannot abort the audit.
        let schema =
            crate::records::schema_format::Schema::parse(crate::records::SCHEMA_BYTES).unwrap();
        crate::records::fidelity::audit_storage(&conn, &schema).unwrap();
    }
}

#[test]
fn missing_admitted_plugin_legacy_publishes_neighbors_and_keeps_slots() {
    assert_missing_admitted_plugin_publishes(crate::config::RecordReader::Legacy);
}

#[test]
fn missing_admitted_plugin_inhouse_publishes_neighbors_and_keeps_slots() {
    assert_missing_admitted_plugin_publishes(crate::config::RecordReader::Inhouse);
}

/// A late unusable input preserves both surrounding plugins and their admitted slots.
#[test]
fn legacy_snapshot_input_failure_keeps_neighbor_plugins() {
    let directory = tempfile::tempdir().unwrap();
    let paths =
        ["HealthyA.esm", "MutableMod.esp", "HealthyC.esp"].map(|name| directory.path().join(name));
    for (path, id) in paths.iter().zip([0x811, 0x822, 0x933]) {
        plugin(path, id);
    }
    let order = load_order::LoadOrder::read(&paths).unwrap();
    // No live map exists here: truncation between admission and reading is safe on both OSes.
    std::fs::write(&paths[1], b"TES4").unwrap();
    let result = EsmParser::merge_plugins_with_load_order(&paths, &order);
    assert!(
        result.is_ok(),
        "unusable plugin input must not abort neighboring plugins: {result:?}"
    );
    let conversion = result.unwrap();
    assert!(conversion.records.contains_key(&0x0000_0811));
    assert!(conversion.records.contains_key(&0x0200_0933));
    assert!(!conversion.records.contains_key(&0x0100_0822));
    assert_eq!(conversion.warnings.len(), 1);
    assert!(conversion.warnings[0].contains("mutablemod.esp"));
    assert!(conversion.warnings[0].contains("skipped unusable plugin input"));
}
