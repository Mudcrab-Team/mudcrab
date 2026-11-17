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
        btt::{BttError, parse_tree_block, parse_tree_block_prefix},
        lst::parse_tree_list,
        name::{RetailLodKind, parse_lod_file_name},
    },
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("MUDCRAB_SKYRIM_DATA").map(PathBuf::from)
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
    let Some(data) = data_dir() else { return };
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
    let Some(data) = data_dir() else { return };
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
    let Some(data) = data_dir() else { return };
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
    let Some(data) = data_dir() else { return };
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
