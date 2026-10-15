//! Fresh record bundles must have reproducible typed lines and bounded diagnostics.
use converter::esm::inhouse::export_record_bundle_typed;
use std::{fs, path::Path};

/// Frame a native subrecord independently of the decoder and authored schema.
fn field(signature: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [
        signature.as_slice(),
        &(bytes.len() as u16).to_le_bytes(),
        bytes,
    ]
    .concat()
}

/// Use distinct source header sentinels and preserve the independently framed payload.
fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    [
        signature.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &0x7351_294Bu32.to_le_bytes(),
        &44u16.to_le_bytes(),
        &0xA913u16.to_le_bytes(),
        payload,
    ]
    .concat()
}

/// Mix physical order, type order and ID order, with damaged fields and optional links.
fn fixture(data: &Path) {
    fs::create_dir(data).unwrap();
    let header = field(
        b"HEDR",
        &[
            1.7f32.to_le_bytes(),
            385u32.to_le_bytes(),
            0x800u32.to_le_bytes(),
        ]
        .concat(),
    );
    // Localized input has deliberately absent lookup keys; usable records still publish.
    let mut plugin = record(b"TES4", 0, 0x80, &header);
    for index in (0..128u32).rev() {
        plugin.extend(record(
            b"STAT",
            0x1000 + index,
            0,
            &[field(b"EDID", b"DamagedBounds\0"), field(b"OBND", &[0xA7])].concat(),
        ));
        plugin.extend(record(
            b"FLST",
            0x2000 + index,
            0,
            &[
                field(b"EDID", b"ClearedOptionalLink\0"),
                // Valid local ownership, absent target: winner validation visits a HashMap.
                field(b"LNAM", &(0x3000 + index).to_le_bytes()),
            ]
            .concat(),
        ));
        plugin.extend(record(
            b"NPC_",
            0x4000 + index,
            0,
            &[
                field(b"EDID", b"UnresolvedLocalizedName\0"),
                field(b"FULL", &(0x5000 + index).to_le_bytes()),
            ]
            .concat(),
        ));
    }
    plugin.extend(record(
        b"KYWD",
        0x6000,
        0,
        &field(b"EDID", b"UsableNeighbor\0"),
    ));
    fs::write(data.join("Synthetic.esm"), plugin).unwrap();
    fs::write(data.join("plugins.txt"), "*Synthetic.esm\n").unwrap();
}

/// Convert the same bytes into two fresh folders under independently sized worker pools.
fn twice(check: impl FnOnce(&Path, &Path)) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    fixture(&data);
    let outputs = [
        directory.path().join("one-thread"),
        directory.path().join("four-threads"),
    ];
    for (threads, output) in [1, 4].into_iter().zip(&outputs) {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| {
                export_record_bundle_typed(
                    &data,
                    &data.join("plugins.txt"),
                    output,
                    &[*b"STAT", *b"NPC_", *b"KYWD", *b"FLST"],
                )
                .unwrap();
            });
        let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        let neighbors: u32 = db
            .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(neighbors, 385, "all safely framed records must survive");
        let cleared: u32 = db.query_row("SELECT count(*) FROM record_diagnostics WHERE category='link' AND action='cleared'", [], |row| row.get(0)).unwrap();
        assert_eq!(cleared, 128, "broken optional links must recover locally");
    }
    check(&outputs[0], &outputs[1]);
}

/// Compare complete scalar rows, including generated identities, under an explicit query order.
fn sql_rows(output: &Path, query: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let mut statement = db.prepare(query).unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Line permutation is observable even when every exported record value is identical.
#[test]
fn typed_export_is_byte_identical_in_fresh_folders() {
    twice(|first, second| {
        let a = fs::read_to_string(first.join("typed-records.jsonl")).unwrap();
        let b = fs::read_to_string(second.join("typed-records.jsonl")).unwrap();
        let mut a_values: Vec<_> = a.lines().collect();
        let mut b_values: Vec<_> = b.lines().collect();
        a_values.sort_unstable();
        b_values.sort_unstable();
        assert_eq!(
            a_values, b_values,
            "record values and multiplicity must stay equal"
        );
        assert!(a == b, "typed export bytes depend on HashMap traversal");
        let keys: Vec<([u8; 4], u32)> = a
            .lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    serde_json::from_value(value["record_type"].clone()).unwrap(),
                    value["form_id"].as_u64().unwrap() as u32,
                )
            })
            .collect();
        assert!(
            keys.windows(2).all(|pair| pair[0] < pair[1]),
            "typed lines must sort by signature, then canonical FormID"
        );
    });
}

/// Decoder and adapter examples must not reflect randomly ordered winner processing.
#[test]
fn diagnostics_are_byte_identical_in_fresh_folders() {
    twice(|first, second| {
        let a = fs::read(first.join("inhouse-reader-diagnostics.json")).unwrap();
        let b = fs::read(second.join("inhouse-reader-diagnostics.json")).unwrap();
        let diagnostics: serde_json::Value = serde_json::from_slice(&a).unwrap();
        assert_eq!(
            diagnostics["decoder"]["synthetic.esm"]["skipped_fields"],
            128
        );
        assert_eq!(
            diagnostics["decoder"]["synthetic.esm"]["invalid_links"],
            128
        );
        assert_eq!(diagnostics["adapter"]["synthetic.esm"][0], 128);
        let query = "SELECT * FROM record_diagnostics ORDER BY diagnostic_id";
        assert_eq!(sql_rows(first, query), sql_rows(second, query));
        assert!(
            a == b,
            "diagnostic examples depend on winner processing order"
        );
        assert!(
            diagnostics["decoder"]["synthetic.esm"]["first_by_category"]["link"]
                .as_str()
                .unwrap()
                .contains("0000307F"),
            "earliest physical link wins even though its FormID sorts last"
        );
        assert!(
            diagnostics["adapter"]["synthetic.esm"][1]
                .as_str()
                .unwrap()
                .contains("0000407F"),
            "adapter summaries use physical provenance rather than sorted projection visits"
        );
        let query = "SELECT rowid,* FROM inhouse_localized_fields ORDER BY rowid";
        assert_eq!(sql_rows(first, query), sql_rows(second, query));
        assert_eq!(
            fs::read(first.join("record-reader.json")).unwrap(),
            fs::read(second.join("record-reader.json")).unwrap()
        );
    });
}
