//! Differential controls: the frozen old record remapper pins equivalence and policy differences.
use super::{
    RawRecord, RemapWarnings, binary::PluginMetadata, load_order::LoadOrder, remap_record_form_ids,
};
use shared::{
    form_key::FormKey,
    legacy_id_view::LegacyIdView,
    plugin_table::{PluginFacts, PluginFlags, PluginTable},
    source_form_id::{Resolution, SourceFormId, ZeroPolicy, resolve},
};

/// Compare the frozen permissive header remapper with the new pure view.
fn check(
    order: &LoadOrder,
    table: &PluginTable,
    supplier: usize,
    raw: u32,
    packed: Option<u64>,
    expected: u32,
) {
    let ids: Vec<_> = order
        .names
        .iter()
        .map(|name| table.id(name).unwrap())
        .collect();
    let resolution = resolve(
        table,
        SourceFormId {
            raw,
            containing_plugin: ids[supplier],
        },
        ZeroPolicy::Null,
    );
    assert_eq!(resolution.key().map(FormKey::to_packed), packed);
    let view = LegacyIdView::from_load_order(table, &ids).unwrap();
    let actual = resolution
        .key()
        .map_or(Ok(0), |key| view.to_legacy_u32(key));
    let mut record = RawRecord {
        form_id: raw,
        record_type: *b"STAT",
        flags: 0,
        subrecords: Vec::new(),
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };
    remap_record_form_ids(
        &mut record,
        &order.names[supplier],
        &order.metadata[supplier].masters,
        &order.normal,
        &order.light,
        &RemapWarnings::default(),
    )
    .unwrap();
    assert_eq!(actual, Ok(expected));
    assert_eq!(record.form_id, expected);
    assert_eq!(actual, Ok(record.form_id));
    assert_eq!(view.from_legacy_u32(expected), Ok(resolution.key()));
    if let Some(key) = resolution.key() {
        // The existing reverse mapping independently checks the decoded identity.
        let decoded = view.from_legacy_u32(expected).unwrap().unwrap();
        let stable = order.identity(expected).unwrap();
        let named = table.named(decoded).unwrap();
        assert_eq!(
            named.parts(),
            Some((stable.plugin.as_str(), stable.local_id))
        );
        assert_eq!(
            view.to_legacy_u32(view.from_legacy_u32(expected).unwrap().unwrap()),
            Ok(expected)
        );
        assert_eq!(
            view.from_legacy_u32(view.to_legacy_u32(key).unwrap()),
            Ok(Some(key))
        );
    }
}

#[test]
/// Nonidentity MAST, light overrides, shared locals, boundaries and recovery match old packing.
fn form_key_differential_nonidentity_masters() {
    let names: Vec<String> = ["A.esm", "B.esm", "Light.esp", "Patch.esp"]
        .map(str::to_owned)
        .to_vec();
    let metadata = vec![
        PluginMetadata::default(),
        PluginMetadata::default(),
        PluginMetadata {
            flags: 0x200,
            masters: vec!["A.esm".into()],
        },
        PluginMetadata {
            flags: 0,
            masters: vec!["B.esm".into(), "Light.esp".into(), "A.esm".into()],
        },
    ];
    let table = PluginTable::fresh(
        names
            .iter()
            .zip(&metadata)
            .map(|(name, facts)| {
                (
                    name.clone(),
                    PluginFacts {
                        masters: facts.masters.clone(),
                        flags: PluginFlags {
                            master: facts.flags & 1 != 0,
                            light: facts.flags & 0x200 != 0,
                            medium: false,
                        },
                        header_version: Some(1.71),
                        ..Default::default()
                    },
                )
            })
            .collect(),
    )
    .unwrap();
    let order = LoadOrder::from_metadata(names, metadata).unwrap();
    for (raw, packed, runtime) in [
        (0x0000_0800, Some(0x0200_0800), 0x0100_0800),
        (0x0200_0800, Some(0x0100_0800), 0x0000_0800),
        (0x0100_0fff, Some(0x0300_0fff), 0xfe00_0fff),
        (0x0300_0800, Some(0x0400_0800), 0x0200_0800),
        (0x02ff_ffff, Some(0x01ff_ffff), 0x00ff_ffff),
        (0x0500_0123, Some(0x0400_0123), 0x0200_0123),
        (0, None, 0),
    ] {
        check(&order, &table, 3, raw, packed, runtime);
    }
    check(
        &order,
        &table,
        2,
        0x00ff_ffff,
        Some(0x01ff_ffff),
        0x00ff_ffff,
    );
    check(
        &order,
        &table,
        2,
        0x0100_0800,
        Some(0x0300_0800),
        0xfe00_0800,
    );
    assert!(matches!(
        resolve(
            &table,
            SourceFormId {
                containing_plugin: table.id("Patch.esp").unwrap(),
                raw: 0x0100_1000
            },
            ZeroPolicy::Null
        ),
        Resolution::InvalidWidth { .. }
    ));
    // Malformed wide light locals are deliberately outside differential equivalence:
    // today's permissive STAT path truncates; this contract rejects instead of aliasing.
    let mut malformed = RawRecord {
        form_id: 0x0100_1000,
        record_type: *b"STAT",
        flags: 0,
        subrecords: Vec::new(),
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };
    let warnings = RemapWarnings::default();
    remap_record_form_ids(
        &mut malformed,
        "Light.esp",
        &order.metadata[2].masters,
        &order.normal,
        &order.light,
        &warnings,
    )
    .unwrap();
    assert_eq!(malformed.form_id, 0xfe00_0000);
    malformed.form_id = 0x0100_1000;
    malformed.record_type = *b"GRAS";
    assert!(
        remap_record_form_ids(
            &mut malformed,
            "Light.esp",
            &order.metadata[2].masters,
            &order.normal,
            &order.light,
            &warnings
        )
        .is_err()
    );
    malformed.form_id = 0x0100_0800;
    malformed.record_type = *b"REFR";
    malformed.subrecords = vec![
        (
            b"XESP".to_vec(),
            [0x0500_0800u32.to_le_bytes(), [1, 2, 3, 4]].concat(),
        ),
        (b"XEMI".to_vec(), 0x0000_0801u32.to_le_bytes().to_vec()),
    ];
    remap_record_form_ids(
        &mut malformed,
        "Light.esp",
        &order.metadata[2].masters,
        &order.normal,
        &order.light,
        &warnings,
    )
    .unwrap();
    assert_eq!(
        malformed.subrecords,
        vec![
            (
                b"XESP".to_vec(),
                [0u32.to_le_bytes(), [1, 2, 3, 4]].concat()
            ),
            (b"XEMI".to_vec(), 0x0000_0801u32.to_le_bytes().to_vec())
        ]
    );
    assert_eq!(warnings.skipped_optional.borrow()["Light.esp"].0, 1);
}

/// Generate a tiny real native-format TES4 header, with no proprietary fixture data.
fn native_header(light: bool) -> Vec<u8> {
    let hedr = [
        1.71f32.to_le_bytes(),
        0u32.to_le_bytes(),
        0x800u32.to_le_bytes(),
    ]
    .concat();
    let payload = [b"HEDR".as_slice(), &12u16.to_le_bytes(), &hedr].concat();
    [
        b"TES4".as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &(if light { 0x200u32 } else { 1u32 }).to_le_bytes(),
        &[0; 12],
        &payload,
    ]
    .concat()
}

#[test]
/// Actual player/CCC selection fixes official slots and preserves nonalphabetical CC order.
fn form_key_differential_official_and_ccc_order() {
    let root = tempfile::tempdir().unwrap();
    let names = [
        "Skyrim.esm",
        "Update.esm",
        "Dawnguard.esm",
        "HearthFires.esm",
        "Dragonborn.esm",
        "ccZ-Full.esm",
        "ccA-Full.esm",
        "ccZ-Light.esl",
        "ccA-Light.esl",
    ];
    for name in names {
        std::fs::write(
            root.path().join(name),
            native_header(name.ends_with(".esl")),
        )
        .unwrap();
    }
    let ccc = root.path().join("Skyrim.ccc");
    std::fs::write(
        &ccc,
        "ccZ-Full.esm\nccZ-Light.esl\nccA-Full.esm\nccA-Light.esl\n",
    )
    .unwrap();
    let list = root.path().join("plugins.txt");
    std::fs::write(&list, "# official and CCC inputs are implicit\n").unwrap();
    let paths =
        super::plugins_txt::read_plugins_txt_with_options(&list, root.path(), Some(&ccc), None)
            .unwrap();
    let expected = [
        "skyrim.esm",
        "update.esm",
        "dawnguard.esm",
        "hearthfires.esm",
        "dragonborn.esm",
        "ccz-full.esm",
        "ccz-light.esl",
        "cca-full.esm",
        "cca-light.esl",
    ];
    assert_eq!(
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>(),
        expected
    );
    let order = LoadOrder::read(&paths).unwrap();
    let table = PluginTable::fresh(
        names
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    PluginFacts {
                        header_version: Some(1.71),
                        ..Default::default()
                    },
                )
            })
            .collect(),
    )
    .unwrap();
    // Sorted table ids differ from official/load slots: Skyrim=8, Update=9, Dawnguard=5.
    for (supplier, packed, runtime) in [
        (0, 0x0800_0800, 0x0000_0800),
        (1, 0x0900_0800, 0x0100_0800),
        (2, 0x0500_0800, 0x0200_0800),
        (3, 0x0700_0800, 0x0300_0800),
        (4, 0x0600_0800, 0x0400_0800),
        (5, 0x0300_0800, 0x0500_0800),
        (6, 0x0400_0800, 0xfe00_0800),
        (7, 0x0100_0800, 0x0600_0800),
        (8, 0x0200_0800, 0xfe00_1800),
    ] {
        check(&order, &table, supplier, 0x800, Some(packed), runtime);
    }
}

/// Frame synthetic bytes independently of either remapping implementation.
fn oracle_field(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(data.len() as u16).to_le_bytes(), data].concat()
}

fn oracle_record(tag: &[u8; 4], id: u32, body: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(body.len() as u32).to_le_bytes(),
        &0u32.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        body,
    ]
    .concat()
}

/// Three full owners: Patch's MAST[0] is Base, while self packs into slot 2.
/// The old side calls the frozen real record remapper, never the new resolver.
fn record_oracle_fixture() -> (
    tempfile::TempDir,
    Vec<std::path::PathBuf>,
    std::collections::HashMap<u32, RawRecord>,
) {
    let dir = tempfile::tempdir().unwrap();
    let body = [
        // Strict-header difference, separate from the absent optional-link NULL control.
        oracle_record(b"CELL", 0x0200_0810, &[]),
        oracle_record(
            b"CELL",
            0x0100_0800,
            &[
                oracle_field(b"XOWN", &0x0200_0900u32.to_le_bytes()),
                oracle_field(b"XCWT", &0x0200_0901u32.to_le_bytes()),
            ]
            .concat(),
        ),
        oracle_record(
            b"REFR",
            0x0100_0802,
            &oracle_field(b"NAME", &0x0200_0904u32.to_le_bytes()),
        ),
        // Ordinary equivalence: in-range master, normal self and permissive past-MAST header.
        oracle_record(b"STAT", 0x0000_0803, &oracle_field(b"EDID", b"Master\0")),
        oracle_record(b"STAT", 0x0100_0804, &oracle_field(b"EDID", b"Self\0")),
        oracle_record(b"STAT", 0x0700_0805, &oracle_field(b"EDID", b"Projected\0")),
        oracle_record(
            b"REFR",
            0x0100_0806,
            &oracle_field(b"NAME", &0x0000_0803u32.to_le_bytes()),
        ),
    ]
    .concat();
    let mut paths = Vec::new();
    for name in ["Base.esm", "Filler.esm", "Patch.esp"] {
        let mut header = oracle_field(
            b"HEDR",
            &[1.71f32.to_le_bytes(), [0; 4], 0x800u32.to_le_bytes()].concat(),
        );
        if name == "Patch.esp" {
            header.extend(oracle_field(b"MAST", b"Base.esm\0"));
            header.extend(oracle_field(b"DATA", &[0; 8]));
        }
        let path = dir.path().join(name);
        let mut bytes = oracle_record(b"TES4", 0, &header);
        if name == "Patch.esp" {
            bytes.extend_from_slice(&body);
        }
        std::fs::write(&path, bytes).unwrap();
        paths.push(path);
    }
    let order = LoadOrder::read(&paths).unwrap();
    let mut old = std::collections::HashMap::new();
    for mut record in super::binary::parse_plugin_file(&paths[2]).unwrap() {
        record.load_order = 2;
        if remap_record_form_ids(
            &mut record,
            &order.names[2],
            &order.metadata[2].masters,
            &order.normal,
            &order.light,
            &RemapWarnings::default(),
        )
        .is_ok()
        {
            old.insert(record.form_id, record);
        }
    }
    (dir, paths, old)
}

/// Read the one independent four-byte literal in an oracle record field.
fn oracle_word(record: &RawRecord, tag: &[u8; 4]) -> u32 {
    let data = &record
        .subrecords
        .iter()
        .find(|(t, _)| t.as_slice() == tag)
        .unwrap()
        .1;
    u32::from_le_bytes(data.as_slice().try_into().unwrap())
}

/// Compare production publication with the retained old record remapper.
#[test]
fn real_old_record_oracle_names_strict_header_difference() {
    let (_dir, paths, old) = record_oracle_fixture();
    let new = super::EsmParser::merge_plugins(&paths).unwrap();
    assert!(!old.contains_key(&0x0200_0810));
    assert_eq!(new[&0x0200_0810].record_type, *b"CELL");
}

/// Compare production publication with the retained old record remapper.
#[test]
fn real_old_record_oracle_absent_optional_links_agree_on_null() {
    let (_dir, paths, old) = record_oracle_fixture();
    let new = super::EsmParser::merge_plugins(&paths).unwrap();
    for tag in [b"XOWN", b"XCWT"] {
        assert_eq!(oracle_word(&old[&0x0200_0800], tag), 0);
        assert_eq!(oracle_word(&new[&0x0200_0800], tag), 0);
    }
}

/// Compare production publication with the retained old record remapper.
#[test]
fn real_old_record_oracle_names_required_refr_name_difference() {
    let (_dir, paths, old) = record_oracle_fixture();
    let new = super::EsmParser::merge_plugins(&paths).unwrap();
    assert!(!old.contains_key(&0x0200_0802));
    assert_eq!(oracle_word(&new[&0x0200_0802], b"NAME"), 0);
}

/// Compare production publication with the retained old record remapper.
#[test]
fn real_old_record_oracle_preserves_ordinary_equivalence() {
    let (_dir, paths, old) = record_oracle_fixture();
    let new = super::EsmParser::merge_plugins(&paths).unwrap();
    assert_eq!(old.len(), 5);
    assert_eq!(new.len(), 7);
    for id in [0x0000_0803, 0x0200_0804, 0x0200_0805, 0x0200_0806] {
        let (old, new) = (&old[&id], &new[&id]);
        assert_eq!(old.form_id, id);
        assert_eq!(new.form_id, id);
        assert_eq!(old.record_type, new.record_type);
        assert_eq!(old.flags, new.flags);
        assert_eq!(old.subrecords, new.subrecords);
        assert_eq!(old.cell_form_id, new.cell_form_id);
        assert_eq!(old.worldspace_form_id, new.worldspace_form_id);
        assert_eq!(old.load_order, new.load_order);
    }
    assert_eq!(oracle_word(&new[&0x0200_0806], b"NAME"), 0x0000_0803);
}

/// Compare production publication with the retained old record remapper.
#[test]
fn real_old_record_oracle_sol3_publication_clears_absent_links_and_keeps_reference() {
    use rusqlite::Connection;
    let (dir, paths, old) = record_oracle_fixture();
    let old_db = Connection::open_in_memory().unwrap();
    super::exporter::create_tables(&old_db).unwrap();
    super::exporter::export_to_db(&old_db, &old).unwrap();
    let new_path = dir.path().join("new.db");
    super::EsmParser::convert_plugins(&paths, &new_path).unwrap();
    let new_db = Connection::open(new_path).unwrap();
    let published = |conn: &Connection, id: u32| {
        let blob: Vec<u8> = conn
            .query_row("SELECT data FROM records WHERE form_id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        rkyv::from_bytes::<super::types::ArchivedRecordData, rkyv::rancor::Error>(&blob).unwrap()
    };
    let old_cell = published(&old_db, 0x0200_0800);
    let new_cell = published(&new_db, 0x0200_0800);
    for tag in [b"XOWN", b"XCWT"] {
        let word = |record: &super::types::ArchivedRecordData| {
            let field = record.subrecords.iter().find(|f| &f.tag == tag).unwrap();
            u32::from_le_bytes(field.data.as_slice().try_into().unwrap())
        };
        assert_eq!(word(&old_cell), 0);
        assert_eq!(word(&new_cell), 0);
    }
    let old_count: u32 = old_db
        .query_row(
            "SELECT COUNT(*) FROM records WHERE form_id=?1",
            [0x0200_0802u32],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_count, 0);
    let reference = published(&new_db, 0x0200_0802);
    let name = reference
        .subrecords
        .iter()
        .find(|f| f.tag == *b"NAME")
        .unwrap();
    assert_eq!(name.data, 0u32.to_le_bytes());
}

/// Handwritten remaps emit one counted projection notice rather than two summaries.
#[test]
fn handwritten_projection_notice_is_not_duplicated() {
    let (_dir, paths, _old) = record_oracle_fixture();
    let order = LoadOrder::read(&paths).unwrap();
    let conversion = super::EsmParser::merge_plugins_with_load_order(&paths, &order).unwrap();
    let projected: Vec<_> = conversion
        .notices
        .iter()
        .filter(|line| line.contains("master list") || line.contains("out-of-range master"))
        .collect();
    assert_eq!(projected.len(), 1, "{projected:?}");
    assert!(projected[0].starts_with("notice:"), "{projected:?}");
    assert!(projected[0].contains(" in CELL"), "{projected:?}");
}

/// Build the absent-target fixture inside the test, retaining a valid neighbouring record.
fn absent_optional_fixture() -> (tempfile::TempDir, Vec<std::path::PathBuf>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Base.esm");
    std::fs::write(
        &path,
        [
            oracle_record(b"TES4", 0, &[]),
            oracle_record(
                b"CELL",
                0x811,
                &[
                    oracle_field(b"DATA", &[1, 0]),
                    oracle_field(b"XCWT", &0x0200_1234u32.to_le_bytes()),
                ]
                .concat(),
            ),
            oracle_record(b"STAT", 0x812, &oracle_field(b"EDID", b"Neighbor\0")),
        ]
        .concat(),
    )
    .unwrap();
    (dir, vec![path])
}

#[test]
fn handwritten_absent_self_link_retains_diagnostic_and_neighbor() {
    let (_dir, paths) = absent_optional_fixture();
    let order = LoadOrder::read(&paths).unwrap();
    let result = super::EsmParser::merge_plugins_with_load_order(&paths, &order).unwrap();
    assert_eq!(oracle_word(&result.records[&0x811], b"XCWT"), 0);
    assert!(result.records.contains_key(&0x812));
    assert!(
        result
            .warnings
            .iter()
            .any(|line| line.contains("optional FormID 02001234 exceeds master list")),
        "{result:?}"
    );
    assert!(
        result.notices.is_empty(),
        "cleared fields have no self notice: {result:?}"
    );
}

#[test]
fn typed_cleared_self_link_does_not_count_as_resolved() {
    let (_dir, paths) = absent_optional_fixture();
    let order = LoadOrder::read(&paths).unwrap();
    let result = crate::records::read_plugins(&paths, &order).unwrap();
    assert!(result.records.contains_key(&0x812));
    assert!(
        order
            .form_key_context()
            .unwrap()
            .projection_warnings()
            .is_empty(),
        "a cleared link is not a self publication"
    );
}

/// Typed publication also filters explicit filenames before both decode and plugin export.
#[test]
fn typed_publication_skips_invalid_name_before_identity_construction() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for (name, local) in [
        ("Before.esp", 0x811u32),
        (" Invalid.esp", 0x822),
        ("Dependent.esp", 0x0100_0844),
        ("Transitive.esp", 0x0100_0855),
        ("After.esp", 0x833),
    ] {
        let mut header = oracle_field(
            b"HEDR",
            &[1.71f32.to_le_bytes(), [0; 4], 0x800u32.to_le_bytes()].concat(),
        );
        let master = match name {
            "Dependent.esp" => Some(" Invalid.esp"),
            "Transitive.esp" => Some("Dependent.esp"),
            _ => None,
        };
        if let Some(master) = master {
            header.extend(oracle_field(b"MAST", format!("{master}\0").as_bytes()));
            header.extend(oracle_field(b"DATA", &[0; 8]));
        }
        let path = dir.path().join(name);
        std::fs::write(
            &path,
            [
                oracle_record(b"TES4", 0, &header),
                oracle_record(b"STAT", local, &oracle_field(b"EDID", b"valid\0")),
            ]
            .concat(),
        )
        .unwrap();
        paths.push(path);
    }
    let db_path = dir.path().join("world.db");
    let conversion = super::inhouse::convert_plugins_with_string_sources_report(
        &paths,
        &db_path,
        super::inhouse::StringSources::default_language(
            super::inhouse::StringRoots::PluginLooseFirst(dir.path()),
        ),
        None,
    );
    assert!(
        conversion.is_ok(),
        "skip an invalid explicit path: {:?}",
        conversion.as_ref().err()
    );
    let conversion = conversion.unwrap();
    assert!(
        conversion
            .warnings
            .iter()
            .any(|line| line.contains("invalid plugin name")
                && line.contains("skipped 2 dependent plugins"))
    );
    let conn = rusqlite::Connection::open(db_path).unwrap();
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
}

/// Every special field clears locally and reports the same bounded absent-target diagnostic.
#[test]
fn handwritten_special_layouts_clear_and_warn_without_self_notices() {
    let mut vmad = [
        5u16.to_le_bytes(),
        2u16.to_le_bytes(),
        1u16.to_le_bytes(),
        1u16.to_le_bytes(),
    ]
    .concat();
    vmad.extend(b"S");
    vmad.extend([0, 1, 0, 1, 0, b'P', 1, 1, 0xa1, 0xb2, 0x56, 0x34]);
    vmad.extend(0x0200_1234u32.to_le_bytes());
    let mods = [
        1u32.to_le_bytes().as_slice(),
        &1u32.to_le_bytes(),
        b"S",
        &0x0200_1234u32.to_le_bytes(),
        &17u32.to_le_bytes(),
    ]
    .concat();
    for (kind, tag, payload) in [
        (b"ACTI", b"VMAD", vmad),
        (b"STAT", b"MODS", mods.clone()),
        (b"GRAS", b"MODS", mods),
        (
            b"LAND",
            b"BTXT",
            [0x0200_1234u32.to_le_bytes(), [2, 0, 0, 0]].concat(),
        ),
        (
            b"LAND",
            b"ATXT",
            [0x0200_1234u32.to_le_bytes(), [2, 0, 3, 0]].concat(),
        ),
        (b"LAND", b"VTEX", 0x0200_1234u32.to_le_bytes().to_vec()),
        (b"REFR", b"NAME", 0x0200_1234u32.to_le_bytes().to_vec()),
        (b"NPC_", b"DOFT", 0x0200_1234u32.to_le_bytes().to_vec()),
        (b"OTFT", b"INAM", 0x0200_1234u32.to_le_bytes().to_vec()),
        (b"ARMA", b"NAM0", 0x0200_1234u32.to_le_bytes().to_vec()),
        (b"ARMA", b"NAM1", 0x0200_1234u32.to_le_bytes().to_vec()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Base.esm");
        let mut fields = oracle_field(b"EDID", b"Special\0");
        if kind == b"GRAS" {
            fields.extend(oracle_field(b"DATA", &[0; 32]));
        }
        if tag == b"ATXT" {
            fields.extend(oracle_field(b"BTXT", &[0, 0, 0, 0, 2, 0, 0, 0]));
        }
        fields.extend(oracle_field(tag, &payload));
        std::fs::write(
            &path,
            [
                oracle_record(b"TES4", 0, &[]),
                oracle_record(kind, 0x811, &fields),
                oracle_record(b"STAT", 0x812, &oracle_field(b"EDID", b"Neighbor\0")),
            ]
            .concat(),
        )
        .unwrap();
        let paths = [path];
        let order = LoadOrder::read(&paths).unwrap();
        let result = super::EsmParser::merge_plugins_with_load_order(&paths, &order).unwrap();
        assert!(
            result.records.contains_key(&0x811),
            "{kind:?}/{tag:?}: {result:?}"
        );
        assert!(result.records.contains_key(&0x812));
        assert_eq!(result.warnings.len(), 1, "{kind:?}/{tag:?}: {result:?}");
        assert!(result.warnings[0].contains("optional FormID 02001234 exceeds master list"));
        assert!(
            result.notices.is_empty(),
            "cleared fields do not count as self: {result:?}"
        );
    }
}
