//! Reads real retail LOD files from a local install and checks the documented layouts.
//!
//! Ignored by default: it needs `MUDCRAB_SKYRIM_DATA` to name a Skyrim Special Edition `Data`
//! folder. It extracts into a temporary directory, never writes the install, and commits no
//! retail bytes. Run with `cargo test -p converter --test retail_lod_readers -- --ignored`.

use converter::{
    archive::ArchiveExtractor,
    lod::objects::{chunk_key_for_block, compile_object_chunk, validate_object_glb},
    lod::retail::{
        atlas::{DdsFormat, read_dds_info},
        bto::read_object_lod,
        btt::{BttError, parse_tree_block, parse_tree_block_prefix},
        lst::parse_tree_list,
        name::{RetailLodKind, parse_lod_file_name},
    },
};
use shared::lod::LodOrigin;
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

#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
fn tamriel_level_16_blocks_compile_to_valid_chunk_local_object_chunks() {
    let data = data_dir();
    let temp = tempfile::tempdir().unwrap();
    let files = extract(&archives(&data, "Skyrim - Meshes"), temp.path(), |path| {
        let text = path.to_string_lossy();
        parse_lod_file_name(&text).is_some_and(|name| {
            name.kind == RetailLodKind::Object && name.world == "tamriel" && name.level == Some(16)
        })
    });
    assert_eq!(files.len(), 48);
    // The pack's LOD origin for Tamriel (worldspace 0x3c).
    let origin = LodOrigin::new(-96, -96);
    let atlas = {
        let dds = dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 8, 8),
            &mut dummy_content::rng::Rng::new(0),
        )
        .unwrap();
        converter::texture::TextureConverter::convert(
            &dds,
            converter::texture::TextureEncoding::ColorSrgb,
        )
        .unwrap()
    };
    let mut triangles = 0;
    let mut flag_counts = std::collections::BTreeMap::<u32, usize>::new();
    for file in &files {
        let name = parse_lod_file_name(&file.to_string_lossy()).unwrap();
        let (x, y) = name.cell.unwrap();
        let key = chunk_key_for_block(0x3c, origin, 16, x, y).unwrap();
        let bytes = std::fs::read(file).unwrap();
        let block = read_object_lod(&bytes, file).unwrap();
        let chunk = compile_object_chunk(key, origin, &block, &atlas).unwrap();
        for shape in &block.shapes {
            *flag_counts.entry(shape.flags).or_default() += 1;
        }
        validate_object_glb(&chunk.glb).unwrap();
        triangles += chunk.triangles;
        // The shipped files store a level-16 shape at scale 16 (the block level), translated to the block
        // origin; the compiler applies the stored transform.
        assert_eq!(block.shapes[0].scale, 16.0);
        assert_eq!(
            block.shapes[0].translation,
            [x as f32 * 4096.0, y as f32 * 4096.0, 0.0]
        );
        // The merged meshes cover about the block's own 16 x 16 cells and overshoot its edges by
        // a cell or two (largest seen: 1.5 cells); a wrong transform would be far off or span a
        // fraction of the block.
        let (cell_x, cell_y) = (f64::from(x) * 4096.0, f64::from(y) * 4096.0);
        let side = 16.0 * 4096.0;
        let margin = 3.0 * 4096.0;
        assert!(f64::from(chunk.bounds_min[0]) >= cell_x - margin, "{x},{y}");
        assert!(f64::from(chunk.bounds_min[1]) >= cell_y - margin, "{x},{y}");
        assert!(
            f64::from(chunk.bounds_max[0]) <= cell_x + side + margin,
            "{x},{y}"
        );
        assert!(
            f64::from(chunk.bounds_max[1]) <= cell_y + side + margin,
            "{x},{y}"
        );
    }
    println!("level 16 shape node flags: {flag_counts:?}");
    assert_eq!(triangles, 311_641);
}

#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
fn tamriel_level_8_and_4_blocks_compile_and_level_4_segments_are_cells() {
    let data = data_dir();
    let temp = tempfile::tempdir().unwrap();
    let origin = LodOrigin::new(-96, -96);
    let atlas = {
        let dds = dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 8, 8),
            &mut dummy_content::rng::Rng::new(0),
        )
        .unwrap();
        converter::texture::TextureConverter::convert(
            &dds,
            converter::texture::TextureEncoding::ColorSrgb,
        )
        .unwrap()
    };
    // (blocks, triangles) measured on the shipped files.
    for (level, expected_blocks, expected_triangles) in
        [(8u32, 152usize, 1_016_520usize), (4, 517, 2_350_870)]
    {
        let files = extract(&archives(&data, "Skyrim - Meshes"), temp.path(), |path| {
            let text = path.to_string_lossy();
            parse_lod_file_name(&text).is_some_and(|name| {
                name.kind == RetailLodKind::Object
                    && name.world == "tamriel"
                    && name.level == Some(level)
            })
        });
        assert_eq!(
            files.len(),
            expected_blocks,
            "Tamriel level {level} block count"
        );
        let (mut triangles, mut pieces, mut with_large_refs) = (0usize, 0usize, 0usize);
        // Segment i of a level-4 shape covers cell (x + i / 4, y + i % 4) (verified in the loader,
        // skyrim_decomp#816): the geometry check counts segments whose mean vertex is within 0.75
        // cell of that cell.
        let (mut fitted, mut total) = (0usize, 0usize);
        for file in &files {
            let name = parse_lod_file_name(&file.to_string_lossy()).unwrap();
            let (x, y) = name.cell.unwrap();
            let key = chunk_key_for_block(0x3c, origin, level, x, y).unwrap();
            let block = read_object_lod(&std::fs::read(file).unwrap(), file).unwrap();
            let chunk = compile_object_chunk(key, origin, &block, &atlas).unwrap();
            validate_object_glb(&chunk.glb).unwrap();
            triangles += chunk.triangles;
            pieces += chunk.cell_pieces;
            with_large_refs += usize::from(chunk.large_ref_blocks > 0);
            if level != 4 {
                continue;
            }
            let (cell_fit, cell_total) = converter::lod::objects::segment_cell_fit(&block, (x, y));
            fitted += cell_fit;
            total += cell_total;
        }
        println!(
            "level {level}: {triangles} triangles, {pieces} cell pieces, {with_large_refs} blocks with large-reference data, segment fit {fitted}/{total}"
        );
        assert_eq!(triangles, expected_triangles);
        if level == 4 {
            assert!(
                total > 6_000 && fitted * 1000 >= total * 999,
                "segment fit {fitted}/{total}"
            );
            assert!(pieces > 0 && with_large_refs == 496);
        } else {
            assert_eq!(pieces, 0);
        }
    }
}
