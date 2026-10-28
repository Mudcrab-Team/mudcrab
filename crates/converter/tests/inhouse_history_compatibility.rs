//! Public retained-pack history keeps chain facts without manufacturing replay authority.
use converter::records::history::read_history;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

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

/// Author the actual eighteen-column predecessor independently of the current writer.
/// The optional columns let the same original bytes supply a complete replay control.
fn snapshot(root: &Path, omitted: &[&str], schema_hash: Option<&str>) -> std::path::PathBuf {
    let pack = root.join("history.db");
    let conn = Connection::open(&pack).unwrap();
    conn.execute_batch(
        "CREATE TABLE plugins(id INTEGER PRIMARY KEY,name TEXT,priority INTEGER,checksum BLOB);
         CREATE TABLE record_overrides(
            form_id INTEGER,chain_position INTEGER,plugin_id INTEGER,source_form_id INTEGER,
            deleted INTEGER,record_type TEXT,flags INTEGER,source_record_offset INTEGER,
            source_record_size INTEGER,status TEXT,reason TEXT,source_cell_form_id INTEGER,
            source_worldspace_form_id INTEGER,source_topic_form_id INTEGER,topic_group_offset INTEGER,
            source_path TEXT,schema_sha256 TEXT,plugin_masters TEXT);
         CREATE TABLE inhouse_source_records(form_id INTEGER,record_type TEXT);
         INSERT INTO inhouse_source_records VALUES(6144,'STAT'),(6145,'STAT');",
    )
    .unwrap();
    for (name, kind) in [
        ("source_group_end", "INTEGER"),
        ("frame_complete", "INTEGER"),
        ("record_signature", "BLOB"),
    ] {
        if !omitted.contains(&name) {
            conn.execute_batch(&format!(
                "ALTER TABLE record_overrides ADD COLUMN {name} {kind}"
            ))
            .unwrap();
        }
    }
    for (index, name, masters) in [(0u32, "base.esm", "[]"), (1, "patch.esp", "[\"BASE.ESM\"]")] {
        let mut header = sub(b"HEDR", &[0; 12]);
        if index == 1 {
            header.extend(sub(b"MAST", b"BASE.ESM\0"));
            header.extend(sub(b"DATA", &[0; 8]));
        }
        let mut original = frame(b"TES4", 0, &header);
        let offset = original.len() as u64;
        let payload = sub(
            b"EDID",
            if index == 0 {
                b"base_version\0"
            } else {
                b"patch_version\0"
            },
        );
        original.extend(frame(b"STAT", 0x1800, &payload));
        let neighbor_offset = original.len() as u64;
        let neighbor = sub(b"EDID", b"usable_neighbor\0");
        if index == 0 {
            original.extend(frame(b"STAT", 0x1801, &neighbor));
        }
        let source = root.join(name);
        fs::write(&source, &original).unwrap();
        let checksum = Sha256::digest(&original).to_vec();
        conn.execute(
            "INSERT INTO plugins VALUES(?1,?2,?1,?3)",
            params![index, name, checksum],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO record_overrides(plugin_id,record_type,flags,source_record_offset,source_path,schema_sha256,plugin_masters) VALUES(?1,'TES4',0,0,?2,?3,?4)",
            params![index, source.to_string_lossy(), schema_hash, masters],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO record_overrides(form_id,chain_position,plugin_id,source_form_id,deleted,record_type,flags,source_record_offset,source_record_size,status,reason) VALUES(6144,?1,?1,6144,0,'STAT',0,?2,?3,?4,?5)",
            params![index, offset, payload.len() as u32, if index == 0 { "accepted" } else { "skipped" }, (index == 1).then_some("retained rejection")],
        )
        .unwrap();
        if index == 0 {
            conn.execute(
                "INSERT INTO record_overrides(form_id,chain_position,plugin_id,source_form_id,deleted,record_type,flags,source_record_offset,source_record_size,status) VALUES(6145,0,0,6145,0,'STAT',0,?1,?2,'accepted')",
                params![neighbor_offset, neighbor.len() as u32],
            )
            .unwrap();
        }
        if !omitted.contains(&"source_group_end") {
            conn.execute(
                "UPDATE record_overrides SET source_group_end=?1 WHERE plugin_id=?2",
                params![original.len() as u64, index],
            )
            .unwrap();
        }
        if !omitted.contains(&"frame_complete") {
            conn.execute(
                "UPDATE record_overrides SET frame_complete=1 WHERE plugin_id=?1",
                [index],
            )
            .unwrap();
        }
        if !omitted.contains(&"record_signature") {
            conn.execute(
                "UPDATE record_overrides SET record_signature=?1 WHERE plugin_id=?2 AND record_type='STAT'",
                params![b"STAT".as_slice(), index],
            )
            .unwrap();
        }
    }
    pack
}

#[test]
fn retained_eighteen_column_history_returns_available_chain_and_neighbor() {
    let fixture = tempfile::tempdir().unwrap();
    let hash = converter::cache::hash_bytes(converter::records::SCHEMA_BYTES);
    let pack = snapshot(
        fixture.path(),
        &["source_group_end", "frame_complete", "record_signature"],
        Some(&hash),
    );
    let conn = Connection::open(&pack).unwrap();
    let columns: u32 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('record_overrides')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 18);
    drop(conn);
    let report = read_history(&pack, None, 0x1800).unwrap();
    assert_eq!(report.versions.len(), 2);
    assert_eq!(
        report
            .versions
            .iter()
            .map(|v| v.chain_position)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        report
            .versions
            .iter()
            .map(|v| v.plugin.as_str())
            .collect::<Vec<_>>(),
        ["base.esm", "patch.esp"]
    );
    assert_eq!(report.versions[0].status, "accepted");
    assert!(report.versions[0].winner);
    assert_eq!(report.versions[1].status, "skipped");
    assert_eq!(
        report.versions[1].reason.as_deref(),
        Some("retained rejection")
    );
    assert!(!report.versions[1].winner);
    assert!(report.versions.iter().all(|version| {
        version.decode_status == "undecodable"
            && version.record.is_none()
            && version
                .decode_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("containing-group bounds"))
            && version.source_form_id == 0x1800
            && version.source_record_offset > 24
            && version.source_record_size > 0
    }));
    assert!(report.fields.is_empty());
    let neighbor = read_history(&pack, Some(fixture.path()), 0x1801).unwrap();
    assert_eq!(neighbor.versions.len(), 1);
    assert!(neighbor.versions[0].winner);
    assert_eq!(neighbor.versions[0].source_form_id, 0x1801);
    assert_eq!(neighbor.versions[0].decode_status, "undecodable");
}

#[test]
fn replay_requires_each_authenticated_column_and_matching_schema() {
    let hash = converter::cache::hash_bytes(converter::records::SCHEMA_BYTES);
    for (omitted, schema, expected) in [
        (vec![], Some(hash.as_str()), "decoded"),
        (
            vec!["source_group_end"],
            Some(hash.as_str()),
            "containing-group bounds",
        ),
        (
            vec!["frame_complete"],
            Some(hash.as_str()),
            "frame completeness",
        ),
        (
            vec!["record_signature"],
            Some(hash.as_str()),
            "raw record signature",
        ),
        (vec![], Some("older_schema"), "diagnostic schema differs"),
        (vec![], None, "schema_sha256"),
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let pack = snapshot(fixture.path(), &omitted, schema);
        let report = read_history(&pack, None, 0x1800).unwrap();
        assert_eq!(report.versions.len(), 2);
        for version in &report.versions {
            if expected == "decoded" {
                assert_eq!(version.decode_status, "decoded");
                assert_eq!(version.record.as_ref().unwrap().form_id, 0x1800);
            } else {
                assert_eq!(version.decode_status, "undecodable");
                assert!(version.record.is_none());
                assert!(version.decode_reason.as_deref().unwrap().contains(expected));
            }
        }
        assert!(report.versions[0].winner);
        assert!(!report.versions[1].winner);
    }
}
