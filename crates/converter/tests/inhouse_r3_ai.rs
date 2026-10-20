//! Retain native AI/perk meaning separately from comparison-tool presentation.
use converter::{
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::inhouse_ai as native;

/// Read a small independently framed synthetic plugin through the public reader.
fn read(records: &[u8], record_count: u32) -> ReadResult {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Skyrim.esm");
    let mut header = Vec::new();
    header.extend(1.7_f32.to_le_bytes());
    header.extend(record_count.to_le_bytes());
    header.extend(0x900_u32.to_le_bytes());
    let mut plugin = native::record(b"TES4", 0, 0, &native::subrecord(b"HEDR", &header));
    plugin.extend(records);
    std::fs::write(&path, plugin).unwrap();
    let paths = [path];
    read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap()
}

/// Inspect an authored struct member without decoding its canonical bytes again.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}");
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// Return one role's typed value while preserving repeated fields for other checks.
fn named<'a>(record: &'a DecodedRecord, name: &str) -> &'a Value {
    &record
        .fields
        .iter()
        .find(|field| field.name == name)
        .unwrap()
        .value
}

/// A package with asymmetric schedule values and independently framed location inputs.
fn package(id: u32, weekday: u8, locations: &[(i32, [u8; 4])]) -> Vec<u8> {
    let mut body = native::subrecord(b"EDID", b"R3NativePackage\0");
    body.extend(native::subrecord(b"PKDT", &[0; 12]));
    let mut schedule = vec![0xFF, weekday, 17, 23, 41, 0xA7, 0x53, 0xC9];
    schedule.extend((-97_i32).to_le_bytes());
    body.extend(native::subrecord(b"PSDT", &schedule));
    let mut package_data = Vec::new();
    package_data.extend((locations.len() as u32).to_le_bytes());
    package_data.extend(0_u32.to_le_bytes());
    package_data.extend(13_u32.to_le_bytes());
    body.extend(native::subrecord(b"PKCU", &package_data));
    for &(kind, value) in locations {
        body.extend(native::subrecord(b"ANAM", b"Location\0\xA7\x53"));
        let mut location = kind.to_le_bytes().to_vec();
        location.extend(value);
        location.extend((-71_i32).to_le_bytes());
        body.extend(native::subrecord(b"PLDT", &location));
    }
    body.extend(native::subrecord(b"XNAM", &[]));
    native::record(b"PACK", id, 0, &body)
}

/// FF is the signed any-day sentinel; positive special-day values remain distinct.
#[test]
fn package_weekday_retains_signed_sentinel_and_positive_selector() {
    let records = [package(0x801, 0xFF, &[]), package(0x802, 9, &[])].concat();
    let result = read(&records, 2);
    for (id, expected) in [(0x801, -1), (0x802, 9)] {
        let record = &result.records[&id];
        assert!(record.rejected_fields.is_empty());
        let schedule = named(record, "schedule");
        assert_eq!(member(schedule, "day_of_week"), &Value::Signed(expected));
        assert_eq!(member(schedule, "date"), &Value::Unsigned(17));
        assert_eq!(member(schedule, "duration_minutes"), &Value::Signed(-97));
    }
}

/// Opaque location words retain all bytes while alias branches retain signed i32 meaning.
#[test]
fn package_location_distinguishes_opaque_word_from_signed_alias() {
    let opaque = [0xA7, 0x53, 0xC9, 0xF1];
    let negative_alias = (-71_i32).to_le_bytes();
    let result = read(
        &package(
            0x803,
            0xFF,
            &[
                (2, opaque),
                (12, opaque),
                (8, negative_alias),
                (9, negative_alias),
            ],
        ),
        1,
    );
    let record = &result.records[&0x803];
    assert!(record.rejected_fields.is_empty());
    let locations = record
        .fields
        .iter()
        .filter(|field| field.name == "location")
        .collect::<Vec<_>>();
    assert_eq!(locations.len(), 4);
    for (field, kind) in locations.iter().zip([2, 12, 8, 9]) {
        assert_eq!(member(&field.value, "kind"), &Value::Signed(kind));
        assert_eq!(member(&field.value, "radius"), &Value::Signed(-71));
        if kind == 2 || kind == 12 {
            assert_eq!(
                member(&field.value, "value"),
                &Value::Bytes(opaque.to_vec())
            );
            assert_eq!(&field.canonical_bytes[4..8], &opaque);
        } else {
            assert_eq!(member(&field.value, "value"), &Value::Signed(-71));
            assert_eq!(&field.canonical_bytes[4..8], &negative_alias);
        }
    }
}

/// Absent EPF3 and authored zero EPF3 remain separate in adjacent native effects.
#[test]
fn perk_effect_flags_preserve_absence_and_authored_zero_presence() {
    let mut body = native::subrecord(b"EDID", b"R3NativePerk\0");
    body.extend(native::subrecord(b"DATA", &[0, 1, 2, 1, 0]));
    body.extend(native::subrecord(b"PRKE", &[2, 3, 29]));
    body.extend(native::subrecord(b"DATA", &[29, 6, 0]));
    body.extend(native::subrecord(b"PRKF", &[]));
    body.extend(native::subrecord(b"PRKE", &[2, 7, 53]));
    body.extend(native::subrecord(b"DATA", &[17, 9, 0]));
    body.extend(native::subrecord(b"EPFT", &[4]));
    body.extend(native::subrecord(b"EPF3", &[0; 4]));
    body.extend(native::subrecord(b"PRKF", &[]));
    let result = read(&native::record(b"PERK", 0x804, 0, &body), 1);
    let record = &result.records[&0x804];
    assert!(record.rejected_fields.is_empty());
    let starts = record
        .fields
        .iter()
        .enumerate()
        .filter_map(|(index, field)| (field.signature == *b"PRKE").then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 2);
    assert!(
        record.fields[starts[0]..starts[1]]
            .iter()
            .all(|field| field.signature != *b"EPF3")
    );
    let authored = record.fields[starts[1]..]
        .iter()
        .filter(|field| field.signature == *b"EPF3")
        .collect::<Vec<_>>();
    assert_eq!(authored.len(), 1);
    assert_eq!(member(&authored[0].value, "flags"), &Value::Unsigned(0));
    assert_eq!(
        member(&authored[0].value, "fragment_index"),
        &Value::Unsigned(0)
    );
    assert_eq!(authored[0].canonical_bytes, [0; 4]);
}
