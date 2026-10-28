//! Master-index compatibility retains source bytes and valid neighboring records
//! through the established records-only publication API.

use converter::esm::{inhouse, types::ArchivedRecordData};
use rusqlite::Connection;
use std::fs;

/// Frame a native subrecord independently of the schema-driven reader.
fn subrecord(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    out.extend(bytes);
    out
}

/// Frame a native header with asymmetric revision and padding sentinels.
fn record(tag: &[u8; 4], raw: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(raw.to_le_bytes());
    out.extend(0x5566_7788u32.to_le_bytes());
    out.extend(44u16.to_le_bytes());
    out.extend(0x1234u16.to_le_bytes());
    out.extend(payload);
    out
}

/// Write only the original declared masters; no reference implementation is used.
fn plugin(masters: &[&str], body: &[u8]) -> Vec<u8> {
    let mut header = subrecord(b"HEDR", &[0; 12]);
    for master in masters {
        header.extend(subrecord(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(subrecord(b"DATA", &[0; 8]));
    }
    let mut bytes = record(b"TES4", 0, 0, &header);
    bytes.extend(body);
    bytes
}

/// Required headers use supplier compatibility, optional links still clear, and bad neighbors
/// do not prevent complete publication of usable records or exact source bytes.
#[test]
fn required_header_compatibility_publishes_with_optional_link_and_broken_neighbor() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("Base.esm"), plugin(&[], &[])).unwrap();
    let mut compatibility = subrecord(b"EDID", b"compatibility_one\0");
    let mut material = 1.25f32.to_le_bytes().to_vec();
    material.extend(0x0200_0712u32.to_le_bytes());
    compatibility.extend(subrecord(b"DNAM", &material));
    let neighbor = subrecord(b"EDID", b"proven_self_neighbor\0");
    let broken = [5u32.to_le_bytes().as_slice(), &[0x78, 0x9c, 0]].concat();
    let mut body = record(b"STAT", 0x0200_0711, 0, &compatibility);
    body.extend(record(b"STAT", 0x0100_0713, 0x40000, &broken));
    body.extend(record(b"STAT", 0x0100_0722, 0, &neighbor));
    let patch = plugin(&["Base.esm"], &body);
    fs::write(data.join("Patch.esp"), &patch).unwrap();
    let order = temp.path().join("plugins.txt");
    fs::write(&order, "*Base.esm\n*Patch.esp\n").unwrap();
    let output = temp.path().join("bundle");
    inhouse::export_record_bundle_typed(&data, &order, &output, &[*b"STAT"]).unwrap();
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let count: u32 = db
        .query_row(
            "SELECT count(*) FROM records WHERE form_id=?1",
            [0x0100_0711u32],
            |row| row.get(0),
        )
        .unwrap();
    // The established supplier fallback remains a runtime convention, not proof of ownership.
    assert_eq!(
        count, 1,
        "required beyond-MAST header lost its supplier-compatible winner"
    );
    let columns: Vec<String> = db
        .prepare("PRAGMA table_info(record_overrides)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    // This is the intended RED on c6: strict source identity was not yet persisted.
    for name in ["original_owner", "full_local", "source_identity_state"] {
        assert!(
            columns.iter().any(|column| column == name),
            "published history lacks strict source identity column {name}"
        );
    }
    let identity: (Option<String>, Option<u32>, String) = db.query_row(
        "SELECT original_owner,full_local,source_identity_state FROM record_overrides WHERE source_form_id=?1",
        [0x0200_0711u32], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(
        identity,
        (None, Some(0x711), "master_index_out_of_range".into())
    );
    for (id, expected) in [(0x0100_0711u32, compatibility), (0x0100_0722, neighbor)] {
        let payload: Vec<u8> = db
            .query_row(
                "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            payload, expected,
            "original source bytes changed for {id:08X}"
        );
    }
    let canonical: Vec<u8> = db
        .query_row(
            "SELECT data FROM records WHERE form_id=?1",
            [0x0100_0711u32],
            |r| r.get(0),
        )
        .unwrap();
    let archived = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&canonical).unwrap();
    let dnam = archived
        .subrecords
        .iter()
        .find(|sub| sub.tag == *b"DNAM")
        .unwrap();
    assert_eq!(
        dnam.data,
        [1.25f32.to_le_bytes(), 0u32.to_le_bytes()].concat()
    );
    let rejected: u32 = db
        .query_row(
            "SELECT count(*) FROM records WHERE form_id=?1",
            [0x0100_0713u32],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rejected, 0, "invalid compressed bytes must remain local");
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["patch.esp"]["invalid_links"], 1);
    assert_eq!(diagnostics["decoder"]["patch.esp"]["skipped_records"], 1);
    assert!(
        output.join("record-reader.json").exists(),
        "completed producer identity missing"
    );
    assert_eq!(fs::read(data.join("Patch.esp")).unwrap(), patch);
}
