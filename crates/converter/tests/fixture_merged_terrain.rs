//! Merged terrain (#215) through a conversion: the land texture arrays, one mesh per exterior
//! cell and the chunk files, written only when the option asks for them.

use dummy_content::layout;
use shared::terrain_mesh::{
    LAND_TEXTURE_TABLE_PATH, LandTextureTable, TERRAIN_MESH_FORMAT_VERSION, terrain_chunk_path,
};
use std::{collections::BTreeSet, fs, path::Path};

/// The default LOD atlas encoder is the GPU; these fixtures run on the CPU encoder only.
fn cpu_config(data: &Path, output: &Path) -> converter::PipelineConfig {
    let mut config = converter::PipelineConfig::new(data, output);
    config.lod_texture_encoder = converter::TextureEncoder::Cpu;
    config
}

fn generate_data(root: &Path) {
    layout::prepare_directory(root, false).unwrap();
    let formats = layout::Formats::parse("dds,pex,nif,esm").unwrap();
    layout::generate(root, layout::DEFAULT_SEED, formats).unwrap();
}

async fn convert_config(config: converter::PipelineConfig) -> converter::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::run_async(config, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    report
}

/// Every exterior cell of the converted fixture with a LAND row, and its chunk path.
fn expected_chunks(output: &Path) -> Vec<(u32, i32, i32)> {
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    connection
        .prepare(
            "SELECT c.worldspace_id, c.grid_x, c.grid_y FROM cells c
             JOIN land l ON l.cell_id = c.id
             WHERE c.grid_x IS NOT NULL AND c.grid_y IS NOT NULL AND c.worldspace_id IS NOT NULL",
        )
        .unwrap()
        .query_map([], |row| {
            let worldspace: u32 = row.get(0)?;
            let cell: (i32, i32) = (row.get(1)?, row.get(2)?);
            Ok((worldspace, cell))
        })
        .unwrap()
        .map(|row| {
            let (worldspace, (grid_x, grid_y)) = row.unwrap();
            (worldspace, grid_x, grid_y)
        })
        .collect()
}

#[tokio::test]
async fn merged_terrain_writes_the_table_arrays_and_chunk_files() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    generate_data(&data);
    let mut config = cpu_config(&data, &output);
    config.merged_terrain = true;
    let report = convert_config(config).await;
    assert!(report.complete, "{:?}", report.warnings);
    let terrain = report
        .terrain
        .as_ref()
        .expect("the run reports its merged terrain");
    assert!(report.terrain_elapsed_ms > 0);
    assert!(
        terrain.skipped_cells.is_empty(),
        "{:?}",
        terrain.skipped_cells
    );
    assert!(
        terrain.skipped_textures.is_empty(),
        "{:?}",
        terrain.skipped_textures
    );

    // The table, at the shared path, is the one the engine validates.
    let table: LandTextureTable =
        serde_json::from_slice(&fs::read(output.join(LAND_TEXTURE_TABLE_PATH)).unwrap()).unwrap();
    assert_eq!(table.format_version, TERRAIN_MESH_FORMAT_VERSION);
    table.validate().unwrap();
    let ltex: u32 = {
        let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        connection
            .query_row("SELECT id FROM landscape_textures LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let entry = table
        .entries
        .iter()
        .find(|entry| entry.ltex_form_id == ltex)
        .expect("the fixture's LTEX has a table entry");
    assert!(
        entry.normal.is_some(),
        "the fixture's texture set names a normal map, so the entry carries one"
    );

    // At least one array file, at the path the report and the table name, holding the layers
    // they say and the mip chain of the class.
    assert!(!terrain.arrays.is_empty(), "{terrain:#?}");
    assert_eq!(
        terrain.arrays.len(),
        table.arrays.len(),
        "the report names every array file the table lists"
    );
    for array in &terrain.arrays {
        let file = fs::read(output.join(&array.path)).unwrap();
        let reader = ktx2::Reader::new(&file).unwrap();
        let header = reader.header();
        assert_eq!(header.layer_count, array.layers);
        assert_eq!(header.level_count, array.levels);
        assert_eq!(header.pixel_width, array.size);
        assert!(
            array.layers >= 2,
            "a single member is padded so the file loads as an array"
        );
    }

    // The chunk files: exactly the groups the fixture's LAND cells land in, each reading back
    // with a key per cell and the cell count the report claims.
    let expected = expected_chunks(&output);
    assert!(!expected.is_empty(), "the fixture has exterior LAND cells");
    let expected_paths: BTreeSet<String> = expected
        .iter()
        .map(|&(worldspace, grid_x, grid_y)| terrain_chunk_path(worldspace, grid_x, grid_y))
        .collect();
    let written_paths: BTreeSet<String> = terrain.chunk_files.iter().cloned().collect();
    assert_eq!(
        written_paths, expected_paths,
        "one chunk file per group of exterior LAND cells"
    );
    let mut cells = 0;
    for path in &terrain.chunk_files {
        let chunk = converter::terrain_chunks::read_terrain_chunk(&output.join(path)).unwrap();
        assert_eq!(chunk.format_version, TERRAIN_MESH_FORMAT_VERSION);
        assert!(
            chunk.cells.iter().all(|cell| cell.key != [0; 32]),
            "every cell carries its derived-data key"
        );
        // The file sits under the worldspace whose cells it holds.
        let worldspace = u32::from_str_radix(&path["terrain/".len()..][..8], 16).unwrap();
        assert_eq!(chunk.worldspace_id, worldspace);
        cells += chunk.cells.len();
    }
    assert_eq!(cells as u64, terrain.cells_written);
    assert_eq!(cells, expected.len(), "one mesh per exterior LAND cell");
    assert_eq!(
        terrain
            .worldspaces
            .iter()
            .map(|world| world.cells)
            .sum::<u64>(),
        cells as u64
    );

    // Every file the stage wrote is published by name, not only staged.
    for relative in terrain
        .chunk_files
        .iter()
        .map(String::as_str)
        .chain(terrain.arrays.iter().map(|array| array.path.as_str()))
        .chain([LAND_TEXTURE_TABLE_PATH])
    {
        assert!(
            output.join(relative).is_file(),
            "{relative} is not published"
        );
        assert!(
            report
                .artifacts
                .iter()
                .any(|artifact| artifact.to_string_lossy().replace('\\', "/") == relative),
            "{relative} is not in the run's artifacts"
        );
    }
}

/// The metadata rebuild re-derives the records and rebuilds the merged terrain from them, and
/// the rebuild validates the payloads it produced (`metadata.rs`), so a rebuild with the option
/// on publishes terrain files a check accepts.
#[tokio::test]
async fn a_metadata_rebuild_rebuilds_the_terrain_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    let rebuilt = directory.path().join("rebuilt");
    generate_data(&data);
    let mut config = cpu_config(&data, &output);
    config.merged_terrain = true;
    assert!(convert_config(config).await.terrain.is_some());

    let mut config = cpu_config(&data, &rebuilt);
    config.merged_terrain = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::rebuild_metadata_async(config, &output, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let terrain = report
        .terrain
        .as_ref()
        .expect("the rebuild reports its merged terrain");
    assert!(!terrain.arrays.is_empty());
    assert!(!terrain.chunk_files.is_empty());
    assert!(rebuilt.join(LAND_TEXTURE_TABLE_PATH).is_file());
    for array in &terrain.arrays {
        assert!(rebuilt.join(&array.path).is_file(), "{}", array.path);
    }
}

#[tokio::test]
async fn without_the_option_no_terrain_output_is_written() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    generate_data(&data);
    let report = convert_config(cpu_config(&data, &output)).await;
    assert!(report.complete, "{:?}", report.warnings);
    assert!(report.terrain.is_none());
    assert!(
        report.terrain_warnings.is_empty(),
        "{:?}",
        report.terrain_warnings
    );
    assert!(!output.join("terrain").exists());
    assert!(!output.join("terrain/").exists());

    // The same input with the option on does write it, so the pair discriminates.
    let with_option = directory.path().join("with-option");
    let mut config = cpu_config(&data, &with_option);
    config.merged_terrain = true;
    assert!(convert_config(config).await.terrain.is_some());
    assert!(with_option.join("terrain").is_dir());
}
