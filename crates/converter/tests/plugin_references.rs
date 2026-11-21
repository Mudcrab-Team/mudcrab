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
        let mut records = [
            record(b"STAT", 0x0100_0801, 0, Vec::new()),
            record(b"NPC_", 0x0100_0802, 0, Vec::new()),
            record(b"PROJ", 0x0100_0803, 0, Vec::new()),
        ]
        .concat();
        let mut all_cases = cases.to_vec();
        let record_types_with_common_links = [
            b"CELL", b"REFR", b"ACHR", b"ACRE", b"PGRE", b"PMIS", b"PHZD", b"PARW", b"PBAR",
            b"PBEA", b"PCON", b"PFLA",
        ];
        for record_type in record_types_with_common_links {
            for tag in [b"XOWN", b"XGLB", b"XEZN", b"XLCN", b"XLRL"] {
                all_cases.push((record_type, tag, 4));
            }
            if record_type != b"CELL" {
                all_cases.push((record_type, b"NAME", 4));
            }
        }
        let target = |kind: &[u8; 4], tag: &[u8; 4]| match (kind, tag) {
            (_, tag) if tag != b"NAME" => 0x801,
            (b"ACHR", b"NAME") => 0x802u32,
            (b"REFR" | b"ACRE", b"NAME") => 0x801,
            _ => 0x803,
        };
        for (i, (kind, tag, len)) in all_cases.iter().enumerate() {
            let mut data = vec![0xAB; *len];
            data[..4].copy_from_slice(&(0x0100_0000 | target(kind, tag)).to_le_bytes());
            let field = sub(tag, &data);
            let body = if *kind != b"CELL"
                && *tag != b"NAME"
                && record_types_with_common_links.contains(kind)
            {
                [sub(b"NAME", &0x42u32.to_le_bytes()), field].concat()
            } else {
                field
            };
            records.extend(record(kind, 0x0100_0900 + i as u32, 0, body));
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
            let fields = &merged[&(expected_prefix | (0x900 + i as u32))].subrecords;
            let mut expected = vec![0xAB; *len];
            expected[..4].copy_from_slice(&(expected_prefix | target(kind, tag)).to_le_bytes());
            assert_eq!(
                SubrecordView::new(fields).find(tag),
                Some(expected.as_slice()),
                "light={light}, {kind:?}.{tag:?}"
            );
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
            placement(
                0x0200_0800,
                0x42,
                0,
                0.0,
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
                    if record_type == b"CELL" {
                        b"LTMP"
                    } else {
                        b"STAT"
                    },
                    0x0100_0901,
                    0,
                    Vec::new(),
                ),
                record(b"STAT", 0x0100_0902, 0, Vec::new()),
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
        placement(
            0x0100_0800,
            0x42,
            0,
            0.0,
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
            placement(
                0x0100_0800,
                0x42,
                0,
                0.0,
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
        let field = sub(tag, &payload);
        let body = if !matches!(record_type, b"CELL" | b"WRLD") && tag != b"NAME" {
            [sub(b"NAME", &0x42u32.to_le_bytes()), field].concat()
        } else {
            field
        };
        let mut payload_records = record(record_type, 0x0100_0800, 0, body);
        if tag == b"NAME" {
            payload_records.extend(record(b"PROJ", 0x0100_0900, 0, Vec::new()));
        }
        let patch = plugin(dir.path(), "Patch.esm", &["Base.esm"], 1, payload_records);
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
        let field = sub(tag, &0x0100_0900u32.to_le_bytes());
        let body = if record_type == b"ACHR" {
            [sub(b"NAME", &0x42u32.to_le_bytes()), field].concat()
        } else {
            field
        };
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
        let filler = plugin(dir.path(), "Filler.esm", &["Base.esm"], 1, Vec::new());
        let patch = plugin(
            dir.path(),
            "Patch.esm",
            &["Base.esm"],
            1,
            record(record_type, 0x0100_0800, 0, body),
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
        placement(0x0100_0800, 0x42, 0, 0.0, sub(b"XTEL", &xtel)),
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
                    placement(0x0100_0800, 0x42, 0, 0.0, sub(tag, &payload)),
                    placement(
                        0x0100_0802,
                        0x42,
                        0,
                        0.0,
                        sub(b"XEMI", &0x42u32.to_le_bytes()),
                    ),
                ]
                .concat(),
            );
            let merged = EsmParser::merge_plugins(&[base, filler, patch]).unwrap();
            assert_eq!(
                SubrecordView::new(&merged[&record_id].subrecords).find(tag),
                Some(expected.as_slice())
            );
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
        plugin(
            dir.path(),
            "Second.esl",
            &[],
            0x201,
            record(b"STAT", 0x42, 0, Vec::new()),
        ),
        plugin(dir.path(), "Filler.esm", &[], 1, Vec::new()),
        plugin(
            dir.path(),
            "Patch.esp",
            &["Second.esl", "Base.esm"],
            0,
            placement(
                0x0200_0800,
                0x42,
                0,
                0.0,
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
        SubrecordView::new(&merged[&0x0200_0800].subrecords)
            .find(b"XLRT")
            .unwrap(),
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
            placement(0x800, 0x801, 0, 0.0, sub(b"XESP", &payload)),
            record(b"STAT", 0x801, 0, sub(b"XTEL", &[1, 2, 3])),
        ]
        .concat(),
    );
    let merged = EsmParser::merge_plugins(&[base, patch]).unwrap();
    assert_eq!(
        SubrecordView::new(&merged[&0x0100_0800].subrecords).find(b"XESP"),
        Some(payload.as_slice())
    );
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
                                sub(b"NAME", &0x0100_0003u32.to_le_bytes()),
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
                        sub(b"NAME", &0x0000_0003u32.to_le_bytes()),
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
        placement(0x0100_0900, 0x20, 0, 0.0, sub(b"XPOD", &payload)),
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
        placement(0x0100_0900, 0x20, 0, 0.0, sub(b"XPOD", &payload[..12])),
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
        placement(0x0100_0900, 0x20, 0, 0.0, sub(b"XPOD", &payload[..14])),
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
fn invalid_room_names_keep_the_predecessors_and_uncertify_both_cells() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let room_name = sub(b"NAME", &0x1fu32.to_le_bytes());
    let static_name = sub(b"NAME", &0x830u32.to_le_bytes());
    let short_name = sub(b"NAME", &[0x1f, 0, 0]);
    let long_name = sub(b"NAME", &[0x1f, 0, 0, 0, 0]);
    let cases = [
        ("missing", Vec::new()),
        ("short", short_name.clone()),
        ("long", long_name.clone()),
        (
            "room first",
            [room_name.clone(), static_name.clone()].concat(),
        ),
        ("static first", [static_name, room_name.clone()].concat()),
        (
            "short first",
            [short_name.clone(), room_name.clone()].concat(),
        ),
        ("short last", [room_name.clone(), short_name].concat()),
        (
            "long first",
            [long_name.clone(), room_name.clone()].concat(),
        ),
        ("long last", [room_name.clone(), long_name].concat()),
    ];
    let predecessor = 0x900;
    let neighbor = 0x901;
    let mut placed = record(b"STAT", 0x830, 0, sub(b"EDID", b"OrdinaryStatic\0"));
    placed.extend(record(b"CELL", predecessor, 0, sub(b"DATA", &[1, 0])));
    placed.extend(record(b"CELL", neighbor, 0, sub(b"DATA", &[1, 0])));
    placed.extend(placement_group(
        neighbor,
        placement_marker(0x880, 0x1f, Vec::new()),
    ));
    let mut patch_contents = Vec::new();
    for (index, (_, fields)) in cases.iter().enumerate() {
        let id = 0x800 + index as u32;
        let cell = 0x910 + index as u32;
        let marker = placement_marker(id, 0x1f, Vec::new());
        placed.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        placed.extend(placement_group(
            cell,
            placement_marker(0x8a0 + index as u32, 0x1f, Vec::new()),
        ));
        placed.extend(placement_group(predecessor, marker.clone()));
        let malformed = record(
            b"REFR",
            id,
            0,
            [fields, &marker[24 + room_name.len()..]].concat(),
        );
        patch_contents.extend(placement_group(cell, malformed));
    }
    let base = plugin(data, "Skyrim.esm", &[], 1, placed);
    let patch = plugin(data, "Malformed.esp", &["Skyrim.esm"], 0, patch_contents);
    let check = |db: &rusqlite::Connection, context: &str| {
        assert!(placement_certificate(db, neighbor), "{context}");
        assert_eq!(
            stored_placement_certificate(db, predecessor),
            Some(false),
            "{context}"
        );
        for (index, (name, _)) in cases.iter().enumerate() {
            let id = 0x800 + index as u32;
            let cell = 0x910 + index as u32;
            assert_eq!(
                stored_placement_certificate(db, cell),
                Some(false),
                "{context}: {name}"
            );
            assert_eq!(
                placement_winner_cell(db, id),
                Some(predecessor),
                "{context}: {name}"
            );
            let room_cell: u32 = db
                .query_row(
                    "SELECT cell_id FROM room_bounds WHERE ref_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(room_cell, predecessor, "{context}: {name}");
        }
    };
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let path = data.join(format!("{reader:?}.db"));
        EsmParser::convert_plugins_with_reader(&[base.clone(), patch.clone()], &path, reader, data)
            .unwrap();
        check(
            &rusqlite::Connection::open(path).unwrap(),
            &format!("{reader:?}"),
        );
    }
    let assets = data.join("incremental");
    fs::create_dir(&assets).unwrap();
    let path = assets.join("skyrim_world.db");
    EsmParser::convert_plugins_with_reader(&[base], &path, RecordReader::Inhouse, data).unwrap();
    let result = run_placement_import(&assets, "--plugin", &patch);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    check(
        &rusqlite::Connection::open(path).unwrap(),
        "incremental plugin import",
    );
}

#[test]
fn broken_portal_links_do_not_become_authored_outside_endpoints() {
    for destination in [0u32, 0x0900_0802, 0x0100_0000] {
        let dir = tempfile::tempdir().unwrap();
        let base = plugin(dir.path(), "Base.esm", &[], 1, Vec::new());
        let other = plugin(dir.path(), "Other.esm", &[], 1, Vec::new());
        let payload = [0x0100_0801u32.to_le_bytes(), destination.to_le_bytes()].concat();
        let patch = plugin(
            dir.path(),
            "Patch.esm",
            &["Other.esm", "Base.esm"],
            1,
            placement(0x0200_0900, 0x0100_0020, 0, 0.0, sub(b"XPOD", &payload)),
        );
        let merged = EsmParser::merge_plugins(&[base, other, patch]).unwrap();
        let xpods: Vec<_> = merged[&0x0200_0900]
            .subrecords
            .iter()
            .filter(|(tag, _)| tag.as_slice() == b"XPOD")
            .collect();
        assert_eq!(xpods.len(), 1, "the source XPOD keeps one occurrence");
        if destination == 0 {
            assert_eq!(
                xpods[0].1,
                [0x0000_0801u32.to_le_bytes(), 0u32.to_le_bytes()].concat()
            );
        } else {
            assert!(xpods[0].1.is_empty());
        }
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
            record(b"STAT", 0x999, 0, Vec::new()),
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
            "SELECT count(*) FROM room_occluders WHERE ref_id=0x832",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM room_graph_stats WHERE name='occluders'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        3, // Includes the plane with two XMBR fields. Only its ownership is ambiguous.
    );
    let reason: String = conn.query_row(
        "SELECT reason FROM projection_omission_samples WHERE category='occluder_unusable_geometry' AND form_id=0x832",
        [],
        |row| row.get(0),
    ).unwrap();
    assert!(reason.contains("XSCL"), "{reason}");
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
    for (links, complete, verified) in [
        (&[0; 16][..], true, 1),
        (&[0; 12], false, 0),
        (&unresolved, false, 1),
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
            verified
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

#[test]
fn truncated_occluder_link_to_another_cell_clears_pack_verification() {
    for truncated in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Data");
        fs::create_dir(&data).unwrap();
        let mut field = sub(b"XORD", &[0x802u32, 0, 0, 0].map(u32::to_le_bytes).concat());
        if truncated {
            field.pop();
        }
        plugin(
            &data,
            "Skyrim.esm",
            &[],
            1,
            [
                record(b"STAT", 0x17, 0, sub(b"EDID", b"PlaneMarker\0")),
                record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
                placement_group(0x900, placement_marker(0x801, 0x17, field)),
                record(b"CELL", 0x901, 0, sub(b"DATA", &[1, 0])),
                placement_group(0x901, placement_marker(0x802, 0x17, Vec::new())),
            ]
            .concat(),
        );
        let db = export_room_review_fixture(&data, &dir.path().join("bundle"), true);
        let conn = rusqlite::Connection::open(db).unwrap();
        let read = |sql| conn.query_row(sql, [], |row| row.get::<_, u32>(0)).unwrap();
        assert_eq!(
            read("SELECT count(*) FROM room_occluders WHERE ref_id=0x802 AND cell_id=0x901"),
            1
        );
        assert_eq!(
            read(
                "SELECT count(*) FROM room_occluder_links WHERE ref_id=0x801 AND linked_ref=0x802"
            ),
            u32::from(!truncated)
        );
        assert_eq!(
            read("SELECT value FROM room_graph_stats WHERE name='occlusion_links_verified'"),
            u32::from(!truncated)
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
            record(b"STAT", 0x999, 0, Vec::new()),
            record(b"NPC_", 0x99a, 0, Vec::new()),
            record(b"PROJ", 0x99b, 0, Vec::new()),
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
fn first_placement_without_name_is_a_reader_link_target_but_has_no_runtime_row() {
    use converter::{config::RecordReader, records::read_plugins};

    let dir = tempfile::tempdir().unwrap();
    let reference = 0x802u32;
    let control = 0x803u32;
    let package = 0x804u32;
    let cell = 0x900u32;
    let location = [0u32.to_le_bytes(), reference.to_le_bytes(), [0; 4]].concat();
    let paths = [plugin(
        dir.path(),
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", PLACEMENT_STAT, 0, Vec::new()),
            record(b"CELL", cell, 0, sub(b"XCLC", &[0; 8])),
            placement_group(
                cell,
                [
                    record(b"REFR", reference, 0, sub(b"DATA", &[0; 24])),
                    placement(control, PLACEMENT_STAT, 0, 0.0, Vec::new()),
                ]
                .concat(),
            ),
            record(
                b"PACK",
                package,
                0,
                [sub(b"ANAM", b"Location\0"), sub(b"PLDT", &location)].concat(),
            ),
        ]
        .concat(),
    )];

    let order = LoadOrder::read(&paths).unwrap();
    let read = read_plugins(&paths, &order).unwrap();
    let placed = &read.records[&reference];
    assert_eq!(placed.record_type, *b"REFR");
    assert_eq!(placed.cell_form_id, Some(cell));
    assert!(
        !placed
            .fields
            .iter()
            .any(|field| field.signature == *b"NAME")
    );
    let raw_package = read.records[&package].to_raw_record();
    let view = SubrecordView::new(&raw_package.subrecords);
    assert_eq!(view.find(b"PLDT"), Some(location.as_slice()));

    let db_path = dir.path().join("world.db");
    EsmParser::convert_plugins_with_reader(&paths, &db_path, RecordReader::Inhouse, dir.path())
        .unwrap();
    let db = rusqlite::Connection::open(db_path).unwrap();
    let rows: (bool, bool, bool, bool) = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM records WHERE form_id=?1),
                    EXISTS(SELECT 1 FROM [references] WHERE id=?1),
                    EXISTS(SELECT 1 FROM exterior_spatial WHERE id=?1),
                    EXISTS(SELECT 1 FROM [references] r JOIN exterior_spatial s USING(id)
                           WHERE r.id=?2 AND r.base_form_id=?3 AND s.cell_id=?4)",
            [reference, control, PLACEMENT_STAT, cell],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(rows, (false, false, false, true));
    assert_eq!(database_field(&db, "records", package, b"PLDT"), [0; 12]);
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
fn room_transform_and_link_damage_is_local_to_the_source_cell() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    fs::create_dir(&data).unwrap();
    let mut records = Vec::new();
    for (cell, source, target, repeated) in [
        (0x900, 0x800, 0x801u32, false),
        (0x910, 0x810, 0x801, false),
        (0x920, 0x820, 0x8ff, false),
        (0x930, 0x830, 0x831, true),
    ] {
        records.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        let mut fields = sub(b"XRMR", &[1, 0, 0, 0]);
        fields.extend(sub(b"XLRM", &target.to_le_bytes()));
        if repeated {
            fields.extend(sub(b"DATA", &[0; 24]));
        }
        records.extend(placement_group(
            cell,
            [
                placement_marker(source, 0x1f, fields),
                placement_marker(source + 1, 0x1f, vec![]),
            ]
            .concat(),
        ));
    }
    let mut box_shape = [0; 32];
    for component in box_shape[..12].as_chunks_mut::<4>().0 {
        component.copy_from_slice(&100f32.to_le_bytes());
    }
    box_shape[28..].copy_from_slice(&1u32.to_le_bytes());
    let mut oversized = box_shape.to_vec();
    oversized.push(0);
    for (cell, marker) in [
        (
            0x940,
            placement_marker(0x840, 0x1f, sub(b"XPRM", &box_shape)),
        ),
        (
            0x950,
            placement(0x850, 0x1f, 0, 0.0, sub(b"XPRM", &oversized)),
        ),
    ] {
        records.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        records.extend(placement_group(cell, marker));
    }
    plugin(&data, "Skyrim.esm", &[], 1, records);
    for (name, typed) in [("legacy", false), ("typed", true)] {
        let db = rusqlite::Connection::open(export_room_review_fixture(
            &data,
            &directory.path().join(name),
            typed,
        ))
        .unwrap();
        let mut cells = db
            .prepare(
                "SELECT c.cell_id,c.complete,count(b.ref_id) FROM room_graph_cells c
             LEFT JOIN room_bounds b ON b.cell_id=c.cell_id GROUP BY c.cell_id ORDER BY c.cell_id",
            )
            .unwrap();
        let rows = cells
            .query_map([], |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            [
                (0x900, true, 2),
                (0x910, false, 2),
                (0x920, false, 2),
                (0x930, false, 1),
                (0x940, false, 0),
                (0x950, false, 0)
            ],
            "{name}"
        );
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
            marker(
                0x805,
                [sub(b"XRMR", &[1, 0, 0, 0]), sub(b"XLRM", &[0; 4])].concat(),
            ),
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
        assert_eq!(report["room_graph"]["rooms"], 5);
        assert_eq!(
            report["room_graph"]["room_link_anomalies"],
            serde_json::json!({
                "skyrim.esm": {"count": 3, "first_marker": 0x804, "first_cell": 0x900},
                "patch.esp": {"count": 1, "first_marker": 0x801, "first_cell": 0x900},
            }),
            "typed={typed}: attribute winning overrides to the supplying plugin"
        );
    }
    // The same source populates the full projection above and exercises both archive modes.
    let work = directory.path().join("archive-work");
    for (name, working) in [("archive", None), ("archive-copied", Some(work.as_path()))] {
        let output = directory.path().join(name);
        converter::esm::inhouse::export_record_bundle_typed_with_options(
            &data,
            &order,
            &output,
            &[],
            working,
            converter::esm::inhouse::RecordBundleMode::RecordArchiveOnly,
        )
        .unwrap();
        let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        let rows: u32 = db
            .query_row("SELECT count(*) FROM room_link_anomalies", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "{name}");
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
        let flags: (bool, bool) = conn
            .query_row(
                "SELECT unbounded_a,unbounded_b FROM room_portals WHERE ref_id=0x802 AND pair_index=0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(flags, (false, false), "typed={typed}");
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
            let base: u32 = match record_type {
                b"REFR" | b"ACRE" => 0x999,
                b"ACHR" => 0x99a,
                _ => 0x99b,
            };
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
                    let mut fields =
                        [sub(b"NAME", &base.to_le_bytes()), sub(b"DATA", &[0; 24])].concat();
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
fn rejected_assignment_correction_rolls_back_the_graph_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    let source = room_graph_review_fixture_with_fields(
        &data,
        None,
        None,
        vec![placement(0x820, 0x999, 0, 0., sub(b"XMBR", &[1, 2, 3]))],
    );
    let database = dir.path().join("world.db");
    let export = || {
        EsmParser::convert_plugins_with_reader(
            std::slice::from_ref(&source),
            &database,
            converter::config::RecordReader::Inhouse,
            &data,
        )
    };
    export().unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT assignment FROM room_reference_rules WHERE reference_id=0x820",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unknown"
    );
    db.execute_batch("INSERT INTO room_graph_stats(name,value) VALUES('previous',19);
        CREATE TRIGGER fail_correction BEFORE UPDATE ON room_reference_rules WHEN NEW.assignment='unknown'
        BEGIN SELECT RAISE(ABORT,'test correction failure'); END;").unwrap();
    let err = export().unwrap_err();
    assert!(
        format!("{err:#}").contains("test correction failure"),
        "{err:#}"
    );
    assert_eq!(
        db.query_row(
            "SELECT value FROM room_graph_stats WHERE name='previous'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        19
    );
}

#[test]
fn rejected_native_primitive_keeps_unknown_room_assignment() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let mut primitive = [0; 32];
    primitive[28..].copy_from_slice(&3u32.to_le_bytes());
    let oversized = [primitive.as_slice(), &[0]].concat();
    room_graph_review_fixture_with_fields(
        &data,
        None,
        None,
        vec![
            placement(0x820, 0x999, 0, 0.0, sub(b"XPRM", &oversized)),
            placement(0x821, 0x999, 0, 0.0, sub(b"XPRM", &primitive)),
            placement(0x822, 0x999, 0, 0.0, Vec::new()),
        ],
    );
    let db = export_room_review_fixture(&data, &directory.path().join("bundle"), true);
    let conn = rusqlite::Connection::open(db).unwrap();
    assert_eq!(
        placement_rules(&conn, 0x900),
        [
            (0x820, "unknown".into(), None),
            (0x821, "unknown".into(), None),
            (0x822, "auto".into(), None),
        ]
    );
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
fn placement_cycle_recovers_static_before_checking_its_dependent() {
    use converter::config::RecordReader;
    use rusqlite::OptionalExtension;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            record(b"CELL", 0x901, 0, sub(b"DATA", &[1, 0])),
            record(b"STAT", 0x830, 0, sub(b"EDID", b"OriginalStatic\0")),
        ]
        .concat(),
    );
    let patch = plugin(
        data,
        "Patch.esp",
        &["Skyrim.esm"],
        0,
        [
            placement_group(0x901, placement(0x830, 0x850, 0, 0.0, Vec::new())),
            placement_group(0x901, placement(0x851, 0x830, 0, 2.0, Vec::new())),
            placement_group(0x900, placement(0x850, 0x830, 0, 1.0, Vec::new())),
        ]
        .concat(),
    );
    let second_override = plugin(
        data,
        "Later.esp",
        &["Skyrim.esm", "Patch.esp"],
        0,
        placement_group(0x901, placement(0x830, 0x850, 0, 3.0, Vec::new())),
    );
    let mut failures = Vec::new();
    let mut layers = vec![base, patch];
    for extra in [None, Some(second_override)] {
        layers.extend(extra);
        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let output = data.join("cycle.db");
            EsmParser::convert_plugins_with_reader(&layers, &output, reader, data).unwrap();
            let db = rusqlite::Connection::open(output).unwrap();
            let winner = db
                .query_row(
                    "SELECT cell_id, base_form_id FROM [references] WHERE id=0x850",
                    [],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
                )
                .optional()
                .unwrap();
            if winner != Some((0x900, 0x830)) {
                failures.push(format!(
                    "{reader:?}, {} layers: missing dependent on restored STAT: {winner:?}",
                    layers.len()
                ));
            }
            assert_eq!(placement_winner_cell(&db, 0x851), Some(0x901), "{reader:?}");
            assert_eq!(placement_winner_cell(&db, 0x830), None, "{reader:?}");
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn placement_cycle_recovers_queued_static_before_checking_its_dependent() {
    use converter::config::RecordReader;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            placement_group(0x900, placement(0x830, 0x850, 0, 0.0, Vec::new())),
            placement_group(0x900, placement(0x850, 0x830, 0, 0.0, Vec::new())),
            record(b"STAT", 0x830, 0, sub(b"EDID", b"RecoveredStatic\0")),
        ]
        .concat(),
    );
    let output = data.join("cycle.db");
    EsmParser::convert_plugins_with_reader(&[base], &output, RecordReader::Inhouse, data).unwrap();
    let db = rusqlite::Connection::open(output).unwrap();
    assert_eq!(placement_winner_cell(&db, 0x850), Some(0x900));
    let kind: String = db
        .query_row(
            "SELECT record_type FROM records WHERE form_id=0x830",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "STAT");
}

#[test]
fn unresolved_parent_of_a_new_placement_keeps_interiors_open_until_superseded() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cells = [0x900, 0x901];
    let base = plugin(
        root,
        "Skyrim.esm",
        &[],
        1,
        cells
            .iter()
            .flat_map(|&cell| {
                [
                    record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
                    placement_group(cell, placement_marker(cell - 0x100, 0x1f, Vec::new())),
                ]
                .concat()
            })
            .collect(),
    );
    let damaged = plugin(
        root,
        "Bad.esp",
        &["Skyrim.esm"],
        0,
        placement_group(0x9900_0900, placement_marker(0x0100_0880, 0x1f, Vec::new())),
    );
    let fixed = plugin(
        root,
        "Fixed.esp",
        &["Skyrim.esm", "Bad.esp"],
        0,
        record(b"REFR", 0x0100_0880, 0x20, Vec::new()),
    );
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        for recovered in [false, true] {
            let db_path = root.join(format!("{reader:?}-{recovered}.db"));
            let mut paths = vec![base.clone(), damaged.clone()];
            if recovered {
                paths.push(fixed.clone());
            }
            EsmParser::convert_plugins_with_reader(&paths, &db_path, reader, root).unwrap();
            let db = rusqlite::Connection::open(db_path).unwrap();
            for cell in cells {
                assert_eq!(
                    placement_certificate(&db, cell),
                    recovered,
                    "{reader:?} {cell:x}"
                );
            }
        }
    }
}

#[test]
fn bounded_damaged_cell_preserves_the_winner_and_invalidates_its_graph() {
    use converter::config::RecordReader;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [0x900, 0x901]
            .into_iter()
            .flat_map(|cell| {
                [
                    record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
                    placement_group(cell, placement_marker(cell - 0x100, 0x1f, Vec::new())),
                ]
                .concat()
            })
            .collect(),
    );
    for (id, other_certified) in [(0x900, true), (0xff00_0900, false)] {
        let patch = plugin(
            data,
            "Damaged.esp",
            &["Skyrim.esm"],
            0,
            record(b"CELL", id, 0, vec![0]),
        );
        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let output = data.join(format!("graph-{reader:?}-{id}.db"));
            EsmParser::convert_plugins_with_reader(
                &[base.clone(), patch.clone()],
                &output,
                reader,
                data,
            )
            .unwrap();
            let db = rusqlite::Connection::open(output).unwrap();
            assert!(!placement_certificate(&db, 0x900));
            assert_eq!(placement_certificate(&db, 0x901), other_certified);
            let count: usize = db
                .query_row("SELECT count(*) FROM cells WHERE id=0x900", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(count, 1);
        }
    }
}

#[test]
fn typed_placement_duplicates_claim_the_slot_only_after_admission() {
    use converter::config::RecordReader;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let portal = |room: u32| {
        placement_marker(
            0x850,
            0x20,
            sub(b"XPOD", &[room.to_le_bytes(), [0; 4]].concat()),
        )
    };
    let mut contents = Vec::new();
    for (cell, room) in [(0x900, 0x800), (0x901, 0x810), (0x902, 0x820)] {
        contents.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        let mut placements = placement_marker(room, 0x1f, Vec::new());
        if cell == 0x900 {
            placements.extend(portal(room));
        }
        contents.extend(placement_group(cell, placements));
    }
    let base = plugin(data, "Skyrim.esm", &[], 1, contents);
    let invalid = placement_group(0x901, placement(0x850, 0x832, 0, 0.0, Vec::new()));
    let invalid_transform = placement_group(0x901, placement(0x850, 0x20, 0, f32::NAN, Vec::new()));
    let first = placement_group(0x901, portal(0x810));
    let second = placement_group(0x902, portal(0x820));
    let deletion = record(b"REFR", 0x850, 0x20, Vec::new());
    let cases = [
        (
            "late rejection",
            vec![&invalid, &second],
            Some(0x902),
            vec!["skipped", "accepted"],
        ),
        (
            "first usable fallback in file order",
            vec![&invalid, &invalid_transform, &first, &second],
            Some(0x901),
            vec!["skipped", "skipped", "accepted", "skipped"],
        ),
        (
            "valid first",
            vec![&first, &second],
            Some(0x901),
            vec!["accepted", "skipped"],
        ),
        (
            "late rejection before deletion",
            vec![&invalid, &deletion],
            None,
            vec!["skipped", "accepted"],
        ),
        (
            "valid first before deletion",
            vec![&first, &deletion],
            Some(0x901),
            vec!["accepted", "skipped"],
        ),
        (
            "deletion first",
            vec![&deletion, &second],
            None,
            vec!["accepted", "skipped"],
        ),
    ];
    let superseding = plugin(data, "Superseding.esp", &["Skyrim.esm"], 0, second.clone());
    let mut failures = Vec::new();
    for (name, placements, expected_cell, expected_history) in cases {
        let patch = plugin(
            data,
            "Patch.esp",
            &["Skyrim.esm"],
            0,
            placements.into_iter().flatten().copied().collect(),
        );
        let output = data.join("duplicates.db");
        for (plugins, expected_cell) in [
            (vec![base.clone(), patch.clone()], expected_cell),
            (vec![base.clone(), patch, superseding.clone()], Some(0x902)),
        ] {
            EsmParser::convert_plugins_with_reader(&plugins, &output, RecordReader::Inhouse, data)
                .unwrap();
            let db = rusqlite::Connection::open(&output).unwrap();
            let winner = placement_winner_cell(&db, 0x850);
            let projected = db
            .prepare("SELECT cell_id, pair_index FROM room_portals WHERE ref_id=0x850 ORDER BY cell_id, pair_index")
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
            let expected_pairs = expected_cell.map(|cell| (cell, 0));
            let history = db
                .prepare(
                    "SELECT status FROM record_overrides WHERE form_id=0x850 AND plugin_id=1
                 ORDER BY chain_position",
                )
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            if winner != expected_cell
                || projected != expected_pairs.as_slice()
                || history != expected_history
            {
                failures.push(format!(
                "{name} with {} plugins: winner={winner:?}, portal={projected:?}, history={history:?}; expected {expected_cell:?}, {expected_history:?}",
                plugins.len()
            ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn repeated_multibound_targets_stay_unknown_in_both_readers() {
    let mut unexpected = Vec::new();
    for typed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let mut references = Vec::new();
        for (record_type, base) in [(b"REFR", 0x999u32), (b"ACHR", 0x99a)] {
            for flags in [0, 0x8000_0000] {
                for targets in [[0x800u32, 0x801], [0x800, 0x800]] {
                    let id = 0xa00 + references.len() as u32;
                    let fields = [
                        sub(b"NAME", &base.to_le_bytes()),
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
                    record(b"STAT", 0x999, 0, Vec::new()),
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

#[test]
fn discarded_frames_and_bounded_records_preserve_room_graph_scope() {
    use converter::config::RecordReader;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let a = 0x900;
    let b = 0x901;
    let c = 0x902;
    let cells = [(a, 0x800u32), (b, 0x810), (c, 0x820)];
    let moved_portal = |room: u32| {
        placement_marker(
            0x870,
            0x20,
            sub(b"XPOD", &[room.to_le_bytes(), 0u32.to_le_bytes()].concat()),
        )
    };
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        cells
            .into_iter()
            .flat_map(|(cell, room)| {
                [
                    record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
                    placement_group(
                        cell,
                        [
                            placement_marker(room, 0x1f, Vec::new()),
                            if cell == b {
                                moved_portal(room)
                            } else {
                                Vec::new()
                            },
                        ]
                        .concat(),
                    ),
                ]
                .concat()
            })
            .collect(),
    );
    let portal = |room: u32| {
        let endpoints = [room.to_le_bytes(), 0u32.to_le_bytes()].concat();
        placement_marker(room + 1, 0x20, sub(b"XPOD", &endpoints))
    };
    let mut bad_group = group(6, a, Vec::new());
    bad_group[4..8].copy_from_slice(&0xffffu32.to_le_bytes());
    let mut overrun = record(b"NAVM", 0x0100_0803, 0, Vec::new());
    overrun[4..8].copy_from_slice(&0xffffu32.to_le_bytes());
    let unknown_frame = |prefix| {
        group(
            0,
            u32::from_le_bytes(*b"CELL"),
            [prefix, placement_group(b, portal(0x810))].concat(),
        )
    };
    let broken_compressed_record = |tag: &[u8; 4], flags: u32| {
        record(
            tag,
            0x0100_0804,
            flags | 0x40000,
            vec![4, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
        )
    };
    let cases = [
        (
            "bad-group",
            unknown_frame(bad_group.clone()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "unscoped-navmesh-overrun",
            unknown_frame(overrun.clone()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "cell-navmesh-overrun",
            [
                placement_group(a, [overrun.clone(), moved_portal(0x800)].concat()),
                placement_group(b, portal(0x810)),
            ]
            .concat(),
            [false, false, false],
            [false, true, true],
        ),
        (
            "cell-group-hides-move",
            placement_group(a, [bad_group.clone(), moved_portal(0x800)].concat()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "cell-group-hides-partial-portal-header",
            placement_group(
                a,
                [bad_group.clone(), moved_portal(0x800)[..20].to_vec()].concat(),
            ),
            [false, false, true],
            [false, false, true],
        ),
        (
            "invalid-group-hides-partial-cell-group",
            placement_group(
                a,
                [bad_group.clone(), group(9, b, Vec::new())[..16].to_vec()].concat(),
            ),
            [false, false, true],
            [false, false, true],
        ),
        (
            "invalid-group-hides-partial-kind",
            placement_group(
                a,
                [bad_group.clone(), group(9, b, Vec::new())[..15].to_vec()].concat(),
            ),
            [false, false, true],
            [false, false, true],
        ),
        (
            "partial-group-tag",
            placement_group(a, b"GRU".to_vec()),
            [false; 3],
            [false, false, true],
        ),
        (
            "partial-group-label-without-kind",
            placement_group(a, group(9, b, Vec::new())[..12].to_vec()),
            [false, false, true],
            [false, false, true],
        ),
        (
            "partial-group-label",
            placement_group(a, group(9, b, Vec::new())[..11].to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "partial-kind-excludes-cell",
            placement_group(a, group(7, b, Vec::new())[..13].to_vec()),
            [false, true, true],
            [false, false, true],
        ),
        (
            "cell-group-hides-short-nonplacement-tail",
            placement_group(a, [bad_group, overrun[..20].to_vec()].concat()),
            [false, true, true],
            [false, false, true],
        ),
        (
            "partial-portal-header",
            placement_group(a, moved_portal(0x800)[..20].to_vec()),
            [false, false, true],
            [false, false, true],
        ),
        (
            "overrun-with-complete-portal-header",
            placement_group(a, {
                let mut header = moved_portal(0x800)[..24].to_vec();
                header[4..8].copy_from_slice(&100u32.to_le_bytes());
                header
            }),
            [false, false, true],
            [false, false, true],
        ),
        (
            "possible-reference-one",
            placement_group(a, b"R".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "possible-reference-two",
            placement_group(a, b"RE".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "possible-actor",
            placement_group(a, b"A".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "possible-character",
            placement_group(a, b"AC".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "possible-projectile",
            placement_group(a, b"PMI".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "possible-grenade",
            placement_group(a, b"PGR".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "possible-placement-tag",
            placement_group(a, b"REF".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "non-placement-tag-prefix",
            placement_group(a, b"REG".to_vec()),
            [false, true, true],
            [false, false, true],
        ),
        (
            "placement-tag-alone",
            placement_group(a, b"REFR".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "partial-portal-id",
            placement_group(a, moved_portal(0x800)[..15].to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "short-cell-record",
            placement_group(a, record(b"CELL", b, 0, Vec::new())[..16].to_vec()),
            [false, false, true],
            [false, false, true],
        ),
        (
            "short-cell-record-without-id",
            placement_group(a, record(b"CELL", b, 0, Vec::new())[..15].to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "partial-cell-tag",
            placement_group(a, b"CEL".to_vec()),
            [false, false, false],
            [false, false, true],
        ),
        (
            "parentless-placement-with-unknown-id",
            record(
                b"REFR",
                0x0900_0850,
                0,
                [sub(b"NAME", &[0; 3]), sub(b"DATA", &[0; 24])].concat(),
            ),
            [false, false, false],
            [false, false, true],
        ),
        (
            "unreadable-cell-override",
            record(b"CELL", b, 0x40000, vec![4, 0, 0, 0, 0xff]),
            [true, false, true],
            [false, false, true],
        ),
        (
            "partial-cell-group",
            placement_group(a, group(9, b, Vec::new())[..16].to_vec()),
            [false, false, true],
            [false, false, true],
        ),
        (
            "partial-topic-group",
            placement_group(a, group(7, b, Vec::new())[..16].to_vec()),
            [false, true, true],
            [false, false, true],
        ),
        (
            "partial-header-unresolved-identity",
            placement_group(
                a,
                record(b"REFR", 0xff00_0870, 0, Vec::new())[..16].to_vec(),
            ),
            [false, false, false],
            [false, false, true],
        ),
        (
            "partial-header-then-valid-override",
            [
                placement_group(a, moved_portal(0x800)[..16].to_vec()),
                placement_group(b, moved_portal(0x810)),
            ]
            .concat(),
            [false, true, true],
            [false, false, true],
        ),
        (
            "overrun-payload-resembles-portal-header",
            placement_group(
                a,
                [overrun.clone(), moved_portal(0x800)[..20].to_vec()].concat(),
            ),
            [false, true, true],
            [false, false, true],
        ),
        (
            "overrun-payload-resembles-cell-header",
            placement_group(
                a,
                [
                    overrun.clone(),
                    record(b"CELL", b, 0, Vec::new())[..16].to_vec(),
                ]
                .concat(),
            ),
            [false, true, true],
            [false, false, true],
        ),
        (
            "short-navmesh-tail",
            placement_group(a, [overrun, vec![0; 23]].concat()),
            [false, true, true],
            [false, false, true],
        ),
        (
            "parentless-invalid-compressed-override",
            record(
                b"REFR",
                0x870,
                0x40000,
                vec![4, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
            ),
            [true, false, true],
            [false, false, true],
        ),
        (
            "invalid-compressed-placement",
            placement_group(
                a,
                [broken_compressed_record(b"REFR", 0), portal(0x800)].concat(),
            ),
            [false, true, true],
            [true, false, true],
        ),
        (
            "invalid-compressed-deletion",
            placement_group(
                a,
                [broken_compressed_record(b"REFR", 0x20), portal(0x800)].concat(),
            ),
            [true, true, true],
            [true, false, true],
        ),
        (
            "invalid-compressed-navmesh",
            placement_group(
                a,
                [broken_compressed_record(b"NAVM", 0), portal(0x800)].concat(),
            ),
            [true, true, true],
            [true, false, true],
        ),
    ];
    let mut failures = Vec::new();
    for (name, mut contents, expected_complete, expected_portals) in cases {
        contents.extend(placement_group(c, portal(0x820)));
        let patch = plugin(data, &format!("{name}.esp"), &["Skyrim.esm"], 0, contents);
        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let output = data.join(format!("{name}-{reader:?}.db"));
            EsmParser::convert_plugins_with_reader(
                &[base.clone(), patch.clone()],
                &output,
                reader,
                data,
            )
            .unwrap();
            let db = rusqlite::Connection::open(output).unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT cell_id FROM room_portals WHERE ref_id=0x870",
                    [],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
                b,
                "prior portal survives in its original cell: {reader:?} {name}"
            );
            for (index, (cell, room)) in cells.into_iter().enumerate() {
                let actual: (bool, bool, bool) = db
                    .query_row(
                        "SELECT complete,
                            EXISTS(SELECT 1 FROM room_bounds WHERE ref_id=?2 AND cell_id=?1),
                            EXISTS(SELECT 1 FROM room_portals WHERE ref_id=?3 AND cell_id=?1)
                         FROM room_graph_cells WHERE cell_id=?1",
                        [cell, room, room + 1],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
                let expected = (expected_complete[index], true, expected_portals[index]);
                if actual != expected {
                    failures.push(format!(
                        "{reader:?} {name} cell {cell:X}: (complete, room, portal)={actual:?}, expected {expected:?}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn complete_deletion_headers_supersede_the_predecessor() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            record(b"CELL", 0x901, 0, sub(b"DATA", &[1, 0])),
            placement_group(0x900, placement_marker(0x800, 0x1f, Vec::new())),
            placement_group(0x901, placement_marker(0x801, 0x1f, Vec::new())),
        ]
        .concat(),
    );
    let bounded = record(b"REFR", 0x800, 0x40020, vec![0; 3]);
    let mut unbounded = bounded.clone();
    unbounded[4..8].copy_from_slice(&4u32.to_le_bytes());
    for (name, deletion, frame_complete) in
        [("bounded", bounded, true), ("unbounded", unbounded, false)]
    {
        let patch = plugin(
            data,
            &format!("{name}.esp"),
            &["Skyrim.esm"],
            0,
            placement_group(0x901, deletion),
        );
        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let db = data.join(format!("{name}-{reader:?}.db"));
            EsmParser::convert_plugins_with_reader(
                &[base.clone(), patch.clone()],
                &db,
                reader,
                data,
            )
            .unwrap();
            let conn = rusqlite::Connection::open(db).unwrap();
            assert_eq!(
                placement_winner_cell(&conn, 0x800),
                None,
                "{reader:?} {name}"
            );
            assert_eq!(
                stored_placement_certificate(&conn, 0x901),
                Some(frame_complete),
                "{reader:?} {name}"
            );
            if matches!(reader, RecordReader::Inhouse) {
                let history: (String, bool, bool) = conn
                    .query_row(
                        "SELECT status,deleted,frame_complete FROM record_overrides
                         WHERE form_id=0x800 AND plugin_id=1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
                assert_eq!(history, ("accepted".into(), true, frame_complete));
            }
        }
    }
}

#[test]
fn a_damaged_legacy_light_move_tracks_its_remapped_predecessor() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut cells = Vec::new();
    for (cell, room) in [(0x900, 0x810), (0x901, 0x811), (0x902, 0x812)] {
        cells.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        cells.extend(placement_group(
            cell,
            placement_marker(room, 0x1f, Vec::new()),
        ));
    }
    let base = plugin(data, "Skyrim.esm", &[], 1, cells);
    let light = plugin(
        data,
        "Light.esl",
        &["Skyrim.esm"],
        0x200,
        placement_group(0x900, placement_marker(0x0100_1800, 0x1f, Vec::new())),
    );
    let damaged = plugin(
        data,
        "Damaged.esp",
        &["Skyrim.esm", "Light.esl"],
        0,
        placement_group(0x901, record(b"REFR", 0x0100_1800, 0x40000, vec![0; 3])),
    );
    let restored = plugin(
        data,
        "Restored.esp",
        &["Skyrim.esm", "Light.esl"],
        0,
        placement_group(0x901, placement_marker(0x0100_1800, 0x1f, Vec::new())),
    );
    for (suffix, target, certificates) in [
        (Vec::new(), 0x900, [false, false, true]),
        (vec![restored], 0x901, [true, true, true]),
    ] {
        let paths = [base.clone(), light.clone(), damaged.clone()]
            .into_iter()
            .chain(suffix)
            .collect::<Vec<_>>();
        let db = data.join("legacy.db");
        EsmParser::convert_plugins_with_reader(&paths, &db, RecordReader::Legacy, data).unwrap();
        let conn = rusqlite::Connection::open(db).unwrap();
        assert_eq!(placement_winner_cell(&conn, 0xfe00_0800), Some(target));
        assert_eq!(
            [0x900, 0x901, 0x902].map(|id| placement_certificate(&conn, id)),
            certificates
        );
    }
}

const PLACEMENT_CELL_A: u32 = 0x900;
const PLACEMENT_CELL_B: u32 = 0x901;
const PLACEMENT_CELL_UNRELATED: u32 = 0x902;
const PLACEMENT_CELL_INCOMPLETE: u32 = 0x903;
const PLACEMENT_CELL_UNCERTIFIED: u32 = 0x904;
const PLACEMENT_STAT: u32 = 0x999;

fn placement(id: u32, base: u32, flags: u32, x: f32, extra: Vec<u8>) -> Vec<u8> {
    let mut transform = vec![0; 24];
    transform[..4].copy_from_slice(&x.to_le_bytes());
    record(
        b"REFR",
        id,
        flags,
        [
            sub(b"NAME", &base.to_le_bytes()),
            sub(b"DATA", &transform),
            extra,
        ]
        .concat(),
    )
}

fn placement_marker(id: u32, base: u32, extra: Vec<u8>) -> Vec<u8> {
    placement_marker_at(id, base, 0.0, extra)
}

fn placement_marker_at(id: u32, base: u32, x: f32, extra: Vec<u8>) -> Vec<u8> {
    let mut bounds = [0; 32];
    for offset in [0, 4, 8] {
        bounds[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
    }
    let kind: u32 = if base == 0x1f { 1 } else { 3 };
    bounds[28..].copy_from_slice(&kind.to_le_bytes());
    placement(id, base, 0, x, [sub(b"XPRM", &bounds), extra].concat())
}

fn placement_group(cell: u32, placements: Vec<u8>) -> Vec<u8> {
    group(6, cell, group(9, cell, placements))
}

fn converted_placement_fixture(root: &Path) -> PathBuf {
    let mut source = record(b"STAT", PLACEMENT_STAT, 0, sub(b"EDID", b"TestStatic\0"));
    for (cell, room, outside) in [
        (PLACEMENT_CELL_A, 0x800u32, 0u32),
        (PLACEMENT_CELL_B, 0x820, 0),
        (PLACEMENT_CELL_UNRELATED, 0x840, 0),
        // The typed reader clears the invalid master index to zero. The runtime portal row
        // records that the zero cannot be treated as authored outside.
        (PLACEMENT_CELL_INCOMPLETE, 0x860, 0x9900_0123),
    ] {
        source.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        let portal = placement_marker(
            room + 1,
            0x20,
            sub(
                b"XPOD",
                &[room.to_le_bytes(), outside.to_le_bytes()].concat(),
            ),
        );
        source.extend(placement_group(
            cell,
            [
                placement_marker(room, 0x1f, Vec::new()),
                portal,
                placement(room + 2, PLACEMENT_STAT, 0, 0.0, Vec::new()),
                placement(room + 3, PLACEMENT_STAT, 0, 105.0, Vec::new()),
                placement(room + 4, PLACEMENT_STAT, 0, 0.0, sub(b"XMBR", &[0; 3])),
            ]
            .concat(),
        ));
    }
    source.extend(record(
        b"CELL",
        PLACEMENT_CELL_UNCERTIFIED,
        0,
        sub(b"DATA", &[1, 0]),
    ));
    let original = plugin(root, "Skyrim.esm", &[], 1, source);
    let assets = root.join("assets");
    fs::create_dir(&assets).unwrap();
    let database = assets.join("skyrim_world.db");
    EsmParser::convert_plugins_with_reader(
        std::slice::from_ref(&original),
        &database,
        converter::config::RecordReader::Inhouse,
        root,
    )
    .unwrap();
    // Neither placement input needs the original game sources on disk.
    fs::remove_file(original).unwrap();
    let mut conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute(
        "UPDATE statics SET bounds_valid=1, bounds_min_x=-10,bounds_max_x=10,
         bounds_min_y=-10,bounds_max_y=10,bounds_min_z=-10,bounds_max_z=10 WHERE id=?1",
        [PLACEMENT_STAT],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO room_model_bounds VALUES (?1,1,2,3,4)",
        [PLACEMENT_STAT],
    )
    .unwrap();
    converter::esm::rooms::add_overlap_rows(&mut conn).unwrap();
    assets
}

fn placement_rules(db: &rusqlite::Connection, cell: u32) -> Vec<(u32, String, Option<u32>)> {
    db.prepare(
        "SELECT reference_id,assignment,target_ref FROM room_reference_rules
         WHERE cell_id=?1 ORDER BY reference_id",
    )
    .unwrap()
    .query_map([cell], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn placement_certificate(db: &rusqlite::Connection, cell: u32) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_graph_cells WHERE cell_id=?1 AND complete=1)",
        [cell],
        |row| row.get(0),
    )
    .unwrap()
}

fn stored_placement_certificate(db: &rusqlite::Connection, cell: u32) -> Option<bool> {
    use rusqlite::OptionalExtension;

    db.query_row(
        "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
        [cell],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

fn run_placement_import(assets: &Path, flag: &str, source: &Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_converter"))
        .arg("apply-room-placements")
        .arg(assets)
        .arg(flag)
        .arg(source)
        .output()
        .unwrap()
}

fn assert_placement_import(result: std::process::Output, summary: &str) {
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), summary);
}

#[test]
fn a_damaged_repeat_in_the_same_plugin_uncertifies_the_moved_reference() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let a = PLACEMENT_CELL_A;
    let b = PLACEMENT_CELL_B;
    let id = 0x802;
    let patch = plugin(
        dir.path(),
        "MovedThenDamaged.esp",
        &["Skyrim.esm"],
        0,
        [
            placement_group(b, placement(id, PLACEMENT_STAT, 0, 16.0, Vec::new())),
            placement_group(
                a,
                record(b"REFR", id, 0, sub(b"NAME", &[0; 3])[..7].to_vec()),
            ),
        ]
        .concat(),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &patch),
        "Applied 1 room placements in 2 cells (0 certified)",
    );
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    assert_eq!(placement_winner_cell(&db, id), Some(b));
    assert_eq!(stored_placement_certificate(&db, a), Some(false), "cell A");
    assert_eq!(stored_placement_certificate(&db, b), Some(false), "cell B");
    assert_eq!(
        stored_placement_certificate(&db, PLACEMENT_CELL_UNRELATED),
        Some(true)
    );
}

#[test]
fn plugin_placement_import_keeps_neighbors_of_damaged_records_and_frames() {
    let a = PLACEMENT_CELL_A;
    let b = PLACEMENT_CELL_B;
    let other = PLACEMENT_CELL_UNRELATED;
    let bad_move = record(b"REFR", 0x842, 0, sub(b"NAME", &[0; 3])[..7].to_vec());
    let good_neighbor = || placement(0x802, PLACEMENT_STAT, 0, 32.0, Vec::new());
    let mut overrun = bad_move.clone();
    overrun[4..8].copy_from_slice(&0xffffu32.to_le_bytes());
    let mut bad_group = group(6, a, Vec::new());
    bad_group[4..8].copy_from_slice(&0xffffu32.to_le_bytes());
    let cases = [
        (
            "bad-reference-header-id",
            placement_group(
                a,
                [
                    placement(0x9900_0842, PLACEMENT_STAT, 0, 5.0, Vec::new()),
                    good_neighbor(),
                ]
                .concat(),
            ),
            [false, true, true],
            2,
        ),
        (
            "bad-reference-parent-id",
            [
                placement_group(
                    0x9900_0800,
                    placement(0x842, PLACEMENT_STAT, 0, 5.0, Vec::new()),
                ),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false; 3],
            5,
        ),
        (
            "bad-base-master-index",
            placement_group(
                a,
                [
                    placement(0x842, 0x9900_0830, 0, 5.0, Vec::new()),
                    good_neighbor(),
                ]
                .concat(),
            ),
            [false, false, true],
            3,
        ),
        (
            "bounded-record",
            placement_group(a, [bad_move.clone(), good_neighbor()].concat()),
            [false, false, true],
            3,
        ),
        (
            "partial-move-header",
            [
                placement_group(a, bad_move[..20].to_vec()),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, true],
            3,
        ),
        (
            "partial-actor-signature",
            [
                placement_group(a, b"AC".to_vec()),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, false],
            5,
        ),
        (
            "partial-move-id",
            [
                placement_group(a, bad_move[..15].to_vec()),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, false],
            5,
        ),
        (
            "partial-group-tag",
            [
                placement_group(a, b"G".to_vec()),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false; 3],
            5,
        ),
        (
            "partial-child-group",
            [
                placement_group(a, group(10, b, Vec::new())[..16].to_vec()),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, true],
            3,
        ),
        (
            "discarded-cell-frame",
            [
                placement_group(a, overrun.clone()),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, true],
            3,
        ),
        (
            "discarded-cell-frame-hides-override",
            [
                placement_group(
                    a,
                    [
                        overrun,
                        placement(0x842, PLACEMENT_STAT, 0, 5.0, Vec::new()),
                    ]
                    .concat(),
                ),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, false],
            5,
        ),
        (
            "parentless-bounded-record",
            [bad_move, placement_group(a, good_neighbor())].concat(),
            [false, false, false],
            5,
        ),
        (
            "discarded-unknown-frame",
            [
                group(0, u32::from_le_bytes(*b"CELL"), bad_group.clone()),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, false],
            5,
        ),
        (
            "discarded-group-hides-partial-portal-header",
            [
                placement_group(
                    a,
                    [
                        bad_group.clone(),
                        record(b"REFR", 0x821, 0, Vec::new())[..20].to_vec(),
                    ]
                    .concat(),
                ),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, true],
            3,
        ),
        (
            "discarded-group-hides-child-cell",
            [
                placement_group(
                    a,
                    [bad_group.clone(), group(10, b, Vec::new())[..23].to_vec()].concat(),
                ),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, true],
            3,
        ),
        (
            "discarded-group-hides-partial-kind",
            [
                placement_group(
                    a,
                    [bad_group.clone(), group(10, b, Vec::new())[..15].to_vec()].concat(),
                ),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, false, true],
            3,
        ),
        (
            "discarded-group-hides-short-nonplacement-tail",
            [
                placement_group(
                    a,
                    [
                        bad_group,
                        record(b"NAVM", 0x0100_0803, 0, Vec::new())[..20].to_vec(),
                    ]
                    .concat(),
                ),
                placement_group(a, good_neighbor()),
            ]
            .concat(),
            [false, true, true],
            2,
        ),
    ];
    for (name, mut contents, certificates, affected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let assets = converted_placement_fixture(dir.path());
        let prior = dir.path().join("prior.json");
        fs::write(
            &prior,
            serde_json::json!({"edits": [{
                "op": "put", "id": 0x842, "cell": b, "base": PLACEMENT_STAT,
                "position": [16, 0, 0],
            }]})
            .to_string(),
        )
        .unwrap();
        assert_placement_import(
            run_placement_import(&assets, "--native", &prior),
            "Applied 1 room placements in 2 cells (2 certified)",
        );
        contents.extend(placement_group(
            other,
            placement(0x843, PLACEMENT_STAT, 0, 64.0, Vec::new()),
        ));
        let source = plugin(
            dir.path(),
            &format!("{name}.esp"),
            &["Skyrim.esm"],
            0,
            contents,
        );
        let result = run_placement_import(&assets, "--plugin", &source);
        let warning = match name {
            "bad-reference-header-id" => "99000842 with invalid record FormID",
            "bad-reference-parent-id" => "00000842 with invalid parent CELL",
            "bad-base-master-index" => "malformed NAME",
            _ => "skipped 1 malformed",
        };
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(warning),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let certified = usize::from(certificates[2]);
        let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
        assert_eq!(
            [a, b, other].map(|cell| stored_placement_certificate(&db, cell)),
            certificates.map(Some),
            "{name}: certificates for cells {a:X}, {b:X}, {other:X}"
        );
        assert_placement_import(
            result,
            &format!("Applied 2 room placements in {affected} cells ({certified} certified)"),
        );
        assert_eq!(placement_winner_cell(&db, 0x821), Some(b), "{name}");
        let positions_and_edits: (f64, f64, u32, f64, u32) = db
            .query_row(
                "SELECT (SELECT pos_x FROM [references] WHERE id=0x802),
                        (SELECT pos_x FROM [references] WHERE id=0x843),
                        (SELECT cell_id FROM [references] WHERE id=0x842),
                        (SELECT pos_x FROM [references] WHERE id=0x842),
                        (SELECT count(*) FROM room_placement_edits WHERE layer_id=2)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(positions_and_edits, (32.0, 64.0, b, 16.0, 2), "{name}");
        assert_placement_import(
            run_placement_import(&assets, "--plugin", &source),
            "Applied 0 room placements in 0 cells (0 certified)",
        );
    }
}

#[test]
fn plugin_placement_import_skips_children_of_a_damaged_new_interior() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let damaged_cell = 0x0100_0900;
    let good_cell = 0x0100_0901;
    let source = plugin(
        dir.path(),
        "Interiors.esp",
        &["Skyrim.esm"],
        0,
        [
            record(b"CELL", damaged_cell, 0, vec![0]),
            placement_group(
                damaged_cell,
                [
                    record(b"REFR", 0x0100_0800, 0, vec![0]),
                    placement(0x0100_0800, PLACEMENT_STAT, 0, 0.0, Vec::new()),
                    placement(0x802, PLACEMENT_STAT, 0, 12.0, Vec::new()),
                ]
                .concat(),
            ),
            record(
                b"CELL",
                good_cell,
                0,
                [sub(b"EDID", b"GoodInterior\0"), sub(b"DATA", &[1])].concat(),
            ),
            placement_group(good_cell, placement_marker(0x0100_0801, 0x1f, Vec::new())),
        ]
        .concat(),
    );
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let links_verified = || {
        db.query_row(
            "SELECT value FROM room_graph_stats WHERE name='occlusion_links_verified'",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    assert!(links_verified());
    let result = run_placement_import(&assets, "--plugin", &source);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("its new CELL is damaged"), "{stderr}");
    assert_placement_import(
        result,
        "Applied 1 room placements in 2 cells (1 created, 1 certified)",
    );
    assert!(!links_verified());
    assert!(placement_certificate(&db, good_cell));
    assert!(!placement_certificate(&db, PLACEMENT_CELL_A));
    assert_eq!(placement_winner_cell(&db, 0x802), Some(PLACEMENT_CELL_A));
    let skipped: (u32, u32) = db
        .query_row(
            "SELECT (SELECT count(*) FROM cells WHERE id=?1),
                    (SELECT count(*) FROM [references] WHERE id=0x01000800)",
            [damaged_cell],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(skipped, (0, 0));
}

#[test]
fn plugin_placement_import_uses_good_declaration_of_damaged_new_cell() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let cell = 0x0100_0900;
    let child = 0x0100_0800;
    let missing_cell = cell + 1;
    let truncated = |id| {
        group(
            0,
            u32::from_le_bytes(*b"CELL"),
            record(b"CELL", id, 0, vec![])[..16].to_vec(),
        )
    };
    let source = plugin(
        dir.path(),
        "Recovered.esp",
        &["Skyrim.esm"],
        0,
        [
            record(b"CELL", cell, 0, vec![0]),
            truncated(missing_cell),
            placement_group(
                missing_cell,
                placement(child + 1, PLACEMENT_STAT, 0, 7.0, Vec::new()),
            ),
            truncated(cell),
            record(
                b"CELL",
                cell,
                0,
                [sub(b"EDID", b"RecoveredInterior\0"), sub(b"DATA", &[1])].concat(),
            ),
            placement_group(cell, placement(child, PLACEMENT_STAT, 0, 12.0, Vec::new())),
        ]
        .concat(),
    );
    let result = run_placement_import(&assets, "--plugin", &source);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    assert_eq!(placement_winner_cell(&db, child), Some(cell));
    assert_eq!(placement_winner_cell(&db, child + 1), None);
    assert!(String::from_utf8_lossy(&result.stderr).contains("its new CELL is damaged"));
}

#[test]
fn plugin_placement_import_skips_bad_transforms_and_scales() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let a = PLACEMENT_CELL_A;
    let b = PLACEMENT_CELL_B;
    let source = plugin(
        dir.path(),
        "BadFields.esp",
        &["Skyrim.esm"],
        0,
        [
            placement_group(
                b,
                [
                    placement(0x802, PLACEMENT_STAT, 0, f32::NAN, Vec::new()),
                    placement(
                        0x803,
                        PLACEMENT_STAT,
                        0,
                        12.0,
                        sub(b"XSCL", &0f32.to_le_bytes()),
                    ),
                    placement(0x0100_0800, PLACEMENT_STAT, 0, 12.0, sub(b"XSCL", &[0; 3])),
                    placement(
                        0x0100_0801,
                        PLACEMENT_STAT,
                        0,
                        12.0,
                        sub(b"XSCL", &1f32.to_le_bytes()).repeat(2),
                    ),
                    placement(0x0100_0802, PLACEMENT_STAT, 0, 12.0, sub(b"DATA", &[0; 24])),
                ]
                .concat(),
            ),
            placement_group(a, placement(0x822, PLACEMENT_STAT, 0, 32.0, Vec::new())),
        ]
        .concat(),
    );
    let result = run_placement_import(&assets, "--plugin", &source);
    let stderr = String::from_utf8_lossy(&result.stderr);
    for (id, field) in [
        ("00000802", "DATA"),
        ("00000803", "XSCL"),
        ("01000800", "XSCL"),
        ("01000801", "XSCL"),
        ("01000802", "DATA"),
    ] {
        assert!(
            stderr.contains(&format!("{id} with malformed {field}")),
            "{stderr}"
        );
    }
    assert_placement_import(result, "Applied 1 room placements in 2 cells (0 certified)");
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    assert_eq!(placement_winner_cell(&db, 0x802), Some(a));
    assert_eq!(placement_winner_cell(&db, 0x803), Some(a));
    assert_eq!(placement_winner_cell(&db, 0x0100_0800), None);
    assert_eq!(stored_placement_certificate(&db, a), Some(false));
    assert_eq!(stored_placement_certificate(&db, b), Some(false));
    assert_eq!(
        stored_placement_certificate(&db, PLACEMENT_CELL_UNRELATED),
        Some(true)
    );
    assert_eq!(
        db.query_row("SELECT pos_x FROM [references] WHERE id=0x822", [], |r| r
            .get::<_, f32>(
            0
        ))
        .unwrap(),
        32.0
    );
}

#[test]
fn plugin_placement_import_rolls_back_unsupported_content_alongside_damage() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let a = PLACEMENT_CELL_A;
    let bad = record(b"REFR", 0x822, 0, vec![0]);
    let good = placement(0x802, PLACEMENT_STAT, 0, 32.0, Vec::new());
    let unsupported = placement(0x803, PLACEMENT_STAT, 0, 32.0, sub(b"XPRM", &[0; 32]));
    let source = plugin(
        dir.path(),
        "Mixed.esp",
        &["Skyrim.esm"],
        0,
        placement_group(a, [bad, good, unsupported].concat()),
    );
    let result = run_placement_import(&assets, "--plugin", &source);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success() && stderr.contains("static primitives"),
        "{stderr}"
    );
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    assert!(placement_certificate(&db, a));
    assert!(placement_certificate(&db, PLACEMENT_CELL_B));
    let (x, layers): (f64, u32) = db
        .query_row(
            "SELECT pos_x, (SELECT count(*) FROM sqlite_schema WHERE name='room_placement_layers')
             FROM [references] WHERE id=0x802",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((x, layers), (0.0, 0));
}

#[test]
fn post_conversion_plugin_placements_refresh_both_cells_and_keep_source_proof() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let original_move = || {
        db.query_row(
            "SELECT r.cell_id,r.data,r.load_order,f.plugin_name,f.internal_id,s.payload
             FROM records r JOIN formid_map f USING(form_id)
             JOIN inhouse_source_records s USING(form_id) WHERE r.form_id=0x802",
            [],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                ))
            },
        )
        .unwrap()
    };
    let before = original_move();
    let unrelated = placement_rules(&db, PLACEMENT_CELL_UNRELATED);
    let new_cell = plugin(
        dir.path(),
        "NewCell.esp",
        &["Skyrim.esm"],
        0,
        record(b"CELL", 0x0100_0900, 0, sub(b"DATA", &[1, 0])),
    );
    let result = run_placement_import(&assets, "--plugin", &new_cell);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires DATA and EDID"));
    let add = plugin(
        dir.path(),
        "Placement.esl",
        &["Skyrim.esm"],
        0x200,
        [
            placement_group(
                PLACEMENT_CELL_A,
                [
                    record(b"REFR", 0x803, 0x20, Vec::new()),
                    placement(0x0100_0900, PLACEMENT_STAT, 0, 105.0, Vec::new()),
                ]
                .concat(),
            ),
            placement_group(
                PLACEMENT_CELL_B,
                [
                    placement(
                        0x802,
                        PLACEMENT_STAT,
                        0,
                        40.0,
                        sub(b"XMBR", &0x820u32.to_le_bytes()),
                    ),
                    placement_marker(0x0100_0901, 0x1f, Vec::new()),
                    placement_marker(
                        0x0100_0902,
                        0x20,
                        sub(
                            b"XPOD",
                            &[0x0100_0901u32.to_le_bytes(), 0x820u32.to_le_bytes()].concat(),
                        ),
                    ),
                ]
                .concat(),
            ),
            placement_group(
                PLACEMENT_CELL_UNCERTIFIED,
                placement_marker(0x0100_0903, 0x1f, Vec::new()),
            ),
        ]
        .concat(),
    );
    let bytes = fs::read(&add).unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &add),
        "Applied 6 room placements in 3 cells (2 certified)",
    );
    assert_eq!(original_move(), before);
    assert_eq!(placement_rules(&db, PLACEMENT_CELL_UNRELATED), unrelated);
    assert!(placement_certificate(&db, PLACEMENT_CELL_UNRELATED));
    assert!(!placement_certificate(&db, PLACEMENT_CELL_UNCERTIFIED));
    assert_eq!(
        placement_rules(&db, PLACEMENT_CELL_A),
        [
            (0x804, "unknown".into(), None),
            (0xfe00_0900, "auto".into(), None)
        ],
    );
    assert!(placement_rules(&db, PLACEMENT_CELL_B).contains(&(
        0x802,
        "target".into(),
        Some(0x820)
    )));
    assert_eq!(
        db.query_row(
            "SELECT cell_id,pos_x FROM [references] WHERE id=0x802",
            [],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, f32>(1)?)),
        )
        .unwrap(),
        (PLACEMENT_CELL_B, 40.0)
    );
    assert_eq!(
        db.query_row(
            "SELECT room_ref,contained FROM reference_rooms WHERE reference_id=0xfe000900",
            [],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?)),
        )
        .unwrap(),
        (0x800, false)
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM [references] WHERE id=0x803",
            [],
            |row| row.get::<_, u32>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT source FROM room_placement_layers WHERE name='placement.esl'",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap(),
        bytes
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &add),
        "Applied 0 room placements in 0 cells (0 certified)",
    );

    // The appended ESL is a usable master. Move its marker from the higher-numbered cell
    // into the lower one, leaving the old cell's portal with an invalid endpoint.
    fs::remove_file(add).unwrap();
    let move_marker = plugin(
        dir.path(),
        "Move.esp",
        &["Skyrim.esm", "Placement.esl"],
        0,
        placement_group(
            PLACEMENT_CELL_A,
            placement_marker(0x0100_0901, 0x1f, Vec::new()),
        ),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &move_marker),
        "Applied 1 room placements in 2 cells (1 certified)",
    );
    assert!(placement_certificate(&db, PLACEMENT_CELL_A));
    assert!(!placement_certificate(&db, PLACEMENT_CELL_B));
    assert_eq!(
        db.query_row(
            "SELECT cell_id FROM room_bounds WHERE ref_id=0xfe000901",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        PLACEMENT_CELL_A
    );
    assert_eq!(
        db.query_row(
            "SELECT room_a,unbounded_a FROM room_portals WHERE ref_id=0xfe000902",
            [],
            |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, bool>(1)?))
        )
        .unwrap(),
        (None, false)
    );
    assert_eq!(original_move(), before);
    assert_eq!(placement_rules(&db, PLACEMENT_CELL_UNRELATED), unrelated);
}

#[test]
fn corrected_plugin_placement_import_requires_replay_or_a_new_patch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let assets = converted_placement_fixture(root);
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let cell = PLACEMENT_CELL_A;
    let source = |name, second| {
        plugin(
            root,
            name,
            &["Skyrim.esm"],
            0x200,
            placement_group(
                cell,
                [
                    placement(0x0100_0910, PLACEMENT_STAT, 0, 12.0, Vec::new()),
                    second,
                ]
                .concat(),
            ),
        )
    };
    let bad_neighbor = record(b"REFR", 0x0100_0911, 0, sub(b"NAME", &[0; 3])[..7].to_vec());
    let original = source("Decor.esl", bad_neighbor.clone());
    let original_bytes = fs::read(&original).unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &original),
        "Applied 1 room placements in 1 cells (0 certified)",
    );
    let repeated = source("DECOR.ESL", bad_neighbor);
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &repeated),
        "Applied 0 room placements in 0 cells (0 certified)",
    );
    let dependent = plugin(
        root,
        "FollowUp.esp",
        &["Skyrim.esm", "Decor.esl"],
        0,
        placement_group(
            cell,
            [
                placement(0x0100_0910, PLACEMENT_STAT, 0, 23.0, Vec::new()),
                placement(0x0200_0912, PLACEMENT_STAT, 0, 34.0, Vec::new()),
            ]
            .concat(),
        ),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &dependent),
        "Applied 2 room placements in 1 cells (0 certified)",
    );

    let corrected = source(
        "DECOR.ESL",
        placement(0x0100_0911, PLACEMENT_STAT, 0, 45.0, Vec::new()),
    );
    let result = run_placement_import(&assets, "--plugin", &corrected);
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("decor.esl is already imported with different contents"),
        "{error}"
    );
    assert!(
        error.contains("replay the placement layers on a fresh conversion"),
        "{error}"
    );
    assert!(
        error.contains("patch with a new filename and use decor.esl as a master"),
        "{error}"
    );

    let rows = |db: &rusqlite::Connection| {
        db.prepare("SELECT id, pos_x FROM [references] WHERE id>0x00ffffff ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, f32>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let retained_source = |db: &rusqlite::Connection| {
        db.query_row(
            "SELECT source, (SELECT count(*) FROM room_placement_layers)
             FROM room_placement_layers WHERE name='decor.esl'",
            [],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, usize>(1)?)),
        )
        .unwrap()
    };
    let existing_rows = [(0x0100_0912, 34.0), (0xfe00_0910, 23.0)];
    assert_eq!(rows(&db), existing_rows);
    assert_eq!(retained_source(&db), (original_bytes, 2));
    assert!(!placement_certificate(&db, cell));

    let replay = root.join("replay");
    fs::create_dir(&replay).unwrap();
    let assets = converted_placement_fixture(&replay);
    for layer in [&corrected, &dependent] {
        assert_placement_import(
            run_placement_import(&assets, "--plugin", layer),
            "Applied 2 room placements in 1 cells (1 certified)",
        );
    }
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    assert_eq!(
        rows(&db),
        [existing_rows[0], existing_rows[1], (0xfe00_0911, 45.0)]
    );
    assert_eq!(retained_source(&db), (fs::read(&corrected).unwrap(), 2));
    assert!(placement_certificate(&db, cell));
}

#[test]
fn placement_import_rejects_retired_base_and_clears_links_on_deletion() {
    use converter::esm::{exporter::export_to_db, records::RawRecord};
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let target = 0x802u32;
    let source = 0x880u32;
    let reference = RawRecord {
        form_id: source,
        record_type: *b"REFR",
        flags: 0,
        load_order: 0,
        cell_form_id: Some(PLACEMENT_CELL_A),
        worldspace_form_id: None,
        subrecords: vec![
            (b"NAME".to_vec(), PLACEMENT_STAT.to_le_bytes().to_vec()),
            (b"DATA".to_vec(), vec![0; 24]),
            (b"XLKR".to_vec(), target.to_le_bytes().to_vec()),
        ],
    };
    export_to_db(&db, &[(source, reference.clone())].into()).unwrap();
    let link = || {
        db.query_row(
            "SELECT linked_ref_id FROM reference_linked_refs WHERE reference_id=?1 AND position=0",
            [source],
            |r| r.get::<_, Option<u32>>(0),
        )
        .unwrap()
    };
    let put = dir.path().join("put.json");
    fs::write(
        &put,
        serde_json::json!({"edits":[{"op":"put","id":target,"base":PLACEMENT_STAT,
        "position":[15,0,0],"cell":PLACEMENT_CELL_A}]})
        .to_string(),
    )
    .unwrap();
    assert!(
        run_placement_import(&assets, "--native", &put)
            .status
            .success()
    );
    assert_eq!(link(), Some(target));
    let tombstone = RawRecord {
        form_id: PLACEMENT_STAT,
        record_type: *b"STAT",
        flags: 0x20,
        subrecords: Vec::new(),
        cell_form_id: None,
        ..reference
    };
    export_to_db(&db, &[(PLACEMENT_STAT, tombstone)].into()).unwrap();
    fs::write(
        &put,
        serde_json::json!({"edits":[{"op":"put","id":0xff00_0001u32,"base":PLACEMENT_STAT,
        "position":[0,0,0],"cell":PLACEMENT_CELL_A}]})
        .to_string(),
    )
    .unwrap();
    let rejected = run_placement_import(&assets, "--native", &put);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("base was deleted"));
    let delete = dir.path().join("delete.json");
    fs::write(
        &delete,
        serde_json::json!({"edits":[{"op":"delete","id":target}]}).to_string(),
    )
    .unwrap();
    assert!(
        run_placement_import(&assets, "--native", &delete)
            .status
            .success()
    );
    assert_eq!(link(), None);
}

#[test]
fn native_placement_import_preserves_runtime_enable_state_across_puts() {
    use rusqlite::OptionalExtension;
    use shared::initial_enable::{EnableStateResolver, InitialEnableInputs};

    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let child = 0x802;
    let parent = 0x803u32;
    let xesp = [parent.to_le_bytes(), [2, 11, 12, 13]].concat();
    let plugin = plugin(
        dir.path(),
        "EnableState.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_A,
            [
                placement(child, PLACEMENT_STAT, 0, 0.0, sub(b"XESP", &xesp)),
                placement(parent, PLACEMENT_STAT, 0x800, 105.0, Vec::new()),
            ]
            .concat(),
        ),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &plugin),
        "Applied 2 room placements in 1 cells (1 certified)",
    );
    let load_state = |id| {
        db.query_row(
            "SELECT header_flags,enable_parent_id,coalesce(enable_parent_flags,0)
             FROM [references] WHERE id=?1",
            [id],
            |row| {
                Ok(InitialEnableInputs {
                    header_flags: row.get(0)?,
                    parent_id: row.get(1)?,
                    parent_flags: row.get(2)?,
                })
            },
        )
        .optional()
    };
    let assert_state = || {
        let child_state = load_state(child).unwrap().unwrap();
        let parent_state = load_state(parent).unwrap().unwrap();
        assert_eq!(
            (child_state.parent_id, child_state.parent_flags),
            (Some(parent), 2)
        );
        assert_eq!(parent_state.header_flags & 0x800, 0x800);
        assert_eq!(
            EnableStateResolver::default()
                .resolve(child, child_state, &load_state)
                .unwrap(),
            Ok(false)
        );
        assert_eq!(database_field(&db, "references", child, b"XESP"), xesp);
    };
    assert_state();

    let native = dir.path().join("move.json");
    for (cell, x, summary) in [
        (
            PLACEMENT_CELL_B,
            40.0,
            "Applied 3 room placements in 2 cells (2 certified)",
        ),
        (
            PLACEMENT_CELL_B,
            50.0,
            "Applied 3 room placements in 2 cells (2 certified)",
        ),
    ] {
        fs::write(
            &native,
            serde_json::json!({"edits": [
                {"op":"put","id":child,"cell":cell,"base":PLACEMENT_STAT,"position":[x,0,0]},
                {"op":"put","id":parent,"cell":PLACEMENT_CELL_A,"base":PLACEMENT_STAT,"position":[x,0,0]},
                {"op":"put","id":0x804,"cell":PLACEMENT_CELL_A,"base":PLACEMENT_STAT,"position":[x,0,0]},
            ]}).to_string(),
        )
        .unwrap();
        assert_placement_import(run_placement_import(&assets, "--native", &native), summary);
        assert_state();
        for (id, destination) in [
            (child, cell),
            (parent, PLACEMENT_CELL_A),
            (0x804, PLACEMENT_CELL_A),
        ] {
            let actual = db
                .query_row(
                    "SELECT cell_id,pos_x FROM [references] WHERE id=?1",
                    [id],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, f32>(1)?)),
                )
                .unwrap();
            assert_eq!(actual, (destination, x));
        }
        assert_eq!(load_state(0x804).unwrap().unwrap().parent_id, None);
    }

    fs::write(
        &native,
        serde_json::json!({"edits":[{"op":"delete","id":parent}]}).to_string(),
    )
    .unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &native),
        "Applied 1 room placements in 1 cells (1 certified)",
    );
    fs::write(&native, serde_json::json!({"edits":[{
        "op":"put", "id":parent, "cell":PLACEMENT_CELL_A, "base":PLACEMENT_STAT,"position":[0,0,0]
    }]}).to_string()).unwrap();
    let result = run_placement_import(&assets, "--native", &native);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("native replacement requires an existing runtime enable state")
    );
    assert!(load_state(parent).unwrap().is_none());
}

#[test]
fn native_placement_import_rejects_negative_room_extents_and_accepts_zero() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let native = dir.path().join("room.json");
    let cell = 0xff00_0010u32;
    let room = 0xff00_0011u32;
    let run = |half_y| {
        fs::write(
            &native,
            serde_json::json!({
                "cells":[{"id":cell,"editor_id":"NewInterior"}],
                "edits":[
                    {"op":"put","id":0x802,"cell":PLACEMENT_CELL_A,"base":PLACEMENT_STAT,"position":[15,0,0]},
                    {"op":"put_room","id":room,"cell":cell,"position":[0,0,0],"half_extents":[10,half_y,5]},
                ]
            }).to_string(),
        )
        .unwrap();
        run_placement_import(&assets, "--native", &native)
    };
    let result = run(-1);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("room half-extents must be finite and nonnegative")
    );
    assert_eq!(
        db.query_row(
            "SELECT (SELECT pos_x FROM [references] WHERE id=0x802),
                    EXISTS(SELECT 1 FROM cells WHERE id=?1),
                    EXISTS(SELECT 1 FROM sqlite_schema WHERE name='room_placement_layers')",
            [cell],
            |row| Ok((
                row.get::<_, f32>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, bool>(2)?
            )),
        )
        .unwrap(),
        (0.0, false, false)
    );
    assert_placement_import(
        run(0),
        "Applied 2 room placements in 2 cells (1 created, 2 certified)",
    );
    assert_eq!(
        db.query_row(
            "SELECT half_x,half_y,half_z FROM room_bounds WHERE ref_id=?1 AND cell_id=?2",
            (room, cell),
            |row| Ok((
                row.get::<_, f32>(0)?,
                row.get::<_, f32>(1)?,
                row.get::<_, f32>(2)?
            )),
        )
        .unwrap(),
        (10.0, 0.0, 5.0)
    );
}

#[test]
fn native_placement_import_is_atomic_and_preserves_incomplete_cell_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let (outside_before, complete_before) = (
        db.query_row(
            "SELECT unbounded_b FROM room_portals WHERE ref_id=0x861",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap(),
        placement_certificate(&db, PLACEMENT_CELL_INCOMPLETE),
    );
    assert_eq!((outside_before, complete_before), (false, false));
    let untouched = placement_rules(&db, PLACEMENT_CELL_UNRELATED);
    let native_id = 0xff00_0001u32;
    let source = dir.path().join("decor.json");
    let put = |id, cell, ownership| {
        serde_json::json!({
            "op": "put", "id": id, "cell": cell, "base": PLACEMENT_STAT,
            "position": [10.0, 20.0, 30.0], "ownership": ownership,
        })
    };
    let valid = serde_json::json!({"edits": [
        put(0x802, PLACEMENT_CELL_B, serde_json::json!({"kind": "none"})),
        put(native_id, PLACEMENT_CELL_INCOMPLETE, serde_json::json!({"kind": "target", "room": 0x860})),
        {"op": "delete", "id": 0x803},
    ]});
    let mut invalid = valid.clone();
    invalid["edits"][2]["id"] = serde_json::json!(PLACEMENT_STAT);
    fs::write(&source, serde_json::to_vec(&invalid).unwrap()).unwrap();
    let result = run_placement_import(&assets, "--native", &source);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("FormID belongs to a STAT record"));
    assert_eq!(
        db.query_row(
            "SELECT cell_id FROM [references] WHERE id=0x802",
            [],
            |row| row.get::<_, u32>(0),
        )
        .unwrap(),
        PLACEMENT_CELL_A
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='room_placement_layers'",
            [],
            |row| row.get::<_, u32>(0),
        )
        .unwrap(),
        0
    );

    fs::write(&source, serde_json::to_vec(&valid).unwrap()).unwrap();
    let lock = shared::asset_lock::AssetLock::acquire_shared(&assets).unwrap();
    let locked = run_placement_import(&assets, "--native", &source);
    assert!(!locked.status.success());
    assert!(String::from_utf8_lossy(&locked.stderr).contains("exclusive-write"));
    drop(lock);
    assert_placement_import(
        run_placement_import(&assets, "--native", &source),
        "Applied 3 room placements in 3 cells (2 certified)",
    );
    assert!(placement_rules(&db, PLACEMENT_CELL_B).contains(&(0x802, "none".into(), None)));
    assert!(placement_rules(&db, PLACEMENT_CELL_INCOMPLETE).contains(&(
        native_id,
        "target".into(),
        Some(0x860)
    )));
    assert_eq!(placement_rules(&db, PLACEMENT_CELL_UNRELATED), untouched);
    assert!(!placement_certificate(&db, PLACEMENT_CELL_INCOMPLETE));
    assert!(
        !db.query_row(
            "SELECT unbounded_b FROM room_portals WHERE ref_id=0x861",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert!(
        db.query_row(
            "SELECT unbounded_b FROM room_portals WHERE ref_id=0x801",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert_eq!(
        db.query_row(
            "SELECT radius FROM room_model_bounds WHERE base_form_id=?1",
            [PLACEMENT_STAT],
            |row| row.get::<_, f32>(0)
        )
        .unwrap(),
        4.0
    );
    // Even removing the visible reason for the old failure cannot prove what the source reader
    // omitted. A new full conversion is required to issue the first certificate.
    fs::write(&source, br#"{"edits":[{"op":"delete","id":2145}]}"#).unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &source),
        "Applied 1 room placements in 1 cells (0 certified)",
    );
    assert!(!placement_certificate(&db, PLACEMENT_CELL_INCOMPLETE));
}

#[test]
fn native_placement_import_keeps_cell_open_when_a_neighbor_blob_is_null() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    assert!(placement_certificate(&db, PLACEMENT_CELL_A));
    db.execute("UPDATE [references] SET data=NULL WHERE id=0x803", [])
        .unwrap();
    let source = dir.path().join("placement.json");
    fs::write(
        &source,
        serde_json::to_vec(&serde_json::json!({"edits": [{
            "op": "put", "id": 0x802, "cell": PLACEMENT_CELL_A, "base": PLACEMENT_STAT,
            "position": [10.0, 20.0, 30.0], "ownership": {"kind": "none"},
        }]}))
        .unwrap(),
    )
    .unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &source),
        "Applied 1 room placements in 1 cells (0 certified)",
    );
    assert_eq!(
        stored_placement_certificate(&db, PLACEMENT_CELL_A),
        Some(false)
    );
    assert!(placement_certificate(&db, PLACEMENT_CELL_B));
    assert!(placement_rules(&db, PLACEMENT_CELL_A).contains(&(0x802, "none".into(), None)));
    assert!(
        db.query_row(
            "SELECT data IS NULL FROM [references] WHERE id=0x803",
            [],
            |row| { row.get::<_, bool>(0) }
        )
        .unwrap()
    );
}

#[test]
fn later_placement_edits_preserve_the_original_room_link_order() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let room = placement_marker(
        0x0100_0900,
        0x1f,
        [
            sub(b"XRMR", &[1, 0, 0, 0]),
            // Legacy ID remapping drops this malformed field, leaving an apparent XLRM slot.
            sub(b"LNAM", &[0]),
            sub(b"XLRM", &0x800u32.to_le_bytes()),
        ]
        .concat(),
    );
    let change = plugin(
        dir.path(),
        "Misplaced.esp",
        &["Skyrim.esm"],
        0,
        placement_group(PLACEMENT_CELL_A, room),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &change),
        "Applied 1 room placements in 1 cells (0 certified)",
    );
    let outgoing = || {
        db.query_row(
            "SELECT count(*) FROM room_links WHERE room_ref=0x01000900",
            [],
            |row| row.get::<_, usize>(0),
        )
        .unwrap()
    };
    assert_eq!(outgoing(), 0);
    let delete = dir.path().join("delete.json");
    fs::write(&delete, r#"{"edits":[{"op":"delete","id":2051}]}"#).unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &delete),
        "Applied 1 room placements in 1 cells (0 certified)",
    );
    assert_eq!(outgoing(), 0);
    assert!(!placement_certificate(&db, PLACEMENT_CELL_A));
}

#[test]
fn room_placement_command_updates_skyrim_and_native_occluders() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let mut shape = [0u8; 32];
    shape[..4].copy_from_slice(&8f32.to_le_bytes());
    shape[8..12].copy_from_slice(&8f32.to_le_bytes());
    shape[28..].copy_from_slice(&3u32.to_le_bytes());
    let path = plugin(
        dir.path(),
        "Plane.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_A,
            placement(0x0100_0800, 23, 0, 0.0, sub(b"XPRM", &shape)),
        ),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &path),
        "Applied 1 room placements in 1 cells (1 certified)",
    );
    let native = dir.path().join("plane.json");
    fs::write(
        &native,
        serde_json::json!({"edits":[{"op":"put","id":0xff00_0800u32,
        "cell":PLACEMENT_CELL_A,"base":23,"position":[0,0,0],
        "shape":{"kind":"plane","half_width":8,"half_height":8},
        "links":[0,0,0x0100_0800u32,0],"ownership":{"kind":"target","room":0x800}}]})
        .to_string(),
    )
    .unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &native),
        "Applied 1 room placements in 1 cells (1 certified)",
    );
    let count = || {
        db.query_row(
            "SELECT count(*) FROM room_occluders WHERE cell_id=?1",
            [PLACEMENT_CELL_A],
            |row| row.get::<_, u32>(0),
        )
        .unwrap()
    };
    let proof = db
        .query_row(
            "SELECT l.link_index,l.linked_ref,l.single_target_verified,r.target_ref
         FROM room_occluder_links l JOIN room_reference_rules r ON r.reference_id=l.ref_id
         WHERE l.ref_id=0xff000800 AND r.assignment='target'",
            [],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(proof, (2, 0x0100_0800, true, 0x800));
    assert_eq!(count(), 2);
    fs::write(
        &native,
        serde_json::json!({"edits":[{"op":"delete","id":0x0100_0800u32},
        {"op":"delete","id":0xff00_0800u32}]})
        .to_string(),
    )
    .unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &native),
        "Applied 2 room placements in 1 cells (1 certified)",
    );
    assert_eq!(count(), 0);
}

#[test]
fn occluder_link_to_a_deleted_archived_placement_is_an_omission() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let remove = dir.path().join("remove.json");
    fs::write(&remove, r#"{"edits":[{"op":"delete","id":2050}]}"#).unwrap();
    assert!(
        run_placement_import(&assets, "--native", &remove)
            .status
            .success()
    );
    let plane = plugin(
        dir.path(),
        "LinkToDeleted.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_A,
            placement_marker(
                0x0100_0910,
                23,
                sub(b"XORD", &[0x802u32, 0, 0, 0].map(u32::to_le_bytes).concat()),
            ),
        ),
    );
    let result = run_placement_import(&assets, "--plugin", &plane);
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success() && error.contains("occluder_unusable_links"),
        "{error}"
    );
    assert!(!placement_certificate(&db, PLACEMENT_CELL_A));
    assert_eq!(
        db.query_row(
            "SELECT link_index,linked_ref,single_target_verified FROM room_occluder_links WHERE ref_id=0x01000910",
            [],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?, row.get::<_, bool>(2)?))
        )
        .unwrap(),
        (0, 0x802, false)
    );
    assert_eq!(
        db.query_row(
            "SELECT record_type FROM records WHERE form_id=0x802",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "REFR"
    );
}

fn authored_cell(id: u32, extra: Vec<u8>) -> Vec<u8> {
    record(
        b"CELL",
        id,
        0,
        [
            sub(b"EDID", b"FreshInterior\0"),
            sub(b"FULL", b"Fresh Interior\0"),
            sub(b"DATA", &[1, 0]),
            extra,
        ]
        .concat(),
    )
}

fn assert_created_interior(
    db: &rusqlite::Connection,
    cell: u32,
    room_a: u32,
    room_b: u32,
    portal: u32,
    object: u32,
) {
    assert_eq!(
        db.query_row(
            "SELECT interior_name,full_name,worldspace_id,grid_x,grid_y,data_flags
             FROM cells JOIN cell_environment ON cell_id=id WHERE id=?1",
            [cell],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u32>>(2)?,
                row.get::<_, Option<i32>>(3)?,
                row.get::<_, Option<i32>>(4)?,
                row.get::<_, u16>(5)?,
            )),
        )
        .unwrap(),
        (
            "FreshInterior".into(),
            "Fresh Interior".into(),
            None,
            None,
            None,
            1
        )
    );
    assert_eq!(
        db.prepare("SELECT ref_id,pos_x FROM room_bounds WHERE cell_id=?1 ORDER BY ref_id")
            .unwrap()
            .query_map([cell], |row| Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, f32>(1)?
            )))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap(),
        [(room_a, -100.0), (room_b, 100.0)]
    );
    assert_eq!(
        db.query_row(
            "SELECT ref_id,room_a,room_b,unbounded_a,unbounded_b FROM room_portals WHERE cell_id=?1",
            [cell],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?, row.get::<_, u32>(2)?,
                     row.get::<_, bool>(3)?, row.get::<_, bool>(4)?)),
        )
        .unwrap(),
        (portal, room_a, room_b, false, false)
    );
    assert_eq!(
        placement_rules(db, cell),
        [(object, "target".into(), Some(room_a))]
    );
    assert_eq!(
        db.query_row(
            "SELECT base_form_id,is_exterior,contained FROM [references]
             JOIN reference_rooms ON id=reference_id WHERE id=?1 AND cell_id=?2 AND room_ref=?3",
            (object, cell, room_a),
            |row| Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, bool>(2)?
            )),
        )
        .unwrap(),
        (PLACEMENT_STAT, false, true)
    );
    assert!(placement_certificate(db, cell));
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM records WHERE form_id=?1 OR cell_id=?1",
            [cell],
            |row| row.get::<_, u32>(0),
        )
        .unwrap(),
        0,
    );
}

#[test]
fn damaged_plugin_occluders_leave_valid_neighbors_and_verified_link_slots_intact() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let cell = 0x0100_0900;
    let fresh = plugin(
        dir.path(),
        "FreshPlane.esp",
        &["Skyrim.esm"],
        0,
        [
            authored_cell(cell, Vec::new()),
            placement_group(
                cell,
                [
                    placement_marker(cell + 1, 0x1f, Vec::new()),
                    placement_marker(cell + 4, 23, Vec::new()),
                    placement_marker(cell + 5, 23, sub(b"XPRM", &[0; 32])),
                    placement(cell + 6, 23, 0, 0.0, sub(b"XPRM", &[0; 33])),
                    placement(cell + 2, 23, 0, 0.0, sub(b"XPRM", &[0; 31])),
                ]
                .concat(),
            ),
            placement_group(PLACEMENT_CELL_B, placement_marker(cell + 3, 23, Vec::new())),
        ]
        .concat(),
    );
    use converter::esm::placement_edits::{PlacementSource, apply};
    let report = apply(&assets, PlacementSource::SkyrimPlugin(&fresh)).unwrap();
    let geometry = &report.occluder_omissions.categories["occluder_unusable_geometry"];
    assert_eq!(geometry.count, 3);
    assert_eq!(
        geometry
            .samples
            .iter()
            .map(|sample| sample.form_id)
            .collect::<Vec<_>>(),
        [cell + 2, cell + 5, cell + 6]
    );
    assert!(
        geometry
            .samples
            .iter()
            .all(|sample| sample.reason.contains("XPRM"))
    );
    assert!(
        apply(&assets, PlacementSource::SkyrimPlugin(&fresh))
            .unwrap()
            .occluder_omissions
            .is_empty()
    );
    let shape_count = |id: u32| {
        db.query_row(
            "SELECT count(*) FROM room_occluders WHERE ref_id=?1",
            [id],
            |r| r.get::<_, u32>(0),
        )
        .unwrap()
    };
    assert_eq!(shape_count(cell + 2), 0);
    assert_eq!(shape_count(cell + 3), 1);
    assert_eq!(shape_count(cell + 5), 0);
    assert_eq!(shape_count(cell + 6), 0);
    assert!(!placement_certificate(&db, cell));
    assert!(placement_certificate(&db, PLACEMENT_CELL_B));

    let links_verified = || {
        db.query_row(
            "SELECT value FROM room_graph_stats WHERE name='occlusion_links_verified'",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    assert!(links_verified());

    // The moved marker has one broken slot and one valid outgoing link. Dropping
    // the entire XORD would make the latter's target usable as an independent plane.
    let slots = [0x0100_0904u32, 0x9900_0000, 0, 0];
    let moved = plugin(
        dir.path(),
        "MovedPlane.esp",
        &["Skyrim.esm", "FreshPlane.esp"],
        0,
        [
            placement_group(
                PLACEMENT_CELL_A,
                placement_marker(
                    cell + 3,
                    23,
                    sub(b"XORD", &slots.map(u32::to_le_bytes).concat()),
                ),
            ),
            placement_group(
                PLACEMENT_CELL_UNRELATED,
                placement_marker(0x0200_0900, 23, Vec::new()),
            ),
        ]
        .concat(),
    );
    let result = run_placement_import(&assets, "--plugin", &moved);
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success() && error.contains("01000903"),
        "{error}"
    );
    assert!(!links_verified());
    assert_eq!(shape_count(cell + 3), 0);
    assert_eq!(shape_count(0x0200_0900), 1);
    assert_eq!(
        db.query_row(
            "SELECT linked_ref FROM room_occluder_links WHERE ref_id=?1 AND link_index=0",
            [cell + 3],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        cell + 4
    );
    assert!(!placement_certificate(&db, PLACEMENT_CELL_A));
    assert!(!placement_certificate(&db, PLACEMENT_CELL_B));
    assert!(placement_certificate(&db, PLACEMENT_CELL_UNRELATED));
}

#[test]
fn plugin_placement_import_creates_and_certifies_a_light_interior() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let (cell, a, b, portal, object) = (
        0x0100_0900u32,
        0x0100_0901u32,
        0x0100_0902u32,
        0x0100_0903,
        0x0100_0904,
    );
    let source = plugin(
        dir.path(),
        "Fresh.esl",
        &["Skyrim.esm"],
        0x200,
        group(
            0,
            u32::from_le_bytes(*b"CELL"),
            group(
                2,
                0,
                group(
                    3,
                    0,
                    [
                        authored_cell(cell, sub(b"XCLL", &[0; 92])),
                        placement_group(
                            cell,
                            [
                                placement_marker_at(a, 0x1f, -100.0, Vec::new()),
                                placement_marker_at(b, 0x1f, 100.0, Vec::new()),
                                placement_marker(
                                    portal,
                                    0x20,
                                    sub(b"XPOD", &[a.to_le_bytes(), b.to_le_bytes()].concat()),
                                ),
                                placement(
                                    object,
                                    PLACEMENT_STAT,
                                    0,
                                    -50.0,
                                    sub(b"XMBR", &a.to_le_bytes()),
                                ),
                            ]
                            .concat(),
                        ),
                    ]
                    .concat(),
                ),
            ),
        ),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &source),
        "Applied 4 room placements in 1 cells (1 created, 1 certified)",
    );
    assert_created_interior(
        &db,
        0xfe00_0900,
        0xfe00_0901,
        0xfe00_0902,
        0xfe00_0903,
        0xfe00_0904,
    );
    assert_eq!(
        db.query_row(
            "SELECT xcll_size FROM cell_lighting WHERE cell_id=0xfe000900",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        92,
    );
    assert_eq!(
        db.query_row(
            "SELECT source_form_id FROM room_placement_edits WHERE form_id=0xfe000900",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        cell,
    );
    let followup = plugin(
        dir.path(),
        "FreshDecor.esp",
        &["Skyrim.esm", "Fresh.esl"],
        0,
        [
            record(b"CELL", cell, 0, Vec::new()),
            placement_group(
                cell,
                placement(0x0200_0800, PLACEMENT_STAT, 0, -70.0, Vec::new()),
            ),
        ]
        .concat(),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &followup),
        "Applied 1 room placements in 1 cells (1 certified)",
    );
    assert!(placement_certificate(&db, 0xfe00_0900));
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &source),
        "Applied 0 room placements in 0 cells (0 certified)",
    );
}

#[test]
fn empty_native_interior_keeps_its_certificate_until_rooms_are_added() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let cell = 0xff00_0010u32;
    let room = cell + 1;
    let portal = cell + 2;
    let run = |name: &str, value: serde_json::Value| {
        let path = dir.path().join(name);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        run_placement_import(&assets, "--native", &path)
    };
    assert_placement_import(
        run(
            "cell.json",
            serde_json::json!({"cells":[{"id":cell,"editor_id":"EmptyInterior"}]}),
        ),
        "Applied 0 room placements in 1 cells (1 created, 1 certified)",
    );
    let edits = |extents: [u32; 2]| {
        serde_json::json!({"edits":[
            {"op":"put_room","id":room,"cell":cell,"position":[0,0,0],"half_extents":[20,20,20]},
            {"op":"put_portal","id":portal,"cell":cell,"position":[0,0,0],"half_extents":extents,
             "endpoints":[room,0]}
        ]})
    };
    for extents in [[0, 8], [8, 0]] {
        let result = run("rooms.json", edits(extents));
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("portal half-extents"));
        assert!(placement_certificate(&db, cell));
        let state: (u32, u32) = db
            .query_row(
                "SELECT (SELECT count(*) FROM room_bounds WHERE cell_id=?1),
                    (SELECT count(*) FROM room_placement_layers)",
                [cell],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (0, 1), "rejected layer left the empty cell intact");
    }
    assert_placement_import(
        run("rooms.json", edits([4, 8])),
        "Applied 2 room placements in 1 cells (1 certified)",
    );
    assert!(placement_certificate(&db, cell));
    assert_eq!(
        db.query_row(
            "SELECT ref_id,room_a,unbounded_b FROM room_portals WHERE cell_id=?1",
            [cell],
            |row| Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, bool>(2)?
            ))
        )
        .unwrap(),
        (portal, room, true)
    );
}

#[test]
fn native_placement_import_creates_and_certifies_rooms_and_a_portal() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let cell = 0xff00_0001u32;
    let (a, b, portal, object) = (cell + 1, cell + 2, cell + 3, cell + 4);
    let source = dir.path().join("fresh.json");
    let native = serde_json::json!({
        "cells": [{"id": cell,"editor_id":"FreshInterior","full_name":"Fresh Interior"}],
        "edits": [
            {"op":"put","id":object,"cell":cell,"base":PLACEMENT_STAT,"position":[-50,0,0],
             "ownership":{"kind":"target","room":a}},
            {"op":"put_portal","id":portal,"cell":cell,"position":[0,0,0],
             "half_extents":[30,60],"endpoints":[a,b]},
            {"op":"put_room","id":b,"cell":cell,"position":[100,0,0],"half_extents":[100,100,100],
             "links":[a]},
            {"op":"put_room","id":a,"cell":cell,"position":[-100,0,0],"half_extents":[100,100,100],
             "links":[b],"master":true},
        ],
    });
    fs::write(&source, serde_json::to_vec(&native).unwrap()).unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &source),
        "Applied 4 room placements in 1 cells (1 created, 1 certified)",
    );
    assert_created_interior(&db, cell, a, b, portal, object);
    assert_eq!(
        db.query_row(
            "SELECT linked_ref,half_x,half_z FROM room_links JOIN room_portals ON ref_id=?2
             WHERE room_ref=?1",
            (a, portal),
            |row| Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, f32>(1)?,
                row.get::<_, f32>(2)?
            )),
        )
        .unwrap(),
        (b, 30.0, 60.0),
    );
    assert_placement_import(
        run_placement_import(&assets, "--native", &source),
        "Applied 0 room placements in 0 cells (0 certified)",
    );
    let master_byte = || {
        db.query_row(
            "SELECT (flags >> 8) & 255 FROM room_bounds WHERE ref_id=?1",
            [a],
            |row| row.get::<_, u8>(0),
        )
        .unwrap()
    };
    assert_eq!(master_byte(), 1);
    let mut edit = serde_json::json!({"op":"put_room","id":a,"cell":cell,"position":[-99,0,0],
        "half_extents":[101,100,100],"links":[b]});
    for expected_master in [1, 0] {
        if expected_master == 0 {
            edit["master"] = false.into();
        }
        fs::write(&source, serde_json::json!({"edits":[edit]}).to_string()).unwrap();
        assert_placement_import(
            run_placement_import(&assets, "--native", &source),
            "Applied 1 room placements in 1 cells (1 certified)",
        );
        assert_eq!(master_byte(), expected_master);
        assert_eq!(
            database_field(&db, "references", a, b"XRMR"),
            [1, 0, expected_master, 0]
        );
    }
}

#[test]
fn plugin_placement_import_rejects_unsupported_cells_and_keeps_an_incomplete_graph_uncertified() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let (cell, room, portal) = (0x0100_0900, 0x0100_0901u32, 0x0100_0902);
    for (bad, diagnostic) in [
        (
            authored_cell(cell + 1, sub(b"XCLC", &[0; 8])),
            "unsupported CELL field XCLC",
        ),
        (
            authored_cell(0x0100_1000, Vec::new()),
            "invalid record FormID",
        ),
        (
            placement_group(cell, placement_marker(cell, 0x1f, Vec::new())),
            "already in use",
        ),
        (
            placement_group(cell, placement_marker(0x800, 0x1f, Vec::new())),
            "new source-owned FormID",
        ),
    ] {
        let source = plugin(
            dir.path(),
            "Partial.esl",
            &["Skyrim.esm"],
            0x200,
            [authored_cell(cell, Vec::new()), bad].concat(),
        );
        let result = run_placement_import(&assets, "--plugin", &source);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(diagnostic));
        assert!(
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM cells WHERE id=0xfe000900)
                     OR EXISTS(SELECT 1 FROM cell_environment WHERE cell_id=0xfe000900)
                     OR EXISTS(SELECT 1 FROM sqlite_schema WHERE name='room_placement_layers')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        );
    }

    let source = plugin(
        dir.path(),
        "Partial.esl",
        &["Skyrim.esm"],
        0x200,
        [
            authored_cell(cell, Vec::new()),
            placement_group(
                cell,
                [
                    placement_marker(cell + 3, 0x1f, sub(b"XPRM", &[0; 32])),
                    placement_marker(room, 0x1f, Vec::new()),
                    placement_marker(
                        portal,
                        0x20,
                        sub(
                            b"XPOD",
                            &[room.to_le_bytes(), 0x9900_0001u32.to_le_bytes()].concat(),
                        ),
                    ),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &source),
        "Applied 2 room placements in 1 cells (1 created, 0 certified)",
    );
    assert_eq!(
        db.query_row(
            "SELECT complete,unbounded_a,unbounded_b FROM room_graph_cells JOIN room_portals USING(cell_id)
             WHERE cell_id=0xfe000900",
            [],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?, row.get::<_, bool>(2)?)),
        )
        .unwrap(),
        (false, false, false),
    );
    let replacement = plugin(
        dir.path(),
        "RepairPortal.esp",
        &["Skyrim.esm", "Partial.esl"],
        0,
        placement_group(
            cell,
            placement_marker(
                portal,
                0x20,
                sub(b"XPOD", &[room.to_le_bytes(), [0; 4]].concat()),
            ),
        ),
    );
    assert_placement_import(
        run_placement_import(&assets, "--plugin", &replacement),
        "Applied 1 room placements in 1 cells (0 certified)",
    );
    assert!(!placement_certificate(&db, 0xfe00_0900));
    assert!(placement_certificate(&db, PLACEMENT_CELL_A));
}

#[test]
fn native_new_cell_and_placement_ids_cannot_reuse_an_earlier_edit() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let used = 0xff00_0001u32;
    let cell = used + 1;
    let room = used + 2;
    let source = dir.path().join("edits.json");
    let put = |id, cell| {
        serde_json::json!({
            "op":"put","id":id,"cell":cell,"base":PLACEMENT_STAT,"position":[0,0,0],
        })
    };
    let run = |value| {
        fs::write(&source, serde_json::to_vec(&value).unwrap()).unwrap();
        run_placement_import(&assets, "--native", &source)
    };
    assert_placement_import(
        run(serde_json::json!({"edits":[put(used, PLACEMENT_CELL_A)]})),
        "Applied 1 room placements in 1 cells (1 certified)",
    );
    assert_placement_import(
        run(serde_json::json!({"edits":[{"op":"delete","id":used}]})),
        "Applied 1 room placements in 1 cells (1 certified)",
    );

    let room_edit = serde_json::json!({
        "op":"put_room","id":room,"cell":cell,"position":[0,0,0],"half_extents":[100,100,100],
    });
    let create = |id| serde_json::json!({"id":id,"editor_id":"FreshInterior"});
    for value in [
        serde_json::json!({"cells":[create(used)]}),
        serde_json::json!({"cells":[create(cell)],"edits":[room_edit.clone(), put(used, cell)]}),
        serde_json::json!({"cells":[create(cell)],"edits":[room_edit.clone(), put(cell, cell)]}),
    ] {
        let result = run(value);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("already in use"));
        assert_eq!(
            db.query_row(
                "SELECT (SELECT count(*) FROM room_placement_layers),
                        (SELECT count(*) FROM room_placement_edits),
                        EXISTS(SELECT 1 FROM cells WHERE id=?1),
                        EXISTS(SELECT 1 FROM [references] WHERE id=?2 OR cell_id=?1)",
                (cell, room),
                |row| Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, bool>(3)?
                )),
            )
            .unwrap(),
            (2, 2, false, false),
        );
    }
    db.execute("INSERT INTO room_graph_cells VALUES (?1,0)", [cell])
        .unwrap();
    let result = run(serde_json::json!({"cells":[create(cell)]}));
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already has associated data"));
    db.execute("DELETE FROM room_graph_cells WHERE cell_id=?1", [cell])
        .unwrap();
    assert_placement_import(
        run(serde_json::json!({"cells":[create(cell)],"edits":[room_edit, put(used + 3, cell)]})),
        "Applied 2 room placements in 1 cells (1 created, 1 certified)",
    );
    let result = run(serde_json::json!({"edits":[put(cell, PLACEMENT_CELL_A)]}));
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("FormID belongs to a CELL"));
    use converter::esm::rooms::{
        NativeOccluderChange, NativeOccluderShape, apply_native_occluders,
    };
    let plane = |id| NativeOccluderChange::Upsert {
        id,
        cell_id: cell,
        position: [0.; 3],
        rotation: [0.; 3],
        shape: NativeOccluderShape::Plane {
            half_width: 8.,
            half_height: 8.,
        },
    };
    let unused = used + 4;
    for occupied in [used, cell] {
        let error = apply_native_occluders(&db, &[plane(unused), plane(occupied)]).unwrap_err();
        assert!(error.to_string().contains("occupied native occluder ID"));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM room_occluders WHERE ref_id=?1",
                [unused],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
    }
    apply_native_occluders(&db, &[plane(unused)]).unwrap();
    let result = run(serde_json::json!({"cells":[create(unused)]}));
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already in use"));
    assert!(placement_certificate(&db, cell));
}

#[test]
fn placement_import_keeps_geometric_ownership_on_a_portal_only_pack() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    db.execute_batch(
        "DELETE FROM room_graph_stats WHERE name='assignment_version';
         DROP TABLE room_model_bounds;
         DROP TABLE room_reference_rules;
         ALTER TABLE room_links DROP COLUMN link_index;",
    )
    .unwrap();
    let source = dir.path().join("named.json");
    let id = 0xff00_0001u32;
    let edit = serde_json::json!({"edits": [{
        "op":"put", "id":id, "cell":PLACEMENT_CELL_A, "base":PLACEMENT_STAT,
        "position":[0,0,0], "ownership":{"kind":"target","room":0x800}
    }]});
    fs::write(&source, serde_json::to_vec(&edit).unwrap()).unwrap();
    assert_placement_import(
        run_placement_import(&assets, "--native", &source),
        "Applied 1 room placements in 1 cells (1 certified)",
    );
    assert_eq!(
        db.query_row(
            "SELECT header_flags, (SELECT count(*) FROM reference_rooms WHERE reference_id=?1),
             (SELECT count(*) FROM room_graph_stats WHERE name='assignment_version')
             FROM [references] WHERE id=?1",
            [id],
            |row| Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?
            ))
        )
        .unwrap(),
        (shared::records::MULTIBOUND_REFERENCE_FLAG, 0, 0)
    );
}

#[test]
fn both_pack_editors_reject_a_shared_database_before_starting_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("other.db");
    fs::write(&other, b"original").unwrap();
    fs::hard_link(&other, dir.path().join("skyrim_world.db")).unwrap();
    let placement = run_placement_import(dir.path(), "--native", &dir.path().join("edit.json"));
    let bounds = std::process::Command::new(env!("CARGO_BIN_EXE_room-model-bounds-refresh"))
        .arg(dir.path())
        .arg("meshes/test.glb")
        .output()
        .unwrap();
    for result in [placement, bounds] {
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            !result.status.success() && error.contains("private copy"),
            "{error}"
        );
    }
    assert_eq!(fs::read(other).unwrap(), b"original");
}

#[test]
fn duplicate_apertures_cannot_replace_a_certified_portal() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let source = plugin(
        dir.path(),
        "Aperture.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_B,
            [
                placement_marker(
                    0x801,
                    0x20,
                    [
                        sub(b"XPOD", &[0x820u32.to_le_bytes(), [0; 4]].concat()),
                        sub(b"XPRM", &[0; 32]),
                        sub(b"VMAD", &[0; 8]),
                    ]
                    .concat(),
                ),
                placement(0x820, 0x1f, 0, 32.0, sub(b"XPRM", &[0; 33])),
                placement(0x822, PLACEMENT_STAT, 0, 42.0, Vec::new()),
            ]
            .concat(),
        ),
    );
    let result = run_placement_import(&assets, "--plugin", &source);
    let warnings = String::from_utf8_lossy(&result.stderr);
    for id in ["00000801", "00000820"] {
        assert!(
            warnings.contains(&format!("{id} with repeated or oversized XPRM")),
            "{warnings}"
        );
    }
    assert_placement_import(result, "Applied 1 room placements in 2 cells (0 certified)");
    assert_eq!(placement_winner_cell(&db, 0x801), Some(PLACEMENT_CELL_A));
    assert_eq!(
        db.query_row(
            "SELECT cell_id,room_a FROM room_portals WHERE ref_id=0x801",
            [],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?))
        )
        .unwrap(),
        (PLACEMENT_CELL_A, 0x800)
    );
    assert_eq!(
        db.query_row("SELECT pos_x FROM [references] WHERE id=0x822", [], |row| {
            row.get::<_, f32>(0)
        })
        .unwrap(),
        42.0
    );
    assert!(!placement_certificate(&db, PLACEMENT_CELL_A));
    assert!(!placement_certificate(&db, PLACEMENT_CELL_B));
    assert!(placement_certificate(&db, PLACEMENT_CELL_UNRELATED));

    let unsupported = plugin(
        dir.path(),
        "Unsupported.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_B,
            [
                placement(0x822, PLACEMENT_STAT, 0, 60.0, Vec::new()),
                placement_marker(0x801, 0x20, sub(b"VMAD", &[0; 8])),
            ]
            .concat(),
        ),
    );
    let result = run_placement_import(&assets, "--plugin", &unsupported);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unsupported REFR field VMAD"));
    assert_eq!(
        db.query_row("SELECT pos_x FROM [references] WHERE id=0x822", [], |row| {
            row.get::<_, f32>(0)
        })
        .unwrap(),
        42.0
    );
}

#[test]
fn rejected_placement_moves_keep_both_cells_open_until_superseded() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    let portal = |room: u32| {
        placement_marker(
            0x850,
            0x20,
            sub(b"XPOD", &[room.to_le_bytes(), [0; 4]].concat()),
        )
    };
    let mut contents = [
        record(b"STAT", 0x830, 0, sub(b"EDID", b"FixtureStatic\0")),
        record(b"KYWD", 0x831, 0, sub(b"EDID", b"FixtureKeyword\0")),
    ]
    .concat();
    for (cell, room) in [(0x900, 0x800), (0x901, 0x810), (0x902, 0x820)] {
        contents.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        let mut placements = placement_marker(room, 0x1f, Vec::new());
        if cell == 0x900 {
            placements.extend(portal(room));
        }
        if cell == 0x902 {
            placements.extend(placement(0x851, 0x830, 0, 1.0, Vec::new()));
        }
        contents.extend(placement_group(cell, placements));
    }
    let base = plugin(&data, "Skyrim.esm", &[], 1, contents);
    let deleted = plugin(
        &data,
        "Fixed.esp",
        &["Skyrim.esm"],
        0,
        record(b"REFR", 0x850, 0x20, Vec::new()),
    );
    let restored = plugin(
        &data,
        "Restored.esp",
        &["Skyrim.esm"],
        0,
        placement_group(0x901, portal(0x810)),
    );
    let later_wrong_base = plugin(
        &data,
        "StillBroken.esp",
        &["Skyrim.esm"],
        0,
        placement_group(0x902, placement(0x850, 0x832, 0, 0.0, Vec::new())),
    );
    let cases = [
        (
            "torn",
            Some(0x901),
            record(b"REFR", 0x850, 0x40000, vec![0xff; 8]),
            [false, false, true],
        ),
        (
            "invalid-parent",
            Some(0x0900_0901),
            placement(0x850, 0x830, 0, 0.0, Vec::new()),
            [false, true, true],
        ),
        (
            "null-parent",
            Some(0),
            placement(0x850, 0x830, 0, 0.0, Vec::new()),
            [false, true, true],
        ),
        (
            "ungrouped",
            None,
            placement(0x850, 0x830, 0, 0.0, Vec::new()),
            [false, true, true],
        ),
        (
            "null-name",
            Some(0x901),
            placement(0x850, 0, 0, 0.0, Vec::new()),
            [false, false, true],
        ),
        (
            "short-transform",
            Some(0x901),
            record(
                b"REFR",
                0x850,
                0,
                [sub(b"NAME", &0x20u32.to_le_bytes()), sub(b"DATA", &[0; 20])].concat(),
            ),
            [false, false, true],
        ),
        (
            "duplicate-transform",
            Some(0x901),
            placement(0x850, 0x20, 0, 0.0, sub(b"DATA", &[0; 24])),
            [false, false, true],
        ),
        (
            "nonfinite-transform",
            Some(0x901),
            placement(0x850, 0x20, 0, f32::NAN, Vec::new()),
            [false, false, true],
        ),
        (
            "duplicate-live-bases",
            Some(0x901),
            placement(0x850, 0x20, 0, 0.0, sub(b"NAME", &0x830u32.to_le_bytes())),
            [false, false, true],
        ),
        (
            "null-first-name",
            Some(0x901),
            placement(0x850, 0, 0, 0.0, sub(b"NAME", &0x830u32.to_le_bytes())),
            [false, false, true],
        ),
        (
            "missing-name",
            Some(0x901),
            record(b"REFR", 0x850, 0, sub(b"DATA", &[0; 24])),
            [false, false, true],
        ),
        (
            "wrong-base-type",
            Some(0x901),
            placement(0x850, 0x831, 0, 0.0, Vec::new()),
            [false, false, true],
        ),
        (
            "missing-base-target",
            Some(0x901),
            placement(0x850, 0x832, 0, 0.0, Vec::new()),
            [false, false, true],
        ),
        (
            "wrong-parent-type",
            Some(0x830),
            placement(0x850, 0x830, 0, 0.0, Vec::new()),
            [false, true, true],
        ),
        (
            "missing-parent-target",
            Some(0x903),
            placement(0x850, 0x830, 0, 0.0, Vec::new()),
            [false, true, true],
        ),
    ];
    let mut failures = Vec::new();
    for (name, destination, candidate, unresolved) in cases {
        let rejected = match destination {
            Some(cell) => placement_group(cell, candidate),
            None => candidate,
        };
        let stat_control = placement_group(0x902, placement(0x851, 0x830, 0, 2.0, Vec::new()));
        let broken = plugin(
            &data,
            "Broken.esp",
            &["Skyrim.esm"],
            0,
            [rejected, stat_control].concat(),
        );
        if matches!(
            name,
            "duplicate-live-bases" | "null-name" | "missing-name" | "null-first-name"
        ) {
            let plugins = [base.clone(), broken.clone()];
            let order = LoadOrder::read(&plugins).unwrap();
            let decoded = converter::records::read_plugins(&plugins, &order).unwrap();
            assert_eq!(decoded.records[&0x850].cell_form_id, Some(0x900));
        }
        let mut cases = vec![
            (vec![&broken], Some(0x900), unresolved),
            (vec![&broken, &restored], Some(0x901), [true; 3]),
            (vec![&broken, &deleted], None, [true; 3]),
        ];
        if name == "wrong-base-type" {
            cases.push((vec![&broken, &later_wrong_base], Some(0x900), [false; 3]));
            cases.push((vec![&deleted, &broken], None, [true, false, true]));
        }
        for (overrides, expected_cell, expected_certificates) in cases {
            let plugins = std::iter::once(&base)
                .chain(overrides)
                .cloned()
                .collect::<Vec<_>>();
            for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
                let output = dir.path().join("world.db");
                EsmParser::convert_plugins_with_reader(&plugins, &output, reader, &data).unwrap();
                let db = rusqlite::Connection::open(output).unwrap();
                let certificates =
                    [0x900, 0x901, 0x902].map(|cell| placement_certificate(&db, cell));
                let winner = placement_winner_cell(&db, 0x850);
                let stat_x: f32 = db
                    .query_row("SELECT pos_x FROM [references] WHERE id=0x851", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                let projected: Option<u32> = db
                    .query_row(
                        "SELECT max(cell_id) FROM room_portals WHERE ref_id=0x850",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                if certificates != expected_certificates
                    || winner != expected_cell
                    || projected != expected_cell
                    || stat_x != 2.0
                {
                    failures.push(format!(
                        "{reader:?} {name} plugins={plugins:?}: certificates={certificates:?}, winner={winner:?}, portal={projected:?}, STAT control x={stat_x}; expected {expected_certificates:?}, {expected_cell:?}, STAT control x=2"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn rejected_placement_collision_keeps_destination_open_without_a_proven_alias() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut contents = record(b"STAT", 0x830, 0, sub(b"EDID", b"FixtureStatic\0"));
    for (cell, room) in [(0x900, 0x800), (0x901, 0x810), (0x902, 0x820)] {
        contents.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        contents.extend(placement_group(
            cell,
            placement_marker(room, 0x1f, Vec::new()),
        ));
    }
    let base = plugin(data, "Skyrim.esm", &[], 1, contents);
    let patch = plugin(
        data,
        "Patch.esp",
        &["Skyrim.esm"],
        0,
        [
            placement_group(0x900, placement(0x0100_0850, 0x830, 0, 0.0, Vec::new())),
            placement_group(0x901, placement(0x0900_0850, 0x830, 0, 0.0, Vec::new())),
        ]
        .concat(),
    );
    let followup = plugin(
        data,
        "Fixed.esp",
        &["Skyrim.esm", "Patch.esp"],
        0,
        placement_group(0x902, placement(0x0100_0850, 0x830, 0, 0.0, Vec::new())),
    );
    let mut failures = Vec::new();
    for moved in [false, true] {
        let plugins = [base.clone(), patch.clone()]
            .into_iter()
            .chain(moved.then(|| followup.clone()))
            .collect::<Vec<_>>();
        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let output = data.join("world.db");
            EsmParser::convert_plugins_with_reader(&plugins, &output, reader, data).unwrap();
            let db = rusqlite::Connection::open(output).unwrap();
            let certificates = [0x900, 0x901, 0x902].map(|cell| placement_certificate(&db, cell));
            let winner = placement_winner_cell(&db, 0x0100_0850);
            let expected_cell = Some(if moved { 0x902 } else { 0x900 });
            if certificates != [true, false, true] || winner != expected_cell {
                failures.push(format!(
                    "{reader:?} moved={moved}: certificates={certificates:?}, winner={winner:?}; expected [true, false, true], {expected_cell:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn placement_winner_cell(db: &rusqlite::Connection, id: u32) -> Option<u32> {
    use rusqlite::OptionalExtension;

    db.query_row(
        "SELECT cell_id FROM [references] WHERE id=?1",
        [id],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

#[test]
fn a_source_null_and_a_missing_room_have_different_cell_certificates() {
    for typed in [false, true] {
        for (target, valid) in [(0u32, true), (0x801, true), (0x910, false)] {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path().join("Data");
            room_graph_review_fixture(
                &data,
                None,
                Some(sub(
                    b"XPOD",
                    &[0x800u32.to_le_bytes(), target.to_le_bytes()].concat(),
                )),
            );
            let db = rusqlite::Connection::open(export_room_review_fixture(
                &data,
                &dir.path().join("bundle"),
                typed,
            ))
            .unwrap();
            assert_eq!(
                placement_certificate(&db, 0x900),
                valid,
                "typed={typed} target={target}"
            );
        }
    }
}

#[test]
fn source_backed_door_edits_refresh_only_affected_links() {
    use converter::esm::{doors::rebuild_door_links, exporter::export_to_db};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let assets = root.join("assets");
    fs::create_dir(&assets).unwrap();
    let database = assets.join("skyrim_world.db");
    let base = 0x500;
    let entry = 0x801;
    let moved = 0x802;
    let neighbor = 0x803;
    let hall = 0x900;
    let cellar = 0x901;
    let named_cell = |id, name: &[u8]| {
        record(
            b"CELL",
            id,
            0,
            [sub(b"DATA", &[1]), sub(b"FULL", name)].concat(),
        )
    };
    let teleport = |target: u32, arrival: f32| {
        let mut xtel = [0; 32];
        xtel[..4].copy_from_slice(&target.to_le_bytes());
        xtel[4..8].copy_from_slice(&arrival.to_le_bytes());
        sub(b"XTEL", &xtel)
    };
    let door = |id, target, arrival| placement(id, base, 0, 0.0, teleport(target, arrival));
    let original = plugin(
        root,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"DOOR", base, 0, Vec::new()),
            named_cell(hall, b"Hall\0"),
            placement_group(
                hall,
                [door(entry, moved, 42.0), door(neighbor, entry, 7.0)].concat(),
            ),
            named_cell(cellar, b"Cellar\0"),
            placement_group(cellar, door(moved, entry, 12.0)),
        ]
        .concat(),
    );
    let convert = || {
        EsmParser::convert_plugins_with_reader(
            std::slice::from_ref(&original),
            &database,
            converter::config::RecordReader::Inhouse,
            root,
        )
        .unwrap();
        rusqlite::Connection::open(&database).unwrap()
    };
    let move_door = plugin(
        root,
        "MoveDoor.esp",
        &["Skyrim.esm"],
        0,
        placement_group(hall, door(moved, entry, 13.0)),
    );
    let links = |db: &rusqlite::Connection| {
        db.prepare(
            "SELECT ref_id,destination_ref_id,destination_cell_id,destination_name,pos_x
             FROM door_links ORDER BY ref_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f32>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    };
    let after_move = vec![
        (entry, moved, hall, "Hall".to_owned(), 42.0),
        (moved, entry, hall, "Hall".to_owned(), 13.0),
        (neighbor, entry, hall, "Hall".to_owned(), 7.0),
    ];
    let protect_neighbor = |db: &rusqlite::Connection| {
        db.execute_batch(&format!(
            "CREATE TRIGGER protect_neighbor BEFORE DELETE ON door_links
             WHEN OLD.ref_id={neighbor}
             BEGIN SELECT RAISE(ABORT, 'unrelated door link was deleted'); END;"
        ))
        .unwrap();
    };
    let import = |source: &Path| {
        let output = run_placement_import(&assets, "--plugin", source);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    let db = convert();
    assert_eq!(
        links(&db)[0],
        (entry, moved, cellar, "Cellar".to_owned(), 42.0)
    );
    protect_neighbor(&db);
    let mut winning = EsmParser::merge_plugins(&[original.clone(), move_door.clone()]).unwrap();
    export_to_db(&db, &[(moved, winning.remove(&moved).unwrap())].into()).unwrap();
    assert_eq!(links(&db), after_move, "subset export");
    db.execute_batch("DROP TRIGGER protect_neighbor").unwrap();
    drop(db);

    let db = convert();
    let before = links(&db);
    protect_neighbor(&db);
    db.execute_batch(&format!(
        "CREATE TRIGGER reject_door_refresh BEFORE INSERT ON door_links
         WHEN NEW.destination_ref_id={moved}
         BEGIN SELECT RAISE(ABORT, 'door refresh rejected'); END;"
    ))
    .unwrap();
    let rejected = run_placement_import(&assets, "--plugin", &move_door);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("door refresh rejected"));
    assert_eq!(links(&db), before);
    let cell: u32 = db
        .query_row(
            "SELECT cell_id FROM [references] WHERE id=?1",
            [moved],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cell, cellar);
    db.execute_batch("DROP TRIGGER reject_door_refresh")
        .unwrap();

    import(&move_door);
    assert_eq!(links(&db), after_move, "placement import");

    let remove_teleport = plugin(
        root,
        "RemoveTeleport.esp",
        &["Skyrim.esm"],
        0,
        placement_group(hall, placement(moved, base, 0, 0.0, Vec::new())),
    );
    import(&remove_teleport);
    assert_eq!(links(&db), [after_move[0].clone(), after_move[2].clone()]);

    let delete_door = plugin(
        root,
        "DeleteDoor.esp",
        &["Skyrim.esm"],
        0,
        placement_group(hall, record(b"REFR", moved, 0x20, Vec::new())),
    );
    import(&delete_door);
    assert_eq!(links(&db), [after_move[2].clone()]);

    let add_door = plugin(
        root,
        "AddDoor.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            hall,
            [door(moved, entry, 13.0), door(0x0100_0810, moved, 17.0)].concat(),
        ),
    );
    import(&add_door);
    let mut after_add = after_move;
    after_add.push((0x0400_0810, moved, hall, "Hall".to_owned(), 17.0));
    assert_eq!(links(&db), after_add);
    db.execute_batch("DROP TRIGGER protect_neighbor").unwrap();
    assert_eq!(rebuild_door_links(&db).unwrap(), 4);
    assert_eq!(links(&db), after_add, "rebuild after placement imports");

    let mut invalid_target = [0; 32];
    invalid_target[..4].copy_from_slice(&0x9900_0801u32.to_le_bytes());
    let mut nonfinite_rotation = teleport(entry, 2.0);
    nonfinite_rotation[30..34].copy_from_slice(&f32::NEG_INFINITY.to_le_bytes());
    for (name, xtel, arrival) in [
        ("WrongWidth.esp", sub(b"XTEL", &[0; 28]), 21.0),
        ("InvalidMaster.esp", sub(b"XTEL", &invalid_target), 22.0),
        (
            "ConflictingDestinations.esp",
            [teleport(entry, 2.0), teleport(neighbor, 3.0)].concat(),
            23.0,
        ),
        ("NonfinitePosition.esp", teleport(entry, f32::NAN), 24.0),
        ("NonfiniteRotation.esp", nonfinite_rotation, 25.0),
    ] {
        let malformed = plugin(
            root,
            name,
            &["Skyrim.esm"],
            0,
            [
                placement_group(cellar, placement(moved, base, 0, 0.0, xtel)),
                placement_group(hall, door(neighbor, entry, arrival)),
            ]
            .concat(),
        );
        let result = run_placement_import(&assets, "--plugin", &malformed);
        assert!(String::from_utf8_lossy(&result.stderr).contains("malformed XTEL"));
        assert_placement_import(result, "Applied 1 room placements in 2 cells (0 certified)");
        after_add[2].4 = arrival;
        assert_eq!(
            links(&db),
            after_add,
            "malformed teleport retains both original links"
        );
    }
    assert!(!placement_certificate(&db, hall));
    assert!(!placement_certificate(&db, cellar));
}

#[test]
fn portal_geometry_requires_one_valid_transform_and_positive_aperture() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    fs::create_dir(&data).unwrap();
    let shape = |x: f32, z: f32, kind: u32| {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(&x.to_le_bytes());
        bytes[8..12].copy_from_slice(&z.to_le_bytes());
        bytes[28..].copy_from_slice(&kind.to_le_bytes());
        bytes
    };
    let aperture = |x, z| sub(b"XPRM", &shape(x, z, 3));
    let transform = sub(b"DATA", &[0; 24]);
    let aperture_valid = aperture(40.0, 60.0);
    let mut source = Vec::new();
    let cases = [
        ("valid", transform.clone(), aperture_valid.clone()),
        (
            "duplicate transform",
            [transform.clone(), sub(b"DATA", &[1; 24])].concat(),
            aperture_valid.clone(),
        ),
        (
            "duplicate aperture",
            transform.clone(),
            [aperture_valid.clone(), aperture(80.0, 90.0)].concat(),
        ),
        (
            "malformed duplicate aperture",
            transform.clone(),
            [aperture_valid.clone(), sub(b"XPRM", &[0; 4])].concat(),
        ),
        ("missing transform", Vec::new(), aperture_valid.clone()),
        (
            "short transform",
            sub(b"DATA", &[0; 20]),
            aperture_valid.clone(),
        ),
        ("missing aperture", transform.clone(), Vec::new()),
        (
            "12-byte aperture",
            transform.clone(),
            sub(b"XPRM", &shape(40.0, 60.0, 3)[..12]),
        ),
        (
            "wrong shape",
            transform.clone(),
            sub(b"XPRM", &shape(40.0, 60.0, 1)),
        ),
        ("zero width", transform.clone(), aperture(0.0, 60.0)),
        ("zero height", transform, aperture(40.0, 0.0)),
    ];
    for (index, (_, transform, aperture)) in cases.iter().enumerate() {
        let cell = 0x900 + index as u32;
        let room = 0x800 + index as u32 * 3;
        let ends = [room.to_le_bytes(), (room + 1).to_le_bytes()].concat();
        source.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        source.extend(placement_group(
            cell,
            [
                placement_marker(room, 0x1f, Vec::new()),
                placement_marker_at(room + 1, 0x1f, 500.0, Vec::new()),
                record(
                    b"REFR",
                    room + 2,
                    0,
                    [
                        sub(b"NAME", &0x20u32.to_le_bytes()),
                        transform.clone(),
                        aperture.clone(),
                        sub(b"XPOD", &ends),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        ));
    }
    plugin(&data, "Skyrim.esm", &[], 1, source);
    for typed in [false, true] {
        let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
        let conn = rusqlite::Connection::open(db).unwrap();
        for (index, (case, _, _)) in cases.iter().enumerate() {
            let cell = 0x900 + index as u32;
            let (complete, portals): (bool, u32) = conn
                .query_row(
                    "SELECT complete,(SELECT count(*) FROM room_portals WHERE cell_id=?1)
                     FROM room_graph_cells WHERE cell_id=?1",
                    [cell],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let valid = *case == "valid";
            assert_eq!(
                (complete, portals),
                (valid, u32::from(valid)),
                "typed={typed}, {case}"
            );
        }
    }
}

#[test]
fn damaged_groups_and_roomless_placements_keep_negative_certificates() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    fs::create_dir(&data).unwrap();
    let cell = |id| record(b"CELL", id, 0, sub(b"DATA", &[1, 0]));
    let room = |id| placement_marker(id, 0x1f, Vec::new());
    let truncated_group = |kind, label| {
        let mut bytes = group(kind, label, vec![0]);
        bytes.pop();
        bytes
    };
    let mut torn_placement = placement(0x802, 0x999, 0, 0.0, Vec::new());
    torn_placement.extend(b"XL");
    let size = (torn_placement.len() - 24) as u32;
    torn_placement[4..8].copy_from_slice(&size.to_le_bytes());
    plugin(
        &data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", 0x999, 0, sub(b"EDID", b"RoomlessStatic\0")),
            cell(0x900),
            group(
                6,
                0x900,
                [group(9, 0x900, room(0x800)), truncated_group(9, 0x900)].concat(),
            ),
            cell(0x901),
            placement_group(0x901, room(0x801)),
            cell(0x902),
            group(6, 0x902, truncated_group(9, 0x902)),
            cell(0x903),
            placement_group(0x903, torn_placement),
            cell(0x904),
            placement_group(0x904, placement(0x803, 0x999, 0, 0.0, Vec::new())),
        ]
        .concat(),
    );
    for typed in [false, true] {
        let db = export_room_review_fixture(&data, &directory.path().join("bundle"), typed);
        let conn = rusqlite::Connection::open(db).unwrap();
        assert!(conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM [references] WHERE cell_id=0x904 AND base_form_id=0x999)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
        let mut stmt = conn
            .prepare(
                "SELECT cell_id,complete,(SELECT count(*) FROM room_bounds WHERE cell_id=g.cell_id)
                 FROM room_graph_cells g ORDER BY cell_id",
            )
            .unwrap();
        let certificates = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            certificates,
            [
                (0x900, false, 1),
                (0x901, true, 1),
                (0x902, false, 0),
                (0x903, false, 0)
            ],
            "typed={typed}"
        );
    }
}

#[test]
fn placement_import_accepts_matching_room_marker_extents_and_a_pack_named_like_help() {
    let dir = tempfile::tempdir().unwrap();
    let assets = converted_placement_fixture(dir.path());
    let pack = dir.path().join("-h");
    fs::rename(assets, &pack).unwrap();
    let db = rusqlite::Connection::open(pack.join("skyrim_world.db")).unwrap();
    let extents = 100f32.to_le_bytes().repeat(3);
    let replace = |fields| {
        plugin(
            dir.path(),
            "Marker.esp",
            &["Skyrim.esm"],
            0,
            placement_group(PLACEMENT_CELL_A, placement_marker(0x800, 0x1f, fields)),
        )
    };
    for fields in [
        sub(b"XMBO", &[0; 12]),
        sub(b"XMBO", &[0; 11]),
        [sub(b"XMBO", &extents), sub(b"XMBO", &extents)].concat(),
    ] {
        let failure = run_placement_import(&pack, "--plugin", &replace(fields));
        assert!(!failure.status.success());
        assert!(String::from_utf8_lossy(&failure.stderr).contains("XMBO"));
        assert!(placement_certificate(&db, PLACEMENT_CELL_A));
    }
    assert_placement_import(
        run_placement_import(&pack, "--plugin", &replace(sub(b"XMBO", &extents))),
        "Applied 1 room placements in 1 cells (1 certified)",
    );
    let delete = dir.path().join("delete.json");
    fs::write(&delete, br#"{"edits":[{"op":"delete","id":2048}]}"#).unwrap();
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_converter"))
            .current_dir(dir.path())
            .arg("apply-room-placements")
            .args(args)
            .output()
            .unwrap()
    };
    assert!(String::from_utf8_lossy(&cli(&["--help"]).stdout).contains("--native FILE"));
    assert_placement_import(
        cli(&["-h", "--native", "delete.json"]),
        "Applied 1 room placements in 1 cells (0 certified)",
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM [references] WHERE id=0x800",
            [],
            |row| row.get::<_, usize>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn a_short_nonplacement_header_uncertifies_only_its_own_interior() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    let mut records = Vec::new();
    for (cell, room) in [(0x900, 0x800), (0x901, 0x810)] {
        records.extend(record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])));
        let mut children = placement_marker(room, 0x1f, Vec::new());
        if cell == 0x900 {
            children.extend(b"NAVM");
        }
        records.extend(placement_group(cell, children));
    }
    plugin(&data, "Skyrim.esm", &[], 1, records);
    for typed in [false, true] {
        let path = export_room_review_fixture(&data, &dir.path().join("bundle"), typed);
        let db = rusqlite::Connection::open(path).unwrap();
        assert_eq!(
            stored_placement_certificate(&db, 0x900),
            Some(false),
            "typed={typed}"
        );
        assert!(placement_certificate(&db, 0x901), "typed={typed}");
    }
}

#[test]
fn malformed_placed_base_keeps_its_known_cell_open() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    let damaged = record(b"REFR", 0x810, 0, sub(b"NAME", &[1, 2, 3]));
    room_graph_review_fixture_with_fields(&data, None, None, vec![damaged]);
    for (name, typed) in [("legacy", false), ("typed", true)] {
        let db = rusqlite::Connection::open(export_room_review_fixture(
            &data,
            &dir.path().join(name),
            typed,
        ))
        .unwrap();
        assert_eq!(
            stored_placement_certificate(&db, 0x900),
            Some(false),
            "{name} must persist the damaged placement's cell certificate"
        );
        let admitted: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE form_id=0x810)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!admitted, "{name} admitted a damaged placement");
    }
}

#[test]
fn first_placement_with_nonfinite_transform_keeps_valid_neighbors() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            placement_group(
                0x900,
                [
                    placement_marker_at(0x850, 0x1f, 1.0, Vec::new()),
                    placement_marker_at(0x851, 0x1f, f32::NAN, Vec::new()),
                    placement_marker_at(0x852, 0x1f, 2.0, Vec::new()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let deletion = plugin(
        data,
        "Delete.esp",
        &["Skyrim.esm"],
        0,
        record(b"REFR", 0x850, 0x20, Vec::new()),
    );
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let base_only = data.join(format!("{reader:?}-base.db"));
        EsmParser::convert_plugins_with_reader(
            std::slice::from_ref(&base),
            &base_only,
            reader,
            data,
        )
        .unwrap();
        let db = rusqlite::Connection::open(base_only).unwrap();
        for (id, expected_cell) in [(0x850, Some(0x900)), (0x851, None), (0x852, Some(0x900))] {
            assert_eq!(
                placement_winner_cell(&db, id),
                expected_cell,
                "{reader:?} {id:#x}"
            );
        }
        let output = data.join(format!("{reader:?}.db"));
        let records = EsmParser::convert_plugins_with_reader(
            &[base.clone(), deletion.clone()],
            &output,
            reader,
            data,
        )
        .unwrap();
        assert!(!records.contains_key(&0x851), "{reader:?}");
        let db = rusqlite::Connection::open(output).unwrap();
        assert_eq!(placement_winner_cell(&db, 0x850), None, "{reader:?}");
        assert_eq!(placement_winner_cell(&db, 0x852), Some(0x900), "{reader:?}");
        assert_eq!(
            stored_placement_certificate(&db, 0x900),
            Some(false),
            "{reader:?}"
        );
    }
}

#[test]
fn typed_placement_admission_uses_the_retained_transform() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut ignored = [0; 24];
    ignored[..4].copy_from_slice(&f32::NAN.to_le_bytes());
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            placement_group(
                0x900,
                [
                    placement(0x850, 0x1f, 0, 2., sub(b"DATA", &ignored)),
                    placement(0x851, 0x1f, 0, 3., vec![]),
                    placement(0x852, 0x1f, 0, 5., vec![]),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let patch = plugin(
        data,
        "Update.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            0x900,
            [
                placement(0x851, 0x1f, 0, 4., sub(b"DATA", &ignored)),
                placement(0x852, 0x1f, 0, f32::NAN, sub(b"DATA", &[0; 24])),
            ]
            .concat(),
        ),
    );
    let output = data.join("world.db");
    EsmParser::convert_plugins_with_reader(
        &[base, patch],
        &output,
        converter::config::RecordReader::Inhouse,
        data,
    )
    .unwrap();
    let db = rusqlite::Connection::open(output).unwrap();
    let position = |id| {
        db.query_row("SELECT pos_x FROM [references] WHERE id=?1", [id], |r| {
            r.get::<_, f64>(0)
        })
        .unwrap()
    };
    assert_eq!(position(0x850), 2.);
    assert_eq!(
        position(0x851),
        3.,
        "trailing DATA preserves the predecessor"
    );
    assert_eq!(
        position(0x852),
        0.,
        "a rejected prefix permits the valid override"
    );
}

#[tokio::test]
async fn later_base_deletion_is_a_reported_omission() {
    use converter::AssetPipeline;
    use converter::config::RecordReader;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    let base = 0x000B_D6A5;
    let omitted = 0x0010_C5CD;
    let cell = 0x900;
    plugin(
        &data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
            record(b"STAT", base, 0, sub(b"EDID", b"RemovedStatic\0")),
            placement_group(
                cell,
                [
                    placement(omitted, base, 0, 10.0, Vec::new()),
                    placement_marker_at(0x851, 0x1f, 11.0, Vec::new()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    plugin(
        &data,
        "Dawnguard.esm",
        &["Skyrim.esm"],
        1,
        record(b"STAT", base, 0x20, Vec::new()),
    );
    let list = dir.path().join("plugins.txt");
    for damaged in [false, true] {
        let mut selection = String::from("*Skyrim.esm\n*Dawnguard.esm\n");
        if damaged {
            // An author referring to a base already deleted, or never defined, has lost data.
            plugin(
                &data,
                "Broken.esp",
                &["Skyrim.esm", "Dawnguard.esm"],
                0,
                placement_group(
                    cell,
                    [
                        placement(0x0200_0852, base, 0, 12.0, Vec::new()),
                        placement(0x0200_0853, 0x875, 0, 13.0, Vec::new()),
                    ]
                    .concat(),
                ),
            );
            selection.push_str("*Broken.esp\n");
        }
        fs::write(&list, selection).unwrap();
        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let output = dir.path().join(format!("{reader:?}-{damaged}"));
            let mut config = common::cpu_lod_config(&data, &output);
            config.record_reader = reader;
            config.no_lod = true;
            config.plugins_file = Some(list.clone());
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let report = AssetPipeline::run_async(config, tx).await.unwrap();
            drain.await.unwrap();
            assert!(
                report
                    .notices
                    .iter()
                    .any(|line| line.contains("omitted 1 placement candidate")
                        && line.contains("0010C5CD")
                        && line.contains("000BD6A5")),
                "{:?}",
                report.notices
            );
            assert_eq!(report.complete, !damaged, "{:?}", report.warnings);
            if damaged {
                assert!(
                    report
                        .warnings
                        .iter()
                        .any(|line| line.to_lowercase().contains("broken.esp")
                            && (line.contains("skipped 2 records")
                                || line.contains("skipped 2 unusable"))),
                    "{:?}",
                    report.warnings
                );
            }
            let manifest: converter::cache::ConversionManifest =
                serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
                    .unwrap();
            assert_eq!(manifest.record_data_complete, Some(!damaged));
            let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
            assert_eq!(placement_winner_cell(&db, omitted), None);
            assert_eq!(placement_winner_cell(&db, 0x851), Some(cell));
            assert_eq!(stored_placement_certificate(&db, cell), Some(!damaged));
        }
    }
}

#[test]
fn placement_winner_uses_forward_and_effective_catalogue_targets() {
    use converter::config::RecordReader;
    use rusqlite::OptionalExtension;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            record(b"STAT", 0x830, 0, sub(b"EDID", b"OriginalStatic\0")),
            placement_group(0x900, placement(0x850, 0x830, 0, 1.0, Vec::new())),
        ]
        .concat(),
    );
    let patch = plugin(
        data,
        "Patch.esp",
        &["Skyrim.esm"],
        0,
        [
            placement_group(
                0x0100_0901,
                placement(0x850, 0x0100_0831, 0, 2.0, Vec::new()),
            ),
            record(b"STAT", 0x0100_0831, 0, sub(b"EDID", b"ForwardStatic\0")),
            record(b"CELL", 0x0100_0901, 0, sub(b"DATA", &[1, 0])),
        ]
        .concat(),
    );
    let original = (0x900, 0x830);
    let moved = (0x0100_0901, 0x0100_0831);
    let cases = [
        ("forward declarations", None, moved, None),
        (
            "effective base changed type",
            Some(record(b"KYWD", moved.1, 0, sub(b"EDID", b"NotAStatic\0"))),
            original,
            Some("required NAME"),
        ),
        (
            "effective parent deleted",
            Some(record(b"CELL", moved.0, 0x20, Vec::new())),
            original,
            Some("placement parent"),
        ),
        (
            "invalid placement temporarily shadows the base",
            Some(placement_group(
                0x900,
                placement(moved.1, 0x832, 0, 0.0, Vec::new()),
            )),
            moved,
            None,
        ),
    ];
    let mut failures = Vec::new();
    for (name, extra, expected, rejection) in cases {
        let plugins =
            [base.clone(), patch.clone()]
                .into_iter()
                .chain(extra.map(|payload| {
                    plugin(data, "Later.esp", &["Skyrim.esm", "Patch.esp"], 0, payload)
                }))
                .collect::<Vec<_>>();
        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let output = data.join("winner.db");
            EsmParser::convert_plugins_with_reader(&plugins, &output, reader, data).unwrap();
            let db = rusqlite::Connection::open(output).unwrap();
            let winner = db
                .query_row(
                    "SELECT cell_id, base_form_id FROM [references] WHERE id=0x850",
                    [],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
                )
                .optional()
                .unwrap();
            if winner != Some(expected) {
                failures.push(format!(
                    "{reader:?} {name}: {winner:08X?}, expected {expected:08X?}"
                ));
            }
            if matches!(reader, RecordReader::Inhouse) {
                let (status, reason): (String, Option<String>) = db
                    .query_row(
                        "SELECT status, reason FROM record_overrides
                         WHERE form_id=0x850 ORDER BY chain_position DESC LIMIT 1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                let expected_status = if rejection.is_some() {
                    "skipped"
                } else {
                    "accepted"
                };
                if status != expected_status
                    || rejection
                        .is_some_and(|part| !reason.is_some_and(|reason| reason.contains(part)))
                {
                    failures.push(format!("{reader:?} {name}: expected {expected_status} ({rejection:?}), got {status}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn placement_winner_rejects_deleted_low_base_and_accepts_never_defined_marker() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(
        dir.path(),
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            record(b"STAT", 0x730, 0, sub(b"EDID", b"LowStatic\0")),
            record(b"STAT", 0x830, 0, sub(b"EDID", b"FallbackStatic\0")),
            placement_group(
                0x900,
                [
                    placement(0x850, 0x830, 0, 1.0, Vec::new()),
                    placement_marker(0x851, 0x1f, Vec::new()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Skyrim.esm"],
        0,
        placement_group(0x900, placement(0x850, 0x730, 0, 2.0, Vec::new())),
    );
    let delete = plugin(
        dir.path(),
        "Delete.esp",
        &["Skyrim.esm"],
        0,
        record(b"STAT", 0x730, 0x20, Vec::new()),
    );
    let restore = plugin(
        dir.path(),
        "Restore.esp",
        &["Skyrim.esm"],
        0,
        record(b"STAT", 0x730, 0, sub(b"EDID", b"RestoredStatic\0")),
    );
    let mut failures = Vec::new();
    for (label, plugins, expected) in [
        ("live", vec![base.clone(), patch.clone()], 0x730),
        (
            "deleted",
            vec![base.clone(), patch.clone(), delete.clone()],
            0x830,
        ),
        ("restored", vec![base, patch, delete, restore], 0x730),
    ] {
        check_placement_winners(
            &plugins,
            label,
            &[
                (0x850, Some((Some(0x900), expected))),
                (0x851, Some((Some(0x900), 0x1f))),
            ],
            &mut failures,
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

type CellAndBase = (Option<u32>, u32);

fn check_placement_winners(
    plugins: &[PathBuf],
    label: &str,
    expected: &[(u32, Option<CellAndBase>)],
    failures: &mut Vec<String>,
) {
    use converter::config::RecordReader;

    let output = tempfile::tempdir().unwrap();
    let data = plugins[0].parent().unwrap();
    for (reader, name) in [
        (RecordReader::Legacy, "legacy"),
        (RecordReader::Inhouse, "typed"),
    ] {
        let db = output.path().join(format!("{name}.db"));
        let records = EsmParser::convert_plugins_with_reader(plugins, &db, reader, data).unwrap();
        for &(id, expected) in expected {
            let actual = records.get(&id).map(|record| {
                let base = SubrecordView::new(&record.subrecords)
                    .get_form_id(b"NAME")
                    .unwrap();
                (record.cell_form_id, base)
            });
            if actual != expected {
                failures.push(format!(
                    "{name} {label} {id:08X}: {actual:08X?}, expected {expected:08X?}"
                ));
            }
        }
    }
}

#[test]
fn placement_winner_checks_ungrouped_override_against_usable_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(
        dir.path(),
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            record(b"STAT", 0x830, 0, sub(b"EDID", b"OriginalStatic\0")),
            record(b"STAT", 0x831, 0, sub(b"EDID", b"ReplacementStatic\0")),
            record(b"STAT", 0x834, 0, sub(b"EDID", b"RestorableStatic\0")),
            placement_group(
                0x900,
                [
                    placement(0x850, 0x832, 0, 1.0, Vec::new()),
                    placement(0x851, 0x830, 0, 1.0, Vec::new()),
                    placement(0x852, 0x832, 0, 1.0, Vec::new()),
                    placement(0x853, 0x830, 0, 1.0, Vec::new()),
                    placement(0x854, 0x830, 0, 1.0, Vec::new()),
                    placement(0x855, 0x834, 0, 1.0, Vec::new()),
                    record(b"REFR", 0x856, 0, sub(b"NAME", &0x830u32.to_le_bytes())),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Skyrim.esm"],
        0,
        [
            placement(0x850, 0x831, 0, 2.0, Vec::new()),
            placement(0x856, 0x831, 0, 2.0, Vec::new()),
            placement(0x851, 0x831, 0, 2.0, Vec::new()),
            placement(0x852, 0x832, 0, 2.0, Vec::new()),
            // The prior owner of 855 needs the STAT hidden by 834's invalid move.
            placement(0x855, 0x831, 0, 2.0, Vec::new()),
            placement_group(0x901, placement(0x834, 0x855, 0, 2.0, Vec::new())),
            placement_group(
                0x900,
                [
                    placement(0x853, 0x831, 0, 2.0, Vec::new()),
                    placement(0x854, 0x832, 0, 2.0, Vec::new()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );

    let later = plugin(
        dir.path(),
        "Later.esp",
        &["Skyrim.esm"],
        0,
        placement(0x854, 0x831, 0, 3.0, Vec::new()),
    );

    let mut failures = Vec::new();
    check_placement_winners(
        &[base, patch, later],
        "ungrouped",
        &[
            (0x850, Some((None, 0x831))),
            (0x851, Some((Some(0x900), 0x830))),
            (0x852, None),
            (0x853, Some((Some(0x900), 0x831))),
            (0x854, Some((Some(0x900), 0x830))),
            (0x855, Some((Some(0x900), 0x834))),
            (0x856, Some((None, 0x831))),
        ],
        &mut failures,
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn check_discarded_occluder_override(import: bool) {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", 0x17, 0, sub(b"EDID", b"PlaneMarker\0")),
            record(b"STAT", 0x810, 0, sub(b"EDID", b"TestStatic\0")),
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            placement_group(
                0x900,
                [
                    placement_marker(0x801, 0x17, Vec::new()),
                    placement(0x803, 0x810, 0, 0.0, Vec::new()),
                ]
                .concat(),
            ),
            record(b"CELL", 0x901, 0, sub(b"DATA", &[1, 0])),
            placement_group(0x901, placement_marker(0x802, 0x17, Vec::new())),
            record(b"CELL", 0x902, 0, sub(b"DATA", &[1, 0])),
        ]
        .concat(),
    );
    let mut failures = Vec::new();
    let partial = |id, len| {
        let links = sub(b"XORD", &[0x802u32, 0, 0, 0].map(u32::to_le_bytes).concat());
        placement_marker(id, 0x17, links)[..len].to_vec()
    };
    let compressed = |tag, id| record(tag, id, 0x40000, vec![4, 0, 0, 0, 0xff]);
    let restored = placement_marker(0x801, 0x17, Vec::new());
    let deleted = record(b"REFR", 0x801, 0x20, Vec::new());
    let xord = || sub(b"XORD", &[0x802u32, 0, 0, 0].map(u32::to_le_bytes).concat());
    let ambiguous = |first, second: u32| {
        placement_marker(
            0x801,
            first,
            [sub(b"NAME", &second.to_le_bytes()), xord()].concat(),
        )
    };
    let damaged_name = |name: &[u8]| {
        record(
            b"REFR",
            0x801,
            0,
            [sub(b"NAME", name), sub(b"DATA", &[0; 24]), xord()].concat(),
        )
    };
    let cases = [
        ("short-r", b"R".to_vec(), None, 0),
        ("short-re", b"RE".to_vec(), None, 0),
        ("short-ref", b"REF".to_vec(), None, 0),
        ("short-ach", b"ACH".to_vec(), None, 1),
        ("short-p", b"P".to_vec(), None, 1),
        ("short-reg", b"REG".to_vec(), None, 1),
        ("oversized-name", damaged_name(&[0x17, 0, 0, 0, 1]), None, 0),
        ("short-name", damaged_name(&[0x17]), None, 0),
        ("ambiguous-marker-first", ambiguous(0x17, 0x999), None, 0),
        ("ambiguous-marker-last", ambiguous(0x999, 0x17), None, 0),
        ("ambiguous-ordinary", ambiguous(0x810, 0x999), None, 1),
        (
            "ambiguous-null-links",
            placement_marker(
                0x801,
                0x17,
                [
                    sub(b"NAME", &0x999u32.to_le_bytes()),
                    sub(b"XORD", &[0; 16]),
                ]
                .concat(),
            ),
            None,
            1,
        ),
        (
            "ambiguous-restored",
            ambiguous(0x17, 0x999),
            Some(&restored),
            1,
        ),
        ("ordinary", partial(0x803, 20), None, 0),
        ("ordinary-overrun", partial(0x803, 24), None, 0),
        ("new", partial(0x0100_0801, 20), None, 0),
        ("superseded", partial(0x801, 20), Some(&restored), 1),
        ("superseded-overrun", partial(0x801, 24), Some(&restored), 1),
        ("unknown", partial(0x801, 15), None, 0),
        ("marker", partial(0x801, 20), None, 0),
        ("marker-overrun", partial(0x801, 24), None, 0),
        ("bounded-zlib", compressed(b"REFR", 0x801), None, 0),
        (
            "bounded-subrecord",
            record(b"REFR", 0x801, 0, b"NAME".to_vec()),
            None,
            0,
        ),
        ("new-zlib", compressed(b"REFR", 0x0100_0801), None, 0),
        ("actor-zlib", compressed(b"ACHR", 0x0100_0804), None, 1),
        (
            "zlib-restored",
            compressed(b"REFR", 0x801),
            Some(&restored),
            1,
        ),
        (
            "zlib-deleted",
            compressed(b"REFR", 0x801),
            Some(&deleted),
            1,
        ),
    ]
    .into_iter()
    .map(|(name, damaged, later, expected)| {
        let cell = if name == "bounded-zlib" { 0x902 } else { 0x900 };
        let mut contents = placement_group(cell, damaged);
        if let Some(later) = later {
            contents.extend(placement_group(0x900, later.clone()));
        }
        (name, contents, expected)
    });
    let wrong_parent = || placement_group(0, placement_marker(0x801, 0x17, xord()));
    let invalid_transform =
        || placement_group(0x900, placement_marker_at(0x801, 0x17, f32::NAN, xord()));
    let linked_markers = [
        (
            "unknown-record-id",
            [
                placement_group(0x900, placement_marker(0, 0x17, xord())),
                placement_group(0x900, restored.clone()),
            ]
            .concat(),
            0,
        ),
        ("single-marker-invalid-transform", invalid_transform(), 0),
        (
            "single-marker-restored",
            [
                invalid_transform(),
                placement_group(0x900, restored.clone()),
            ]
            .concat(),
            1,
        ),
        (
            "single-marker-deleted",
            [invalid_transform(), placement_group(0x900, deleted.clone())].concat(),
            1,
        ),
        (
            "single-marker-linked",
            placement_group(0x900, placement_marker(0x801, 0x17, xord())),
            1,
        ),
    ];
    let unreadable_self = compressed(b"REFR", 0x0100_0850);
    let typed_only = [
        (
            "unreadable-before-compatibility-alias",
            [
                placement_group(0x900, unreadable_self.clone()),
                placement_group(0x900, placement(0x0900_0850, 0x810, 0, 0.0, Vec::new())),
            ]
            .concat(),
            0,
        ),
        (
            "unreadable-before-native-replacement",
            [
                placement_group(0x900, unreadable_self.clone()),
                placement_group(0x900, placement(0x0100_0850, 0x810, 0, 0.0, Vec::new())),
            ]
            .concat(),
            1,
        ),
        (
            "unreadable-before-native-deletion",
            [
                placement_group(0x900, unreadable_self),
                record(b"REFR", 0x0100_0850, 0x20, Vec::new()),
            ]
            .concat(),
            1,
        ),
        (
            "rejected-linked-before-compatibility-alias",
            placement_group(
                0x900,
                [
                    placement_marker_at(0x0100_0850, 0x17, f32::NAN, xord()),
                    placement(0x0900_0850, 0x810, 0, 0.0, Vec::new()),
                ]
                .concat(),
            ),
            0,
        ),
        (
            "rejected-compatibility-link-before-native-replacement",
            placement_group(
                0x900,
                [
                    placement_marker_at(0x0900_0850, 0x17, f32::NAN, xord()),
                    placement_marker(0x0100_0850, 0x17, xord()),
                ]
                .concat(),
            ),
            0,
        ),
        ("invalid-parent", wrong_parent(), 0),
        (
            "invalid-parent-unrelated-replacement",
            [
                wrong_parent(),
                placement_group(0x900, placement(0x803, 0x810, 0, 0.0, Vec::new())),
            ]
            .concat(),
            0,
        ),
        (
            "invalid-parent-restored",
            [wrong_parent(), placement_group(0x900, restored.clone())].concat(),
            1,
        ),
        (
            "invalid-parent-deleted",
            [wrong_parent(), placement_group(0x900, deleted.clone())].concat(),
            1,
        ),
    ];
    for (name, contents, expected) in cases
        .chain(linked_markers)
        .chain(typed_only.into_iter().filter(|_| !import))
    {
        let patch = plugin(data, &format!("{name}.esp"), &["Skyrim.esm"], 0, contents);
        let assets = data.join(name);
        fs::create_dir(&assets).unwrap();
        let db = assets.join("skyrim_world.db");
        let certificate =
            "SELECT value FROM room_graph_stats WHERE name='occlusion_links_verified'";
        let read = |conn: &rusqlite::Connection, sql| {
            conn.query_row(sql, [], |row| row.get::<_, u32>(0)).unwrap()
        };
        let plugins = if import {
            vec![base.clone()]
        } else {
            vec![base.clone(), patch.clone()]
        };
        EsmParser::convert_plugins_with_reader(&plugins, &db, RecordReader::Inhouse, data).unwrap();
        let conn = rusqlite::Connection::open(db).unwrap();
        if import {
            assert_eq!(read(&conn, certificate), 1, "{name} before import");
            let result = run_placement_import(&assets, "--plugin", &patch);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        assert_eq!(
            read(
                &conn,
                "SELECT count(*) FROM room_occluders WHERE ref_id=0x802 AND cell_id=0x901"
            ),
            1,
            "{name}"
        );
        // A subset adding a cross-cell link also invalidates its target cell.
        let other_complete = match name {
            "single-marker-linked" => !import,
            "unknown" | "short-r" | "short-re" | "short-ref" | "short-ach" | "short-p" => false,
            _ => true,
        };
        if stored_placement_certificate(&conn, 0x901) != Some(other_complete) {
            failures.push(format!(
                "{name}: cell 0x901 expected complete={other_complete}"
            ));
        }
        if name == "single-marker-linked" {
            assert_eq!(
                read(
                    &conn,
                    "SELECT count(*) FROM room_occluder_links WHERE ref_id=0x801 AND linked_ref=0x802"
                ),
                1
            );
        }
        if name == "bounded-zlib" {
            assert_eq!(stored_placement_certificate(&conn, 0x900), Some(false));
            assert_eq!(stored_placement_certificate(&conn, 0x902), Some(false));
            assert_eq!(
                read(
                    &conn,
                    "SELECT count(*) FROM room_occluders WHERE ref_id=0x801"
                ),
                1
            );
            if !import {
                assert_eq!(
                    read(
                        &conn,
                        "SELECT count(*) FROM record_overrides WHERE form_id=0x801
                    AND status='skipped' AND frame_complete=1 AND reason IS NOT NULL"
                    ),
                    1
                );
            }
        }
        if matches!(
            name,
            "unreadable-before-compatibility-alias"
                | "rejected-linked-before-compatibility-alias"
                | "rejected-compatibility-link-before-native-replacement"
        ) {
            assert_eq!(placement_winner_cell(&conn, 0x0100_0850), Some(0x900));
        }
        let actual = read(&conn, certificate);
        if actual != expected {
            failures.push(format!("{name}: certificate {actual}, expected {expected}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn discarded_occluder_override_revokes_full_conversion_link_certificate() {
    check_discarded_occluder_override(false);
}

#[test]
fn discarded_occluder_override_revokes_plugin_import_link_certificate() {
    check_discarded_occluder_override(true);
}

#[test]
fn plugin_occluder_import_reports_linked_cells_and_keeps_authored_null_links_local() {
    use converter::{
        config::RecordReader,
        esm::placement_edits::{PlacementSource, apply},
    };

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", 0x17, 0, sub(b"EDID", b"PlaneMarker\0")),
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            placement_group(
                0x900,
                [
                    placement_marker(0x801, 0x17, Vec::new()),
                    placement_marker(0x803, 0x17, Vec::new()),
                ]
                .concat(),
            ),
            record(b"CELL", 0x901, 0, sub(b"DATA", &[1, 0])),
            placement_group(0x901, placement_marker(0x802, 0x17, Vec::new())),
        ]
        .concat(),
    );
    let mut failures = Vec::new();
    for (name, slots, affected, complete, verified) in [
        ("null", vec![0; 16], vec![0x900], [true, true], true),
        (
            "linked",
            [0x802u32, 0, 0, 0].map(u32::to_le_bytes).concat(),
            vec![0x900, 0x901],
            [true, false],
            true,
        ),
        ("malformed", vec![0; 12], vec![0x900], [false, true], false),
        (
            "unresolved",
            [0x9900_0802u32, 0, 0, 0].map(u32::to_le_bytes).concat(),
            vec![0x900],
            [false, true],
            false,
        ),
    ] {
        let assets = data.join(name);
        fs::create_dir(&assets).unwrap();
        let path = assets.join("skyrim_world.db");
        EsmParser::convert_plugins_with_reader(
            std::slice::from_ref(&base),
            &path,
            RecordReader::Inhouse,
            data,
        )
        .unwrap();
        let source = plugin(
            data,
            &format!("{name}.esp"),
            &["Skyrim.esm"],
            0,
            placement_group(0x900, placement_marker(0x801, 0x17, sub(b"XORD", &slots))),
        );
        let report = apply(&assets, PlacementSource::SkyrimPlugin(&source)).unwrap();
        let db = rusqlite::Connection::open(path).unwrap();
        if name == "linked" {
            // A verified single edge preserves the source certificate. The target cell
            // loses its certificate, and the receipt includes both cells.
            let edge: (u32, u32, bool) = db
                .query_row(
                    "SELECT link_index, linked_ref, single_target_verified
                     FROM room_occluder_links WHERE ref_id=0x801",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(edge, (0, 0x802, true));
        }
        let certificates = [0x900, 0x901].map(|cell| placement_certificate(&db, cell));
        let links_verified = db
            .query_row(
                "SELECT value FROM room_graph_stats WHERE name='occlusion_links_verified'",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        if report.affected_cells != affected
            || certificates != complete
            || links_verified != verified
        {
            failures.push(format!(
                "{name}: affected {:?} (expected {affected:?}), certificates {certificates:?} (expected {complete:?}), links verified {links_verified} (expected {verified})",
                report.affected_cells
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn placement_layers_refresh_incoming_links_for_changed_targets() {
    use converter::esm::{
        exporter::export_to_db,
        placement_edits::{PlacementSource, apply},
        records::RawRecord,
    };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let assets = converted_placement_fixture(root);
    let db = rusqlite::Connection::open(assets.join("skyrim_world.db")).unwrap();
    let source = 0x880u32;
    let existing = 0x802u32;
    let unrelated = 0x823u32;
    // Remove.esp reserves the first regular plugin slot before Add.esp.
    let plugin_target = 0x0200_0910u32;
    let native_target = 0xff00_0001u32;
    let targets = [existing, plugin_target, native_target, unrelated];
    let mut subrecords = vec![
        (b"NAME".to_vec(), PLACEMENT_STAT.to_le_bytes().to_vec()),
        (b"DATA".to_vec(), vec![0; 24]),
        (b"XAPD".to_vec(), vec![3]),
    ];
    for (index, target) in targets.into_iter().enumerate() {
        let word = target.to_le_bytes();
        subrecords.push((b"XLKR".to_vec(), [unrelated.to_le_bytes(), word].concat()));
        subrecords.push((
            b"XAPR".to_vec(),
            [word, (index as f32 + 0.5).to_le_bytes()].concat(),
        ));
    }
    subrecords.push((
        b"XLKR".to_vec(),
        [existing.to_le_bytes(), unrelated.to_le_bytes()].concat(),
    ));
    subrecords.push((b"XLKR".to_vec(), native_target.to_le_bytes().to_vec()));
    subrecords.push((b"XLKR".to_vec(), vec![0; 4]));
    export_to_db(
        &db,
        &[(
            source,
            RawRecord {
                form_id: source,
                record_type: *b"REFR",
                flags: 0,
                subrecords,
                cell_form_id: Some(PLACEMENT_CELL_B),
                worldspace_form_id: None,
                load_order: 0,
            },
        )]
        .into(),
    )
    .unwrap();

    let source_data = || {
        db.query_row(
            "SELECT records.data, r.data, d.activate_parent_flags
             FROM records JOIN [references] r ON r.id=records.form_id
             JOIN reference_details d ON d.reference_id=r.id WHERE r.id=?1",
            [source],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, u8>(2)?,
                ))
            },
        )
        .unwrap()
    };
    let original_source = source_data();
    let assert_links = |expected: [Option<u32>; 4]| {
        let linked = db
            .prepare("SELECT keyword_id,linked_ref_id FROM reference_linked_refs WHERE reference_id=?1 ORDER BY position")
            .unwrap()
            .query_map([source], |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, Option<u32>>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let parents = db
            .prepare("SELECT parent_ref_id,delay FROM reference_activate_parents WHERE reference_id=?1 ORDER BY position")
            .unwrap()
            .query_map([source], |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, f32>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let mut expected_linked = expected.map(|target| (Some(unrelated), target)).to_vec();
        expected_linked.push((expected[0], Some(unrelated)));
        expected_linked.push((None, expected[2]));
        expected_linked.push((None, None));
        assert_eq!(linked, expected_linked, "XLKR");
        assert_eq!(
            parents,
            expected
                .into_iter()
                .enumerate()
                .map(|(index, target)| (target, index as f32 + 0.5))
                .collect::<Vec<_>>(),
            "XAPR",
        );
        assert_eq!(source_data(), original_source);
    };
    let native_put = |name, id| {
        let path = root.join(name);
        fs::write(&path, serde_json::json!({"edits":[{
            "op":"put","id":id,"cell":PLACEMENT_CELL_A,"base":PLACEMENT_STAT,"position":[16,0,0],
        }]}).to_string()).unwrap();
        path
    };
    assert_links([Some(existing), None, None, Some(unrelated)]);

    let remove = plugin(
        root,
        "Remove.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_A,
            record(b"REFR", existing, 0x20, Vec::new()),
        ),
    );
    apply(&assets, PlacementSource::SkyrimPlugin(&remove)).unwrap();
    assert_links([None, None, None, Some(unrelated)]);

    let add = plugin(
        root,
        "Add.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_A,
            placement(0x0100_0910, PLACEMENT_STAT, 0, 24.0, Vec::new()),
        ),
    );
    apply(&assets, PlacementSource::SkyrimPlugin(&add)).unwrap();
    assert_links([None, Some(plugin_target), None, Some(unrelated)]);

    let add_native = native_put("add-native.json", native_target);
    apply(&assets, PlacementSource::NativeJson(&add_native)).unwrap();
    let absent = [
        None,
        Some(plugin_target),
        Some(native_target),
        Some(unrelated),
    ];
    assert_links(absent);

    let native_restore = native_put("restore.json", existing);
    let error = apply(&assets, PlacementSource::NativeJson(&native_restore)).unwrap_err();
    assert!(format!("{error:#}").contains("existing runtime enable state"));
    assert_links(absent);

    let restore = plugin(
        root,
        "Restore.esp",
        &["Skyrim.esm"],
        0,
        placement_group(
            PLACEMENT_CELL_A,
            placement(existing, PLACEMENT_STAT, 0, 16.0, Vec::new()),
        ),
    );
    db.execute_batch(
        "CREATE TRIGGER reject_incoming BEFORE UPDATE OF parent_ref_id ON reference_activate_parents
         WHEN NEW.parent_ref_id=0x802 BEGIN SELECT RAISE(ABORT, 'reject incoming link'); END;",
    )
    .unwrap();
    let error = apply(&assets, PlacementSource::SkyrimPlugin(&restore)).unwrap_err();
    assert!(
        error.to_string().contains("reject incoming link"),
        "{error}"
    );
    assert_links(absent);
    assert_eq!(
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM [references] WHERE id=?1),
                    EXISTS(SELECT 1 FROM room_placement_layers WHERE lower(name) IN ('restore.json','restore.esp'))",
            [existing],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
        )
        .unwrap(),
        (false, false),
    );
    db.execute_batch("DROP TRIGGER reject_incoming").unwrap();
    apply(&assets, PlacementSource::SkyrimPlugin(&restore)).unwrap();
    let mut restored = absent;
    restored[0] = Some(existing);
    assert_links(restored);
}

#[test]
fn typed_incoming_links_restore_after_target_only_edits() {
    use converter::esm::{
        exporter::{export_placed_subset_with_sources, export_to_db},
        placement_edits::{PlacementSource, apply},
        records::RawRecord,
    };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cell = PLACEMENT_CELL_A;
    let source = 0x880u32;
    let authored_target = 0x0100_0910u32;
    let target = 0x0200_0910u32;
    let parent =
        |id: u32, delay: f32| sub(b"XAPR", &[id.to_le_bytes(), delay.to_le_bytes()].concat());
    let linked_source = || {
        placement_group(
            cell,
            placement(
                source,
                PLACEMENT_STAT,
                0,
                0.0,
                [
                    sub(b"XLKR", &[0; 6]),
                    sub(b"XLKR", &authored_target.to_le_bytes()),
                    sub(
                        b"XLKR",
                        &[authored_target.to_le_bytes(), 0u32.to_le_bytes()].concat(),
                    ),
                    sub(b"XAPD", &[3]),
                    parent(authored_target, f32::NAN),
                    parent(0, 0.5),
                    parent(authored_target, 1.5),
                    parent(0x0200_0910, 2.5), // Invalid supplier-relative index, despite matching the runtime target.
                ]
                .concat(),
            ),
        )
    };
    let paths = [
        plugin(
            root,
            "Skyrim.esm",
            &[],
            1,
            [
                record(b"STAT", PLACEMENT_STAT, 0, Vec::new()),
                record(b"CELL", cell, 0, sub(b"DATA", &[1, 0])),
                placement_group(cell, placement(source, PLACEMENT_STAT, 0, 0.0, Vec::new())),
            ]
            .concat(),
        ),
        plugin(root, "Padding.esp", &["Skyrim.esm"], 0, Vec::new()),
        plugin(root, "Links.esp", &["Skyrim.esm"], 0, linked_source()),
    ];
    let assets = root.join("assets");
    fs::create_dir(&assets).unwrap();
    let database = assets.join("skyrim_world.db");
    EsmParser::convert_plugins_with_reader(
        &paths,
        &database,
        converter::config::RecordReader::Inhouse,
        root,
    )
    .unwrap();
    let order = LoadOrder::read(&paths).unwrap();
    let mut decoded = converter::records::read_plugins(&paths, &order).unwrap();
    let decoded_source = decoded.records.remove(&source).unwrap();
    let subset = plugin(
        root,
        "Subset.esp",
        &["Skyrim.esm", "Links.esp"],
        0,
        linked_source(),
    );
    let mut subset_paths = paths.to_vec();
    subset_paths.push(subset);
    let subset_order = LoadOrder::read(&subset_paths).unwrap();
    let mut decoded = converter::records::read_plugins(&subset_paths, &subset_order).unwrap();
    let subset_source = decoded.records.remove(&source).unwrap();
    assert_ne!(subset_source.load_order, decoded_source.load_order);
    for path in subset_paths {
        fs::remove_file(path).unwrap();
    }
    let mut db = rusqlite::Connection::open(&database).unwrap();
    converter::esm::rooms::add_overlap_rows(&mut db).unwrap();
    let assert_links = |expected| {
        let linked = db.prepare("SELECT keyword_id,linked_ref_id FROM reference_linked_refs WHERE reference_id=?1 ORDER BY position").unwrap()
            .query_map([source], |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, Option<u32>>(1)?))).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        let parents = db.prepare("SELECT parent_ref_id,delay FROM reference_activate_parents WHERE reference_id=?1 ORDER BY position").unwrap()
            .query_map([source], |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, f32>(1)?))).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(parents, [(None, 0.5), (expected, 1.5), (None, 2.5)]);
        assert_eq!(linked, [(None, expected), (expected, None)]);
    };
    assert_links(None);

    let target_record = RawRecord {
        form_id: target,
        record_type: *b"REFR",
        flags: 0,
        subrecords: vec![
            (b"NAME".to_vec(), PLACEMENT_STAT.to_le_bytes().to_vec()),
            (b"DATA".to_vec(), vec![0; 24]),
        ],
        cell_form_id: Some(cell),
        worldspace_form_id: None,
        load_order: 2,
    };
    export_to_db(&db, &[(target, target_record.clone())].into()).unwrap();
    assert_links(Some(target));

    for (name, bytes, expected) in [
        (
            "Delete.esp",
            record(b"REFR", authored_target, 0x20, Vec::new()),
            None,
        ),
        (
            "Restore.esp",
            placement(authored_target, PLACEMENT_STAT, 0, 0.0, Vec::new()),
            Some(target),
        ),
    ] {
        let patch = plugin(
            root,
            name,
            &["Skyrim.esm", "Links.esp"],
            0,
            placement_group(cell, bytes),
        );
        apply(&assets, PlacementSource::SkyrimPlugin(&patch)).unwrap();
        assert_links(expected);
    }

    // A raw subset can replace the source with the same canonical bytes and winning
    // priority. Its zeroes are effective authoring, even with the old source retained.
    let delete =
        || export_placed_subset_with_sources(&db, &Default::default(), &[target], &order).unwrap();
    let add = || export_to_db(&db, &[(target, target_record.clone())].into()).unwrap();
    delete();
    export_to_db(&db, &[(source, decoded_source.to_raw_record())].into()).unwrap();
    add();
    assert_links(None);

    // A typed subset supplies its own authored link words, independent of the base archive.
    export_placed_subset_with_sources(&db, &[(source, subset_source)].into(), &[], &subset_order)
        .unwrap();
    assert_links(Some(target));
    let flags: u32 = db
        .query_row(
            "SELECT activate_parent_flags FROM reference_details WHERE reference_id=?1",
            [source],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(flags, 3);
    delete();
    assert_links(None);
    add();
    assert_links(Some(target));
}

#[test]
fn sourced_placement_cannot_replace_an_unmapped_static() {
    use converter::esm::exporter::{
        create_tables, export_placed_subset_with_sources, export_to_db,
    };

    let dir = tempfile::tempdir().unwrap();
    let id = 0x880;
    let paths = [plugin(
        dir.path(),
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", PLACEMENT_STAT, 0, Vec::new()),
            record(b"CELL", PLACEMENT_CELL_A, 0, sub(b"DATA", &[1, 0])),
            placement_group(
                PLACEMENT_CELL_A,
                placement(id, PLACEMENT_STAT, 0, 0.0, Vec::new()),
            ),
        ]
        .concat(),
    )];
    let order = LoadOrder::read(&paths).unwrap();
    let mut decoded = converter::records::read_plugins(&paths, &order).unwrap();
    let mut static_record = decoded.records[&PLACEMENT_STAT].to_raw_record();
    static_record.form_id = id;
    let db = rusqlite::Connection::open_in_memory().unwrap();
    create_tables(&db).unwrap();
    export_to_db(&db, &[(id, static_record)].into()).unwrap();
    let edits = [(id, decoded.records.remove(&id).unwrap())].into();
    let error = export_placed_subset_with_sources(&db, &edits, &[], &order).unwrap_err();
    assert!(error.to_string().contains("record type"), "{error}");
    let preserved: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM records WHERE form_id=?1 AND record_type='STAT')
            AND EXISTS(SELECT 1 FROM statics WHERE id=?1)
            AND NOT EXISTS(SELECT 1 FROM formid_map WHERE form_id=?1)
            AND NOT EXISTS(SELECT 1 FROM [references] WHERE id=?1)",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(preserved);
}

#[test]
fn typed_placement_winner_checks_grouped_duplicates_before_ungrouped_override() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            record(b"CELL", 0x901, 0, sub(b"DATA", &[1, 0])),
            record(b"CELL", 0x902, 0, sub(b"DATA", &[1, 0])),
            record(b"STAT", 0x830, 0, sub(b"EDID", b"OriginalStatic\0")),
            record(b"STAT", 0x831, 0, sub(b"EDID", b"ReplacementStatic\0")),
            placement_group(0x900, placement(0x853, 0x830, 0, 1.0, Vec::new())),
        ]
        .concat(),
    );
    let grouped = plugin(
        data,
        "Grouped.esp",
        &["Skyrim.esm"],
        0,
        [
            placement_group(
                0x900,
                [
                    placement(0x850, 0x832, 0, 1.0, Vec::new()),
                    placement(0x851, 0x830, 0, 1.0, Vec::new()),
                    placement(0x852, 0x832, 0, 1.0, Vec::new()),
                    placement(0x853, 0x832, 0, 1.0, Vec::new()),
                    placement(0x854, 0x832, 0, 1.0, Vec::new()),
                ]
                .concat(),
            ),
            record(b"REFR", 0x853, 0x20, Vec::new()),
            placement(0x854, 0x830, 0, 2.0, Vec::new()),
            placement_group(
                0x901,
                [
                    placement(0x850, 0x830, 0, 2.0, Vec::new()),
                    placement(0x851, 0x831, 0, 2.0, Vec::new()),
                    placement(0x852, 0x833, 0, 2.0, Vec::new()),
                    placement(0x853, 0x831, 0, 2.0, Vec::new()),
                    placement(0x854, 0x830, 0, 3.0, Vec::new()),
                ]
                .concat(),
            ),
            placement_group(0x902, placement(0x850, 0x831, 0, 3.0, Vec::new())),
        ]
        .concat(),
    );
    let ungrouped = plugin(
        data,
        "Ungrouped.esp",
        &["Skyrim.esm"],
        0,
        (0x850..=0x854)
            .flat_map(|id| placement(id, 0x831, 0, 4.0, Vec::new()))
            .collect(),
    );

    let output = data.join("world.db");
    let records = EsmParser::convert_plugins_with_reader(
        &[base, grouped, ungrouped],
        &output,
        RecordReader::Inhouse,
        data,
    )
    .unwrap();
    let db = rusqlite::Connection::open(output).unwrap();
    for (id, cell, base) in [
        (0x850, Some(0x901), 0x830),
        (0x851, Some(0x900), 0x830),
        (0x852, None, 0x831),
        (0x853, None, 0x831),
        (0x854, None, 0x831),
    ] {
        assert_eq!(
            placement_winner_cell(&db, id),
            Some(cell.unwrap_or(0)),
            "{id:08X}"
        );
        assert_eq!(
            SubrecordView::new(&records[&id].subrecords).get_form_id(b"NAME"),
            Some(base),
            "{id:08X}"
        );
    }
}

#[tokio::test]
async fn deleted_base_notices_follow_usable_shadows_and_live_intervals() {
    use converter::{AssetPipeline, config::RecordReader};

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Data");
    fs::create_dir(&data).unwrap();
    let live = |id| record(b"STAT", id, 0, sub(b"EDID", b"Static\0"));
    let deleted = |id| record(b"STAT", id, 0x20, Vec::new());
    let dependent = |id, base| placement_group(0x900, placement(id, base, 0, 1.0, Vec::new()));
    let layers = [
        (
            "Skyrim.esm",
            [
                record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
                live(0x830),
                live(0x831),
                live(0x832),
                live(0x833),
                live(0x834),
                live(0x835),
                live(0x836),
                live(0x837),
                dependent(0x850, 0x830),
                dependent(0x851, 0x831),
            ]
            .concat(),
        ),
        ("OriginallyLive.esp", dependent(0x852, 0x832)),
        (
            "Shadows.esp",
            [
                dependent(0x830, 0x888),
                deleted(0x830),
                dependent(0x831, 0x1f),
                deleted(0x831),
                deleted(0x832),
                deleted(0x833),
                deleted(0x834),
                deleted(0x835),
                deleted(0x837),
            ]
            .concat(),
        ),
        (
            "Gap.esp",
            [deleted(0x830), deleted(0x831), dependent(0x853, 0x832)].concat(),
        ),
        (
            "EarlyRestores.esp",
            [
                dependent(0x833, 0x888),
                dependent(0x834, 0x1f),
                dependent(0x835, 0x836),
            ]
            .concat(),
        ),
        ("RejectedGap.esp", dependent(0x855, 0x833)),
        ("AcceptedGap.esp", dependent(0x856, 0x834)),
        ("BeforeRetirement.esp", dependent(0x857, 0x835)),
        ("RetireSupport.esp", deleted(0x836)),
        ("AfterRetirement.esp", dependent(0x837, 0x836)),
        ("StillAbsent.esp", dependent(0x858, 0x837)),
        (
            "Restore.esp",
            [
                live(0x832),
                live(0x833),
                live(0x834),
                live(0x835),
                live(0x837),
            ]
            .concat(),
        ),
        ("Live.esp", dependent(0x854, 0x832)),
        (
            "DeleteAgain.esp",
            [
                deleted(0x832),
                deleted(0x833),
                deleted(0x834),
                deleted(0x835),
                deleted(0x837),
            ]
            .concat(),
        ),
    ];
    let list = dir.path().join("plugins.txt");
    let mut names = Vec::new();
    for (name, records) in layers {
        let masters = if names.is_empty() {
            &[][..]
        } else {
            &["Skyrim.esm"][..]
        };
        plugin(&data, name, masters, u32::from(names.is_empty()), records);
        names.push(format!("*{name}\n"));
    }
    fs::write(&list, names.concat()).unwrap();
    let mut failures = Vec::new();
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let output = dir.path().join(format!("{reader:?}"));
        let mut config = common::cpu_lod_config(&data, &output);
        config.record_reader = reader;
        config.no_lod = true;
        config.plugins_file = Some(list.clone());
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = AssetPipeline::run_async(config, tx).await.unwrap();
        drain.await.unwrap();
        let notices = report.notices.join("\n");
        let warnings = report.warnings.join("\n");
        for (id, retired) in [
            (0x850, true),
            (0x851, false),
            (0x852, true),
            (0x853, false),
            (0x854, true),
            (0x855, false),
            (0x856, true),
            (0x857, true),
            (0x858, false),
        ] {
            let id = format!("{id:08X}");
            if notices.contains(&id) != retired || warnings.contains(&id) == retired {
                failures.push(format!(
                    "{reader:?}: {id} retired={retired}\n{notices}\n{warnings}"
                ));
            }
        }
        let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM records WHERE form_id BETWEEN 0x830 AND 0x837",
                [],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn inhouse_first_placement_keeps_short_transform_before_a_truncated_field() {
    use converter::config::RecordReader;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut truncated = sub(b"EDID", b"unfinished\0");
    truncated.truncate(8);
    let short = |id, tail: &[u8]| {
        record(
            b"REFR",
            id,
            0,
            [
                sub(b"NAME", &0x830u32.to_le_bytes()),
                sub(b"DATA", &[0; 20]),
                tail.to_vec(),
            ]
            .concat(),
        )
    };
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"STAT", 0x830, 0, sub(b"EDID", b"Static\0")),
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            placement_group(
                0x900,
                [
                    short(0x850, &[]),
                    short(0x851, &truncated),
                    placement(0x852, 0x830, 0, f32::NAN, truncated.clone()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let path = data.join("world.db");
    let records =
        EsmParser::convert_plugins_with_reader(&[base], &path, RecordReader::Inhouse, data)
            .unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    for (id, canonical) in [(0x850, true), (0x851, true), (0x852, false)] {
        assert_eq!(records.contains_key(&id), canonical, "{id:08X}");
        assert_eq!(placement_winner_cell(&db, id), None, "{id:08X}");
    }
}

#[test]
fn incomplete_queued_duplicate_only_yields_to_a_usable_complete_predecessor() {
    use converter::config::RecordReader;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let base = plugin(
        data,
        "Skyrim.esm",
        &[],
        1,
        [
            record(b"CELL", 0x900, 0, sub(b"DATA", &[1, 0])),
            placement_group(
                0x900,
                [
                    placement(0x850, 0x888, 0, 1.0, Vec::new()),
                    placement(0x851, 0x1f, 0, 1.0, Vec::new()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let mut children = Vec::new();
    for id in [0x850, 0x851] {
        children.extend(placement(id, 0x889, 0, 2.0, Vec::new()));
        children.extend(placement(id, 0x20, 0, 3.0, vec![b'X']));
    }
    let patch = plugin(
        data,
        "Patch.esp",
        &["Skyrim.esm"],
        0,
        placement_group(0x900, children),
    );
    let records = EsmParser::convert_plugins_with_reader(
        &[base, patch],
        &data.join("result.db"),
        RecordReader::Inhouse,
        data,
    )
    .unwrap();
    for (id, expected) in [(0x850, 0x20), (0x851, 0x1f)] {
        assert_eq!(
            records
                .get(&id)
                .and_then(|record| SubrecordView::new(&record.subrecords).get_form_id(b"NAME")),
            Some(expected),
            "{id:08X}"
        );
    }
}
