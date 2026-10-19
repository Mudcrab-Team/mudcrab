//! Independent identity regressions for corrupt aliases and reserved target IDs.
use converter::{
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, layout};
use std::{fs, path::PathBuf};

/// Keep a real dummy-content base alongside independently framed test records.
struct Fixture {
    directory: tempfile::TempDir,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Append our asymmetric cases to the existing generated nested world fixture.
    fn new(records: Vec<u8>) -> Self {
        let mut base = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let size = u32::from_le_bytes(base[4..8].try_into().unwrap()) as usize;
        let header = base[24..24 + size].to_vec();
        base.extend(records);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, base).unwrap();
        Self {
            directory,
            header,
            paths: vec![path],
        }
    }

    /// Give each patch a real master; file-relative zero differs from its own slot.
    fn add(&mut self, name: &str, records: Vec<u8>) {
        let mut payload = self.header.clone();
        payload.extend(subrecord(b"MAST", b"Skyrim.esm\0"));
        payload.extend(subrecord(b"DATA", &[0xa5; 8]));
        let mut bytes = record(b"TES4", 0, 0, &payload);
        bytes.extend(records);
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Exercise the shared load-order owner and actual public reader.
    fn read(&self) -> ReadResult {
        let order = LoadOrder::read(&self.paths).unwrap();
        read_plugins(&self.paths, &order).unwrap()
    }
}

/// Independently frame an SSE record, including a header-only deletion.
fn record(signature: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(signature);
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(flags.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend([0; 4]);
    bytes.extend(44_u16.to_le_bytes());
    bytes.extend([0; 2]);
    bytes.extend(payload);
    bytes
}

/// Frame payloads with their physical signature and independent size.
fn subrecord(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(u16::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Use EDID and deliberately distinct DATA values to expose redirected identities.
fn setting(id: u32, name: &str, value: f32) -> Vec<u8> {
    let mut name = name.as_bytes().to_vec();
    name.push(0);
    record(
        b"GMST",
        id,
        0,
        &[
            subrecord(b"EDID", &name),
            subrecord(b"DATA", &value.to_le_bytes()),
        ]
        .concat(),
    )
}

/// Find by signature and inspect the canonical bytes as well as typed values.
fn field<'a>(
    record: &'a DecodedRecord,
    signature: &[u8; 4],
) -> &'a converter::records::DecodedField {
    record
        .fields
        .iter()
        .find(|field| &field.signature == signature)
        .unwrap()
}

/// A corrupt name/ID alias cannot poison a later legitimate header-only deletion.
#[test]
fn colliding_gmst_alias_keeps_settings_and_does_not_redirect_deletion() {
    let mut fixture = Fixture::new(
        [
            setting(0x1800, "fFirstIndependent", 11.25),
            setting(0x1801, "fSecondIndependent", -42.5),
        ]
        .concat(),
    );
    fixture.add(
        "Collision.esp",
        [
            setting(0x1800, "fSecondIndependent", 99.75),
            setting(0x0100_1810, "fUsableNeighbor", 173.5),
        ]
        .concat(),
    );
    let before = fixture.read();
    assert_eq!(
        field(&before.records[&0x1800], b"DATA").value,
        Value::Float(11.25)
    );
    assert_eq!(
        field(&before.records[&0x1801], b"DATA").value,
        Value::Float(-42.5)
    );
    assert_eq!(
        field(&before.records[&0x0100_1810], b"DATA").value,
        Value::Float(173.5)
    );
    assert_eq!(before.diagnostics["collision.esp"].skipped_records, 1);
    assert!(
        before.diagnostics["collision.esp"]
            .first_example
            .as_ref()
            .unwrap()
            .contains("colliding identity")
    );
    assert!(!before.overrides.contains_key(&0x1801));

    fixture.add("DeleteFirst.esp", record(b"GMST", 0x1800, 0x20, &[]));
    let after = fixture.read();
    assert!(!after.records.contains_key(&0x1800));
    assert_eq!(
        field(&after.records[&0x1801], b"DATA").value,
        Value::Float(-42.5)
    );
    assert_eq!(
        field(&after.records[&0x0100_1810], b"DATA").value,
        Value::Float(173.5)
    );
    let chain = &after.overrides[&0x1800];
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].plugin_index, 0);
    assert!(!chain[0].deleted);
    assert_eq!(chain[1].plugin_index, 2);
    assert!(chain[1].deleted);
    assert_eq!(after.diagnostics["deletefirst.esp"].skipped_records, 0);
}

/// Known low IDs must satisfy target types, while valid sibling links survive.
#[test]
fn known_reserved_ids_still_obey_declared_link_target_types() {
    let npc = |id, race: u32| {
        record(
            b"NPC_",
            id,
            0,
            &[
                subrecord(b"EDID", b"IndependentNpc\0"),
                subrecord(b"RNAM", &race.to_le_bytes()),
                subrecord(b"CNAM", &0x62_u32.to_le_bytes()),
            ]
            .concat(),
        )
    };
    let fixture = Fixture::new(
        [
            record(b"STAT", 0x61, 0, &subrecord(b"EDID", b"LowStatic\0")),
            record(b"RACE", 0x19, 0, &subrecord(b"EDID", b"LowRace\0")),
            record(b"CLAS", 0x62, 0, &subrecord(b"EDID", b"LowClass\0")),
            npc(0x1810, 0x61),
            npc(0x1811, 0x19),
        ]
        .concat(),
    );
    let result = fixture.read();
    let wrong = &result.records[&0x1810];
    let race = field(wrong, b"RNAM");
    assert_eq!(race.value, Value::FormId(0));
    assert_eq!(race.canonical_bytes, [0; 4]);
    let class = field(wrong, b"CNAM");
    assert_eq!(class.value, Value::FormId(0x62));
    assert_eq!(class.canonical_bytes, 0x62_u32.to_le_bytes());
    let valid = field(&result.records[&0x1811], b"RNAM");
    assert_eq!(valid.value, Value::FormId(0x19));
    assert_eq!(valid.canonical_bytes, 0x19_u32.to_le_bytes());
    assert_eq!(result.diagnostics["skyrim.esm"].invalid_links, 1);
    assert!(result.records.contains_key(&0x61));
    assert!(result.records.contains_key(&0x19));
    assert!(result.records.contains_key(&0x62));
}
