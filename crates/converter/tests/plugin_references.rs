//! Synthetic plugins only: no game data or copied assets.
use converter::esm::{EsmParser, extractors::SubrecordView, load_order::LoadOrder};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}
fn group(kind: i32, id: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &(payload.len() as u32 + 24).to_le_bytes(),
        &id.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}
fn plugin(root: &Path, name: &str, masters: &[&str], flags: u32, records: Vec<u8>) -> PathBuf {
    let mut header = Vec::new();
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let path = root.join(name);
    fs::write(&path, [record(b"TES4", 0, flags, header), records].concat()).unwrap();
    path
}
fn grass_data() -> Vec<u8> {
    let mut data = vec![0; 32];
    data[0..3].copy_from_slice(&[35, 10, 65]);
    data[4..6].copy_from_slice(&450u16.to_le_bytes());
    data[8..12].copy_from_slice(&6u32.to_le_bytes());
    for (offset, value) in [(12, 12.5f32), (16, 0.4), (20, 0.2), (24, 1.5)] {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    data[28] = 7;
    data
}
fn grass(id: u32, density: u8) -> Vec<u8> {
    let mut data = grass_data();
    data[0] = density;
    record(
        b"GRAS",
        id,
        0,
        [
            sub(b"EDID", b"SyntheticGrass\0"),
            sub(b"MODL", b"grass/test.nif\0"),
            sub(b"DATA", &data),
        ]
        .concat(),
    )
}
fn texture_layer(id: u32, quadrant: u8, index: u16) -> Vec<u8> {
    [
        id.to_le_bytes().as_slice(),
        &[quadrant, 0],
        &index.to_le_bytes(),
    ]
    .concat()
}

#[test]
fn remaps_reordered_masters_light_plugins_terrain_and_alternate_textures() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 0, grass(0x800, 35));
    let filler = plugin(dir.path(), "Filler.esm", &[], 0, Vec::new());
    let light = plugin(
        dir.path(),
        "Light.esl",
        &["Base.esm"],
        0x200,
        grass(0x01000810, 50),
    );
    let mut mods = 1u32.to_le_bytes().to_vec();
    mods.extend(5u32.to_le_bytes());
    mods.extend(b"Shape");
    mods.extend(0x01000801u32.to_le_bytes());
    mods.extend(2u32.to_le_bytes());
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Light.esl", "Base.esm"],
        0,
        [
            record(
                b"LTEX",
                0x02000820,
                0,
                [
                    sub(b"TNAM", &0x01000801u32.to_le_bytes()),
                    sub(b"MNAM", &0x01000802u32.to_le_bytes()),
                    sub(b"GNAM", &0x00000810u32.to_le_bytes()),
                    sub(b"GNAM", &0x01000800u32.to_le_bytes()),
                ]
                .concat(),
            ),
            record(
                b"GRAS",
                0x01000800,
                0,
                [sub(b"DATA", &grass_data()), sub(b"MODS", &mods)].concat(),
            ),
            group(
                1,
                0x01000900,
                group(
                    6,
                    0x01000901,
                    record(
                        b"LAND",
                        0x02000830,
                        0,
                        [
                            sub(b"BTXT", &texture_layer(0x02000820, 2, 0)),
                            sub(b"ATXT", &texture_layer(0x01000802, 2, 3)),
                            sub(
                                b"VTEX",
                                &[0x02000820u32.to_le_bytes(), 0u32.to_le_bytes()].concat(),
                            ),
                        ]
                        .concat(),
                    ),
                ),
            ),
        ]
        .concat(),
    );
    let paths = vec![base.clone(), filler.clone(), light.clone(), patch.clone()];
    let merged = EsmParser::merge_plugins(&paths).unwrap();
    let ltex = SubrecordView::new(&merged[&0x02000820].subrecords);
    assert_eq!(ltex.get_form_id(b"TNAM"), Some(0x801));
    assert_eq!(ltex.get_form_id(b"MNAM"), Some(0x802));
    assert_eq!(ltex.get_form_id(b"GNAM"), Some(0xfe000810));
    let land = &merged[&0x02000830];
    assert_eq!(land.cell_form_id, Some(0x901));
    assert_eq!(land.worldspace_form_id, Some(0x900));
    let view = SubrecordView::new(&land.subrecords);
    assert_eq!(view.get_form_id(b"BTXT"), Some(0x02000820));
    assert_eq!(view.get_form_id(b"ATXT"), Some(0x802));
    assert_eq!(view.find(b"ATXT").unwrap()[4..], [2, 0, 3, 0]);
    assert_eq!(view.find(b"VTEX").unwrap()[4..], [0; 4]);
    let override_record = &merged[&0x800];
    assert_eq!(override_record.load_order, 3);
    assert_eq!(
        SubrecordView::new(&override_record.subrecords).get_alternate_textures(b"MODS")[0]
            .texture_form_id,
        0x801
    );
    let id = LoadOrder::read(&paths).unwrap().identity(0x800).unwrap();
    assert_eq!(id.plugin, "base.esm");
    assert_eq!(id.local_id, 0x800);
    let changed = vec![filler, base, light, patch];
    assert_eq!(
        LoadOrder::read(&changed)
            .unwrap()
            .identity(0x01000800)
            .unwrap(),
        id
    );
    assert!(
        EsmParser::merge_plugins(&changed)
            .unwrap()
            .contains_key(&0x01000800)
    );
}

#[test]
fn remaps_cell_and_world_water_references_and_preserves_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let filler = plugin(dir.path(), "Filler.esm", &[], 0, Vec::new());
    let base = plugin(dir.path(), "Base.esm", &[], 0, Vec::new());
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Base.esm"],
        0,
        [
            record(
                b"CELL",
                0x01000800,
                0,
                sub(b"XCWT", &0x900u32.to_le_bytes()),
            ),
            record(
                b"WRLD",
                0x01000801,
                0,
                [
                    sub(b"NAM2", &0x901u32.to_le_bytes()),
                    sub(b"NAM3", &0u32.to_le_bytes()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let merged = EsmParser::merge_plugins(&[filler, base, patch]).unwrap();
    assert_eq!(
        SubrecordView::new(&merged[&0x02000800].subrecords).get_form_id(b"XCWT"),
        Some(0x01000900)
    );
    let world = SubrecordView::new(&merged[&0x02000801].subrecords);
    assert_eq!(world.get_form_id(b"NAM2"), Some(0x01000901));
    assert_eq!(world.get_form_id(b"NAM3"), Some(0));
}

#[test]
fn rejects_malformed_land_and_alternate_texture_references() {
    for (kind, payload) in [
        (b"LAND", sub(b"BTXT", &[0; 7])),
        (b"LAND", sub(b"ATXT", &[0; 9])),
        (b"LAND", sub(b"VTEX", &[0; 3])),
        (b"GRAS", sub(b"MODS", &1u32.to_le_bytes())),
        (b"GRAS", sub(b"MODS", &[0; 5])),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(
            dir.path(),
            "Base.esm",
            &[],
            0,
            record(kind, 0x800, 0, payload),
        );
        assert!(EsmParser::merge_plugins(&[base]).is_err());
    }
}

#[test]
fn remaps_every_vtex_entry_without_layer_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let filler = plugin(dir.path(), "Filler.esm", &[], 0, Vec::new());
    let base = plugin(dir.path(), "Base.esm", &[], 0, Vec::new());
    let land = record(
        b"LAND",
        0x01000900,
        0,
        sub(
            b"VTEX",
            &[
                0x800u32.to_le_bytes(),
                0x801u32.to_le_bytes(),
                0u32.to_le_bytes(),
                0x01000802u32.to_le_bytes(),
            ]
            .concat(),
        ),
    );
    let patch = plugin(dir.path(), "Patch.esp", &["Base.esm"], 0, land);
    let merged = EsmParser::merge_plugins(&[filler, base, patch]).unwrap();
    assert_eq!(
        SubrecordView::new(&merged[&0x02000900].subrecords)
            .find(b"VTEX")
            .unwrap(),
        &[
            0x01000800u32.to_le_bytes(),
            0x01000801u32.to_le_bytes(),
            0u32.to_le_bytes(),
            0x02000802u32.to_le_bytes(),
        ]
        .concat()
    );
}

#[test]
fn group_parser_handles_deep_nesting_and_restores_sibling_context_in_file_order() {
    let mut payload = record(b"STAT", 1, 0, Vec::new());
    // Well beyond a recursive parser's practical call-stack depth. Building
    // headers in one pass keeps the fixture linear in size and construction time.
    let depth = 10_000usize;
    let mut nested = Vec::with_capacity(depth * 24 + payload.len());
    for remaining in (1..=depth).rev() {
        nested.extend(b"GRUP");
        nested.extend(((remaining * 24 + payload.len()) as u32).to_le_bytes());
        nested.extend([0; 16]);
    }
    nested.append(&mut payload);
    let input = [
        group(1, 0x900, group(6, 0x901, nested)),
        group(1, 0xa00, group(6, 0xa01, record(b"STAT", 2, 0, Vec::new()))),
        record(b"STAT", 3, 0, Vec::new()),
    ]
    .concat();
    let mut records = Vec::new();
    converter::esm::binary::parse_group(&input, None, None, &mut records).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| (
                record.form_id,
                record.cell_form_id,
                record.worldspace_form_id
            ))
            .collect::<Vec<_>>(),
        [
            (1, Some(0x901), Some(0x900)),
            (2, Some(0xa01), Some(0xa00)),
            (3, None, None)
        ]
    );
}

#[test]
fn group_parser_rejects_partial_headers_in_nested_payloads_and_after_siblings() {
    for length in 1..24 {
        let partial = vec![0; length];
        for input in [
            partial.clone(),
            group(0, 0, partial.clone()),
            [
                group(0, 0, record(b"STAT", 1, 0, Vec::new())),
                partial.clone(),
            ]
            .concat(),
        ] {
            let result = converter::esm::binary::parse_group(&input, None, None, &mut Vec::new());
            assert!(result.unwrap_err().to_string().contains("trailing"));
        }
    }
}

#[test]
fn overrides_replace_grass_and_deletions_do_not_resurrect_it() {
    let dir = tempfile::tempdir().unwrap();
    let a = plugin(dir.path(), "Base.esm", &[], 0, grass(0x800, 35));
    let b = plugin(
        dir.path(),
        "Override.esp",
        &["Base.esm"],
        0,
        grass(0x800, 75),
    );
    let merged = EsmParser::merge_plugins(&[a.clone(), b.clone()]).unwrap();
    assert_eq!(
        SubrecordView::new(&merged[&0x800].subrecords)
            .find(b"DATA")
            .unwrap()[0],
        75
    );
    let c = plugin(
        dir.path(),
        "Delete.esp",
        &["Base.esm"],
        0,
        record(b"GRAS", 0x800, 0x20, Vec::new()),
    );
    assert!(
        !EsmParser::merge_plugins(&[a, b, c])
            .unwrap()
            .contains_key(&0x800)
    );
}

#[test]
fn rejects_duplicate_plugins_missing_masters_and_invalid_indices() {
    let dir = tempfile::tempdir().unwrap();
    // Non-grass GMST identity is a separate legacy path; do not reject it here.
    let base = plugin(
        dir.path(),
        "Base.esm",
        &[],
        0,
        record(b"GMST", 0x01000120, 0, sub(b"EDID", b"fSyntheticSetting\0")),
    );
    assert!(EsmParser::merge_plugins(std::slice::from_ref(&base)).is_ok());
    assert!(LoadOrder::read(&[base.clone(), base.clone()]).is_err());
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Base.esm"],
        0,
        grass(0x02000800, 35),
    );
    assert!(LoadOrder::read(std::slice::from_ref(&patch)).is_err());
    assert!(LoadOrder::read(&[patch.clone(), base.clone()]).is_err());
    assert!(EsmParser::merge_plugins(&[base, patch]).is_err());
    let oversized = plugin(dir.path(), "Oversized.esl", &[], 0x200, grass(0x1800, 35));
    assert!(EsmParser::merge_plugins(&[oversized]).is_err());
}

#[test]
#[ignore = "requires explicit MUDCRAB_GRASS_DATA and MUDCRAB_GRASS_PLUGINS paths"]
fn full_local_load_order_merges() {
    let data = PathBuf::from(std::env::var_os("MUDCRAB_GRASS_DATA").expect("MUDCRAB_GRASS_DATA"));
    let list =
        PathBuf::from(std::env::var_os("MUDCRAB_GRASS_PLUGINS").expect("MUDCRAB_GRASS_PLUGINS"));
    let paths = converter::esm::read_plugins_txt(&list, &data).unwrap();
    assert!(!paths.is_empty());
    let records = EsmParser::merge_plugins(&paths).unwrap();
    assert!(
        records
            .values()
            .any(|record| record.record_type == *b"GRAS")
    );
    eprintln!(
        "Merged {} plugins and {} effective records",
        paths.len(),
        records.len()
    );
}

#[test]
fn stable_identity_uses_independent_full_and_light_slot_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let paths = vec![
        plugin(dir.path(), "Base.esm", &[], 0, Vec::new()),
        plugin(dir.path(), "Light.esl", &[], 0x200, Vec::new()),
        plugin(dir.path(), "Patch.esp", &[], 0, Vec::new()),
        plugin(dir.path(), "Flagged.esp", &[], 0x200, Vec::new()),
    ];
    let mut order = LoadOrder::read(&paths).unwrap();
    for (form_id, name, local_id) in [
        (0x00000800, "base.esm", 0x800),
        (0x01000801, "patch.esp", 0x801),
        (0xfe000802, "light.esl", 0x802),
        (0xfe001803, "flagged.esp", 0x803),
    ] {
        let identity = order.identity(form_id).unwrap();
        assert_eq!(identity.plugin, name);
        assert_eq!(identity.local_id, local_id);
    }
    assert!(
        order
            .identity(0)
            .unwrap_err()
            .to_string()
            .contains("null reference")
    );
    for form_id in [0x02000800, 0xfe002800, 0xff000800] {
        assert!(
            order
                .identity(form_id)
                .unwrap_err()
                .to_string()
                .contains("unresolved slot")
        );
    }
    // Public forward maps must not silently invalidate the cached reverse ownership.
    order.normal.insert("base.esm".into(), 1);
    assert!(order.identity(0x800).is_err());
    order.light.remove("light.esl");
    assert!(order.identity(0xfe000800).is_err());
}

/// A master index past the plugin's master list is treated as the plugin's own, as shipped
/// data relies on (Skyrim.esm and Dawnguard.esm each carry one such ID), so a lone one
/// still converts.
#[test]
fn out_of_range_master_index_resolves_to_the_plugin_itself() {
    let dir = tempfile::tempdir().unwrap();
    let path = plugin(
        dir.path(),
        "Masterless.esm",
        &[],
        1,
        record(b"STAT", 0x0200_0800, 0, sub(b"EDID", b"Stray\0")),
    );
    let merged = EsmParser::merge_plugins(&[path]).unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[&0x0000_0800].subrecords[0].1, b"Stray\0");
}

/// An out-of-range master index that lands on another record of the same plugin would
/// silently replace it, so the merge rejects it whichever record comes first.
#[test]
fn out_of_range_master_index_may_not_replace_another_record() {
    for ids in [[0x0000_0800, 0x0200_0800], [0x0200_0800, 0x0000_0800]] {
        let dir = tempfile::tempdir().unwrap();
        let path = plugin(
            dir.path(),
            "Masterless.esm",
            &[],
            1,
            [
                record(b"STAT", ids[0], 0, sub(b"EDID", b"First\0")),
                record(b"STAT", ids[1], 0, sub(b"EDID", b"Second\0")),
            ]
            .concat(),
        );
        let error = format!("{:?}", EsmParser::merge_plugins(&[path]).unwrap_err());
        assert!(
            error.contains("both resolve to 00000800")
                && error.contains("00000800")
                && error.contains("02000800"),
            "{error}"
        );
    }
}
