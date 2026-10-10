//! Nullable runtime projections must preserve reference archive field identity.
use converter::esm::{inhouse, types::ArchivedRecordData};
use dummy_content::{esm, layout};
use rusqlite::Connection;
use std::fs;

/// Independently frame a bounded native subrecord without schema-derived fields.
fn subrecord(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [
        tag.to_vec(),
        u16::try_from(data.len()).unwrap().to_le_bytes().to_vec(),
        data.to_vec(),
    ]
    .concat()
}

/// Independently frame the 24-byte SSE record header and authored body.
fn record(tag: &[u8; 4], id: u32, flags: u32, fields: &[u8]) -> Vec<u8> {
    [
        tag.to_vec(),
        u32::try_from(fields.len()).unwrap().to_le_bytes().to_vec(),
        flags.to_le_bytes().to_vec(),
        id.to_le_bytes().to_vec(),
        0u32.to_le_bytes().to_vec(),
        44u16.to_le_bytes().to_vec(),
        0u16.to_le_bytes().to_vec(),
        fields.to_vec(),
    ]
    .concat()
}

/// Author one localized map marker; FULL ordinal four belongs to the marker.
fn marker(id: u32, key: u32) -> Vec<u8> {
    let transform = [123.25f32, -456.5, 7.75, 0.125, -0.25, 0.5]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    let fields = [
        subrecord(b"EDID", b"ReferenceLocalizationProbe\0"),
        subrecord(b"NAME", &0x0100_3F10u32.to_le_bytes()),
        subrecord(b"XMRK", &[]),
        subrecord(b"FNAM", &[1]),
        subrecord(b"FULL", &key.to_le_bytes()),
        subrecord(b"TNAM", &[1, 0]),
        subrecord(b"DATA", &transform),
    ]
    .concat();
    record(b"REFR", id, 0, &fields)
}

#[test]
fn unresolved_marker_text_preserves_identical_reference_and_record_archives() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    fs::create_dir(&data).unwrap();
    let generated = esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    fs::write(data.join("Skyrim.esm"), &generated).unwrap();
    let length = u32::from_le_bytes(generated[4..8].try_into().unwrap()) as usize;
    let mut header = generated[24..24 + length].to_vec();
    header.extend(subrecord(b"MAST", b"Skyrim.esm\0"));
    header.extend(subrecord(b"DATA", &[0; 8]));
    let mut plugin = record(b"TES4", 0, 0x80, &header);
    plugin.extend(record(
        b"STAT",
        0x0100_3F10,
        0,
        &subrecord(b"EDID", b"ReferenceLocalizationBase\0"),
    ));
    plugin.extend(marker(0x0100_3F11, 17));
    plugin.extend(marker(0x0100_3F12, 100));
    fs::write(data.join("ReferenceLocalization.esp"), plugin).unwrap();
    let strings = data.join("Strings");
    fs::create_dir(&strings).unwrap();
    let text = b"Usable neighboring marker\0";
    fs::write(
        strings.join("ReferenceLocalization_english.strings"),
        [
            1u32.to_le_bytes().to_vec(),
            u32::try_from(text.len()).unwrap().to_le_bytes().to_vec(),
            100u32.to_le_bytes().to_vec(),
            0u32.to_le_bytes().to_vec(),
            text.to_vec(),
        ]
        .concat(),
    )
    .unwrap();
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*ReferenceLocalization.esp\n").unwrap();
    let output = temp.path().join("bundle");
    inhouse::export_record_bundle_typed(&data, &plugins, &output, &[*b"REFR"]).unwrap();
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for (id, expected) in [
        (0x0100_3F11u32, b"\0".as_slice()),
        (0x0100_3F12u32, text.as_slice()),
    ] {
        let archives: (Vec<u8>, Vec<u8>) = database
            .query_row(
                "SELECT records.data, r.data FROM records JOIN \"references\" r ON r.id=records.form_id WHERE records.form_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        // This semantic RED precedes every new provenance-table assertion.
        assert_eq!(
            archives.1, archives.0,
            "reference archive lost fields for {id:08X}"
        );
        let archived =
            rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&archives.0).unwrap();
        assert_eq!(archived.subrecords[4].tag, *b"FULL");
        assert_eq!(archived.subrecords[4].data, expected);
        assert_eq!(archived.subrecords.len(), 7);
    }
    for (id, key, expected_text, status) in [
        (0x0100_3F11u32, 17u32, None, "missing"),
        (
            0x0100_3F12u32,
            100u32,
            Some("Usable neighboring marker"),
            "resolved",
        ),
    ] {
        let metadata: (u32, String, String, String, u32, u32, Option<String>, String) = database
            .query_row(
                "SELECT field_index,signature,field_name,string_table,string_id,load_order,resolved_text,status FROM inhouse_localized_fields WHERE form_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
            )
            .unwrap();
        assert_eq!(
            metadata,
            (
                4,
                "FULL".into(),
                "full_name".into(),
                "strings".into(),
                key,
                1,
                expected_text.map(str::to_owned),
                status.into()
            )
        );
    }
}
