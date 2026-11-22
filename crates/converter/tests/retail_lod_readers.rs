//! Reads real retail LOD files from a local install and checks the documented layouts.
//!
//! Ignored by default: it needs `MUDCRAB_SKYRIM_DATA` to name a Skyrim Special Edition `Data`
//! folder. It extracts into a temporary directory, never writes the install, and commits no
//! retail bytes. Run with `cargo test -p converter --test retail_lod_readers -- --ignored`.

use converter::{
    archive::ArchiveExtractor,
    lod::retail::{
        atlas::{DdsFormat, read_dds_info},
        bto::read_object_lod,
        btr::{TerrainLodShapeKind, read_terrain_lod},
        btt::{BttError, parse_tree_block, parse_tree_block_prefix},
        lst::parse_tree_list,
        name::{RetailLodKind, parse_lod_file_name},
    },
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The install these tests read. They are `#[ignore]`d, so reaching one means someone asked for
/// it: an unset or wrong `MUDCRAB_SKYRIM_DATA` is a setup error, never a silent pass.
fn data_dir() -> PathBuf {
    let value = std::env::var_os("MUDCRAB_SKYRIM_DATA").expect(
        "set MUDCRAB_SKYRIM_DATA to a Skyrim Special Edition Data folder to run the ignored retail LOD tests",
    );
    let path = PathBuf::from(value);
    assert!(
        path.is_dir(),
        "MUDCRAB_SKYRIM_DATA does not name a directory: {}",
        path.display()
    );
    path
}

fn archives(data: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(data)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "bsa")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
        })
        .collect();
    found.sort();
    found
}

/// Extracts the entries of `archives` that `keep` accepts and returns their paths under `out`.
fn extract(archives: &[PathBuf], out: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut all = Vec::new();
    for archive in archives {
        let wanted: BTreeSet<PathBuf> = ArchiveExtractor::list_paths(archive)
            .unwrap()
            .into_iter()
            .filter(|path| keep(path))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        ArchiveExtractor::extract_paths(archive, out, &wanted).unwrap();
        all.extend(wanted.into_iter().map(|path| out.join(path)));
    }
    all
}

#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
fn tamriel_level_16_object_lod_parses_and_matches_the_format_study() {
    let data = data_dir();
    let temp = tempfile::tempdir().unwrap();
    let files = extract(&archives(&data, "Skyrim - Meshes"), temp.path(), |path| {
        let text = path.to_string_lossy();
        parse_lod_file_name(&text).is_some_and(|name| {
            name.kind == RetailLodKind::Object && name.world == "tamriel" && name.level == Some(16)
        })
    });
    assert_eq!(files.len(), 48, "Tamriel ships 48 level-16 object blocks");
    let (mut triangles, mut vertices, mut large_refs) = (0, 0, 0);
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let parsed = read_object_lod(&bytes, file).unwrap();
        assert_eq!((parsed.bethesda_version, parsed.user_version), (100, 12));
        assert!(!parsed.shapes.is_empty());
        for shape in &parsed.shapes {
            let sum: u32 = shape
                .segments
                .iter()
                .map(|segment| segment.num_primitives)
                .sum();
            assert_eq!(sum as usize, shape.triangles.len());
            assert_eq!(shape.segments.len(), 1, "level 16 shapes have one segment");
        }
        triangles += parsed.triangle_count();
        vertices += parsed.vertex_count();
        large_refs += parsed.large_ref_blocks.len();
        // The only block type the NIF reader does not decode is the large-reference marker.
        for block_type in parsed.unhandled_blocks.keys() {
            assert_eq!(block_type, "BSDistantObjectLargeRefExtraData");
        }
    }
    println!("level 16: {triangles} triangles, {vertices} vertices, {large_refs} large-ref blocks");
    assert_eq!(triangles, 311_641);
    assert_eq!(vertices, 408_951);
}

#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
fn tree_lists_and_blocks_parse_and_solstheim_is_reported_not_guessed() {
    let data = data_dir();
    let temp = tempfile::tempdir().unwrap();
    let files = extract(&archives(&data, "Skyrim - Meshes"), temp.path(), |path| {
        let text = path.to_string_lossy();
        parse_lod_file_name(&text)
            .is_some_and(|name| matches!(name.kind, RetailLodKind::Tree | RetailLodKind::TreeList))
    });
    let (mut lists, mut blocks, mut billboards, mut solstheim_trailing) = (0, 0, 0, 0);
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let name = parse_lod_file_name(&file.to_string_lossy()).unwrap();
        if name.kind == RetailLodKind::TreeList {
            parse_tree_list(&bytes).unwrap();
            lists += 1;
            continue;
        }
        match parse_tree_block(&bytes) {
            Ok(block) => {
                blocks += 1;
                billboards += block.billboard_count();
            }
            Err(BttError::TrailingBytes { consumed, len }) => {
                assert_eq!(
                    name.world, "dlc2solstheimworld",
                    "only Solstheim has extra bytes"
                );
                assert!(consumed < len);
                parse_tree_block_prefix(&bytes).unwrap();
                solstheim_trailing += 1;
            }
            Err(error) => panic!("{}: {error}", file.display()),
        }
    }
    println!(
        "{lists} lists, {blocks} exact blocks, {billboards} billboards, {solstheim_trailing} with extra bytes"
    );
    assert_eq!(lists, 9);
    assert_eq!(blocks + solstheim_trailing, 386);
    assert_eq!(solstheim_trailing, 6);
    assert_eq!(billboards, 43_333 - billboards_in_trailing(&files));
}

/// Billboards the layout explains in the files that also have extra bytes.
fn billboards_in_trailing(files: &[PathBuf]) -> usize {
    files
        .iter()
        .filter(|file| {
            parse_lod_file_name(&file.to_string_lossy())
                .is_some_and(|name| name.kind == RetailLodKind::Tree)
        })
        .filter_map(|file| {
            let bytes = std::fs::read(file).unwrap();
            match parse_tree_block(&bytes) {
                Err(BttError::TrailingBytes { .. }) => {
                    Some(parse_tree_block_prefix(&bytes).unwrap().0.billboard_count())
                }
                _ => None,
            }
        })
        .sum()
}

#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
fn tamriel_object_atlas_header_reads() {
    let data = data_dir();
    let temp = tempfile::tempdir().unwrap();
    let files = extract(&archives(&data, "Skyrim - Textures"), temp.path(), |path| {
        path.to_string_lossy() == "textures/terrain/tamriel/objects/tamriel.objects.dds"
    });
    assert_eq!(files.len(), 1);
    let bytes = std::fs::read(&files[0]).unwrap();
    let info = read_dds_info(&bytes).unwrap();
    println!("{info:?}");
    assert_eq!((info.width, info.height), (2048, 2048));
    assert!(matches!(
        info.format,
        DdsFormat::Uncompressed { bits: 32, .. }
    ));
    let payload = info.expected_payload_bytes().unwrap();
    assert_eq!(bytes.len() as u64, info.data_offset as u64 + payload);
}

#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
fn tamriel_level_4_large_references_are_kept_as_opaque_blocks() {
    let data = data_dir();
    let temp = tempfile::tempdir().unwrap();
    let files = extract(&archives(&data, "Skyrim - Meshes"), temp.path(), |path| {
        let text = path.to_string_lossy();
        parse_lod_file_name(&text).is_some_and(|name| {
            name.kind == RetailLodKind::Object && name.world == "tamriel" && name.level == Some(4)
        })
    });
    assert_eq!(files.len(), 517, "Tamriel ships 517 level-4 object blocks");
    let (mut shapes, mut large_refs, mut segments, mut multi_segment_shapes) = (0, 0, 0, 0);
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let parsed = read_object_lod(&bytes, file).unwrap();
        shapes += parsed.shapes.len();
        large_refs += parsed.large_ref_blocks.len();
        for shape in &parsed.shapes {
            segments += shape.segments.len();
            multi_segment_shapes += usize::from(shape.segments.len() > 1);
        }
        for block in &parsed.large_ref_blocks {
            assert!(!block.bytes.is_empty());
            assert_eq!(block.block_type, "BSDistantObjectLargeRefExtraData");
        }
        // Whatever the NIF reader cannot decode is exactly the opaque block type.
        for block_type in parsed.unhandled_blocks.keys() {
            assert_eq!(block_type, "BSDistantObjectLargeRefExtraData");
        }
    }
    println!(
        "level 4: {shapes} shapes, {large_refs} large-ref blocks, {segments} segment entries, {multi_segment_shapes} shapes with several segments"
    );
    assert!(large_refs > 0, "level 4 carries large references");
}

/// Every retail terrain LOD block of every worldspace in the base game archives: the format study
/// of the `.btr` reader (`lod::retail::btr`).
#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
fn every_base_game_terrain_lod_block_parses_and_matches_the_format_study() {
    use std::collections::BTreeMap;
    let data = data_dir();
    let temp = tempfile::tempdir().unwrap();
    // Only the base-game archives: Creation Club worlds ship in their own archives.
    let files = extract(&archives(&data, "Skyrim - Meshes"), temp.path(), |path| {
        let text = path.to_string_lossy();
        parse_lod_file_name(&text).is_some_and(|name| name.kind == RetailLodKind::Terrain)
    });
    assert_eq!(
        files.len(),
        9_584,
        "the base game ships 9,584 terrain LOD blocks"
    );

    // (blocks, land vertices, land triangles, water shapes, water triangles) per world and level.
    let mut totals: BTreeMap<(String, u32), [usize; 5]> = BTreeMap::new();
    let mut max_uv_error_below_32 = 0.0f64;
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let name = parse_lod_file_name(&file.to_string_lossy()).unwrap();
        let level = name.level.unwrap();
        let (cell_x, cell_y) = name.cell.unwrap();
        let parsed = read_terrain_lod(&bytes, file).unwrap();
        let context = file.display().to_string();
        assert_eq!(
            (parsed.bethesda_version, parsed.user_version),
            (100, 12),
            "{context}"
        );
        // Every node transform is the identity; every block of the file is decoded.
        assert!(parsed.non_identity_nodes.is_empty(), "{context}");
        assert!(parsed.unhandled_blocks.is_empty(), "{context}");
        // One land shape; everything else is a water plane under the WATER node.
        assert_eq!(parsed.unrecognised_shapes(), 0, "{context}");
        let lands: Vec<_> = parsed.shapes_of(TerrainLodShapeKind::Terrain).collect();
        assert_eq!(lands.len(), 1, "{context}");
        let land = lands[0];
        assert_eq!(land.block_type, "BSTriShape");
        assert_eq!(land.parent_name.as_deref(), Some("chunk"), "{context}");
        // Positions are block-local; the stored scale is the block level; no rotation.
        assert_eq!(land.scale, level as f32, "{context}");
        assert_eq!(land.translation, [0.0; 3], "{context}");
        assert!(land.is_axis_aligned_identity_rotation(), "{context}");
        // The shader names this block's own colour and normal maps (and nothing else).
        let shader = land.shader.as_ref().expect("the land shape has a shader");
        assert_eq!(
            (shader.shader_type, shader.flags_1, shader.flags_2),
            (18, 0x8040_1000, 0x3),
            "{context}"
        );
        let stem = format!("{}.{level}.{cell_x}.{cell_y}", name.world);
        let slot = |index: usize| shader.texture_slots[index].clone().unwrap_or_default();
        assert!(
            slot(0).ends_with(&format!("{stem}.dds")),
            "{context}: {:?}",
            slot(0)
        );
        assert!(
            slot(1).ends_with(&format!("{stem}_n.dds")),
            "{context}: {:?}",
            slot(1)
        );
        assert_eq!(
            shader
                .texture_slots
                .iter()
                .filter(|slot| slot.is_some())
                .count(),
            2,
            "{context}"
        );
        // Land vertices carry a position and a UV and nothing else: the normal comes from the map.
        let side = f64::from(level) * 4096.0;
        for vertex in &land.vertices {
            assert!(vertex.uv.is_some() && vertex.normal.is_none() && vertex.color.is_none());
            if level < 32 {
                let uv = vertex.uv.unwrap();
                let [x, y, _] = vertex.position;
                let eu = (f64::from(uv[0]) - f64::from(x) * f64::from(level) / side).abs();
                let ev = (f64::from(uv[1]) - (1.0 - f64::from(y) * f64::from(level) / side)).abs();
                max_uv_error_below_32 = max_uv_error_below_32.max(eu).max(ev);
            }
        }
        // The block lies on the 4-cell grid it is named for; Tamriel's LOD origin is -96.
        if name.world == "tamriel" {
            let lod_origin = -96i32;
            assert_eq!(
                (cell_x - lod_origin).rem_euclid(level as i32),
                0,
                "{context}"
            );
            assert_eq!(
                (cell_y - lod_origin).rem_euclid(level as i32),
                0,
                "{context}"
            );
        }
        let entry = totals.entry((name.world.clone(), level)).or_default();
        entry[0] += 1;
        entry[1] += land.vertices.len();
        entry[2] += land.triangles.len();
        entry[3] += parsed.shapes_of(TerrainLodShapeKind::Water).count();
        entry[4] += parsed.triangle_count(TerrainLodShapeKind::Water);
    }
    for ((world, level), [blocks, vertices, triangles, water, water_triangles]) in &totals {
        println!(
            "{world} level {level}: {blocks} blocks, land {vertices} vertices {triangles} triangles, water {water} shapes {water_triangles} triangles"
        );
    }
    // Levels 4, 8 and 16 follow the planar rule u = x / 4096, v = 1 - y / 4096 (in block units);
    // half-float rounding is far below 0.01. Level 32 does not: its edge vertices overshoot.
    assert!(
        max_uv_error_below_32 < 0.01,
        "max UV error {max_uv_error_below_32}"
    );
    // Tamriel: [blocks, land vertices, land triangles, water shapes, water triangles].
    let tamriel = |level: u32| totals[&("tamriel".to_owned(), level)];
    assert_eq!(tamriel(4), [2_304, 2_178_456, 4_192_762, 1_444, 35_916]);
    assert_eq!(tamriel(8), [576, 617_137, 1_188_877, 410, 35_916]);
    assert_eq!(tamriel(16), [144, 155_872, 299_584, 118, 35_916]);
    assert_eq!(tamriel(32), [36, 40_879, 73_603, 34, 35_930]);
    assert_eq!(
        totals.len(),
        4 * 12 - 1,
        "12 worlds; Japhet's Folly has no level 32"
    );
}
