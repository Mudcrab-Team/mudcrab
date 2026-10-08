//! Public-reader regressions built from dummy-content and independently framed bytes.
//! Layout facts: pinned xEdit TES5 XESP (u8 flags + three padding bytes), XLKR
//! 4/8-byte variants, MODS counted entries, and shared TES4 master-list semantics.
use converter::{
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, layout};
use std::{fs, path::PathBuf};

/// A generated base plugin plus independently authored dependent plugin bytes.
struct Fixture {
    directory: tempfile::TempDir,
    generated: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Retain dummy-content's real nested world/cell fixture as the first input.
    fn new() -> Self {
        let generated = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, &generated).unwrap();
        Self {
            directory,
            generated,
            paths: vec![path],
        }
    }

    /// Copy only the generated TES4 metadata and append our MAST/DATA pairs.
    fn add(&mut self, name: &str, masters: &[&str], flags: u32, records: Vec<u8>) {
        let size = u32::from_le_bytes(self.generated[4..8].try_into().unwrap()) as usize;
        let mut payload = self.generated[24..24 + size].to_vec();
        for master in masters {
            payload.extend(subrecord(b"MAST", &zstring(master)));
            // Nonzero sentinel padding ensures no parser treats DATA as a link.
            payload.extend(subrecord(b"DATA", &[0xa5; 8]));
        }
        let mut bytes = record(b"TES4", 0, flags, &payload);
        bytes.extend(records);
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Exercise the shared load-order authority and public in-house frontend.
    fn read(&self) -> (LoadOrder, ReadResult) {
        let order = LoadOrder::read(&self.paths).unwrap();
        let result = read_plugins(&self.paths, &order).unwrap();
        (order, result)
    }
}

/// Independently emit a 24-byte Skyrim SE record header, never using reader helpers.
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

/// Emit the six-byte subrecord frame with independently supplied payload bytes.
fn subrecord(signature: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.extend(u16::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Use a trailing zero without depending on the production string decoder.
fn zstring(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Supply asymmetric position/rotation values, making offset swaps observable.
fn transform() -> Vec<u8> {
    [13.25_f32, -27.5, 81.75, 0.125, -0.375, 1.25]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect()
}

/// A base object with a real ordered model field and an independently chosen ID.
fn static_record(id: u32, editor_id: &str) -> Vec<u8> {
    record(
        b"STAT",
        id,
        0,
        &[
            subrecord(b"EDID", &zstring(editor_id)),
            subrecord(b"MODL", b"independent.nif\0"),
        ]
        .concat(),
    )
}

/// A placed object with a base link and unambiguous transform bytes.
fn reference_record(id: u32, base: u32) -> Vec<u8> {
    record(
        b"REFR",
        id,
        0,
        &[
            subrecord(b"NAME", &base.to_le_bytes()),
            subrecord(b"DATA", &transform()),
        ]
        .concat(),
    )
}

/// Find an occurrence by its physical signature; do not rely on schema output order.
fn field<'a>(
    record: &'a DecodedRecord,
    signature: &[u8; 4],
) -> &'a converter::records::DecodedField {
    record
        .fields
        .iter()
        .find(|field| &field.signature == signature)
        .unwrap_or_else(|| {
            panic!(
                "missing {} in {:08X}",
                String::from_utf8_lossy(signature),
                record.form_id
            )
        })
}

/// Read a named member from a decoded struct, making expected field roles explicit.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct for {name}: {value:?}");
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// Inspect the consumer bytes independently of the typed-value representation.
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// Pinned TES5 line4092 admits spell, shout and leveled-spell actor effects.
#[test]
fn npc_actor_effects_accept_all_documented_targets_with_divergent_master_slot() {
    let mut fixture = Fixture::new();
    fixture.add("Unused.esm", &[], 1, vec![]);
    fixture.add(
        "Owner.esm",
        &[],
        1,
        [
            record(b"SPEL", 0x1400, 0, &subrecord(b"EDID", b"Spell\0")),
            record(b"SHOU", 0x1401, 0, &subrecord(b"EDID", b"Shout\0")),
            record(b"LVSP", 0x1402, 0, &subrecord(b"EDID", b"Leveled\0")),
            static_record(0x1403, "WrongKind"),
        ]
        .concat(),
    );
    fixture.add(
        "Consumer.esp",
        &["Owner.esm"],
        0,
        record(
            b"NPC_",
            0x0100_1800,
            0,
            &[
                subrecord(b"SPCT", &4_u32.to_le_bytes()),
                subrecord(b"SPLO", &0x1400_u32.to_le_bytes()),
                subrecord(b"SPLO", &0x1401_u32.to_le_bytes()),
                subrecord(b"SPLO", &0x1402_u32.to_le_bytes()),
                subrecord(b"SPLO", &0x1403_u32.to_le_bytes()),
            ]
            .concat(),
        ),
    );
    let (order, result) = fixture.read();
    assert_eq!(order.normal["owner.esm"], 2);
    let effects: Vec<_> = result.records[&0x0300_1800]
        .fields
        .iter()
        .filter(|field| field.signature == *b"SPLO")
        .collect();
    assert_eq!(effects.len(), 4);
    for (effect, expected) in effects
        .iter()
        .zip([0x0200_1400, 0x0200_1401, 0x0200_1402, 0])
    {
        assert_eq!(effect.value, Value::FormId(expected));
        assert_eq!(word(&effect.canonical_bytes, 0), expected);
    }
    assert_eq!(result.diagnostics["consumer.esp"].invalid_links, 1);
    assert_eq!(result.diagnostics["consumer.esp"].skipped_records, 0);
}

/// Master index zero differs from global full slot two; light slot is independent.
#[test]
fn shared_master_lists_resolve_full_and_light_links_with_xesp_padding() {
    let mut fixture = Fixture::new();
    fixture.add("Unused.esm", &[], 1, vec![]);
    fixture.add("Owner.esm", &[], 1, static_record(0x1400, "Owned"));
    fixture.add(
        "Light.esl",
        &["Owner.esm"],
        0x201,
        reference_record(0x0100_0abc, 0x1400),
    );
    let mut enable_parent = 0x0100_0abc_u32.to_le_bytes().to_vec();
    enable_parent.extend([2, 0xa1, 0xb2, 0xc3]);
    fixture.add(
        "Consumer.esp",
        &["Owner.esm", "Light.esl"],
        0,
        record(
            b"REFR",
            0x0200_1800,
            0,
            &[
                subrecord(b"NAME", &0x1400_u32.to_le_bytes()),
                subrecord(b"XSCL", &1.75_f32.to_le_bytes()),
                subrecord(b"XESP", &enable_parent),
                subrecord(b"DATA", &transform()),
            ]
            .concat(),
        ),
    );
    let (order, result) = fixture.read();
    assert_eq!(order.metadata[4].masters, ["Owner.esm", "Light.esl"]);
    assert_eq!(order.normal["owner.esm"], 2);
    assert_eq!(order.normal["consumer.esp"], 3);
    assert_eq!(order.light["light.esl"], 0);
    assert_eq!(result.headers.len(), 5);
    for (index, header) in result.headers.iter().enumerate() {
        assert_eq!(header.record_type, *b"TES4");
        assert_eq!(header.load_order, index as u32);
        let hedr = field(header, b"HEDR");
        assert_eq!(member(&hedr.value, "version"), &Value::Float(1.7));
        assert_eq!(member(&hedr.value, "record_count"), &Value::Unsigned(0));
        assert_eq!(member(&hedr.value, "next_object_id"), &Value::Unsigned(0));
    }
    assert_eq!(result.headers[3].flags, 0x201);
    let consumer_masters: Vec<_> = result.headers[4]
        .fields
        .iter()
        .filter(|field| field.signature == *b"MAST")
        .map(|field| &field.value)
        .collect();
    assert_eq!(
        consumer_masters,
        [
            &Value::String("Owner.esm".into()),
            &Value::String("Light.esl".into())
        ]
    );
    let master_padding: Vec<_> = result.headers[4]
        .fields
        .iter()
        .filter(|field| field.signature == *b"DATA")
        .collect();
    assert_eq!(master_padding.len(), 2);
    assert!(
        master_padding
            .iter()
            .all(|field| field.canonical_bytes == [0xa5; 8])
    );
    let consumer = &result.records[&0x0300_1800];
    assert_eq!(field(consumer, b"NAME").value, Value::FormId(0x0200_1400));
    let parent = field(consumer, b"XESP");
    assert_eq!(
        member(&parent.value, "reference"),
        &Value::FormId(0xfe00_0abc)
    );
    assert_eq!(member(&parent.value, "flags"), &Value::Unsigned(2));
    assert_eq!(word(&parent.canonical_bytes, 0), 0xfe00_0abc);
    assert_eq!(&parent.canonical_bytes[4..], &[2, 0xa1, 0xb2, 0xc3]);
    assert_eq!(field(consumer, b"XSCL").value, Value::Float(1.75));
    assert_eq!(
        member(&field(consumer, b"DATA").value, "position_y"),
        &Value::Float(-27.5)
    );
    assert_eq!(
        field(&result.records[&0xfe00_0abc], b"NAME").value,
        Value::FormId(0x0200_1400)
    );
    assert_eq!(result.diagnostics["consumer.esp"].invalid_links, 0);
}

/// Counted alternate textures distinguish their variable name from the following TXST link.
fn alternate_textures(entries: &[(&str, u32, u32)]) -> Vec<u8> {
    let mut bytes = u32::try_from(entries.len()).unwrap().to_le_bytes().to_vec();
    for &(name, texture, index) in entries {
        let name = zstring(name);
        bytes.extend(u32::try_from(name.len()).unwrap().to_le_bytes());
        bytes.extend(name);
        bytes.extend(texture.to_le_bytes());
        bytes.extend(index.to_le_bytes());
    }
    bytes
}

/// Check both valid and mistyped links through nested unions and variable arrays.
#[test]
fn typed_targets_clear_wrong_kinds_inside_unions_and_alternate_textures() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Owner.esm",
        &[],
        1,
        [
            static_record(0x1400, "Static"),
            record(b"TXST", 0x1401, 0, &subrecord(b"EDID", b"Texture\0")),
            record(b"KYWD", 0x1402, 0, &subrecord(b"EDID", b"Keyword\0")),
            reference_record(0x1403, 0x1400),
            record(b"MATO", 0x1404, 0, &subrecord(b"EDID", b"Material\0")),
        ]
        .concat(),
    );
    let model = alternate_textures(&[("OddName", 0x1401, 17), ("B", 0x1400, 29)]);
    let mut direction = 63.5_f32.to_le_bytes().to_vec();
    direction.extend(0x1404_u32.to_le_bytes());
    direction.extend([1, 0xe1, 0xe2, 0xe3]);
    let mut wrong_direction = direction.clone();
    wrong_direction[4..8].copy_from_slice(&0x1403_u32.to_le_bytes());
    let mut wrong_parent = 0x1400_u32.to_le_bytes().to_vec();
    wrong_parent.extend([1, 0xa1, 0xb2, 0xc3]);
    let good_link = [0x1402_u32.to_le_bytes(), 0x1403_u32.to_le_bytes()].concat();
    let wrong_link = [0x1400_u32.to_le_bytes(), 0x1401_u32.to_le_bytes()].concat();
    fixture.add(
        "Consumer.esp",
        &["Owner.esm"],
        0,
        [
            record(
                b"STAT",
                0x0100_1800,
                0,
                &[
                    subrecord(b"EDID", b"Model\0"),
                    subrecord(b"MODL", b"different.nif\0"),
                    subrecord(b"MODS", &model),
                    subrecord(b"DNAM", &direction),
                ]
                .concat(),
            ),
            record(
                b"STAT",
                0x0100_1801,
                0,
                &[
                    subrecord(b"EDID", b"WrongMaterial\0"),
                    subrecord(b"DNAM", &wrong_direction),
                ]
                .concat(),
            ),
            record(
                b"REFR",
                0x0100_1802,
                0,
                &[
                    subrecord(b"NAME", &0x1402_u32.to_le_bytes()),
                    subrecord(b"XESP", &wrong_parent),
                    subrecord(b"XLKR", &good_link),
                    subrecord(b"XLKR", &wrong_link),
                    subrecord(b"DATA", &transform()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    let object = &result.records[&0x0200_1800];
    let Value::Array(textures) = &field(object, b"MODS").value else {
        panic!("alternate textures not decoded");
    };
    assert_eq!(
        member(&textures[0], "texture_set"),
        &Value::FormId(0x0100_1401)
    );
    assert_eq!(member(&textures[0], "index"), &Value::Unsigned(17));
    assert_eq!(member(&textures[1], "texture_set"), &Value::FormId(0));
    assert_eq!(member(&textures[1], "index"), &Value::Unsigned(29));
    assert_eq!(
        member(&field(object, b"DNAM").value, "material"),
        &Value::FormId(0x0100_1404)
    );
    let wrong = &result.records[&0x0200_1801];
    assert_eq!(word(&field(wrong, b"DNAM").canonical_bytes, 4), 0);
    let placed = &result.records[&0x0200_1802];
    assert_eq!(field(placed, b"NAME").value, Value::FormId(0));
    assert_eq!(word(&field(placed, b"XESP").canonical_bytes, 0), 0);
    let links: Vec<_> = placed
        .fields
        .iter()
        .filter(|field| field.signature == *b"XLKR")
        .collect();
    assert_eq!(links.len(), 2);
    assert_eq!(word(&links[0].canonical_bytes, 0), 0x0100_1402);
    assert_eq!(word(&links[0].canonical_bytes, 4), 0x0100_1403);
    assert_eq!(links[1].canonical_bytes, [0; 8]);
    assert_eq!(result.diagnostics["consumer.esp"].invalid_links, 6);
    assert!(result.diagnostics["consumer.esp"].first_example.is_some());
}

/// A last winning restoration follows a header-only deletion of the original owner.
#[test]
fn override_chain_preserves_deletion_and_restoration_provenance() {
    let mut fixture = Fixture::new();
    fixture.add("Owner.esm", &[], 1, static_record(0x1400, "Original"));
    fixture.add(
        "Delete.esp",
        &["Owner.esm"],
        0,
        record(b"STAT", 0x1400, 0x20, &[]),
    );
    let (_, deleted) = fixture.read();
    assert!(!deleted.records.contains_key(&0x0100_1400));
    assert_eq!(deleted.overrides[&0x0100_1400].len(), 2);
    assert!(deleted.overrides[&0x0100_1400][1].deleted);
    fixture.add(
        "Restore.esp",
        &["Owner.esm"],
        0,
        static_record(0x1400, "Restored"),
    );
    let (_, restored) = fixture.read();
    let winner = &restored.records[&0x0100_1400];
    assert_eq!(winner.load_order, 3);
    assert_eq!(winner.source_form_id, 0x1400);
    assert_eq!(
        field(winner, b"EDID").value,
        Value::String("Restored".into())
    );
    let chain = &restored.overrides[&0x0100_1400];
    assert_eq!(
        chain
            .iter()
            .map(|source| source.plugin_index)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        chain
            .iter()
            .map(|source| source.deleted)
            .collect::<Vec<_>>(),
        [false, true, false]
    );
}

/// Unknown fields leave the cursor intact; late recognized fields cannot rewind it.
#[test]
fn ordered_matching_skips_unknown_and_out_of_order_fields() {
    let mut fixture = Fixture::new();
    let payload = [
        subrecord(b"EDID", b"First\0"),
        subrecord(b"ZZZZ", &[91, 42, 73]),
        subrecord(b"MODL", b"ordered.nif\0"),
        subrecord(b"EDID", b"TooLate\0"),
        subrecord(b"OBND", &[0x5a; 12]),
    ]
    .concat();
    fixture.add("Ordered.esp", &[], 0, record(b"STAT", 0x1800, 0, &payload));
    let (_, result) = fixture.read();
    let object = &result.records[&0x0100_1800];
    assert_eq!(object.fields.len(), 2);
    assert_eq!(field(object, b"EDID").value, Value::String("First".into()));
    assert_eq!(
        field(object, b"MODL").value,
        Value::String("ordered.nif".into())
    );
    assert_eq!(object.raw_payload, payload);
    assert_eq!(result.diagnostics["ordered.esp"].unexpected_subrecords, 3);
}

/// Bad compressed records and local field lengths preserve usable neighboring records.
#[test]
fn malformed_records_and_fields_keep_valid_neighbors() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Broken.esp",
        &[],
        0,
        [
            static_record(0x1800, "Before"),
            record(b"STAT", 0x1801, 0x40000, &[100, 0, 0, 0, 0xff]),
            record(
                b"REFR",
                0x1802,
                0,
                &[
                    subrecord(b"NAME", &[3, 0, 0]),
                    subrecord(b"DATA", &transform()),
                ]
                .concat(),
            ),
            static_record(0x1803, "After"),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    assert!(result.records.contains_key(&0x0100_1800));
    assert!(!result.records.contains_key(&0x0100_1801));
    let partial = &result.records[&0x0100_1802];
    assert!(
        !partial
            .fields
            .iter()
            .any(|field| field.signature == *b"NAME")
    );
    assert_eq!(field(partial, b"DATA").canonical_bytes, transform());
    assert_eq!(
        field(&result.records[&0x0100_1803], b"EDID").value,
        Value::String("After".into())
    );
    let diagnostics = &result.diagnostics["broken.esp"];
    assert_eq!(diagnostics.skipped_records, 1);
    assert_eq!(diagnostics.skipped_fields, 1);
    assert!(diagnostics.first_example.is_some());
}

/// Extended sizes override the short length without crossing their enclosing record.
#[test]
fn xxxx_large_fields_and_truncated_payloads_are_bounded() {
    let mut fixture = Fixture::new();
    let mut extended = subrecord(b"XXXX", &70_000_u32.to_le_bytes());
    extended.extend(b"BLOB\x07\x00");
    extended.extend(vec![0x6d; 70_000]);
    let mut malformed = subrecord(b"EDID", b"PrefixSurvives\0");
    malformed.extend(subrecord(b"XXXX", &70_000_u32.to_le_bytes()));
    malformed.extend(b"BLOB\x00\x00tiny");
    fixture.add(
        "Extended.esp",
        &[],
        0,
        [
            record(b"WXYZ", 0x1800, 0, &extended),
            record(b"STAT", 0x1801, 0, &malformed),
            static_record(0x1802, "Neighbor"),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    let opaque = &result.records[&0x0100_1800];
    assert!(!opaque.supported);
    assert_eq!(field(opaque, b"BLOB").canonical_bytes, vec![0x6d; 70_000]);
    let prefix = &result.records[&0x0100_1801];
    assert_eq!(prefix.fields.len(), 1);
    assert_eq!(
        field(prefix, b"EDID").value,
        Value::String("PrefixSurvives".into())
    );
    assert!(result.records.contains_key(&0x0100_1802));
    assert_eq!(result.diagnostics["extended.esp"].skipped_fields, 1);
}

/// Repeated LAND layers retain their links and nested VHGT offsets before/after repeats.
#[test]
fn land_layers_repeat_and_height_delta_array_excludes_padding() {
    let mut fixture = Fixture::new();
    let mut height = 7.5_f32.to_le_bytes().to_vec();
    height.extend((0..1089).map(|index| (index % 127) as u8));
    height.extend([0xa1, 0xb2, 0xc3]);
    let layer = |texture: u32, quadrant: u8, number: i16| {
        [
            texture.to_le_bytes().as_slice(),
            &[quadrant, 0xe1],
            &number.to_le_bytes(),
        ]
        .concat()
    };
    let alpha = [
        17_u16.to_le_bytes().as_slice(),
        &[0xa1, 0xb2],
        &0.375_f32.to_le_bytes(),
    ]
    .concat();
    fixture.add(
        "Landscape.esp",
        &[],
        0,
        [
            record(b"LTEX", 0x1400, 0, &subrecord(b"EDID", b"LayerTexture\0")),
            record(
                b"LAND",
                0x1800,
                0,
                &[
                    subrecord(b"DATA", &7_u32.to_le_bytes()),
                    subrecord(b"VHGT", &height),
                    subrecord(b"BTXT", &layer(0x1400, 0, -1)),
                    subrecord(b"ATXT", &layer(0x1400, 0, 1)),
                    subrecord(b"VTXT", &alpha),
                    subrecord(b"ATXT", &layer(0x1400, 0, 2)),
                    subrecord(b"VTXT", &alpha),
                    subrecord(b"BTXT", &layer(0x1400, 3, -1)),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    let land = &result.records[&0x0100_1800];
    let height_field = field(land, b"VHGT");
    let Value::Array(deltas) = member(&height_field.value, "deltas") else {
        panic!("missing height deltas");
    };
    assert_eq!(deltas.len(), 1089);
    assert_eq!(deltas[126], Value::Signed(126));
    assert_eq!(height_field.canonical_bytes, height);
    assert_eq!(
        land.fields
            .iter()
            .filter(|field| field.signature == *b"BTXT")
            .count(),
        2
    );
    assert_eq!(
        land.fields
            .iter()
            .filter(|field| field.signature == *b"ATXT")
            .count(),
        2
    );
    for layer in land
        .fields
        .iter()
        .filter(|field| matches!(&field.signature, b"BTXT" | b"ATXT"))
    {
        assert_eq!(word(&layer.canonical_bytes, 0), 0x0100_1400);
    }
    assert_eq!(result.diagnostics["landscape.esp"].skipped_fields, 0);
    assert_eq!(result.diagnostics["landscape.esp"].unexpected_subrecords, 0);
}

/// GMST's EDID selects unsigned Boolean and float storage independently.
#[test]
fn gmst_decider_preserves_boolean_unsigned_width_and_float_value() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Settings.esp",
        &[],
        0,
        [
            record(
                b"GMST",
                0x1800,
                0,
                &[
                    subrecord(b"EDID", b"bIndependent\0"),
                    subrecord(b"DATA", &0xffff_ffff_u32.to_le_bytes()),
                ]
                .concat(),
            ),
            record(
                b"GMST",
                0x1801,
                0,
                &[
                    subrecord(b"EDID", b"fIndependent\0"),
                    subrecord(b"DATA", &173.25_f32.to_le_bytes()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    assert_eq!(
        field(&result.records[&0x0100_1800], b"DATA").value,
        Value::Unsigned(0xffff_ffff)
    );
    assert_eq!(
        field(&result.records[&0x0100_1801], b"DATA").value,
        Value::Float(173.25)
    );
}

/// A TES4 localized bit selects four-byte string IDs rather than decoding their bytes as text.
#[test]
fn localized_header_selects_string_ids_without_guessing_display_text() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Localized.esp",
        &[],
        0x80,
        [
            record(
                b"NPC_",
                0x1800,
                0,
                &[
                    subrecord(b"EDID", b"LocalizedActor\0"),
                    subrecord(b"FULL", &0x0061_6200_u32.to_le_bytes()),
                ]
                .concat(),
            ),
            record(
                b"GMST",
                0x1801,
                0,
                &[
                    subrecord(b"EDID", b"sIndependent\0"),
                    subrecord(b"DATA", &0x1234_5678_u32.to_le_bytes()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let (order, result) = fixture.read();
    assert_eq!(order.metadata[1].flags & 0x80, 0x80);
    assert_eq!(
        field(&result.records[&0x0100_1800], b"FULL").value,
        Value::LocalizedString(0x0061_6200)
    );
    assert_eq!(
        field(&result.records[&0x0100_1801], b"DATA").value,
        Value::LocalizedString(0x1234_5678)
    );
}

/// Old and extended movement/grid layouts have explicit optional tails, never shifted fields.
#[test]
fn movement_and_cell_size_deciders_keep_old_and_extended_layouts() {
    let mut fixture = Fixture::new();
    let speeds: Vec<u8> = (0..11)
        .flat_map(|index| (11.25_f32 + index as f32 * 3.5).to_le_bytes())
        .collect();
    let grid = [(-7_i32).to_le_bytes(), 19_i32.to_le_bytes()].concat();
    let mut extended_grid = grid.clone();
    extended_grid.extend([5, 0xa1, 0xb2, 0xc3]);
    fixture.add(
        "Layouts.esp",
        &[],
        0,
        [
            record(
                b"MOVT",
                0x1800,
                0,
                &[
                    subrecord(b"EDID", b"OldMovement\0"),
                    subrecord(b"SPED", &speeds[..40]),
                ]
                .concat(),
            ),
            record(
                b"MOVT",
                0x1801,
                0,
                &[
                    subrecord(b"EDID", b"NewMovement\0"),
                    subrecord(b"SPED", &speeds),
                ]
                .concat(),
            ),
            record(
                b"CELL",
                0x1802,
                0,
                &[
                    subrecord(b"EDID", b"OldCell\0"),
                    subrecord(b"DATA", &[2]),
                    subrecord(b"XCLC", &grid),
                ]
                .concat(),
            ),
            record(
                b"CELL",
                0x1803,
                0,
                &[
                    subrecord(b"EDID", b"NewCell\0"),
                    subrecord(b"DATA", &0x102_u16.to_le_bytes()),
                    subrecord(b"XCLC", &extended_grid),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    let old = &field(&result.records[&0x0100_1800], b"SPED").value;
    let Value::Struct(old_members) = old else {
        panic!("missing old speeds");
    };
    assert_eq!(old_members.len(), 10);
    assert_eq!(member(old, "back_run"), &Value::Float(35.75));
    assert_eq!(
        member(
            &field(&result.records[&0x0100_1801], b"SPED").value,
            "rotate_moving_run"
        ),
        &Value::Float(46.25)
    );
    assert_eq!(
        field(&result.records[&0x0100_1802], b"DATA").canonical_bytes,
        [2]
    );
    let new_cell = &result.records[&0x0100_1803];
    assert_eq!(field(new_cell, b"DATA").value, Value::Unsigned(0x102));
    assert_eq!(
        member(&field(new_cell, b"XCLC").value, "x"),
        &Value::Signed(-7)
    );
    assert_eq!(
        member(&field(new_cell, b"XCLC").value, "land_flags"),
        &Value::Unsigned(5)
    );
    assert_eq!(
        &field(new_cell, b"XCLC").canonical_bytes[9..],
        &[0xa1, 0xb2, 0xc3]
    );
    assert_eq!(result.diagnostics["layouts.esp"].skipped_fields, 0);
}

/// Texture meanings and gendered world paths are physical source facts, independent of SQL names.
#[test]
fn texture_slots_and_female_armor_world_model_follow_authored_facts() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Assets.esp",
        &[],
        0,
        [
            record(
                b"TXST",
                0x1800,
                0,
                &[
                    subrecord(b"EDID", b"AsymmetricTextures\0"),
                    subrecord(b"TX02", b"environment-mask.dds\0"),
                    subrecord(b"TX03", b"glow.dds\0"),
                    subrecord(b"TX04", b"height.dds\0"),
                    subrecord(b"TX05", b"environment.dds\0"),
                    subrecord(b"TX06", b"multilayer.dds\0"),
                    subrecord(b"TX07", b"backlight-specular.dds\0"),
                ]
                .concat(),
            ),
            record(
                b"ARMO",
                0x1801,
                0,
                &[
                    subrecord(b"EDID", b"FemaleOnly\0"),
                    subrecord(b"MOD4", b"female-world.nif\0"),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    let textures = &result.records[&0x0100_1800];
    assert_eq!(
        field(textures, b"TX02").name,
        "environment_mask_subsurface_tint"
    );
    assert_eq!(field(textures, b"TX03").name, "glow_detail");
    assert_eq!(field(textures, b"TX04").name, "height");
    assert_eq!(field(textures, b"TX05").name, "environment");
    assert_eq!(field(textures, b"TX06").name, "multilayer");
    assert_eq!(field(textures, b"TX07").name, "backlight_mask_specular");
    assert_eq!(
        field(&result.records[&0x0100_1801], b"MOD4").value,
        Value::String("female-world.nif".into())
    );
    assert_eq!(result.diagnostics["assets.esp"].unexpected_subrecords, 0);
}

/// REFR explicitly allows unordered fields; both physical orders retain radius and parent.
#[test]
fn reference_radius_and_parent_survive_both_permitted_physical_orders() {
    let mut fixture = Fixture::new();
    let mut parent = 0x12_u32.to_le_bytes().to_vec();
    parent.extend([1, 0xa1, 0xb2, 0xc3]);
    let name = subrecord(b"NAME", &0x0100_1400_u32.to_le_bytes());
    let radius = subrecord(b"XRDS", &241.75_f32.to_le_bytes());
    let enable = subrecord(b"XESP", &parent);
    let position = subrecord(b"DATA", &transform());
    fixture.add(
        "Radius.esp",
        &["Skyrim.esm"],
        0,
        [
            record(
                b"LIGH",
                0x0100_1400,
                0,
                &[
                    subrecord(b"EDID", b"RadiusBase\0"),
                    subrecord(b"DATA", &[0; 48]),
                ]
                .concat(),
            ),
            record(
                b"REFR",
                0x0100_1800,
                0,
                &[
                    name.clone(),
                    radius.clone(),
                    enable.clone(),
                    position.clone(),
                ]
                .concat(),
            ),
            // The pinned AllowUnordered flag permits this other physical ordering.
            record(
                b"REFR",
                0x0100_1801,
                0,
                &[name, position, radius, enable].concat(),
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    let valid = &result.records[&0x0100_1800];
    assert_eq!(field(valid, b"XRDS").value, Value::Float(241.75));
    assert_eq!(
        field(valid, b"XRDS").canonical_bytes,
        241.75_f32.to_le_bytes()
    );
    assert_eq!(
        member(&field(valid, b"XESP").value, "reference"),
        &Value::FormId(0x12)
    );
    assert_eq!(field(valid, b"DATA").canonical_bytes, transform());
    let late = &result.records[&0x0100_1801];
    assert_eq!(field(late, b"XRDS").value, Value::Float(241.75));
    assert_eq!(
        member(&field(late, b"XESP").value, "reference"),
        &Value::FormId(0x12)
    );
    assert_eq!(
        member(&field(late, b"XESP").value, "flags"),
        &Value::Unsigned(1)
    );
    assert_eq!(field(late, b"DATA").canonical_bytes, transform());
    assert_eq!(result.diagnostics["radius.esp"].unexpected_subrecords, 0);
}

/// Independently frame primary scripts with nonzero object-format padding and alias bytes.
fn primary_vmad() -> Vec<u8> {
    let mut bytes = [
        5_i16.to_le_bytes(),
        2_i16.to_le_bytes(),
        1_u16.to_le_bytes(),
    ]
    .concat();
    bytes.extend(11_u16.to_le_bytes());
    bytes.extend(b"Independent");
    bytes.push(0);
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(6_u16.to_le_bytes());
    bytes.extend(b"Scalar");
    bytes.extend([1, 1]);
    // Format2: two unused bytes, u16 alias, then the four-byte FormID.
    bytes.extend([0xa1, 0xb2]);
    bytes.extend(0x3456_u16.to_le_bytes());
    bytes.extend(0x1400_u32.to_le_bytes());
    bytes.extend(4_u16.to_le_bytes());
    bytes.extend(b"Many");
    bytes.extend([11, 1]);
    bytes.extend(1_u32.to_le_bytes());
    bytes.extend([0xc3, 0xd4]);
    bytes.extend(0x789a_u16.to_le_bytes());
    bytes.extend(0x1400_u32.to_le_bytes());
    bytes.extend(b"opaque-fragment-tail");
    bytes
}

/// Primary script links use the shared master list even on otherwise opaque record types.
#[test]
fn primary_vmad_links_remap_with_divergent_slots_and_preserve_opaque_tail() {
    let mut fixture = Fixture::new();
    fixture.add("Unused.esm", &[], 1, vec![]);
    fixture.add("Owner.esm", &[], 1, static_record(0x1400, "ScriptObject"));
    let vmad = primary_vmad();
    let mut dangling_vmad = vmad.clone();
    dangling_vmad[36..40].copy_from_slice(&0x1fff_u32.to_le_bytes());
    fixture.add(
        "Scripts.esp",
        &["Owner.esm"],
        0,
        [
            record(
                b"ACTI",
                0x0100_1800,
                0,
                &[
                    subrecord(b"EDID", b"TypedScriptBase\0"),
                    subrecord(b"VMAD", &vmad),
                    subrecord(b"MODL", b"scripted.nif\0"),
                ]
                .concat(),
            ),
            record(
                b"PACK",
                0x0100_1801,
                0,
                &[
                    subrecord(b"EDID", b"OpaqueScriptOwner\0"),
                    subrecord(b"VMAD", &vmad),
                ]
                .concat(),
            ),
            record(
                b"PACK",
                0x0100_1802,
                0,
                &[
                    subrecord(b"EDID", b"BrokenPrimaryScript\0"),
                    subrecord(b"VMAD", &[5, 0, 2]),
                ]
                .concat(),
            ),
            static_record(0x0100_1803, "Neighbor"),
            record(
                b"PACK",
                0x0100_1804,
                0,
                &[
                    subrecord(b"EDID", b"DanglingScriptObject\0"),
                    subrecord(b"VMAD", &dangling_vmad),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let (order, result) = fixture.read();
    assert_eq!(order.normal["owner.esm"], 2);
    assert_eq!(order.normal["scripts.esp"], 3);
    for id in [0x0300_1800, 0x0300_1801] {
        let owner = &result.records[&id];
        let canonical = &field(owner, b"VMAD").canonical_bytes;
        assert_eq!(word(canonical, 36), 0x0200_1400);
        assert_eq!(word(canonical, 56), 0x0200_1400);
        assert_eq!(&canonical[32..36], &[0xa1, 0xb2, 0x56, 0x34]);
        assert_eq!(&canonical[52..56], &[0xc3, 0xd4, 0x9a, 0x78]);
        assert_eq!(&canonical[60..], b"opaque-fragment-tail");
        assert!(
            owner
                .raw_payload
                .windows(vmad.len())
                .any(|bytes| bytes == vmad)
        );
    }
    assert!(result.records[&0x0300_1800].supported);
    assert!(!result.records[&0x0300_1801].supported);
    let broken = &result.records[&0x0300_1802];
    assert!(
        !broken
            .fields
            .iter()
            .any(|field| field.signature == *b"VMAD")
    );
    assert!(
        broken
            .fields
            .iter()
            .any(|field| field.signature == *b"EDID")
    );
    assert!(result.records.contains_key(&0x0300_1803));
    assert_eq!(result.diagnostics["scripts.esp"].skipped_fields, 1);
    let dangling = &result.records[&0x0300_1804];
    let canonical = &field(dangling, b"VMAD").canonical_bytes;
    assert_eq!(word(canonical, 36), 0);
    assert_eq!(word(canonical, 56), 0x0200_1400);
    assert!(
        dangling
            .raw_payload
            .windows(dangling_vmad.len())
            .any(|bytes| bytes == dangling_vmad)
    );
    assert_eq!(word(&dangling_vmad, 36), 0x1fff);
    assert_eq!(result.diagnostics["scripts.esp"].invalid_links, 1);
    assert_eq!(result.diagnostics["scripts.esp"].skipped_records, 0);
}

/// XPWR's optional tail admits a reference-only prefix and a full flag word.
#[test]
fn reflected_water_accepts_legacy_prefix_and_preserves_extended_flags() {
    let mut fixture = Fixture::new();
    fixture.add("Unused.esm", &[], 1, vec![]);
    fixture.add(
        "Owner.esm",
        &[],
        1,
        [
            static_record(0x1400, "ReflectionBase"),
            reference_record(0x1401, 0x1400),
        ]
        .concat(),
    );
    let legacy = 0x1401_u32.to_le_bytes().to_vec();
    let extended = [0x1401_u32.to_le_bytes(), 0xa5b6_c703_u32.to_le_bytes()].concat();
    fixture.add(
        "Reflection.esp",
        &["Owner.esm"],
        0,
        record(
            b"REFR",
            0x0100_1800,
            0,
            &[
                subrecord(b"NAME", &0x1400_u32.to_le_bytes()),
                subrecord(b"XPWR", &legacy),
                subrecord(b"XPWR", &extended),
                subrecord(b"DATA", &transform()),
            ]
            .concat(),
        ),
    );
    let (_, result) = fixture.read();
    let object = &result.records[&0x0300_1800];
    let water: Vec<_> = object
        .fields
        .iter()
        .filter(|field| field.signature == *b"XPWR")
        .collect();
    assert_eq!(water.len(), 2);
    assert_eq!(
        member(&water[0].value, "reference"),
        &Value::FormId(0x0200_1401)
    );
    assert_eq!(water[0].canonical_bytes, 0x0200_1401_u32.to_le_bytes());
    let Value::Struct(legacy_members) = &water[0].value else {
        panic!("missing legacy XPWR");
    };
    assert_eq!(
        legacy_members.len(),
        1,
        "an absent tail must not invent flags"
    );
    assert_eq!(
        member(&water[1].value, "reference"),
        &Value::FormId(0x0200_1401)
    );
    assert_eq!(
        member(&water[1].value, "flags"),
        &Value::Unsigned(0xa5b6_c703)
    );
    assert_eq!(
        water[1].canonical_bytes,
        [0x0200_1401_u32.to_le_bytes(), 0xa5b6_c703_u32.to_le_bytes()].concat()
    );
    assert_eq!(result.diagnostics["reflection.esp"].skipped_fields, 0);
    assert_eq!(result.diagnostics["reflection.esp"].invalid_links, 0);
}
