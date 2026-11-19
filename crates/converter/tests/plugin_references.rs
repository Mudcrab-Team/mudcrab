//! Synthetic plugins only: no game data or copied assets.
mod common;

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

/// Independent field cases: do not derive expectations from the remapper's allowlist.
#[test]
fn every_single_form_id_field_remaps_relocated_full_and_light_plugins() {
    let cases: &[(&[u8; 4], &[u8; 4], usize)] = &[
        (b"REFR", b"XMBR", 4),
        (b"ACHR", b"XMBR", 4),
        (b"TREE", b"SNAM", 4),
        (b"TREE", b"PFIG", 4),
        (b"LTEX", b"TNAM", 4),
        (b"LTEX", b"GNAM", 4),
        (b"LTEX", b"MNAM", 4),
        (b"CELL", b"XCWT", 4),
        (b"WRLD", b"NAM2", 4),
        (b"WRLD", b"NAM3", 4),
        (b"WRLD", b"WNAM", 4),
        (b"WRLD", b"CNAM", 4),
        (b"NPC_", b"RNAM", 4),
        (b"NPC_", b"CNAM", 4),
        (b"NPC_", b"INAM", 4),
        // xEdit 9fb016884bec: wbDefinitionsCommon.pas:8801 and TES5 NPC_:8351.
        // Faction FormID, signed rank, then three unused bytes.
        (b"NPC_", b"SNAM", 8),
        (b"RACE", b"WKMV", 4),
        (b"RACE", b"RNMV", 4),
        // The actor body chain: skin armor, each armature entry, and an armature's race.
        (b"RACE", b"WNAM", 4),
        (b"ARMO", b"MODL", 4),
        (b"ARMA", b"RNAM", 4),
    ];
    for (light, expected_prefix) in [(false, 0x0500_0000u32), (true, 0xFE00_2000)] {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = vec![plugin(dir.path(), "Base.esm", &[], 0, vec![])];
        for i in 0..4 {
            paths.push(plugin(
                dir.path(),
                &format!("Filler{i}.esm"),
                &[],
                0,
                vec![],
            ));
        }
        if light {
            for i in 0..2 {
                paths.push(plugin(
                    dir.path(),
                    &format!("Light{i}.esl"),
                    &[],
                    0x200,
                    vec![],
                ));
            }
        }
        let mut records = Vec::new();
        let mut all_cases = cases.to_vec();
        for record_type in [
            b"CELL", b"REFR", b"ACHR", b"ACRE", b"PGRE", b"PMIS", b"PHZD", b"PARW", b"PBAR",
            b"PBEA", b"PCON", b"PFLA",
        ] {
            for tag in [b"XOWN", b"XGLB", b"XEZN", b"XLCN", b"XLRL"] {
                all_cases.push((record_type, tag, 4));
            }
            if record_type != b"CELL" {
                all_cases.push((record_type, b"NAME", 4));
            }
        }
        for (i, (kind, tag, len)) in all_cases.iter().enumerate() {
            let mut data = vec![0xAB; *len];
            data[..4].copy_from_slice(&0x0100_0801u32.to_le_bytes());
            records.extend(record(kind, 0x0100_0900 + i as u32, 0, sub(tag, &data)));
        }
        paths.push(plugin(
            dir.path(),
            if light { "Patch.esl" } else { "Patch.esp" },
            &["Base.esm"],
            if light { 0x200 } else { 0 },
            records,
        ));
        let merged = EsmParser::merge_plugins(&paths).unwrap();
        for (i, (kind, tag, len)) in all_cases.iter().enumerate() {
            let data = &merged[&(expected_prefix | (0x900 + i as u32))].subrecords[0].1;
            let mut expected = vec![0xAB; *len];
            expected[..4].copy_from_slice(&(expected_prefix | 0x801).to_le_bytes());
            assert_eq!(data, &expected, "light={light}, {kind:?}.{tag:?}");
        }
    }
}

#[test]
fn aliased_tags_and_wrong_length_factions_keep_their_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 0, vec![]);
    let filler = plugin(dir.path(), "Filler.esm", &[], 0, vec![]);
    let cases: &[(&[u8; 4], &[u8; 4], usize)] = &[
        (b"TES4", b"SNAM", 4),
        (b"CLFM", b"CNAM", 4),
        (b"AACT", b"CNAM", 4),
        (b"TREE", b"CNAM", 4),
        // Pinned xEdit TES5Edit source at 9fb016884bec138ea6c7b872cec831537d464c3e:
        // `wbWorldLargeRefs` defines WRLD.RNAM as rows with Y/X i16 cell coordinates
        // (Core/wbDefinitionsCommon.pas:10011-10038), included by WRLD at
        // Core/wbDefinitionsTES5.pas:10645-10651. WRLD.TNAM is `wbString` at
        // Core/wbDefinitionsTES5.pas:10708. Keep these four-byte payloads unchanged;
        // nested RNAM references require separate layout-aware remapping.
        // https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsCommon.pas#L10011-L10038
        // https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L10645-L10651
        // https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L10708
        (b"WRLD", b"RNAM", 4),
        (b"WRLD", b"TNAM", 4),
        (b"NPC_", b"SNAM", 0),
        (b"NPC_", b"SNAM", 3),
        (b"NPC_", b"SNAM", 4),
        (b"NPC_", b"SNAM", 5),
        (b"NPC_", b"SNAM", 9),
    ];
    let payload = cases
        .iter()
        .enumerate()
        .flat_map(|(i, (kind, tag, len))| {
            let mut data = vec![0xAB; *len];
            if *len >= 4 {
                data[..4].copy_from_slice(&0x0100_0801u32.to_le_bytes());
            }
            record(kind, 0x0100_0900 + i as u32, 0, sub(tag, &data))
        })
        .collect();
    let patch = plugin(dir.path(), "Patch.esp", &["Base.esm"], 0, payload);
    let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
    for (i, (_, _, len)) in cases.iter().enumerate() {
        let mut expected = vec![0xAB; *len];
        if *len >= 4 {
            expected[..4].copy_from_slice(&0x0100_0801u32.to_le_bytes());
        }
        assert_eq!(merged[&(0x0200_0900 + i as u32)].subrecords[0].1, expected);
    }
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
        assert!(EsmParser::merge_plugins(&[base]).unwrap().is_empty());
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
fn group_parser_preserves_complete_records_around_partial_bounded_frames() {
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
            let mut records = Vec::new();
            converter::esm::binary::parse_group(&input, None, None, &mut records).unwrap();
            assert_eq!(records.len(), usize::from(input.len() > 24 + length));
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
    let merged = EsmParser::merge_plugins(&[base, patch]).unwrap();
    assert_eq!(merged.len(), 1);
    assert!(merged.contains_key(&0x120));
    let oversized = plugin(dir.path(), "Oversized.esl", &[], 0x200, grass(0x1800, 35));
    assert!(EsmParser::merge_plugins(&[oversized]).unwrap().is_empty());
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
/// silently replace it, so the conflicting candidate is omitted whichever comes first.
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
        let merged = EsmParser::merge_plugins(&[path]).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[&0x800].subrecords[0].1, b"First\0");
    }
}

/// Reference and cell fields that hold FormIDs are rewritten into load-order numbering.
/// The last plugin mirrors Dragonborn.esm: its masters are only Skyrim.esm and Update.esm,
/// so its own records are numbered 0x02xxxxxx locally but sit in slot 4 of the load order,
/// where 0x02 is a different plugin.
#[test]
fn reference_and_cell_form_id_fields_use_load_order_numbering() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = vec![
        plugin(dir.path(), "Skyrim.esm", &[], 1, Vec::new()),
        plugin(dir.path(), "Update.esm", &["Skyrim.esm"], 1, Vec::new()),
        plugin(
            dir.path(),
            "Dawnguard.esm",
            &["Skyrim.esm", "Update.esm"],
            1,
            Vec::new(),
        ),
        plugin(
            dir.path(),
            "HearthFires.esm",
            &["Skyrim.esm", "Update.esm"],
            1,
            Vec::new(),
        ),
    ];
    let mut xtel = 0x0200_0801u32.to_le_bytes().to_vec();
    xtel.extend([0u8; 24]); // position and rotation
    xtel.extend(0x1u32.to_le_bytes()); // flags must survive untouched
    let mut xlkr = 0x0000_0042u32.to_le_bytes().to_vec(); // keyword from Skyrim.esm
    xlkr.extend(0x0200_0802u32.to_le_bytes()); // linked reference in this plugin
    let mut xclr = 0x0000_0123u32.to_le_bytes().to_vec();
    xclr.extend(0x0200_0124u32.to_le_bytes());
    paths.push(plugin(
        dir.path(),
        "Dragonborn.esm",
        &["Skyrim.esm", "Update.esm"],
        1,
        [
            record(
                b"REFR",
                0x0200_0800,
                0,
                [sub(b"XTEL", &xtel), sub(b"XLKR", &xlkr)].concat(),
            ),
            record(
                b"CELL",
                0x0200_0900,
                0,
                [
                    sub(b"LTMP", &0x0200_0901u32.to_le_bytes()),
                    sub(b"XCLR", &xclr),
                ]
                .concat(),
            ),
        ]
        .concat(),
    ));
    let merged = EsmParser::merge_plugins(&paths).unwrap();
    let field = |form_id: u32, tag: &[u8; 4]| -> Vec<u8> {
        merged[&form_id]
            .subrecords
            .iter()
            .find(|(name, _)| name.as_slice() == tag)
            .map(|(_, data)| data.clone())
            .unwrap()
    };
    let word = |data: &[u8], offset: usize| {
        u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
    };

    let xtel = field(0x0400_0800, b"XTEL");
    assert_eq!(
        word(&xtel, 0),
        0x0400_0801,
        "door destination is Dragonborn's own reference"
    );
    assert_eq!(word(&xtel, 28), 0x1, "XTEL flags are untouched");
    let xlkr = field(0x0400_0800, b"XLKR");
    assert_eq!(
        word(&xlkr, 0),
        0x0000_0042,
        "Skyrim.esm keyword keeps slot 0"
    );
    assert_eq!(word(&xlkr, 4), 0x0400_0802);
    assert_eq!(word(&field(0x0400_0900, b"LTMP"), 0), 0x0400_0901);
    let xclr = field(0x0400_0900, b"XCLR");
    assert_eq!([word(&xclr, 0), word(&xclr, 4)], [0x0000_0123, 0x0400_0124]);
}

/// Malformed optional fields are dropped while usable fields and neighboring records
/// still remap from plugin-local slot 1 to load-order slot 2.
#[test]
fn malformed_form_id_field_lengths_are_dropped_without_blocking_valid_links() {
    for (record_type, tag, len) in [
        (b"REFR", b"XTEL", 28),
        (b"REFR", b"XESP", 4),
        (b"REFR", b"XESP", 12),
        (b"REFR", b"XLKR", 6),
        (b"REFR", b"XNDP", 4),
        (b"REFR", b"XAPR", 12),
        (b"REFR", b"XAPR", 16),
        (b"REFR", b"XLRT", 6),
        (b"CELL", b"XCLR", 6),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
        let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
        let valid_tag = if record_type == b"CELL" {
            b"LTMP"
        } else {
            b"NAME"
        };
        let mut malformed = vec![0xAB; len];
        malformed[..4].copy_from_slice(&0x0100_0900u32.to_le_bytes());
        let patch = plugin(
            dir.path(),
            "Patch.esm",
            &["Base.esm"],
            1,
            [
                record(
                    record_type,
                    0x0100_0800,
                    0,
                    [
                        sub(tag, &malformed),
                        sub(valid_tag, &0x0100_0901u32.to_le_bytes()),
                    ]
                    .concat(),
                ),
                record(
                    b"REFR",
                    0x0100_0801,
                    0,
                    sub(b"NAME", &0x0100_0902u32.to_le_bytes()),
                ),
            ]
            .concat(),
        );
        let result = EsmParser::merge_plugins(&[base, filler, patch]);
        assert!(
            result.is_ok(),
            "{} {} length {len}: {result:?}",
            String::from_utf8_lossy(record_type),
            String::from_utf8_lossy(tag)
        );
        let merged = result.unwrap();
        let fields = &merged[&0x0200_0800].subrecords;
        assert!(!fields.iter().any(|(name, _)| name.as_slice() == tag));
        assert_eq!(
            fields
                .iter()
                .find(|(name, _)| name.as_slice() == valid_tag)
                .unwrap()
                .1,
            0x0200_0901u32.to_le_bytes()
        );
        assert_eq!(
            merged[&0x0200_0801].subrecords[0].1,
            0x0200_0902u32.to_le_bytes()
        );
    }
}

/// Repeated optional fields are independent: dropping a malformed occurrence preserves
/// both valid neighbors and their non-FormID payload bytes.
#[test]
fn malformed_repeated_xapr_drops_only_the_bad_occurrence() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
    let first = [0x0100_0901u32.to_le_bytes(), 1.25f32.to_le_bytes()].concat();
    let last = [0x0100_0902u32.to_le_bytes(), 2.5f32.to_le_bytes()].concat();
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        record(
            b"REFR",
            0x0100_0800,
            0,
            [
                sub(b"XAPR", &first),
                sub(b"XAPR", &[0xAB; 12]),
                sub(b"XAPR", &last),
            ]
            .concat(),
        ),
    );
    let result = EsmParser::merge_plugins(&[base, filler, patch]);
    assert!(result.is_ok(), "{result:?}");
    let merged = result.unwrap();
    let fields: Vec<_> = merged[&0x0200_0800]
        .subrecords
        .iter()
        .filter(|(tag, _)| tag.as_slice() == b"XAPR")
        .map(|(_, data)| data.clone())
        .collect();
    assert_eq!(
        fields,
        [
            [0x0200_0901u32.to_le_bytes(), 1.25f32.to_le_bytes()].concat(),
            [0x0200_0902u32.to_le_bytes(), 2.5f32.to_le_bytes()].concat(),
        ]
    );
}

/// Forms seen in the official plugins: 14 references use a 4-byte `XLKR` holding only the
/// linked reference, and worldspaces carry `LTMP` lighting templates as well as cells.
#[test]
fn legacy_linked_references_and_worldspace_lighting_templates_are_remapped() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
    // Masters are only Base.esm, so this plugin's own records are 0x01xxxxxx locally but
    // sit in slot 2 of the load order.
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        [
            record(
                b"REFR",
                0x0100_0800,
                0,
                sub(b"XLKR", &0x0100_0801u32.to_le_bytes()),
            ),
            record(
                b"WRLD",
                0x0100_0900,
                0,
                sub(b"LTMP", &0x0100_0901u32.to_le_bytes()),
            ),
        ]
        .concat(),
    );
    let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
    let word = |form_id: u32, tag: &[u8; 4]| {
        let data = &merged[&form_id]
            .subrecords
            .iter()
            .find(|(name, _)| name.as_slice() == tag)
            .unwrap()
            .1;
        assert_eq!(data.len(), 4);
        u32::from_le_bytes(data[..4].try_into().unwrap())
    };
    assert_eq!(word(0x0200_0800, b"XLKR"), 0x0200_0801);
    assert_eq!(word(0x0200_0900, b"LTMP"), 0x0200_0901);
}

/// Every field in the converter's layout table, so a mistyped tag or offset fails here.
/// The plugin's masters are only Base.esm, so its own records (0x01xxxxxx locally) sit in
/// slot 2, and each covered FormID must come out as 0x02xxxxxx.
#[test]
fn every_covered_reference_and_cell_field_is_remapped_at_its_offsets() {
    /// Record type, subrecord tag, payload length, and FormID offsets.
    type FieldCase = (&'static [u8; 4], &'static [u8; 4], usize, &'static [usize]);
    let fields: &[FieldCase] = &[
        (b"REFR", b"XTEL", 32, &[0]),
        (b"REFR", b"XESP", 8, &[0]),
        (b"PHZD", b"XESP", 8, &[0]),
        (b"PHZD", b"NAME", 4, &[0]),
        (b"PARW", b"XESP", 8, &[0]),
        (b"PARW", b"NAME", 4, &[0]),
        (b"PBAR", b"XESP", 8, &[0]),
        (b"PBAR", b"NAME", 4, &[0]),
        (b"PBEA", b"XESP", 8, &[0]),
        (b"PBEA", b"NAME", 4, &[0]),
        (b"PCON", b"XESP", 8, &[0]),
        (b"PCON", b"NAME", 4, &[0]),
        (b"PFLA", b"XESP", 8, &[0]),
        (b"PFLA", b"NAME", 4, &[0]),
        (b"REFR", b"XLKR", 8, &[0, 4]),
        (b"ACHR", b"XLKR", 4, &[0]),
        (b"REFR", b"XNDP", 8, &[0]),
        (b"REFR", b"XEMI", 4, &[0]),
        (b"REFR", b"XAPR", 8, &[0]),
        (b"ACHR", b"XLRT", 8, &[0, 4]),
        (b"ACHR", b"XHOR", 4, &[0]),
        (b"CELL", b"LTMP", 4, &[0]),
        (b"WRLD", b"LTMP", 4, &[0]),
        (b"CELL", b"XCIM", 4, &[0]),
        (b"CELL", b"XCMO", 4, &[0]),
        (b"CELL", b"XCAS", 4, &[0]),
        (b"CELL", b"XCCM", 4, &[0]),
        (b"CELL", b"XCLR", 12, &[0, 4, 8]),
        (b"REFR", b"XPOD", 8, &[0, 4]),
        (b"REFR", b"XLRM", 4, &[0]),
        (b"REFR", b"LNAM", 4, &[0]),
        (b"REFR", b"INAM", 4, &[0]),
    ];
    for &(record_type, tag, len, offsets) in fields {
        let label = format!(
            "{} {}",
            String::from_utf8_lossy(record_type),
            String::from_utf8_lossy(tag)
        );
        // Non-FormID bytes are 0xAB so a write at a wrong offset is visible.
        let mut payload = vec![0xABu8; len];
        for (i, &offset) in offsets.iter().enumerate() {
            payload[offset..offset + 4].copy_from_slice(&(0x0100_0900 + i as u32).to_le_bytes());
        }
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
        let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
        let patch = plugin(
            dir.path(),
            "Patch.esm",
            &["Base.esm"],
            1,
            record(record_type, 0x0100_0800, 0, sub(tag, &payload)),
        );
        let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
        let data = &merged[&0x0200_0800]
            .subrecords
            .iter()
            .find(|(name, _)| name.as_slice() == tag)
            .unwrap_or_else(|| panic!("{label} missing"))
            .1;
        let mut expected = payload.clone();
        for (i, &offset) in offsets.iter().enumerate() {
            expected[offset..offset + 4].copy_from_slice(&(0x0200_0900 + i as u32).to_le_bytes());
        }
        assert_eq!(data, &expected, "{label}");
    }
}

/// `LNAM` and `INAM` are FormIDs only on room marker references. On other record types the
/// same tags keep their old handling: `NPC_` `INAM` was always a FormID, and the room marker
/// layouts must not touch `ACHR` or `CELL` fields.
#[test]
fn room_marker_tags_on_other_record_types_keep_their_handling() {
    let cases: &[(&[u8; 4], &[u8; 4], bool)] = &[
        (b"NPC_", b"INAM", true),
        (b"ACHR", b"LNAM", false),
        (b"ACHR", b"INAM", false),
        (b"ACHR", b"XLRM", false),
        (b"CELL", b"LNAM", false),
        (b"CELL", b"INAM", false),
        (b"CELL", b"XLRM", false),
    ];
    for &(record_type, tag, remapped) in cases {
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
        let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
        let patch = plugin(
            dir.path(),
            "Patch.esm",
            &["Base.esm"],
            1,
            record(
                record_type,
                0x0100_0800,
                0,
                sub(tag, &0x0100_0900u32.to_le_bytes()),
            ),
        );
        let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
        let data = &merged[&0x0200_0800]
            .subrecords
            .iter()
            .find(|(name, _)| name.as_slice() == tag)
            .unwrap()
            .1;
        let expected = if remapped {
            0x0200_0900u32
        } else {
            0x0100_0900
        };
        assert_eq!(
            u32::from_le_bytes(data[..4].try_into().unwrap()),
            expected,
            "{} {}",
            String::from_utf8_lossy(record_type),
            String::from_utf8_lossy(tag)
        );
    }
}

/// Light plugins use the 0xFE prefix with a 12-bit slot; their own references in these
/// fields must land in that space too.
#[test]
fn light_plugin_reference_fields_use_the_light_slot() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let mut xtel = 0x0100_0801u32.to_le_bytes().to_vec();
    xtel.extend([0u8; 28]);
    let light = plugin(
        dir.path(),
        "Light.esl",
        &["Base.esm"],
        0x201,
        record(b"REFR", 0x0100_0800, 0, sub(b"XTEL", &xtel)),
    );
    let merged = EsmParser::merge_plugins(&[base, light]).unwrap();
    let data = &merged[&0xFE00_0800]
        .subrecords
        .iter()
        .find(|(name, _)| name.as_slice() == b"XTEL")
        .unwrap()
        .1;
    assert_eq!(
        u32::from_le_bytes(data[..4].try_into().unwrap()),
        0xFE00_0801
    );
}

/// An invalid optional cell link is cleared without rejecting an otherwise valid cell.
#[test]
fn cell_fields_skip_out_of_range_master_indices() {
    let dir = tempfile::tempdir().unwrap();
    let path = plugin(
        dir.path(),
        "Base.esm",
        &[],
        1,
        record(
            b"CELL",
            0x0000_0800,
            0,
            sub(b"LTMP", &0x0500_0001u32.to_le_bytes()),
        ),
    );
    let merged = EsmParser::merge_plugins(&[path]).unwrap();
    assert_eq!(
        SubrecordView::new(&merged[&0x800].subrecords).get_form_id(b"LTMP"),
        Some(0)
    );
}

/// Invalid optional links do not block other records or corrupt the payload's
/// remaining bytes. Cover both bad master indices and wide light-master IDs.
#[test]
fn reference_fields_skip_invalid_master_and_light_ids() {
    for (tag, len, offset) in [
        (b"XTEL", 32, 0),
        (b"XESP", 8, 0),
        (b"XLKR", 8, 4),
        (b"XNDP", 8, 0),
        (b"XAPR", 8, 0),
    ] {
        for (name, flags, value, record_id) in [
            ("Patch.esp", 0, 0x0200_0801u32, 0x0200_0800),
            ("Patch.esl", 0x200, 0x0100_1801, 0xFE00_0800),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
            let filler = plugin(dir.path(), "Filler.esm", &[], 1, Vec::new());
            let mut payload = vec![0xAB; len];
            if tag == b"XLKR" {
                payload[..4].copy_from_slice(&0x0000_0042u32.to_le_bytes());
            }
            payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            let mut expected = payload.clone();
            expected[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
            let patch = plugin(
                dir.path(),
                name,
                &["Base.esm"],
                flags,
                [
                    record(b"REFR", 0x0100_0800, 0, sub(tag, &payload)),
                    record(
                        b"REFR",
                        0x0100_0802,
                        0,
                        sub(b"XEMI", &0x42u32.to_le_bytes()),
                    ),
                ]
                .concat(),
            );
            let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
            assert_eq!(merged[&record_id].subrecords[0].1, expected);
            assert_eq!(
                SubrecordView::new(&merged[&(record_id + 2)].subrecords).get_form_id(b"XEMI"),
                Some(0x42)
            );
        }
    }
}

/// A broken link to a light master is skipped without truncating it to another
/// light record. Other array elements still resolve using the containing plugin.
#[test]
fn optional_links_to_wide_light_master_ids_preserve_valid_array_elements() {
    let dir = tempfile::tempdir().unwrap();
    let paths = vec![
        plugin(dir.path(), "Base.esm", &[], 1, Vec::new()),
        plugin(dir.path(), "First.esl", &[], 0x201, Vec::new()),
        plugin(dir.path(), "Second.esl", &[], 0x201, Vec::new()),
        plugin(dir.path(), "Filler.esm", &[], 1, Vec::new()),
        plugin(
            dir.path(),
            "Patch.esp",
            &["Second.esl", "Base.esm"],
            0,
            record(
                b"REFR",
                0x0200_0800,
                0,
                sub(
                    b"XLRT",
                    &[
                        0x0000_1801u32.to_le_bytes(), // Too wide for Second.esl.
                        0x0000_0802u32.to_le_bytes(), // Valid light master reference.
                        0x0200_0903u32.to_le_bytes(), // Valid plugin-local reference.
                        0u32.to_le_bytes(),
                    ]
                    .concat(),
                ),
            ),
        ),
    ];
    let merged = EsmParser::merge_plugins(&paths).unwrap();
    assert_eq!(
        merged[&0x0200_0800].subrecords[0].1,
        [
            0u32.to_le_bytes(),
            0xFE00_1802u32.to_le_bytes(),
            0x0200_0903u32.to_le_bytes(),
            0u32.to_le_bytes(),
        ]
        .concat()
    );
}

/// Null links stay null; unrelated record kinds sharing a tag remain opaque.
#[test]
fn null_links_and_unrelated_subrecords_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let payload = [0u32.to_le_bytes(), 0xABCD_EF12u32.to_le_bytes()].concat();
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &[],
        0,
        [
            record(b"REFR", 0x800, 0, sub(b"XESP", &payload)),
            record(b"STAT", 0x801, 0, sub(b"XTEL", &[1, 2, 3])),
        ]
        .concat(),
    );
    let merged = EsmParser::merge_plugins(&[base, patch]).unwrap();
    assert_eq!(merged[&0x0100_0800].subrecords[0].1, payload);
    assert_eq!(merged[&0x0100_0801].subrecords[0].1, [1, 2, 3]);
}

/// Read all occurrences of a subrecord from a published database projection.
fn database_fields(
    conn: &rusqlite::Connection,
    table: &str,
    id: u32,
    tag: &[u8; 4],
) -> Vec<Vec<u8>> {
    let key = if table == "records" { "form_id" } else { "id" };
    let blob: Vec<u8> = conn
        .query_row(
            &format!("SELECT data FROM \"{table}\" WHERE {key}=?1"),
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let decoded =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&blob)
            .unwrap();
    decoded
        .subrecords
        .into_iter()
        .filter(|sub| &sub.tag == tag)
        .map(|sub| sub.data)
        .collect()
}

/// Read one required subrecord from a published database projection.
fn database_field(conn: &rusqlite::Connection, table: &str, id: u32, tag: &[u8; 4]) -> Vec<u8> {
    let fields = database_fields(conn, table, id, tag);
    assert_eq!(fields.len(), 1);
    fields.into_iter().next().unwrap()
}

/// The real pipeline publishes resolved IDs in every blob projection, including
/// non-identity master lists, light masters, overrides and repeated subrecords.
/// Malformed fields are absent from both record and reference blobs; valid neighbors survive.
/// Enable-parent flags use one byte even when all three unused bytes are nonzero;
/// zero flags and the combined inversion/pop-in bits preserve their raw payloads.
/// Rerunning replaces stale database blobs and old textures, retaining proven meshes/scripts.
#[tokio::test]
async fn pipeline_publishes_and_rebuilds_reference_and_cell_links() {
    pipeline_publishes_and_rebuilds_reference_and_cell_links_with_reader(None).await;
}

/// Explicit legacy conversion preserves the same valid links while reporting malformed fields.
#[tokio::test]
async fn legacy_pipeline_publishes_and_rebuilds_reference_and_cell_links() {
    pipeline_publishes_and_rebuilds_reference_and_cell_links_with_reader(Some(
        converter::config::RecordReader::Legacy,
    ))
    .await;
}

/// Exercise publication and cache replacement with real link targets and deliberate bad fields.
async fn pipeline_publishes_and_rebuilds_reference_and_cell_links_with_reader(
    reader: Option<converter::config::RecordReader>,
) {
    use converter::AssetPipeline;
    use rusqlite::Connection;
    /// Adds a complete authored transform so link tests retain a usable placement.
    fn placed(id: u32, mut payload: Vec<u8>) -> Vec<u8> {
        let transform: Vec<_> = [31.25f32, -97.5, 14.75, 0.13, -0.27, 0.49]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        payload.extend(sub(b"DATA", &transform));
        record(b"REFR", id, 0, payload)
    }
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    dummy_content::layout::prepare_directory(&data, false).unwrap();
    dummy_content::layout::generate(
        &data,
        dummy_content::layout::DEFAULT_SEED,
        dummy_content::layout::Formats::all(),
    )
    .unwrap();
    plugin(&data, "Filler.esm", &["Skyrim.esm"], 1, Vec::new());
    plugin(&data, "Filler2.esm", &["Skyrim.esm"], 1, Vec::new());
    plugin(
        &data,
        "Light.esl",
        &["Skyrim.esm"],
        0x201,
        placed(0x0100_0802, sub(b"NAME", &0x0000_0003u32.to_le_bytes())),
    );
    let mut xtel = 0x0200_0801u32.to_le_bytes().to_vec();
    xtel.extend([0xAB; 28]);
    let xesp = [0x0000_0802u32.to_le_bytes(), [1, 0x94, 0x27, 0xE1]].concat();
    let xapr = [0x0200_0803u32.to_le_bytes(), 1.25f32.to_le_bytes()].concat();
    let cell = 0x0200_0900;
    // Light.esl is local master 0, Skyrim.esm is 1; the plugin itself is 2.
    plugin(
        &data,
        "Links.esp",
        &["Light.esl", "Skyrim.esm"],
        0,
        [
            record(
                b"LGTM",
                0x0200_0901,
                0,
                [
                    sub(b"EDID", b"SyntheticLinkLighting\0"),
                    sub(b"DATA", &[0; 92]),
                ]
                .concat(),
            ),
            record(
                b"CELL",
                cell,
                0,
                [
                    sub(b"DATA", &[1, 0]),
                    sub(b"LTMP", &0x0200_0901u32.to_le_bytes()),
                    sub(b"XCLR", &[0xAB; 6]),
                ]
                .concat(),
            ),
            group(
                6,
                cell,
                group(
                    9,
                    cell,
                    [
                        placed(0x0200_0803, sub(b"NAME", &0x0100_0003u32.to_le_bytes())),
                        placed(0x0200_0804, sub(b"NAME", &0x0100_0003u32.to_le_bytes())),
                        placed(0x0200_0805, sub(b"NAME", &0x0100_0003u32.to_le_bytes())),
                        placed(
                            0x0200_0800,
                            [
                                sub(b"XTEL", &xtel),
                                sub(b"XESP", &xesp),
                                sub(b"XAPR", &xapr),
                                sub(b"XAPR", &xapr),
                            ]
                            .concat(),
                        ),
                        placed(
                            0x0200_0806,
                            [
                                sub(
                                    b"XESP",
                                    &[0x0300_0802u32.to_le_bytes(), [2, 0x35, 0x6B, 0xC4]].concat(),
                                ),
                                sub(b"NAME", &0x0100_0003u32.to_le_bytes()),
                            ]
                            .concat(),
                        ),
                        placed(
                            0x0200_0807,
                            [
                                sub(b"XESP", &[0xAB; 4]),
                                sub(b"NAME", &0x0100_0003u32.to_le_bytes()),
                            ]
                            .concat(),
                        ),
                        placed(
                            0x0200_0808,
                            [
                                sub(
                                    b"XESP",
                                    &[0x0000_0802u32.to_le_bytes(), [0, 0xD8, 0x18, 0x7C]].concat(),
                                ),
                                sub(b"NAME", &0x0100_0003u32.to_le_bytes()),
                            ]
                            .concat(),
                        ),
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat(),
    );
    // Override using a different local master index for Links.esp (1, not 2).
    let mut override_xtel = xtel.clone();
    override_xtel[..4].copy_from_slice(&0x0100_0804u32.to_le_bytes());
    plugin(
        &data,
        "Override.esp",
        &["Skyrim.esm", "Links.esp", "Light.esl"],
        0,
        group(
            6,
            0x0100_0900,
            group(
                9,
                0x0100_0900,
                placed(
                    0x0100_0800,
                    [
                        sub(b"XTEL", &override_xtel),
                        sub(b"XLRT", &[0xAB; 6]),
                        sub(b"XAPR", &[0xAB; 12]),
                        sub(
                            b"XESP",
                            &[0x0200_0802u32.to_le_bytes(), [3, 0xA7, 0x51, 0xD2]].concat(),
                        ),
                        sub(
                            b"XAPR",
                            &[0x0100_0803u32.to_le_bytes(), 1.25f32.to_le_bytes()].concat(),
                        ),
                        sub(
                            b"XAPR",
                            &[0x0100_0805u32.to_le_bytes(), 2.5f32.to_le_bytes()].concat(),
                        ),
                    ]
                    .concat(),
                ),
            ),
        ),
    );
    // Checked filler masters deliberately separate plugin-local master indices from full slots.
    let list = dir.path().join("plugins.txt");
    fs::write(
        &list,
        "Skyrim.esm\n*Filler.esm\n*Filler2.esm\n*Light.esl\n*Links.esp\n*Override.esp\n",
    )
    .unwrap();
    let output = dir.path().join("modern");
    let mut config = common::cpu_lod_config(&data, &output);
    config.plugins_file = Some(list);
    if let Some(reader) = reader {
        config.record_reader = reader;
    }
    let mut expected_asset_rebuilds = 0;
    for pass in 0..2 {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = AssetPipeline::run_async(config.clone(), tx).await.unwrap();
        drain.await.unwrap();
        assert!(!report.complete, "malformed fields must remain visible");
        assert_eq!(report.skipped, 0, "usable assets and records still publish");
        assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
        for plugin in ["links.esp", "override.esp"] {
            let warnings: Vec<_> = report
                .warnings
                .iter()
                .filter(|warning| warning.to_ascii_lowercase().contains(plugin))
                .collect();
            assert_eq!(warnings.len(), 1, "{:?}", report.warnings);
            assert!(warnings[0].contains("first:"), "{}", warnings[0]);
        }
        let published: converter::cache::ConversionManifest =
            serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                .unwrap();
        assert!(!published.complete, "manifest must retain reader warnings");
        if pass == 1 {
            assert_eq!(report.converted, expected_asset_rebuilds);
            assert!(report.cache_hits > 0);
        }
        let conn = Connection::open(output.join("skyrim_world.db")).unwrap();
        let position: (f32, f32, f32) = conn
            .query_row(
                "SELECT pos_x,pos_y,pos_z FROM \"references\" WHERE id=?1",
                [0x0300_0800u32],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(position, (31.25, -97.5, 14.75));
        let schema: u32 = conn
            .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
            .unwrap();
        assert_eq!(schema, converter::shared::WORLD_DATABASE_SCHEMA_VERSION);
        let mut expected_xtel = xtel.clone();
        expected_xtel[..4].copy_from_slice(&0x0300_0804u32.to_le_bytes());
        for table in ["records", "references"] {
            assert!(database_fields(&conn, table, 0x0300_0800, b"XLRT").is_empty());
            assert_eq!(
                database_field(&conn, table, 0x0300_0806, b"NAME"),
                0x0000_0003u32.to_le_bytes()
            );
            assert_eq!(
                database_field(&conn, table, 0x0300_0806, b"XESP"),
                [0u32.to_le_bytes(), [2, 0x35, 0x6B, 0xC4]].concat()
            );
            assert_eq!(
                database_field(&conn, table, 0x0300_0800, b"XTEL"),
                expected_xtel
            );
            assert_eq!(
                database_field(&conn, table, 0x0300_0800, b"XESP"),
                [0xFE00_0802u32.to_le_bytes(), [3, 0xA7, 0x51, 0xD2]].concat()
            );
            assert_eq!(
                database_field(&conn, table, 0x0300_0808, b"XESP"),
                [0xFE00_0802u32.to_le_bytes(), [0, 0xD8, 0x18, 0x7C]].concat()
            );
            assert_eq!(
                database_fields(&conn, table, 0x0300_0800, b"XAPR"),
                [
                    [0x0300_0803u32.to_le_bytes(), 1.25f32.to_le_bytes()].concat(),
                    [0x0300_0805u32.to_le_bytes(), 2.5f32.to_le_bytes()].concat(),
                ]
            );
        }
        // The enable-parent columns follow the remapped XESP: valid, cleared to
        // zero with its flags kept, and NULL when the malformed subrecord was dropped.
        for (id, expected) in [
            (0x0300_0800u32, (Some(0xFE00_0802i64), Some(3i64))),
            (0x0300_0806, (Some(0), Some(2))),
            (0x0300_0807, (None, None)),
            (0x0300_0808, (Some(0xFE00_0802), Some(0))),
        ] {
            let columns: (Option<i64>, Option<i64>) = conn
                .query_row(
                    "SELECT enable_parent_id, enable_parent_flags FROM \"references\" WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(columns, expected, "{id:08X}");
        }
        for table in ["records", "references"] {
            assert!(database_fields(&conn, table, 0x0300_0807, b"XESP").is_empty());
        }
        let identity: (String, u32, u32) = conn
            .query_row(
                "SELECT m.plugin_name, m.internal_id, r.load_order FROM formid_map m \
                 JOIN records r ON r.form_id=m.form_id WHERE m.form_id=?1",
                [0x0300_0800u32],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(identity, ("links.esp".to_owned(), 0x800, 5));
        for table in ["records", "cells"] {
            assert!(database_fields(&conn, table, 0x0300_0900, b"XCLR").is_empty());
            assert_eq!(
                database_field(&conn, table, 0x0300_0900, b"LTMP"),
                0x0300_0901u32.to_le_bytes()
            );
        }
        // Simulate an old pack with plugin-local IDs; the next conversion must
        // replace it even though the source plugins are unchanged.
        if pass == 0 {
            let old = converter::esm::extractors::serialize_subrecords(&[(
                b"XTEL".to_vec(),
                override_xtel.clone(),
            )]);
            conn.execute(
                "UPDATE records SET data=?1 WHERE form_id=?2",
                rusqlite::params![old, 0x0300_0800u32],
            )
            .unwrap();
            conn.execute(
                "UPDATE \"references\" SET data=?1 WHERE id=?2",
                rusqlite::params![old, 0x0300_0800u32],
            )
            .unwrap();
            // Producer 24's world projection contains padding in enable-parent
            // flags. Rebuild the world while reusing proved scripts; the old
            // textures and, since producer 27 changed the GLB collision data,
            // the old meshes all regenerate.
            let manifest_path = output.join("conversion-manifest.json");
            let mut manifest: converter::cache::ConversionManifest =
                serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
            expected_asset_rebuilds = manifest
                .entries
                .values()
                .filter(|entry| entry.output.ends_with(".ktx2") || entry.output.ends_with(".glb"))
                .count() as u64;
            manifest.schema_version = 24;
            manifest.asset_configuration_hashes.clear();
            manifest.configuration_hash =
                converter::cache::configuration_hash_for_schema(&config, 24).unwrap();
            manifest.save(&manifest_path).unwrap();
            conn.execute("UPDATE schema_info SET version=7", [])
                .unwrap();
        }
    }
}

/// The load-order export writes the room graph, with the marker FormIDs in load-order
/// numbering: the plugin's own records sit in slot 2 here, its masters only name Base.esm.
#[test]
fn load_order_export_writes_the_room_graph_with_remapped_marker_ids() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
    let floats =
        |values: [f32; 6]| -> Vec<u8> { values.iter().flat_map(|v| v.to_le_bytes()).collect() };
    let xprm = |half: [f32; 3], shape: u32| -> Vec<u8> {
        let mut bytes: Vec<u8> = half.iter().flat_map(|v| v.to_le_bytes()).collect();
        bytes.extend([0u8; 12]);
        bytes.extend(0.2f32.to_le_bytes());
        bytes.extend(shape.to_le_bytes());
        bytes
    };
    let word = |value: u32| value.to_le_bytes().to_vec();
    let marker = |id: u32, base: u32, position: [f32; 3], extra: Vec<u8>| {
        record(
            b"REFR",
            id,
            0,
            [
                sub(b"NAME", &word(base)),
                sub(
                    b"DATA",
                    &floats([position[0], position[1], position[2], 0.0, 0.0, 0.0]),
                ),
                extra,
            ]
            .concat(),
        )
    };
    let cell = 0x0100_0900;
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        [
            // No XCLC and no worldspace: an interior cell.
            record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
            group(
                6,
                cell,
                group(
                    9,
                    cell,
                    [
                        marker(
                            0x0100_0801,
                            0x1F,
                            [0.0; 3],
                            sub(b"XPRM", &xprm([100.0; 3], 1)),
                        ),
                        marker(
                            0x0100_0802,
                            0x1F,
                            [500.0, 0.0, 0.0],
                            [
                                sub(b"XPRM", &xprm([100.0; 3], 1)),
                                sub(b"XRMR", &[1, 0xc0, 1, 0]),
                                sub(b"LNAM", &word(0x0100_0A00)),
                                sub(b"INAM", &word(0x0100_0A01)),
                                sub(b"XLRM", &word(0x0100_0801)),
                            ]
                            .concat(),
                        ),
                        marker(
                            0x0100_0803,
                            0x20,
                            [250.0, 0.0, 0.0],
                            [
                                sub(b"XPRM", &xprm([40.0, 1.0, 60.0], 3)),
                                sub(b"XPOD", &[word(0x0100_0801), word(0x0100_0802)].concat()),
                            ]
                            .concat(),
                        ),
                        marker(0x0100_0804, 0x99, [10.0, 20.0, 30.0], Vec::new()),
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat(),
    );
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&[base, filler, patch], &db).unwrap();
    let conn = Connection::open(&db).unwrap();
    let ids = |sql: &str| -> Vec<(u32, u32)> {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        ids("SELECT ref_id, cell_id FROM room_bounds ORDER BY ref_id"),
        vec![(0x0200_0801, 0x0200_0900), (0x0200_0802, 0x0200_0900)]
    );
    assert_eq!(
        ids("SELECT lighting_template_id, image_space_id FROM room_bounds WHERE ref_id=0x02000802"),
        vec![(0x0200_0A00, 0x0200_0A01)]
    );
    assert_eq!(
        ids("SELECT room_a, room_b FROM room_portals"),
        vec![(0x0200_0801, 0x0200_0802)]
    );
    assert_eq!(
        ids("SELECT room_ref, linked_ref FROM room_links"),
        vec![(0x0200_0802, 0x0200_0801)]
    );
    assert_eq!(
        ids("SELECT reference_id, room_ref FROM reference_rooms WHERE contained=1"),
        vec![(0x0200_0804, 0x0200_0801)]
    );
}

/// Every placed type in `is_placed_record_type` joins interior rooms: the six projectile,
/// hazard and flora types (`PHZD`, `PARW`, `PBAR`, `PBEA`, `PCON`, `PFLA`) each get a
/// membership row, and one outside the room box gets none.
#[test]
fn placed_projectile_and_hazard_references_join_interior_rooms() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
    let floats =
        |values: [f32; 6]| -> Vec<u8> { values.iter().flat_map(|v| v.to_le_bytes()).collect() };
    let xprm = |half: [f32; 3], shape: u32| -> Vec<u8> {
        let mut bytes: Vec<u8> = half.iter().flat_map(|v| v.to_le_bytes()).collect();
        bytes.extend([0u8; 12]);
        bytes.extend(0.2f32.to_le_bytes());
        bytes.extend(shape.to_le_bytes());
        bytes
    };
    let word = |value: u32| value.to_le_bytes().to_vec();
    let placed = |tag: &[u8; 4], id: u32, position: [f32; 3]| {
        record(
            tag,
            id,
            0,
            [
                // 0x1F and 0x20 are the room and portal markers, which are excluded from
                // membership; 0x99 is an ordinary base.
                sub(b"NAME", &word(0x99)),
                sub(
                    b"DATA",
                    &floats([position[0], position[1], position[2], 0.0, 0.0, 0.0]),
                ),
            ]
            .concat(),
        )
    };
    let cell = 0x0100_0900;
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        [
            // No XCLC and no worldspace: an interior cell.
            record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
            group(
                6,
                cell,
                group(
                    9,
                    cell,
                    [
                        record(
                            b"REFR",
                            0x0100_0801,
                            0,
                            [
                                sub(b"NAME", &word(0x1F)),
                                sub(b"DATA", &floats([0.0; 6])),
                                sub(b"XPRM", &xprm([100.0; 3], 1)),
                            ]
                            .concat(),
                        ),
                        placed(b"PHZD", 0x0100_0810, [10.0, 20.0, 30.0]),
                        placed(b"PARW", 0x0100_0811, [10.0, 20.0, 30.0]),
                        placed(b"PBAR", 0x0100_0812, [10.0, 20.0, 30.0]),
                        placed(b"PBEA", 0x0100_0813, [10.0, 20.0, 30.0]),
                        placed(b"PCON", 0x0100_0814, [10.0, 20.0, 30.0]),
                        placed(b"PFLA", 0x0100_0815, [10.0, 20.0, 30.0]),
                        // Outside the box: no membership row.
                        placed(b"PHZD", 0x0100_0820, [5000.0, 0.0, 0.0]),
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat(),
    );
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&[base, filler, patch], &db).unwrap();
    let conn = Connection::open(&db).unwrap();
    let ids = |sql: &str| -> Vec<(u32, u32)> {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        ids(
            "SELECT reference_id, room_ref FROM reference_rooms WHERE contained=1 \
              ORDER BY reference_id"
        ),
        vec![
            (0x0200_0810, 0x0200_0801),
            (0x0200_0811, 0x0200_0801),
            (0x0200_0812, 0x0200_0801),
            (0x0200_0813, 0x0200_0801),
            (0x0200_0814, 0x0200_0801),
            (0x0200_0815, 0x0200_0801),
        ]
    );
}

/// `XPOD` is an array of (origin, destination) pairs (xEdit 4.1.5f defs): a mod's portal with
/// two pairs has all four FormIDs remapped from a non-first plugin.
#[test]
fn a_two_pair_xpod_is_remapped_in_every_pair() {
    let word = |value: u32| value.to_le_bytes().to_vec();
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
    let payload = [
        word(0x0100_0801),
        word(0x0100_0802),
        word(0x0100_0803),
        word(0x0100_0804),
    ]
    .concat();
    assert_eq!(payload.len(), 16);
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        record(b"REFR", 0x0100_0900, 0, sub(b"XPOD", &payload)),
    );
    let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
    let data = &merged[&0x0200_0900]
        .subrecords
        .iter()
        .find(|(name, _)| name.as_slice() == b"XPOD")
        .expect("XPOD kept")
        .1;
    let expected = [
        word(0x0200_0801),
        word(0x0200_0802),
        word(0x0200_0803),
        word(0x0200_0804),
    ]
    .concat();
    assert_eq!(data, &expected);

    // A 12-byte XPOD (a pair and a half) is a whole number of FormIDs: kept, all three remapped.
    // The old fixed 8-byte layout dropped it.
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        record(b"REFR", 0x0100_0900, 0, sub(b"XPOD", &payload[..12])),
    );
    let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
    let data = &merged[&0x0200_0900]
        .subrecords
        .iter()
        .find(|(name, _)| name.as_slice() == b"XPOD")
        .expect("12-byte XPOD kept")
        .1;
    assert_eq!(
        data,
        &[word(0x0200_0801), word(0x0200_0802), word(0x0200_0803)].concat()
    );

    // An invalid XPOD keeps empty framing, so it cannot hide conflicting sibling fields.
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        record(b"REFR", 0x0100_0900, 0, sub(b"XPOD", &payload[..14])),
    );
    let merged = EsmParser::merge_plugins(&[base, patch]).unwrap();
    let xpods: Vec<_> = merged[&0x0100_0900]
        .subrecords
        .iter()
        .filter(|(name, _)| name.as_slice() == b"XPOD")
        .collect();
    assert_eq!(
        xpods.len(),
        1,
        "the malformed XPOD keeps one empty occurrence"
    );
    assert!(xpods[0].1.is_empty());
}

#[test]
fn broken_portal_links_do_not_become_authored_outside_endpoints() {
    for (destination, kept) in [(0u32, true), (0x0900_0802, false)] {
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
        let payload = [0x0100_0801u32.to_le_bytes(), destination.to_le_bytes()].concat();
        let patch = plugin(
            dir.path(),
            "Patch.esm",
            &["Base.esm"],
            1,
            record(b"REFR", 0x0100_0900, 0, sub(b"XPOD", &payload)),
        );
        let merged = EsmParser::merge_plugins(&[base, patch]).unwrap();
        let xpods: Vec<_> = merged[&0x0100_0900]
            .subrecords
            .iter()
            .filter(|(tag, _)| tag.as_slice() == b"XPOD")
            .collect();
        assert_eq!(xpods.len(), 1, "the source XPOD keeps one occurrence");
        assert_eq!(!xpods[0].1.is_empty(), kept);
    }
}

#[test]
fn typed_room_export_publishes_outside_only_from_source_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    let mut shape = vec![0; 32];
    for offset in [0, 4, 8] {
        shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
    }
    let marker = |id: u32, base: u32, kind: u32, endpoint: Option<u32>| {
        let mut shape = shape.clone();
        shape[28..32].copy_from_slice(&kind.to_le_bytes());
        let mut payload = [
            sub(b"NAME", &base.to_le_bytes()),
            sub(b"DATA", &[0; 24]),
            sub(b"XPRM", &shape),
        ]
        .concat();
        if id == 0x800 {
            payload.extend(sub(b"XRMR", &[2, 0, 1, 0]));
            payload.extend(sub(b"XLRM", &0x811u32.to_le_bytes()));
            payload.extend(sub(b"XLRM", &0x810u32.to_le_bytes()));
        }
        if id == 0x830 || id == 0x833 {
            payload.extend(sub(b"XMBR", &0x810u32.to_le_bytes()));
        }
        if id == 0x833 {
            payload.extend(sub(b"XMBR", &0x811u32.to_le_bytes()));
        }
        if id == 0x803 {
            payload.extend(sub(b"XPOD", &[0; 9]));
            payload.extend(sub(
                b"XPOD",
                &[0u32.to_le_bytes(), 0x0900_0800u32.to_le_bytes()].concat(),
            ));
        }
        if id == 0x804 {
            payload.extend(sub(
                b"XPOD",
                &[
                    0u32.to_le_bytes(),
                    0u32.to_le_bytes(),
                    0x0900_0800u32.to_le_bytes(),
                    0u32.to_le_bytes(),
                ]
                .concat(),
            ));
        }
        if let Some(endpoint) = endpoint {
            payload.extend(sub(
                b"XPOD",
                &[0x800u32.to_le_bytes(), endpoint.to_le_bytes()].concat(),
            ));
        }
        record(b"REFR", id, 0, payload)
    };
    plugin(
        &data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            group(
                6,
                0x900,
                group(
                    9,
                    0x900,
                    [
                        marker(0x800, 0x1F, 1, None),
                        marker(0x801, 0x20, 3, Some(0)),
                        marker(0x802, 0x20, 3, Some(0x0900_0800)),
                        marker(0x803, 0x20, 3, None),
                        marker(0x804, 0x20, 3, None),
                        marker(0x810, 0x1F, 1, None),
                        marker(0x811, 0x1F, 1, None),
                        marker(0x830, 0x17, 3, None),
                        marker(0x833, 0x17, 3, None),
                        record(
                            b"REFR",
                            0x831,
                            0x8000_0800,
                            [
                                sub(b"NAME", &0x17u32.to_le_bytes()),
                                sub(b"DATA", &[0; 24]),
                                sub(b"XPRM", &{
                                    let mut primitive = shape.clone();
                                    primitive[28..32].copy_from_slice(&3u32.to_le_bytes());
                                    primitive
                                }),
                                sub(b"XESP", &[]),
                                sub(b"XMBR", &[0; 3]),
                            ]
                            .concat(),
                        ),
                        record(
                            b"REFR",
                            0x832,
                            0,
                            [
                                sub(b"NAME", &0x17u32.to_le_bytes()),
                                sub(b"DATA", &[0; 24]),
                                sub(b"XPRM", &{
                                    let mut primitive = shape.clone();
                                    primitive[28..32].copy_from_slice(&3u32.to_le_bytes());
                                    primitive
                                }),
                                sub(b"XSCL", &[0; 3]),
                            ]
                            .concat(),
                        ),
                        record(
                            b"REFR",
                            0x820,
                            0x8000_0000,
                            [
                                sub(b"NAME", &0x999u32.to_le_bytes()),
                                sub(b"DATA", &[0; 24]),
                                sub(b"XMBR", &0x810u32.to_le_bytes()),
                            ]
                            .concat(),
                        ),
                        record(
                            b"REFR",
                            0x821,
                            0x8000_0000,
                            [
                                sub(b"NAME", &0x999u32.to_le_bytes()),
                                sub(b"DATA", &[0; 24]),
                            ]
                            .concat(),
                        ),
                        record(
                            b"REFR",
                            0x822,
                            0,
                            [
                                sub(b"NAME", &0x999u32.to_le_bytes()),
                                sub(b"DATA", &[0; 24]),
                            ]
                            .concat(),
                        ),
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat(),
    );
    let order = dir.path().join("plugins.txt");
    fs::write(&order, "*Skyrim.esm\n").unwrap();
    let output = dir.path().join("bundle");
    converter::esm::inhouse::export_record_bundle_typed(&data, &order, &output, &[]).unwrap();
    let conn = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT value FROM room_graph_stats WHERE name='occlusion_version'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT shape FROM room_occluders WHERE ref_id=2096",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        3
    );
    assert_eq!(conn.query_row("SELECT header_flags,has_enable_parent,target_ref FROM room_occluders WHERE ref_id=2097", [], |row| Ok((row.get::<_,u32>(0)?,row.get::<_,bool>(1)?,row.get::<_,Option<u32>>(2)?))).unwrap(), (0x8000_0800,true,Some(0)));
    assert_eq!(
        conn.query_row(
            "SELECT scale FROM room_occluders WHERE ref_id=2098",
            [],
            |row| row.get::<_, f32>(0)
        )
        .unwrap(),
        0.0
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM room_graph_stats WHERE name='occluders'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        3
    );
    let explicit_planes = conn
        .prepare(
            "SELECT reference_id,target_ref FROM room_reference_rules
         WHERE reference_id BETWEEN 0x830 AND 0x833 AND assignment='target' ORDER BY reference_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(explicit_planes, [(0x830, 0x810)]);
    let portal_pairs = conn
        .prepare(
            "SELECT ref_id, pair_index, room_a, room_b, unbounded_a, unbounded_b FROM room_portals
             WHERE ref_id BETWEEN 0x801 AND 0x804 ORDER BY ref_id, pair_index",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<u32>>(2)?,
                row.get::<_, Option<u32>>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        portal_pairs,
        [
            (0x801, 0, Some(0x800), None, false, true),
            (0x802, 0, Some(0x800), None, false, false),
            (0x803, 0, None, None, false, false),
            (0x804, 0, None, None, true, true),
            (0x804, 1, None, None, false, true),
        ]
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM room_graph_stats WHERE name='visibility_version'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM room_graph_stats WHERE name='assignment_version'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let links: Vec<(u32, u32)> = conn
        .prepare(
            "SELECT linked_ref,link_index FROM room_links WHERE room_ref=2048 ORDER BY link_index",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(links, [(0x811, 0), (0x810, 1)]);
    assert_eq!(
        conn.query_row(
            "SELECT flags FROM room_bounds WHERE ref_id=2048",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0x100
    );
    for (id, kind, target) in [
        (0x820, "target", Some(0x810)),
        (0x821, "none", None),
        (0x822, "auto", None),
    ] {
        let rule: (String, Option<u32>) = conn
            .query_row(
                "SELECT assignment,target_ref FROM room_reference_rules WHERE reference_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(rule, (kind.to_owned(), target));
    }
}

/// Exterior cells are told apart in the real export by a grid position or a worldspace; markers
/// in them write no room rows.
#[test]
fn load_order_export_ignores_markers_in_exterior_cells() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
    let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
    let word = |value: u32| value.to_le_bytes().to_vec();
    let xprm = |half: [f32; 3], shape: u32| -> Vec<u8> {
        let mut bytes: Vec<u8> = half.iter().flat_map(|v| v.to_le_bytes()).collect();
        bytes.extend([0u8; 12]);
        bytes.extend(0.2f32.to_le_bytes());
        bytes.extend(shape.to_le_bytes());
        bytes
    };
    let marker = |id: u32, base: u32, extra: Vec<u8>| {
        record(
            b"REFR",
            id,
            0,
            [sub(b"NAME", &word(base)), sub(b"DATA", &[0u8; 24]), extra].concat(),
        )
    };
    // A cell with its children: one room marker, one portal marker and one object.
    let cell_with_markers = |cell: u32, first: u32, cell_subs: Vec<u8>| {
        [
            record(b"CELL", cell, 0, cell_subs),
            group(
                6,
                cell,
                group(
                    9,
                    cell,
                    [
                        marker(first, 0x1F, sub(b"XPRM", &xprm([100.0; 3], 1))),
                        marker(
                            first + 1,
                            0x20,
                            [
                                sub(b"XPRM", &xprm([40.0, 1.0, 60.0], 3)),
                                sub(b"XPOD", &[word(first), word(first)].concat()),
                            ]
                            .concat(),
                        ),
                        marker(first + 2, 0x99, Vec::new()),
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat()
    };
    let grid = sub(b"XCLC", &[0u8; 8]);
    let patch = plugin(
        dir.path(),
        "Patch.esm",
        &["Base.esm"],
        1,
        [
            // A grid position, with no worldspace group.
            cell_with_markers(0x0100_0900, 0x0100_0800, grid.clone()),
            // A worldspace's persistent cell (a worldspace, no grid), and a gridded one.
            record(b"WRLD", 0x0100_0A00, 0, Vec::new()),
            group(
                1,
                0x0100_0A00,
                [
                    cell_with_markers(0x0100_0910, 0x0100_0810, Vec::new()),
                    cell_with_markers(0x0100_0920, 0x0100_0820, grid),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&[base, filler, patch], &db).unwrap();
    let conn = Connection::open(&db).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
    // The markers were parsed as references of those cells...
    assert_eq!(
        count("SELECT count(*) FROM \"references\" WHERE base_form_id IN (31, 32)"),
        6
    );
    // ...and none of them became room data.
    for table in [
        "room_bounds",
        "room_portals",
        "room_links",
        "reference_rooms",
    ] {
        assert_eq!(
            count(&format!("SELECT count(*) FROM {table}")),
            0,
            "{table}"
        );
    }
}

#[test]
fn ambiguous_occluder_names_are_rejected_by_both_readers() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();

    let marker = 0x17u32.to_le_bytes();
    let other = 0x800u32.to_le_bytes();
    let good_id = 0x805;
    let cell = 0x900;
    let mut shape = [0; 32];
    shape[..4].copy_from_slice(&12.0f32.to_le_bytes());
    shape[8..12].copy_from_slice(&12.0f32.to_le_bytes());
    shape[28..].copy_from_slice(&3u32.to_le_bytes());
    let references = [
        (0x802, vec![marker.as_slice(), &other]),
        (0x803, vec![other.as_slice(), &marker]),
        (0x804, vec![marker.as_slice(), &[0; 3]]),
        (good_id, vec![marker.as_slice()]),
    ]
    .into_iter()
    .flat_map(|(id, names)| {
        let mut payload = names
            .into_iter()
            .flat_map(|name| sub(b"NAME", name))
            .collect::<Vec<_>>();
        payload.extend(sub(b"XPRM", &shape));
        payload.extend(sub(b"DATA", &[0; 24]));
        record(b"REFR", id, 0, payload)
    })
    .collect::<Vec<_>>();
    plugin(
        &data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", 0x17, 0, sub(b"EDID", b"PlaneMarker\0")),
            record(b"STAT", 0x800, 0, sub(b"EDID", b"OtherBase\0")),
            record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
            group(6, cell, group(9, cell, references)),
        ]
        .concat(),
    );

    let published = [false, true].map(|typed| {
        let output = dir.path().join(if typed { "inhouse" } else { "legacy" });
        let conn =
            rusqlite::Connection::open(export_room_review_fixture(&data, &output, typed)).unwrap();
        conn.prepare("SELECT ref_id FROM room_occluders ORDER BY ref_id")
            .unwrap()
            .query_map([], |row| row.get::<_, u32>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    });
    assert_eq!(
        published,
        [vec![good_id], vec![good_id]],
        "legacy and in-house rows"
    );
}

fn occluder_fixture(data: &Path, half_x: f32, links: &[&[u8]]) {
    let marker = |id, half_x: f32, links: &[&[u8]]| {
        let mut shape = [0; 32];
        shape[..4].copy_from_slice(&half_x.to_le_bytes());
        shape[8..12].copy_from_slice(&12.0f32.to_le_bytes());
        shape[28..].copy_from_slice(&3u32.to_le_bytes());
        let mut payload = [sub(b"NAME", &0x17u32.to_le_bytes()), sub(b"XPRM", &shape)].concat();
        for bytes in links {
            payload.extend(sub(b"XORD", bytes));
        }
        payload.extend(sub(b"DATA", &[0; 24]));
        record(b"REFR", id, 0, payload)
    };
    let cell = 0x900;
    plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", 0x17, 0, sub(b"EDID", b"PlaneMarker\0")),
            record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
            group(
                6,
                cell,
                group(
                    9,
                    cell,
                    [marker(0x801, half_x, links), marker(0x802, 12.0, &[])].concat(),
                ),
            ),
        ]
        .concat(),
    );
}

#[test]
fn occluder_links_keep_the_source_occurrence_after_a_rejected_xord() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    let link = [0u32, 0, 0x802, 0].map(u32::to_le_bytes).concat();
    occluder_fixture(&data, 12.0, &[&[0; 3], &link]);

    for typed in [false, true] {
        let output = dir.path().join(if typed { "inhouse" } else { "legacy" });
        let conn =
            rusqlite::Connection::open(export_room_review_fixture(&data, &output, typed)).unwrap();
        let links = conn
            .prepare("SELECT link_index, linked_ref FROM room_occluder_links WHERE ref_id=0x801")
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(links, [(6, 0x802)], "typed={typed}");
        let complete: bool = conn
            .query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!complete, "typed={typed}");
    }
}

#[test]
fn occluder_link_verification_distinguishes_null_from_unusable_source() {
    let unresolved = [0u32, 0x0900_0802, 0, 0].map(u32::to_le_bytes).concat();
    for (links, complete) in [
        (&[0; 16][..], true),
        (&[0; 12], false),
        (&unresolved, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Data");
        fs::create_dir(&data).unwrap();
        occluder_fixture(&data, 12.0, &[links]);
        let db = export_room_review_fixture(&data, &dir.path().join("bundle"), true);
        let conn = rusqlite::Connection::open(db).unwrap();
        let read = |sql| conn.query_row(sql, [], |row| row.get::<_, u32>(0)).unwrap();
        assert_eq!(
            read("SELECT value FROM room_graph_stats WHERE name='occlusion_links_verified'"),
            1
        );
        assert_eq!(read("SELECT count(*) FROM room_occluder_links"), 0);
        assert_eq!(
            read("SELECT has_links FROM room_occluders WHERE ref_id=0x801"),
            1
        );
        assert_eq!(
            read("SELECT complete FROM room_graph_cells WHERE cell_id=0x900"),
            u32::from(complete)
        );
    }
}

#[tokio::test]
async fn dropped_occluder_fields_make_conversion_incomplete() {
    use converter::{AssetPipeline, config::RecordReader};

    let outgoing = [0u32, 0, 0x802, 0].map(u32::to_le_bytes).concat();
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        for (half_x, link, category, expected_refs, expected_links) in [
            (
                f32::NAN,
                outgoing.as_slice(),
                "occluder_unusable_geometry",
                &[0x802][..],
                &[(0x801, 2, 0x802)][..],
            ),
            (
                12.0,
                &[0; 12][..],
                "occluder_unusable_links",
                &[0x801, 0x802][..],
                &[][..],
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path().join("Data");
            fs::create_dir(&data).unwrap();
            occluder_fixture(&data, half_x, &[link]);
            let list = dir.path().join("plugins.txt");
            fs::write(&list, "*Skyrim.esm\n").unwrap();
            let output = dir.path().join("bundle");
            let mut config = common::cpu_lod_config(&data, &output);
            config.plugins_file = Some(list);
            config.record_reader = reader;
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let report = AssetPipeline::run_async(config, tx).await.unwrap();
            drain.await.unwrap();
            assert!(
                report
                    .warnings
                    .iter()
                    .any(|warning| { warning.contains(category) && warning.contains("00000801") }),
                "{reader:?} {category}: {:?}",
                report.warnings
            );
            assert!(!report.complete, "{reader:?} {category}");
            assert_eq!(report.skipped, 0);
            let manifest: converter::cache::ConversionManifest =
                serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                    .unwrap();
            assert!(!manifest.complete);

            let conn = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
            let refs = conn
                .prepare("SELECT ref_id FROM room_occluders ORDER BY ref_id")
                .unwrap()
                .query_map([], |row| row.get::<_, u32>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(refs, expected_refs, "{reader:?} {category}");
            let links = conn
                .prepare("SELECT ref_id, link_index, linked_ref FROM room_occluder_links")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<(u32, u32, u32)>>>()
                .unwrap();
            assert_eq!(links, expected_links, "{reader:?} {category}");
            let complete: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!complete, "{reader:?} {category}");
        }
    }
}

fn room_graph_review_fixture(
    data: &Path,
    link: Option<u32>,
    endpoints: Option<Vec<u8>>,
) -> PathBuf {
    room_graph_review_fixture_with_fields(
        data,
        link.map(|id| id.to_le_bytes().to_vec()),
        endpoints,
        Vec::new(),
    )
}

fn room_graph_review_fixture_with_fields(
    data: &Path,
    link: Option<Vec<u8>>,
    endpoints: Option<Vec<u8>>,
    references: Vec<Vec<u8>>,
) -> PathBuf {
    fs::create_dir_all(data).unwrap();
    let marker = |id: u32, base: u32, kind: u32, position: f32, extra: Vec<u8>| {
        let mut shape = vec![0; 32];
        for offset in [0, 4, 8] {
            shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
        }
        shape[28..32].copy_from_slice(&kind.to_le_bytes());
        let mut transform = vec![0; 24];
        transform[..4].copy_from_slice(&position.to_le_bytes());
        record(
            b"REFR",
            id,
            0,
            [
                sub(b"NAME", &base.to_le_bytes()),
                sub(b"DATA", &transform),
                sub(b"XPRM", &shape),
                extra,
            ]
            .concat(),
        )
    };
    let mut extra = sub(b"XRMR", &[u8::from(link.is_some()), 0, 1, 0]);
    if let Some(link) = link {
        extra.extend(sub(b"XLRM", &link));
    }
    let mut markers = vec![
        marker(0x800, 0x1F, 1, 0.0, extra),
        marker(0x801, 0x1F, 1, 500.0, Vec::new()),
    ];
    if let Some(endpoints) = endpoints {
        markers.push(marker(0x802, 0x20, 3, 100.0, endpoints));
    }
    markers.extend(references);
    plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            group(6, 0x900, group(9, 0x900, markers.concat())),
        ]
        .concat(),
    )
}

fn export_room_review_fixture(data: &Path, output: &Path, typed: bool) -> PathBuf {
    if typed {
        let order = output.parent().unwrap().join("plugins.txt");
        fs::write(&order, "*Skyrim.esm\n").unwrap();
        converter::esm::inhouse::export_record_bundle_typed(data, &order, output, &[]).unwrap();
        output.join("skyrim_world.db")
    } else {
        let db = output.with_extension("db");
        EsmParser::convert_plugins(&[data.join("Skyrim.esm")], &db).unwrap();
        db
    }
}

#[test]
fn conflicting_portal_occurrences_keep_both_readers_conservative() {
    for typed in [false, true] {
        for second in [
            None,
            Some(vec![0x800, 0x801]),
            Some(vec![0x800, 0x0900_0801]),
            Some(vec![0x800]),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let data = directory.path().join("Data");
            let mut endpoints = sub(
                b"XPOD",
                &[0x800u32.to_le_bytes(), 0u32.to_le_bytes()].concat(),
            );
            if let Some(values) = &second {
                endpoints.extend(sub(
                    b"XPOD",
                    &values
                        .iter()
                        .flat_map(|id: &u32| id.to_le_bytes())
                        .collect::<Vec<_>>(),
                ));
            }
            room_graph_review_fixture(&data, None, Some(endpoints));
            let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
            let conn = rusqlite::Connection::open(db).unwrap();
            let complete: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                complete,
                second.is_none(),
                "typed={typed}, second={second:?}"
            );
            let outside: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM room_portals WHERE unbounded_a OR unbounded_b",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(outside, i64::from(second.is_none()));
            let rooms: i64 = conn
                .query_row("SELECT COUNT(*) FROM room_bounds", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rooms, 2);
        }
    }
}

#[test]
fn ambiguous_room_headers_keep_both_readers_conservative() {
    for typed in [false, true] {
        for headers in [
            vec![],
            vec![vec![0, 0, 1, 0]],
            vec![vec![0, 0, 1, 0], vec![0, 0, 0, 0]],
            vec![vec![0, 0, 1, 0], vec![0, 0, 1, 0]],
            vec![vec![]],
            vec![vec![0, 0, 1]],
            vec![vec![0, 0, 1, 0, 0]],
        ] {
            let directory = tempfile::tempdir().unwrap();
            let data = directory.path().join("Data");
            let mut shape = vec![0; 32];
            for offset in [0, 4, 8] {
                shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
            }
            shape[28..32].copy_from_slice(&1u32.to_le_bytes());
            let mut fields = [
                sub(b"NAME", &0x1Fu32.to_le_bytes()),
                sub(b"DATA", &[0; 24]),
                sub(b"XPRM", &shape),
            ]
            .concat();
            for header in &headers {
                fields.extend(sub(b"XRMR", header));
            }
            room_graph_review_fixture_with_fields(
                &data,
                None,
                None,
                vec![record(b"REFR", 0x803, 0, fields)],
            );
            let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
            let conn = rusqlite::Connection::open(db).unwrap();
            let complete: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let expected = headers.is_empty() || (headers.len() == 1 && headers[0].len() == 4);
            assert_eq!(complete, expected, "typed={typed}, headers={headers:?}");
            let rooms: i64 = conn
                .query_row("SELECT COUNT(*) FROM room_bounds", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                rooms, 3,
                "ambiguous metadata must not discard valid room geometry"
            );
        }
    }
}

#[test]
fn authored_room_link_counts_keep_only_mismatched_cells_incomplete() {
    let cell = |id, first_room: u32, header: Option<[u8; 4]>, link_count| {
        let mut shape = vec![0; 32];
        for offset in [0, 4, 8] {
            shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
        }
        shape[28..32].copy_from_slice(&1u32.to_le_bytes());
        let room = |room_id, extra: Vec<u8>| {
            record(
                b"REFR",
                room_id,
                0,
                [
                    sub(b"NAME", &0x1Fu32.to_le_bytes()),
                    sub(b"DATA", &[0; 24]),
                    sub(b"XPRM", &shape),
                    extra,
                ]
                .concat(),
            )
        };
        let target = first_room + 1;
        let mut links = header.map_or_else(Vec::new, |bytes| sub(b"XRMR", &bytes));
        if header.is_some_and(|bytes| bytes[1] == 0xc0) {
            links.extend(sub(b"LNAM", &0u32.to_le_bytes()));
            links.extend(sub(b"INAM", &0u32.to_le_bytes()));
        }
        links.extend(sub(b"XLRM", &target.to_le_bytes()).repeat(link_count));
        let sibling_link = [
            sub(b"XRMR", &[1, 0, 1, 0]),
            sub(b"XLRM", &first_room.to_le_bytes()),
        ]
        .concat();
        [
            record(b"CELL", id, 0, sub(b"DATA", &[1, 0])),
            group(
                6,
                id,
                group(
                    9,
                    id,
                    [room(first_room, links), room(target, sibling_link)].concat(),
                ),
            ),
        ]
        .concat()
    };

    let mut failures = Vec::new();
    for typed in [false, true] {
        for (header, link_count, expected) in [
            (Some([1, 0, 0, 0]), 0, false),
            (Some([0, 0, 0, 0]), 1, false),
            (None, 1, false),
            (Some([0, 1, 0, 0]), 0, false),
            (Some([0, 1, 0, 0]), 256, true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let data = directory.path().join("Data");
            fs::create_dir_all(&data).unwrap();
            plugin(
                &data,
                "Skyrim.esm",
                &[],
                1,
                [
                    cell(0x900, 0x800, header, link_count),
                    cell(0x901, 0x810, Some([2, 0xc0, 1, 0]), 2),
                ]
                .concat(),
            );

            let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
            let conn = rusqlite::Connection::open(db).unwrap();
            assert!(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM room_links WHERE room_ref=0x801 AND linked_ref=0x800)", [], |row| row.get::<_, bool>(0)
            ).unwrap());
            let states = conn
                .prepare("SELECT cell_id,complete FROM room_graph_cells ORDER BY cell_id")
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            if states != [(0x900, expected), (0x901, true)] {
                failures.push(format!(
                    "typed={typed}, header={header:?}, XLRM count={link_count}: \
                     expected completeness {expected} and true, got {states:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn authored_room_link_order_uses_source_slots_in_both_readers() {
    use converter::config::RecordReader;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let base = plugin(data, "Skyrim.esm", &[], 1, Vec::new());
    let filler = plugin(data, "Filler.esm", &["Skyrim.esm"], 1, Vec::new());
    let header = |count, flags| sub(b"XRMR", &[count, flags, 1, 0]);
    let lighting = || sub(b"LNAM", &[0; 4]);
    let image_space = || sub(b"INAM", &[0; 4]);
    let link = |target_offset: u8| sub(b"XLRM", &[target_offset, 0, 0, 0]);
    let cases = [
        (
            "ordered with both flagged fields and a repeated target",
            vec![
                header(3, 0xc0),
                lighting(),
                image_space(),
                link(1),
                link(2),
                link(1),
            ],
            true,
            vec![(2, 1), (1, 2)],
        ),
        (
            "early link",
            vec![link(1), header(1, 0), sub(b"ZZZZ", &[0; 4])],
            false,
            vec![],
        ),
        (
            "missing lighting slot",
            vec![header(1, 0x80), link(1)],
            false,
            vec![],
        ),
        (
            "malformed flagged lighting slot",
            vec![header(1, 0x80), sub(b"LNAM", &[0; 3]), link(1)],
            false,
            vec![],
        ),
        (
            "missing image-space slot",
            vec![header(1, 0xc0), lighting(), link(1)],
            false,
            vec![],
        ),
        (
            "reversed flagged fields",
            vec![header(1, 0xc0), image_space(), lighting(), link(1)],
            false,
            vec![],
        ),
        (
            "unflagged lighting slot",
            vec![header(1, 0), lighting(), link(1)],
            false,
            vec![],
        ),
        (
            "stray image space after a valid run",
            vec![header(1, 0), link(1), image_space()],
            false,
            vec![(1, 0)],
        ),
        (
            "duplicate lighting slot",
            vec![header(1, 0x80), lighting(), lighting(), link(1)],
            false,
            vec![],
        ),
        (
            "duplicate header",
            vec![header(1, 0), link(1), header(1, 0)],
            false,
            vec![],
        ),
        ("no header", vec![link(1)], false, vec![]),
        (
            "gap removed during remapping",
            vec![header(2, 0), link(1), sub(b"XESP", &[0; 3]), link(2)],
            false,
            vec![(1, 0)],
        ),
        (
            "wrong-width link",
            vec![header(2, 0), sub(b"XLRM", &[0; 5]), link(2)],
            false,
            vec![],
        ),
        (
            "short run",
            vec![header(2, 0), link(1)],
            false,
            vec![(1, 0)],
        ),
        (
            "long run",
            vec![header(1, 0), link(1), link(2)],
            false,
            vec![(1, 0)],
        ),
        (
            "early link before a valid run",
            vec![link(2), header(1, 0), link(1)],
            false,
            vec![(1, 0)],
        ),
    ];
    let mut shape = vec![0; 32];
    for offset in [0, 4, 8] {
        shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
    }
    shape[28..32].copy_from_slice(&1u32.to_le_bytes());
    let marker = |id, fields: &[Vec<u8>]| {
        record(
            b"REFR",
            id,
            0,
            [
                sub(b"NAME", &0x1Fu32.to_le_bytes()),
                sub(b"DATA", &[0; 24]),
                sub(b"XPRM", &shape),
                fields.concat(),
            ]
            .concat(),
        )
    };
    let mut payload = Vec::new();
    for (index, (_, fields, _, _)) in cases.iter().enumerate() {
        let cell = 0x0100_0900 + index as u32;
        let source = 0x0100_1000 + 3 * index as u32;
        // Each case links to its own targets, both in the mod's file-relative slot 1.
        let mut fields = fields.clone();
        for field in &mut fields {
            if field.starts_with(b"XLRM") && field.len() == 10 {
                let offset = u32::from(field[6]);
                field[6..].copy_from_slice(&(source + offset).to_le_bytes());
            }
        }
        payload.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        payload.extend(group(
            6,
            cell,
            group(
                9,
                cell,
                [
                    marker(source, &fields),
                    marker(source + 1, &[]),
                    marker(source + 2, &[]),
                ]
                .concat(),
            ),
        ));
    }
    let patch = plugin(data, "Patch.esp", &["Skyrim.esm"], 0, payload);
    let mut failures = Vec::new();
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let db = data.join(format!("{reader:?}.db"));
        EsmParser::convert_plugins_with_reader(
            &[base.clone(), filler.clone(), patch.clone()],
            &db,
            reader,
            data,
        )
        .unwrap();
        let conn = rusqlite::Connection::open(db).unwrap();
        let mut links = conn.prepare("SELECT linked_ref,link_index FROM room_links WHERE room_ref=?1 ORDER BY link_index").unwrap();
        for (index, (name, _, expected_complete, expected_links)) in cases.iter().enumerate() {
            let cell = 0x0200_0900 + index as u32;
            let source = 0x0200_1000 + 3 * index as u32;
            let complete: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                    [cell],
                    |row| row.get(0),
                )
                .unwrap();
            let actual = links
                .query_map([source], |row| {
                    Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let expected: Vec<_> = expected_links
                .iter()
                .map(|&(target, index)| (source + target, index))
                .collect();
            if complete != *expected_complete || actual != expected {
                failures.push(format!("{reader:?} {name}: complete={complete}, links={actual:08X?}; expected complete={expected_complete}, links={expected:08X?}"));
            }
        }
        let report = converter::esm::rooms::RoomGraphReport::read(&conn).unwrap();
        let expected = converter::esm::rooms::RoomLinkAnomalies {
            count: (cases.len() - 1) as u64,
            first_marker: 0x0200_1003,
            first_cell: 0x0200_0901,
        };
        if report.room_link_anomalies != [("patch.esp".into(), expected)].into() {
            failures.push(format!(
                "{reader:?} anomaly report: {:?}",
                report.room_link_anomalies
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn room_link_anomalies_reach_both_readers_integration_reports() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let marker = |id, extra: Vec<u8>| {
        let mut shape = vec![0; 32];
        for offset in [0, 4, 8] {
            shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
        }
        shape[28..32].copy_from_slice(&1u32.to_le_bytes());
        record(
            b"REFR",
            id,
            0,
            [
                sub(b"NAME", &0x1Fu32.to_le_bytes()),
                sub(b"DATA", &[0; 24]),
                sub(b"XPRM", &shape),
                extra,
            ]
            .concat(),
        )
    };
    let base = room_graph_review_fixture_with_fields(
        &data,
        None,
        None,
        vec![
            marker(0x806, sub(b"XRMR", &[1, 0, 0, 0])),
            marker(0x804, sub(b"XRMR", &[0, 0, 0, 0]).repeat(2)),
        ],
    );
    let patch = plugin(
        &data,
        "Patch.esp",
        &["Skyrim.esm"],
        0,
        group(
            6,
            0x900,
            group(9, 0x900, marker(0x801, sub(b"XRMR", &[0; 3]))),
        ),
    );
    let order = directory.path().join("plugins.txt");
    fs::write(&order, "*Skyrim.esm\n*Patch.esp\n").unwrap();
    for typed in [false, true] {
        let output = directory
            .path()
            .join(if typed { "typed" } else { "legacy" });
        if typed {
            converter::esm::inhouse::export_record_bundle_typed(&data, &order, &output, &[])
                .unwrap();
        } else {
            fs::create_dir(&output).unwrap();
            EsmParser::convert_plugins(
                &[base.clone(), patch.clone()],
                &output.join("skyrim_world.db"),
            )
            .unwrap();
        }
        converter::integration::finalize_world_database(&output).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("integration-report.json")).unwrap())
                .unwrap();
        assert_eq!(report["room_graph"]["rooms"], 4);
        assert_eq!(
            report["room_graph"]["room_link_anomalies"],
            serde_json::json!({
                "skyrim.esm": {"count": 2, "first_marker": 0x804, "first_cell": 0x900},
                "patch.esp": {"count": 1, "first_marker": 0x801, "first_cell": 0x900},
            }),
            "typed={typed}: attribute winning overrides to the supplying plugin"
        );
    }
}

#[test]
fn failed_room_link_remapping_keeps_both_readers_conservative() {
    for typed in [false, true] {
        for link in [None, Some(0x801), Some(0x0900_0801)] {
            let directory = tempfile::tempdir().unwrap();
            let data = directory.path().join("Data");
            room_graph_review_fixture(&data, link, None);
            let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
            let conn = rusqlite::Connection::open(db).unwrap();
            let complete: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                complete,
                link != Some(0x0900_0801),
                "typed={typed}, link={link:?}"
            );
            let links: i64 = conn
                .query_row("SELECT COUNT(*) FROM room_links", [], |r| r.get(0))
                .unwrap();
            assert_eq!(links, i64::from(link == Some(0x801)));
            let rooms: i64 = conn
                .query_row("SELECT COUNT(*) FROM room_bounds", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rooms, 2);
        }
    }
}

#[test]
fn trailing_portal_endpoints_keep_both_readers_conservative() {
    for typed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let endpoints = [0x800u32, 0, 0x801]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        room_graph_review_fixture(&data, None, Some(sub(b"XPOD", &endpoints)));
        let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
        let conn = rusqlite::Connection::open(db).unwrap();
        let complete: bool = conn
            .query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !complete,
            "typed={typed}: an unmatched endpoint cannot prove complete connectivity"
        );
    }
}

#[test]
fn malformed_authored_assignments_stay_unknown_in_both_readers() {
    for typed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let mut references = Vec::new();
        let mut expected = Vec::new();
        for (type_index, record_type) in [
            b"REFR", b"ACHR", b"ACRE", b"PGRE", b"PMIS", b"PHZD", b"PARW", b"PBAR", b"PBEA",
            b"PCON", b"PFLA",
        ]
        .into_iter()
        .enumerate()
        {
            for (index, assignment) in [
                None,
                Some(vec![0; 3]),
                Some(vec![0; 4]),
                Some(0x800u32.to_le_bytes().to_vec()),
                Some([0x800u32.to_le_bytes().as_slice(), &[1]].concat()),
            ]
            .into_iter()
            .enumerate()
            {
                for (flag_index, flags) in [0, 0x8000_0000].into_iter().enumerate() {
                    let id = 0x820 + (type_index * 10 + index * 2 + flag_index) as u32;
                    let mut fields = [
                        sub(b"NAME", &0x999u32.to_le_bytes()),
                        sub(b"DATA", &[0; 24]),
                    ]
                    .concat();
                    if let Some(bytes) = &assignment {
                        fields.extend(sub(b"XMBR", bytes));
                    }
                    references.push(record(record_type, id, flags, fields));
                    let rule = match index {
                        0 if flags != 0 => "none",
                        0 => "auto",
                        3 => "target",
                        _ => "unknown",
                    };
                    expected.push((id, rule.to_owned()));
                }
            }
        }
        room_graph_review_fixture_with_fields(&data, None, None, references);
        let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
        let conn = rusqlite::Connection::open(db).unwrap();
        let rules = conn
            .prepare(
                "SELECT reference_id,assignment FROM room_reference_rules ORDER BY reference_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rules, expected,
            "typed={typed}: malformed XMBR is authored but unusable"
        );
    }
}

#[test]
fn public_load_order_export_requires_exact_multibound_target_width() {
    use converter::esm::{binary, exporter};

    let directory = tempfile::tempdir().unwrap();
    let target = 0x800u32.to_le_bytes();
    let references = [target.to_vec(), [target.as_slice(), &[1]].concat()]
        .into_iter()
        .enumerate()
        .map(|(index, bytes)| {
            record(
                b"REFR",
                0x820 + index as u32,
                0x8000_0000,
                [
                    sub(b"NAME", &0x999u32.to_le_bytes()),
                    sub(b"DATA", &[0; 24]),
                    sub(b"XMBR", &bytes),
                ]
                .concat(),
            )
        })
        .collect();
    let plugin = room_graph_review_fixture_with_fields(directory.path(), None, None, references);
    let records = binary::parse_plugin_file(&plugin)
        .unwrap()
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
    let order = LoadOrder::read(&[plugin]).unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    exporter::export_to_db_with_load_order(&conn, &records, &order).unwrap();
    let rules = conn
        .prepare("SELECT assignment,target_ref FROM room_reference_rules ORDER BY reference_id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<u32>>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rules,
        [("target".into(), Some(0x800)), ("unknown".into(), None)]
    );
}

#[test]
fn repeated_multibound_targets_stay_unknown_in_both_readers() {
    let mut unexpected = Vec::new();
    for typed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let mut references = Vec::new();
        for record_type in [b"REFR", b"ACHR"] {
            for flags in [0, 0x8000_0000] {
                for targets in [[0x800u32, 0x801], [0x800, 0x800]] {
                    let id = 0xa00 + references.len() as u32;
                    let fields = [
                        sub(b"NAME", &0x999u32.to_le_bytes()),
                        sub(b"DATA", &[0; 24]),
                        sub(b"XMBR", &targets[0].to_le_bytes()),
                        sub(b"XMBR", &targets[1].to_le_bytes()),
                    ]
                    .concat();
                    references.push(record(record_type, id, flags, fields));
                }
            }
        }
        room_graph_review_fixture_with_fields(&data, None, None, references);
        let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
        let conn = rusqlite::Connection::open(db).unwrap();
        let rules = conn
            .prepare(
                "SELECT reference_id,assignment,target_ref FROM room_reference_rules \
                 WHERE reference_id BETWEEN 0xa00 AND 0xa07 ORDER BY reference_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<u32>>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rules.len(), 8, "typed={typed}: retain every placement rule");
        unexpected.extend(
            rules
                .into_iter()
                .filter(|(_, rule, target)| rule != "unknown" || target.is_some())
                .map(|rule| (typed, rule)),
        );
    }
    assert!(
        unexpected.is_empty(),
        "repeated XMBR must not select a target in either reader: {unexpected:?}"
    );
}

#[test]
fn malformed_authored_room_links_keep_both_readers_incomplete() {
    for typed in [false, true] {
        for link in [
            None,
            Some(vec![0; 3]),
            Some(0x801u32.to_le_bytes().to_vec()),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let data = directory.path().join("Data");
            room_graph_review_fixture_with_fields(&data, link.clone(), None, Vec::new());
            let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
            let conn = rusqlite::Connection::open(db).unwrap();
            let complete: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x900",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                complete,
                link.as_ref().is_none_or(|bytes| bytes.len() == 4),
                "typed={typed}, link={link:?}"
            );
        }
    }
}

#[test]
fn incomplete_placed_payloads_keep_their_cells_open() {
    for base in [0x1Fu32, 0x999] {
        for truncated in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let data = directory.path().join("Data");
            let mut shape = vec![0; 32];
            for offset in [0, 4, 8] {
                shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
            }
            shape[28..32].copy_from_slice(&1u32.to_le_bytes());
            let room = |id| {
                record(
                    b"REFR",
                    id,
                    0,
                    [
                        sub(b"NAME", &0x1Fu32.to_le_bytes()),
                        sub(b"DATA", &[0; 24]),
                        sub(b"XPRM", &shape),
                        sub(b"XRMR", &[0, 0, 1, 0]),
                    ]
                    .concat(),
                )
            };
            let mut payload = [sub(b"NAME", &base.to_le_bytes()), sub(b"DATA", &[0; 24])].concat();
            if base == 0x1F {
                payload.extend(sub(b"XPRM", &shape));
            }
            if truncated {
                payload.extend(b"XL");
            }
            fs::create_dir_all(&data).unwrap();
            plugin(
                &data,
                "Skyrim.esm",
                &[],
                1,
                [
                    record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
                    group(
                        6,
                        0x900,
                        group(
                            9,
                            0x900,
                            [room(0x800), record(b"REFR", 0x803, 0, payload)].concat(),
                        ),
                    ),
                    record(b"CELL", 0x901, 0, sub(b"DATA", &[1, 0])),
                    group(6, 0x901, group(9, 0x901, room(0x810))),
                ]
                .concat(),
            );
            for typed in [true, false] {
                let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
                let conn = rusqlite::Connection::open(db).unwrap();
                let complete = |cell| {
                    conn.query_row(
                        "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                        [cell],
                        |row| row.get::<_, bool>(0),
                    )
                    .unwrap()
                };
                assert_eq!(
                    complete(0x900),
                    !truncated,
                    "typed={typed}, base={base:X}, truncated={truncated}"
                );
                assert!(complete(0x901), "a healthy neighbor must remain complete");
            }
        }
    }
}

#[test]
fn malformed_portal_primitives_only_disable_their_own_cells() {
    fn primitive(shape: u32) -> Vec<u8> {
        let mut bytes: Vec<_> = [40.0f32, 1.0, 60.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        bytes.resize(28, 0);
        bytes.extend(shape.to_le_bytes());
        bytes
    }
    fn cell(id: u32, aperture: &[u8]) -> Vec<u8> {
        let room_id = id + 1;
        let marker = |id, base: u32, shape: &[u8], extra: Vec<u8>| {
            record(
                b"REFR",
                id,
                0,
                [
                    sub(b"NAME", &base.to_le_bytes()),
                    sub(b"DATA", &[0; 24]),
                    sub(b"XPRM", shape),
                    extra,
                ]
                .concat(),
            )
        };
        let room = marker(room_id, 0x1f, &primitive(1), Vec::new());
        let portal = marker(
            id + 2,
            0x20,
            aperture,
            sub(
                b"XPOD",
                &[room_id.to_le_bytes(), 0u32.to_le_bytes()].concat(),
            ),
        );
        [
            record(b"CELL", id, 0, sub(b"DATA", &[1, 0])),
            group(6, id, group(9, id, [room, portal].concat())),
        ]
        .concat()
    }
    for typed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        fs::create_dir(&data).unwrap();
        plugin(
            &data,
            "Skyrim.esm",
            &[],
            1,
            [
                cell(0x800, &primitive(3)[..12]),
                cell(0x810, &primitive(1)),
                cell(0x820, &primitive(3)),
            ]
            .concat(),
        );
        let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
        let conn = rusqlite::Connection::open(db).unwrap();
        let skipped: u32 = conn
            .query_row(
                "SELECT value FROM room_graph_stats WHERE name='invalid_markers_skipped'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(skipped, 2, "typed={typed}");
        let mut stmt = conn
            .prepare(
                "SELECT cell_id, complete, half_x, half_z
                 FROM room_graph_cells LEFT JOIN room_portals USING (cell_id)
                 ORDER BY cell_id",
            )
            .unwrap();
        let cells: Vec<(u32, bool, Option<f32>, Option<f32>)> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            cells,
            [
                (0x800, false, None, None),
                (0x810, false, None, None),
                (0x820, true, Some(40.0), Some(60.0))
            ],
            "typed={typed}"
        );
    }
}

#[test]
fn typed_plane_links_require_one_source_field_with_one_nonzero_target() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    let mut shape = vec![0; 32];
    for offset in [0, 8] {
        shape[offset..offset + 4].copy_from_slice(&20f32.to_le_bytes());
    }
    shape[28..].copy_from_slice(&3u32.to_le_bytes());
    let plane = |id, links: &[[u32; 4]]| {
        let mut payload = [
            sub(b"NAME", &0x17u32.to_le_bytes()),
            sub(b"DATA", &[0; 24]),
            sub(b"XPRM", &shape),
        ]
        .concat();
        for words in links {
            payload.extend(sub(
                b"XORD",
                &words
                    .iter()
                    .flat_map(|word| word.to_le_bytes())
                    .collect::<Vec<_>>(),
            ));
        }
        record(b"REFR", id, 0x400, payload)
    };
    plugin(
        &data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            group(
                6,
                0x900,
                group(
                    8,
                    0x900,
                    [
                        plane(0x810, &[[0, 0x811, 0, 0]]),
                        plane(0x811, &[[0, 0, 0, 0x810]]),
                        plane(0x812, &[[0x811, 0, 0, 0], [0; 4]]),
                        plane(0x813, &[[0x810, 0, 0, 0x811]]),
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat(),
    );
    let order = dir.path().join("plugins.txt");
    fs::write(&order, "*Skyrim.esm\n").unwrap();
    let output = dir.path().join("bundle");
    converter::esm::inhouse::export_record_bundle_typed(&data, &order, &output, &[]).unwrap();
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let mut edges = db
        .prepare(
            "SELECT ref_id, link_index, linked_ref FROM room_occluder_links
         WHERE single_target_verified=1 ORDER BY ref_id",
        )
        .unwrap();
    let verified: Vec<(u32, u32, u32)> = edges
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(verified, [(0x810, 1, 0x811), (0x811, 3, 0x810)]);
}
