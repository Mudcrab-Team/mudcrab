//! Post-publish cluster overlay derivation: a conversion never writes cluster data into the
//! base; `cluster` and `auto` both derive one overlay over the published base afterwards.
//!
//! Every test converts a real fixture and derives for real. Only the environment is substituted
//! (`PostPublish::FIXTURE`: a synthetic adapter capability and the CPU-only derive entry with an
//! injected free-space reader); nothing here touches a GPU or the disk-floor probe.

use super::*;
use crate::{
    check::{CheckMode, CheckProblem, check_output},
    cluster::{DEFAULT_TRIANGLE_THRESHOLD, LodMode, PostPublish, probe::ProbeResult},
    derive_extensions::{self, DeriveOptions, DeriveReport},
};
use dummy_content::{Entry, bsa, layout};
use std::sync::atomic::AtomicUsize;

// Skyrim layout: two i16 origins, then i32 stride/minimum/maximum levels.
const PACKED_SETTINGS: [u8; 16] = [0xfc, 0xff, 0xfc, 0xff, 32, 0, 0, 0, 4, 0, 0, 0, 32, 0, 0, 0];

struct World {
    _directory: tempfile::TempDir,
    data: PathBuf,
    output: PathBuf,
}

/// Generated game data with a worldspace whose LOD settings give the base a core identity.
fn world(generate: bool) -> World {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    if generate {
        layout::prepare_directory(&data, false).unwrap();
        layout::generate(
            &data,
            layout::DEFAULT_SEED,
            layout::Formats::parse("dds,pex,nif,esm").unwrap(),
        )
        .unwrap();
        fs::write(
            data.join("Skyrim - Misc.bsa"),
            bsa::v105(
                &[Entry {
                    name: "LODSettings/GeneratedWorld.LOD",
                    data: &PACKED_SETTINGS,
                }],
                bsa::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();
    } else {
        fs::create_dir_all(&data).unwrap();
    }
    World {
        _directory: directory,
        data,
        output,
    }
}

fn config(world: &World, mode: LodMode) -> PipelineConfig {
    let mut config = PipelineConfig::new(&world.data, &world.output);
    config.lod_mode = mode;
    config.cpu_jobs = 2;
    config.texture_encoder = TextureEncoder::Cpu;
    config.lod_texture_encoder = TextureEncoder::Cpu;
    config
}

fn use_tree_placements(world: &World) {
    // Keep fixture model paths, placements and FormIDs; the classifier owns TREE rules.
    let plugin_path = discover(&world.data)
        .unwrap()
        .into_iter()
        .find(|path| extension(path, &["esm"]))
        .unwrap();
    let mut plugin = fs::read(&plugin_path).unwrap();
    let signatures = plugin
        .windows(4)
        .enumerate()
        .filter_map(|(offset, bytes)| (bytes == b"STAT").then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(signatures.len(), 1);
    for offset in signatures {
        plugin[offset..offset + 4].copy_from_slice(b"TREE");
    }
    fs::write(plugin_path, plugin).unwrap();
}

async fn convert(
    config: PipelineConfig,
    hooks: PostPublish,
) -> Result<PipelineReport, PipelineFailure> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result =
        AssetPipeline::run_with_post_publish(config, sender, Cancellation::new(), hooks).await;
    drain.await.unwrap();
    result
}

/// The published base carries no cluster table, payload, manifest or extension entry.
fn assert_clusterless_base(output: &Path) {
    for absent in [
        "clusters",
        "cluster-manifest.json",
        "cluster-terrain-manifest.json",
    ] {
        assert!(
            !output.join(absent).exists(),
            "the base holds {absent}; cluster data belongs to the overlay"
        );
    }
    let database = Connection::open_with_flags(
        output.join("skyrim_world.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let tables: i64 = database
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'cluster_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "the base database holds cluster tables");
    let manifest = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert!(
        manifest
            .extensions
            .iter()
            .all(|entry| entry.id != shared::extensions::CLUSTERS
                && entry.id != shared::extensions::CLUSTER_TERRAIN),
        "the base manifest declares a cluster extension"
    );
}

/// The only overlay under `derived/`; a leftover derive staging folder would be a second one.
fn sole_overlay(output: &Path) -> PathBuf {
    let mut overlays: Vec<_> = fs::read_dir(output.join("derived"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(overlays.len(), 1, "{overlays:?}");
    overlays.remove(0)
}

/// The engine-side binder accepts the overlay for this base.
fn assert_accepted_overlay(output: &Path) {
    let overlay = fs::canonicalize(sole_overlay(output)).unwrap();
    let metadata =
        shared::cluster_overlay::checked_overlay(&fs::canonicalize(output).unwrap(), &overlay)
            .unwrap();
    assert_eq!(metadata.label, "default");
    assert!(
        metadata
            .extensions
            .iter()
            .any(|entry| entry.id == shared::extensions::CLUSTERS)
    );
    assert!(overlay.join("clusters.db").is_file());
    assert!(
        !overlay.join("skyrim_world.db").exists(),
        "the overlay copied the core database"
    );
}

fn no_staging_beside(output: &Path) {
    for entry in fs::read_dir(output.parent().unwrap()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!name.contains(".staging-"), "left behind: {name}");
    }
}

/// The text between `marker` and the next double quote.
fn quoted_after(text: &str, marker: &str) -> PathBuf {
    let rest = &text[text
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} in {text}"))
        + marker.len()..];
    PathBuf::from(&rest[..rest.find(marker.chars().last().unwrap()).unwrap()])
}

fn failing_derive(_: &DeriveOptions, _: Option<ProbeResult>) -> color_eyre::Result<DeriveReport> {
    Err(color_eyre::eyre::eyre!("injected derive failure"))
}

const FAILING: PostPublish = PostPublish {
    probe: crate::cluster::fixture_probe,
    derive: failing_derive,
};

#[tokio::test]
async fn cluster_and_auto_conversions_publish_a_clusterless_base_and_one_accepted_overlay() {
    for mode in [LodMode::Cluster, LodMode::Auto] {
        let world = world(true);
        let report = convert(config(&world, mode), PostPublish::FIXTURE)
            .await
            .unwrap();
        assert!(
            report.cluster.is_some(),
            "{mode}: preserve the overlay stage counters in the pipeline report"
        );
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.contains("cluster overlay derived at")),
            "{mode}: {:?}",
            report.notices
        );
        assert_clusterless_base(&world.output);
        assert_accepted_overlay(&world.output);
        no_staging_beside(&world.output);
    }
}

#[tokio::test]
async fn cluster_without_native_lod_origin_derives_cell_minima_and_admits_terrain() {
    let world = world(true);
    use_tree_placements(&world);
    fs::remove_file(world.data.join("Skyrim - Misc.bsa")).unwrap();
    let report = convert(config(&world, LodMode::Cluster), PostPublish::FIXTURE)
        .await
        .unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(report.lod_chunks, 0);
    assert!(report.cluster.as_ref().unwrap().counts.references_eligible > 0);
    assert!(
        report.notices.iter().any(|notice| {
            notice.contains("GeneratedWorld") && notice.contains("exterior cell minima")
        }),
        "{:?}",
        report.notices
    );
    assert_derived_cell_origin(&world.output, (-1, -1));
    let geometry = shared::geometry::GeometryManifest::load(&world.output).unwrap();
    assert_eq!(
        geometry.geometry_mode,
        shared::geometry::GeometryMode::Cluster
    );
    let overlay = fs::canonicalize(world.output.join(geometry.cluster_package.unwrap())).unwrap();
    let metadata = shared::cluster_overlay::checked_overlay(
        &fs::canonicalize(&world.output).unwrap(),
        &overlay,
    )
    .unwrap();
    assert!(metadata.build_settings.terrain);
    assert!(metadata.build_settings.include_trees);
    assert!(
        metadata
            .extensions
            .iter()
            .any(|entry| entry.id == shared::extensions::CLUSTER_TERRAIN)
    );
    let database = Connection::open(overlay.join("clusters.db")).unwrap();
    let terrain_rows: i64 = database
        .query_row("SELECT count(*) FROM cluster_terrain", [], |row| row.get(0))
        .unwrap();
    assert!(terrain_rows > 0);
    assert_accepted_overlay(&world.output);
}

fn assert_derived_cell_origin(output: &Path, expected: (i32, i32)) {
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    let (worldspace_id, x, y): (u32, i32, i32) = database
        .query_row(
            "SELECT id, lod_origin_x, lod_origin_y FROM worldspaces WHERE editor_id = ?1",
            [layout::GENERATED_WORLDSPACE],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let minima: (i32, i32) = database
        .query_row(
            "SELECT MIN(grid_x), MIN(grid_y) FROM cells WHERE worldspace_id = ?1
             AND grid_x IS NOT NULL AND grid_y IS NOT NULL",
            [worldspace_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(minima, expected);
    assert_eq!((x, y), minima);
    let chunks: i64 = database
        .query_row("SELECT count(*) FROM lod_chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(chunks, 0);
    let recipe: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("lod-manifest.json")).unwrap()).unwrap();
    let settings = recipe["world_settings"].as_array().unwrap();
    let settings = settings
        .iter()
        .find(|settings| settings["worldspace_id"] == worldspace_id)
        .unwrap();
    assert_eq!(settings["origin"], serde_json::json!([x, y]));
    assert_eq!(settings["origin_source"], "exterior_cell_minima");
    assert!(settings["sidecar"].is_null());
}

#[tokio::test]
async fn baked_and_auto_without_native_lod_origin_keep_missing_origin() {
    for mode in [LodMode::Baked, LodMode::Auto] {
        let world = world(true);
        fs::remove_file(world.data.join("Skyrim - Misc.bsa")).unwrap();
        let report = convert(config(&world, mode), PostPublish::FIXTURE)
            .await
            .unwrap();
        assert_eq!(report.lod_chunks, 0);
        assert!(
            report
                .lod_warnings
                .iter()
                .any(|warning| warning.contains("has no LOD origin"))
        );
        assert!(
            !report
                .notices
                .iter()
                .any(|notice| notice.contains("exterior cell minima"))
        );
        let database = Connection::open(world.output.join("skyrim_world.db")).unwrap();
        let origin: (Option<i32>, Option<i32>) = database
            .query_row(
                "SELECT lod_origin_x, lod_origin_y FROM worldspaces WHERE editor_id = ?1",
                [layout::GENERATED_WORLDSPACE],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, (None, None));
        assert!(!world.output.join("lod-manifest.json").exists());
    }
}

#[tokio::test]
async fn cluster_without_native_lod_origin_clusters_object_only_world_at_nonzero_minima() {
    let world = world(true);
    fs::remove_file(world.data.join("Skyrim - Misc.bsa")).unwrap();
    // Different cells supply each axis minimum; neither cell is the derived origin.
    let cells = [
        dummy_content::esm::Cell {
            grid_x: -7,
            grid_y: 18,
        },
        dummy_content::esm::Cell {
            grid_x: 4,
            grid_y: 11,
        },
    ];
    let mut plugin = dummy_content::esm::plugin(&dummy_content::esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &cells,
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    let land_offsets: Vec<_> = plugin
        .windows(4)
        .enumerate()
        .filter_map(|(offset, signature)| (signature == b"LAND").then_some(offset))
        .collect();
    assert_eq!(land_offsets.len(), cells.len());
    for offset in land_offsets {
        // The authored deleted flag removes LAND through the real plugin merger.
        let flags = u32::from_le_bytes(plugin[offset + 8..offset + 12].try_into().unwrap());
        plugin[offset + 8..offset + 12].copy_from_slice(&(flags | 0x20).to_le_bytes());
    }
    fs::write(world.data.join("Skyrim.esm"), plugin).unwrap();
    let report = convert(config(&world, LodMode::Cluster), PostPublish::FIXTURE)
        .await
        .unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(
        report.cluster.as_ref().unwrap().counts.references_eligible,
        cells.len() as u64
    );
    assert_derived_cell_origin(&world.output, (-7, 11));
    let database = Connection::open(world.output.join("skyrim_world.db")).unwrap();
    let land_rows: i64 = database
        .query_row("SELECT count(*) FROM land", [], |row| row.get(0))
        .unwrap();
    assert_eq!(land_rows, 0);
    let geometry = shared::geometry::GeometryManifest::load(&world.output).unwrap();
    let overlay = sole_overlay(&world.output);
    assert_eq!(
        world.output.join(geometry.cluster_package.unwrap()),
        overlay
    );
    let metadata = shared::cluster_overlay::read_metadata(&overlay).unwrap();
    assert!(!metadata.build_settings.terrain);
    assert!(metadata.build_settings.include_trees);
    assert!(metadata.terrain_sources.is_none());
    assert_eq!(metadata.settings_id, metadata.build_settings.settings_id());
    assert_eq!(
        overlay.file_name().unwrap().to_string_lossy(),
        format!("clusters-{}", metadata.settings_id)
    );
    assert!(
        !metadata
            .extensions
            .iter()
            .any(|entry| entry.id == shared::extensions::CLUSTER_TERRAIN)
    );
    assert_accepted_overlay(&world.output);
}

fn invalid_terrain_cache_derive(
    options: &DeriveOptions,
    probe: Option<ProbeResult>,
) -> color_eyre::Result<DeriveReport> {
    fs::write(
        options.source.join("cell_cache.rkyv"),
        b"invalid terrain cache",
    )?;
    derive_extensions::run_fixture_with_probe(options, probe)
}

#[tokio::test]
async fn selected_land_with_an_invalid_cache_fails_instead_of_publishing_object_only() {
    let world = world(true);
    let failure = convert(
        config(&world, LodMode::Cluster),
        PostPublish {
            probe: crate::cluster::fixture_probe,
            derive: invalid_terrain_cache_derive,
        },
    )
    .await
    .unwrap_err();
    let text = format!("{:#}", failure.error);
    assert!(text.contains("invalid cell cache"), "{text}");
    assert!(text.contains("--terrain --terrain-sources"), "{text}");
    let base = Connection::open(world.output.join("skyrim_world.db")).unwrap();
    let land_rows: i64 = base
        .query_row("SELECT count(*) FROM land", [], |row| row.get(0))
        .unwrap();
    assert!(land_rows > 0);
    assert_clusterless_base(&world.output);
    let derived = world.output.join("derived");
    assert!(
        fs::read_dir(derived)
            .unwrap()
            .all(|entry| !entry.unwrap().path().is_dir())
    );
}

#[tokio::test]
async fn cluster_invalid_native_lod_settings_still_fail() {
    let world = world(true);
    fs::write(
        world.data.join("Skyrim - Misc.bsa"),
        bsa::v105(
            &[Entry {
                name: "LODSettings/GeneratedWorld.LOD",
                data: b"invalid authored settings",
            }],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    let failure = convert(config(&world, LodMode::Cluster), PostPublish::FIXTURE)
        .await
        .unwrap_err();
    assert!(format!("{failure:#}").contains("invalid cluster terrain settings for GeneratedWorld"));
    assert!(!world.output.exists());
}

#[test]
fn cluster_grid_origin_ignores_partial_coordinates_and_leaves_empty_worlds_missing() {
    let world = world(true);
    let database_path = world._directory.path().join("grid-origin.db");
    EsmParser::convert_plugins_with_records(&[world.data.join("Skyrim.esm")], &database_path)
        .unwrap();
    let database = Connection::open(database_path).unwrap();
    let worldspace_id: u32 = database
        .query_row(
            "SELECT id FROM worldspaces WHERE editor_id = ?1",
            [layout::GENERATED_WORLDSPACE],
            |row| row.get(0),
        )
        .unwrap();
    database
        .execute_batch(
            "UPDATE cells SET grid_x = -99, grid_y = NULL WHERE id = (SELECT MIN(id) FROM cells);
         UPDATE cells SET grid_x = NULL, grid_y = -99 WHERE id = (SELECT MAX(id) FROM cells);",
        )
        .unwrap();
    assert_eq!(
        exterior_cell_grid_origin(&database, worldspace_id).unwrap(),
        Some(LodOrigin::new(-1, -1))
    );
    assert_eq!(
        exterior_cell_grid_origin(&database, worldspace_id + 1).unwrap(),
        None
    );
    database
        .execute("UPDATE cells SET grid_x = NULL", [])
        .unwrap();
    assert_eq!(
        exterior_cell_grid_origin(&database, worldspace_id).unwrap(),
        None
    );
}

/// CPU-only integration: native inputs are deliberately invalid. Reaching publication proves
/// neither archive/loose inventory nor native mesh/billboard conversion consumed them.
#[tokio::test]
async fn cluster_conversion_omits_baked_products_and_binds_the_exact_tree_and_terrain_package() {
    let world = world(true);
    use_tree_placements(&world);
    let native = [
        "meshes/terrain/generatedworld/objects/generatedworld.4.0.0.bto",
        "meshes/terrain/generatedworld/generatedworld.4.0.0.btr",
        "meshes/terrain/generatedworld/trees/generatedworld.lst",
        "meshes/terrain/generatedworld/trees/generatedworld.4.0.0.btt",
        "textures/terrain/generatedworld/trees/generatedworldtreeatlas.dds",
        "textures/terrain/generatedworld/objects/atlas.dds",
        "textures/terrain/generatedworld/generatedworld.4.0.0.dds",
        "textures/lod/generatedworld/atlas.dds",
    ];
    for relative in native {
        let path = world.data.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"must not be read in cluster mode").unwrap();
    }
    let entries: Vec<_> = native
        .iter()
        .map(|name| Entry {
            name,
            data: b"must not be extracted in cluster mode",
        })
        .collect();
    fs::write(
        world.data.join("Skyrim - Meshes.bsa"),
        bsa::v105(&entries, bsa::Compression::None).unwrap(),
    )
    .unwrap();
    let mut settings = config(&world, LodMode::Cluster);
    settings.cluster_triangle_threshold = 1;
    assert!(
        !settings.cluster_terrain,
        "terrain must be forced by the mode"
    );
    let report = convert(settings, PostPublish::FIXTURE).await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(report.lod_chunks, 0);
    assert!(
        report.cluster.as_ref().unwrap().counts.references_eligible > 0,
        "the TREE placements must enter the cluster package"
    );
    assert!(!world.output.join("lod").exists());
    for relative in native {
        assert!(!world.output.join(relative).exists());
        assert!(!world.output.join(relative).with_extension("glb").exists());
        assert!(!world.output.join(relative).with_extension("ktx2").exists());
    }
    assert!(
        !world
            .output
            .join("meshes/terrain/generatedworld/trees")
            .exists()
    );
    let manifest =
        ConversionManifest::load(&world.output.join("conversion-manifest.json")).unwrap();
    assert!(
        manifest
            .entries
            .keys()
            .all(|key| !baked_sources::is_baked_geometry_input(Path::new(key)))
    );
    assert!(
        manifest
            .archives
            .values()
            .flat_map(|archive| &archive.files)
            .all(|file| !baked_sources::is_baked_geometry_input(Path::new(&file.path)))
    );
    let geometry = shared::geometry::GeometryManifest::load(&world.output).unwrap();
    assert_eq!(
        geometry.geometry_mode,
        shared::geometry::GeometryMode::Cluster
    );
    let overlay = world
        .output
        .join(geometry.cluster_package.as_ref().unwrap());
    assert_eq!(
        fs::canonicalize(&overlay).unwrap(),
        fs::canonicalize(sole_overlay(&world.output)).unwrap()
    );
    let options = DeriveOptions {
        source: world.output.clone(),
        label: "default".into(),
        replace: false,
        keep_failed_staging: false,
        terrain: true,
        terrain_sources: None,
        jobs: 2,
        triangle_threshold: 1,
        include_trees: true,
    };
    assert_eq!(
        geometry.cluster_package.unwrap(),
        format!(
            "derived/clusters-{}",
            derive_extensions::requested_settings(&options).settings_id()
        )
    );
    let metadata = shared::cluster_overlay::checked_overlay(
        &fs::canonicalize(&world.output).unwrap(),
        &fs::canonicalize(&overlay).unwrap(),
    )
    .unwrap();
    let database = Connection::open(world.output.join("skyrim_world.db")).unwrap();
    let expected = derive_extensions::resolved_settings(&options, &database).unwrap();
    assert_eq!(metadata.build_settings, expected);
    let origin: (i32, i32) = database
        .query_row(
            "SELECT lod_origin_x, lod_origin_y FROM worldspaces WHERE editor_id = ?1",
            [layout::GENERATED_WORLDSPACE],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        origin,
        (-4, -4),
        "authored sidecar must precede cell minima"
    );
    let rows: i64 = database
        .query_row("SELECT count(*) FROM lod_chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        rows, 0,
        "cluster-only conversion must not bake terrain chunks"
    );
    for table in [
        "lod_block",
        "lod_grid",
        "lod_tree_type",
        "lod_tree_instance",
    ] {
        let exists: bool = database
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1 AND type='table')",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists, "excluded native inventory table {table}");
    }
    assert!(world.output.join("cell_cache.rkyv").is_file());
    assert_eq!(
        shared::world_assets::validate_lod_build_contract(&world.output, CONVERTER_SCHEMA_VERSION)
            .unwrap(),
        shared::WORLD_DATABASE_SCHEMA_VERSION
    );
    let recipe: LodManifest =
        serde_json::from_slice(&fs::read(world.output.join("lod-manifest.json")).unwrap()).unwrap();
    assert_eq!(recipe.chunks, 0);
    assert!(recipe.chunk_inputs.is_empty());
    assert!(!recipe.terrain_sources.is_empty());
    assert_accepted_overlay(&world.output);
    // Runtime admission must not read the bake recipe; derivation verified it above.
    fs::write(
        world.output.join("lod-manifest.json"),
        b"not a runtime cluster dependency",
    )
    .unwrap();
    assert_eq!(
        shared::world_assets::validate_geometry_build_contract(
            &world.output,
            CONVERTER_SCHEMA_VERSION
        )
        .unwrap(),
        shared::WORLD_DATABASE_SCHEMA_VERSION
    );
    assert!(
        shared::world_assets::validate_lod_build_contract(&world.output, CONVERTER_SCHEMA_VERSION)
            .is_err()
    );
}

#[tokio::test]
async fn conversion_mode_changes_rebuild_products_and_cluster_threshold_keeps_full_coverage() {
    let world = world(true);
    let baked = convert(config(&world, LodMode::Baked), PostPublish::FIXTURE)
        .await
        .unwrap();
    assert!(baked.lod_chunks > 0);
    let baked_manifest = fs::read(world.output.join("conversion-manifest.json")).unwrap();
    assert!(!String::from_utf8_lossy(&baked_manifest).contains("geometry_mode"));
    let staging = world.output.with_file_name("assets.staging-mode-change");
    fs::create_dir_all(staging.join("lod/stale")).unwrap();
    fs::write(staging.join("lod/stale/chunk.glb"), b"old baked chunk").unwrap();
    fs::create_dir_all(staging.join("meshes/terrain/generatedworld/trees")).unwrap();
    fs::write(
        staging.join("meshes/terrain/generatedworld/trees/generatedworld.tree.0.glb"),
        b"old billboard",
    )
    .unwrap();
    let mut cluster = config(&world, LodMode::Cluster);
    cluster.resume_staging = Some(staging.clone());
    let report = convert(cluster, PostPublish::FIXTURE).await.unwrap();
    assert_eq!(report.lod_chunks, 0);
    assert!(!world.output.join("lod").exists());
    assert!(!world.output.join("meshes/terrain").exists());
    let first = shared::geometry::GeometryManifest::load(&world.output).unwrap();
    assert_accepted_overlay(&world.output);
    let mut changed = config(&world, LodMode::Cluster);
    changed.cluster_triangle_threshold += 1;
    changed.discard_derived = true;
    convert(changed, PostPublish::FIXTURE).await.unwrap();
    let second = shared::geometry::GeometryManifest::load(&world.output).unwrap();
    assert_eq!(first.cluster_package, second.cluster_package);
    assert!(world.output.join(first.cluster_package.unwrap()).exists());
    assert_accepted_overlay(&world.output);
    let mut baked = config(&world, LodMode::Baked);
    baked.discard_derived = true;
    let report = convert(baked, PostPublish::FIXTURE).await.unwrap();
    assert!(report.lod_chunks > 0);
    assert_eq!(
        fs::read(world.output.join("conversion-manifest.json")).unwrap(),
        baked_manifest,
        "baked manifest bytes return unchanged after a cluster conversion"
    );
    assert!(!world.output.join("derived").exists());
    assert!(!staging.exists());
}

#[tokio::test]
async fn cluster_conversion_retains_a_billboard_atlas_used_by_a_near_mesh() {
    let world = world(true);
    let atlas = "textures/terrain/generatedworld/trees/shared.dds";
    let atlas_path = world.data.join(atlas);
    fs::create_dir_all(atlas_path.parent().unwrap()).unwrap();
    fs::copy(world.data.join(layout::GENERATED_DIFFUSE_PATH), &atlas_path).unwrap();
    let shape = dummy_content::nif::static_shape(&dummy_content::nif::StaticShape {
        name: "NearMeshUsesSharedAtlas",
        positions: &[[0., 0., 0.], [100., 0., 0.], [0., 100., 0.]],
        normals: &[[0., 0., 1.]; 3],
        uvs: &[[0., 0.], [1., 0.], [0., 1.]],
        indices: &[[0, 1, 2]],
        diffuse: atlas,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    fs::write(world.data.join(layout::GENERATED_MODEL_PATH), shape).unwrap();
    convert(config(&world, LodMode::Cluster), PostPublish::FIXTURE)
        .await
        .unwrap();
    let manifest =
        ConversionManifest::load(&world.output.join("conversion-manifest.json")).unwrap();
    let texture = &manifest.entries[atlas];
    assert!(world.output.join(&texture.output).is_file());
    assert!(
        !manifest
            .pruned_texture_references
            .contains_key("meshes/generated.glb")
    );
    assert_accepted_overlay(&world.output);
}

#[tokio::test]
async fn cluster_reuse_restores_a_previously_pruned_near_texture_from_a_lod_namespace_archive() {
    let world = world(true);
    let atlas = "textures/lod/shared.dds";
    let shape = dummy_content::nif::static_shape(&dummy_content::nif::StaticShape {
        name: "NearMeshUsesLodTexture",
        positions: &[[0., 0., 0.], [100., 0., 0.], [0., 100., 0.]],
        normals: &[[0., 0., 1.]; 3],
        uvs: &[[0., 0.], [1., 0.], [0., 1.]],
        indices: &[[0, 1, 2]],
        diffuse: atlas,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    fs::write(world.data.join(layout::GENERATED_MODEL_PATH), shape).unwrap();
    convert(config(&world, LodMode::Baked), PostPublish::FIXTURE)
        .await
        .unwrap();
    let prior = ConversionManifest::load(&world.output.join("conversion-manifest.json")).unwrap();
    assert!(
        prior
            .pruned_texture_references
            .contains_key("meshes/generated.glb")
    );
    let bytes = fs::read(world.data.join(layout::GENERATED_DIFFUSE_PATH)).unwrap();
    fs::write(
        world.data.join("Skyrim - Textures.bsa"),
        bsa::v105(
            &[Entry {
                name: atlas,
                data: &bytes,
            }],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    convert(config(&world, LodMode::Cluster), PostPublish::FIXTURE)
        .await
        .unwrap();
    let manifest =
        ConversionManifest::load(&world.output.join("conversion-manifest.json")).unwrap();
    assert!(world.output.join(&manifest.entries[atlas].output).is_file());
    assert!(
        !manifest
            .pruned_texture_references
            .contains_key("meshes/generated.glb")
    );
    assert!(
        MeshConverter::glb_texture_uris(&world.output.join("meshes/generated.glb"))
            .unwrap()
            .iter()
            .any(|uri| uri.contains("/lod/shared.") && uri.ends_with(".ktx2"))
    );
    assert_accepted_overlay(&world.output);
    let mut rebuild = config(&world, LodMode::Cluster);
    rebuild.output_dir = world.output.with_file_name("rebuilt-shared");
    let rebuilt = rebuild.output_dir.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    AssetPipeline::rebuild_metadata_with_post_publish(
        rebuild,
        &world.output,
        sender,
        PostPublish::FIXTURE,
    )
    .await
    .unwrap();
    drain.await.unwrap();
    let manifest = ConversionManifest::load(&rebuilt.join("conversion-manifest.json")).unwrap();
    assert!(rebuilt.join(&manifest.entries[atlas].output).is_file());
    assert_accepted_overlay(&rebuilt);
}

/// Reader migration must retain verified textures used only by the rebuilt record graph.
#[tokio::test]
async fn cluster_metadata_reader_migration_retains_current_record_lod_texture_and_alias() {
    use crate::config::RecordReader;
    use dummy_content::inhouse_perks as native;

    const TEXTURE_SET_ID: u32 = 0x809;
    const TEXTURE: &str = "textures/lod/café.dds";
    const OUTPUT: &str = "textures/lod/café.ktx2";
    const ALIAS: &str = "textures/lod/café.opensky-srgb.opensky-wrap0.ktx2";
    const BAKED_ONLY: &str = "textures/lod/baked_only.dds";

    let world = world(true);
    let plugin = world.data.join("Skyrim.esm");
    let mut bytes = fs::read(&plugin).unwrap();
    let fields = [
        native::subrecord(b"EDID", &native::text("ReaderMigrationTexture")),
        native::subrecord(b"TX00", b"textures/lod/caf\xe9.dds\0"),
    ]
    .concat();
    bytes.extend(native::record(b"TXST", TEXTURE_SET_ID, 0, &fields));
    fs::write(&plugin, bytes).unwrap();
    fs::create_dir_all(world.data.join("textures/lod")).unwrap();
    for relative in [TEXTURE, BAKED_ONLY] {
        fs::copy(
            world.data.join(layout::GENERATED_DIFFUSE_PATH),
            world.data.join(relative),
        )
        .unwrap();
    }
    let mut source_config = config(&world, LodMode::Baked);
    source_config.record_reader = RecordReader::Legacy;
    let source_report = convert(source_config, PostPublish::FIXTURE).await.unwrap();
    assert!(source_report.complete, "{:?}", source_report.warnings);
    assert!(source_report.lod_chunks > 0);
    let texture_path = |root: &Path| -> String {
        Connection::open_with_flags(
            root.join("skyrim_world.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT diffuse_path FROM texture_sets WHERE id=?1",
            [TEXTURE_SET_ID],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(texture_path(&world.output), "textures/lod/caf\u{fffd}.dds");
    let source_manifest =
        ConversionManifest::load(&world.output.join("conversion-manifest.json")).unwrap();
    assert!(source_manifest.entries.contains_key(BAKED_ONLY));
    let texture = &source_manifest.entries[TEXTURE];
    assert_eq!(texture.output, OUTPUT);
    let verified_bytes = fs::read(world.output.join(OUTPUT)).unwrap();
    assert_eq!(hash_bytes(&verified_bytes), texture.output_hash);
    // Sampler aliases share the manifest-backed texture's exact bytes and hash authority.
    fs::write(world.output.join(ALIAS), &verified_bytes).unwrap();
    let snapshot = || -> BTreeMap<PathBuf, String> {
        discover(&world.output)
            .unwrap()
            .into_iter()
            .map(|path| {
                (
                    path.strip_prefix(&world.output).unwrap().to_path_buf(),
                    hash_file(&path).unwrap(),
                )
            })
            .collect()
    };
    let source_before = snapshot();
    let mut rebuild = config(&world, LodMode::Cluster);
    rebuild.record_reader = RecordReader::Inhouse;
    rebuild.output_dir = world.output.with_file_name("rebuilt-reader-migration");
    let rebuilt = rebuild.output_dir.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::rebuild_metadata_with_post_publish(
        rebuild,
        &world.output,
        sender,
        PostPublish::FIXTURE,
    )
    .await
    .unwrap();
    drain.await.unwrap();

    assert!(report.complete, "{:?}", report.warnings);
    assert_eq!(texture_path(&rebuilt), TEXTURE);
    let retained = ConversionManifest::load(&rebuilt.join("conversion-manifest.json")).unwrap();
    assert_eq!(retained.entries[TEXTURE].output_hash, texture.output_hash);
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(rebuilt.join("metadata-rebuild.json")).unwrap()).unwrap();
    for relative in [OUTPUT, ALIAS] {
        assert_eq!(fs::read(rebuilt.join(relative)).unwrap(), verified_bytes);
        assert!(report.artifacts.contains(&PathBuf::from(relative)));
        assert_eq!(
            provenance["retained_asset_hashes"][relative],
            texture.output_hash
        );
    }
    assert_eq!(provenance["record_reader"]["mode"], "inhouse");
    assert_eq!(provenance["retained_asset_record_reader"]["mode"], "legacy");
    assert_eq!(report.lod_chunks, 0);
    assert!(!rebuilt.join("lod").exists());
    assert!(!rebuilt.join("textures/lod/baked_only.ktx2").exists());
    assert!(!retained.entries.contains_key(BAKED_ONLY));
    assert_accepted_overlay(&rebuilt);
    assert_eq!(
        snapshot(),
        source_before,
        "metadata rebuild changed the source package"
    );
}

static PROBES: AtomicUsize = AtomicUsize::new(0);
static DERIVE_CALLS: AtomicUsize = AtomicUsize::new(0);
static DERIVE_SAW_FAILED_PROBE: AtomicBool = AtomicBool::new(false);

fn counting_failed_probe(config: &PipelineConfig) -> Option<ProbeResult> {
    PROBES.fetch_add(1, Ordering::SeqCst);
    crate::cluster::derive_requested(config).then(|| ProbeResult {
        passed: false,
        missing: vec!["synthetic missing feature".into()],
        backend: "synthetic-no-device".into(),
        adapter_name: "CPU fixture: probe failed".into(),
        json: serde_json::json!({"test": true}),
    })
}

fn checking_derive(
    options: &DeriveOptions,
    probe: Option<ProbeResult>,
) -> color_eyre::Result<DeriveReport> {
    DERIVE_CALLS.fetch_add(1, Ordering::SeqCst);
    DERIVE_SAW_FAILED_PROBE.store(
        probe.as_ref().is_some_and(|result| !result.passed),
        Ordering::SeqCst,
    );
    derive_extensions::run_fixture_with_probe(options, probe)
}

#[tokio::test]
async fn auto_derives_even_after_a_failed_probe_and_asks_the_adapter_once() {
    let world = world(true);
    let hooks = PostPublish {
        probe: counting_failed_probe,
        derive: checking_derive,
    };
    let report = convert(config(&world, LodMode::Auto), hooks).await.unwrap();
    let stage = report.cluster.as_ref().unwrap();
    assert_eq!(stage.mode, "cluster");
    assert_eq!(stage.probe.as_ref().unwrap()["forced"], true);
    assert_eq!(
        PROBES.load(Ordering::SeqCst),
        1,
        "one adapter probe per run"
    );
    assert_eq!(DERIVE_CALLS.load(Ordering::SeqCst), 1);
    assert!(
        DERIVE_SAW_FAILED_PROBE.load(Ordering::SeqCst),
        "the derivation receives the run's own probe instead of probing again"
    );
    assert_clusterless_base(&world.output);
    assert_accepted_overlay(&world.output);
}

#[tokio::test]
async fn baked_and_no_lod_do_no_cluster_work() {
    let hooks = PostPublish {
        probe: crate::cluster::fixture_probe,
        derive: |_, _| panic!("a baked or --no-lod conversion must not derive"),
    };
    for (mode, no_lod) in [(LodMode::Baked, false), (LodMode::Auto, true)] {
        let world = world(true);
        let mut config = config(&world, mode);
        config.no_lod = no_lod;
        assert!(crate::cluster::fixture_probe(&config).is_none());
        let report = convert(config, hooks).await.unwrap();
        assert!(!report.notices.iter().any(|n| n.contains("cluster overlay")));
        assert_clusterless_base(&world.output);
        assert!(!world.output.join("derived").exists());
    }
}

#[tokio::test]
async fn derive_failure_keeps_the_published_base_and_names_the_exact_retry() {
    let world = world(true);
    let config = config(&world, LodMode::Auto);
    let jobs = config.cpu_jobs;
    let failure = convert(config, FAILING).await.unwrap_err();
    let text = format!("{:#}", failure.error);
    assert!(text.contains("injected derive failure"), "{text}");
    assert!(text.contains("published and valid"), "{text}");
    assert!(text.contains("not a resumable conversion"), "{text}");
    assert!(
        text.contains(&format!(
            "' --triangle-threshold {DEFAULT_TRIANGLE_THRESHOLD} --jobs {jobs}"
        )),
        "{text}"
    );
    assert!(!text.contains("--terrain"), "{text}");
    let retry_base = quoted_after(&text, "converter derive-clusters '");
    assert_eq!(
        fs::canonicalize(retry_base).unwrap(),
        fs::canonicalize(&world.output).unwrap()
    );
    assert!(failure.staging.is_none(), "the conversion is not resumable");
    assert!(!failure.cancelled);
    // No staging is kept when the retry does not need it; the base is intact and has no overlay.
    no_staging_beside(&world.output);
    assert_clusterless_base(&world.output);
    assert!(!world.output.join("derived").exists());
    let check = check_output(&world.output, CheckMode::Full, |_, _| {}).unwrap();
    assert!(
        check
            .problems
            .iter()
            .all(|problem| matches!(problem, CheckProblem::Incomplete)),
        "{:?}",
        check.problems
    );
    // The retry the message names works on that base.
    derive_extensions::run_fixture(&DeriveOptions {
        source: world.output.clone(),
        label: "default".into(),
        replace: false,
        keep_failed_staging: false,
        terrain: false,
        terrain_sources: None,
        jobs,
        triangle_threshold: DEFAULT_TRIANGLE_THRESHOLD,
        include_trees: false,
    })
    .unwrap();
    assert_accepted_overlay(&world.output);
}

/// Folders beside the output that hold detached raw terrain textures.
fn terrain_source_folders_beside(output: &Path) -> Vec<PathBuf> {
    fs::read_dir(output.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".terrain-sources-")
        })
        .collect()
}

#[tokio::test]
async fn terrain_derive_failure_keeps_only_the_raw_vfs_in_a_folder_a_resume_cannot_find() {
    let world = world(true);
    let mut config = config(&world, LodMode::Cluster);
    config.cluster_terrain = true;
    config.cluster_triangle_threshold = 2048;
    let failure = convert(config, FAILING).await.unwrap_err();
    let text = format!("{:#}", failure.error);
    assert!(text.contains("injected derive failure"), "{text}");
    assert!(
        text.contains("--triangle-threshold 1 --jobs 2 --terrain --terrain-sources '"),
        "{text}"
    );
    let raw = quoted_after(&text, "--terrain-sources '");
    assert!(raw.ends_with("vfs") && raw.is_dir(), "{text}");
    assert!(
        raw.read_dir().unwrap().next().is_some(),
        "the kept raw terrain textures are empty"
    );
    let folder = raw.parent().unwrap();
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.contains(".terrain-sources-"), "{name}");
    assert!(!name.contains(".staging-"), "{name}");
    assert_eq!(
        fs::canonicalize(folder.parent().unwrap()).unwrap(),
        fs::canonicalize(world.output.parent().unwrap()).unwrap()
    );
    assert!(folder.join("OWNER.txt").is_file());
    assert_eq!(
        terrain_source_folders_beside(&world.output)
            .iter()
            .map(|path| fs::canonicalize(path).unwrap())
            .collect::<Vec<_>>(),
        vec![fs::canonicalize(folder).unwrap()]
    );
    assert!(
        text.contains(&format!("kept in {}", folder.display())),
        "{text}"
    );
    assert!(text.contains("delete that folder"), "{text}");
    assert!(failure.staging.is_none(), "the conversion is not resumable");
    // The conversion staging goes whole: nothing a resume could pick up is left behind.
    no_staging_beside(&world.output);
    assert_eq!(crate::find_resumable_staging(&world.output), None);
    assert!(
        !world.output.join("vfs").exists(),
        "the runtime base never holds the raw terrain textures"
    );
    assert_clusterless_base(&world.output);
}

static TERRAIN_SOURCES_SEEN: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Checks the raw root is present while deriving, then derives the actual requested package.
fn terrain_sources_checking_derive(
    options: &DeriveOptions,
    probe: Option<ProbeResult>,
) -> color_eyre::Result<DeriveReport> {
    let sources = options.terrain_sources.clone().expect("raw terrain root");
    assert!(options.terrain);
    assert!(sources.is_dir(), "the raw root is gone before the derive");
    assert!(sources.read_dir().unwrap().next().is_some());
    *TERRAIN_SOURCES_SEEN.lock().unwrap() = Some(sources);
    derive_extensions::run_fixture_with_probe(options, probe)
}

#[tokio::test]
async fn successful_terrain_derive_consumes_the_raw_sources_and_the_staging() {
    let world = world(true);
    let mut config = config(&world, LodMode::Cluster);
    config.cluster_terrain = true;
    let hooks = PostPublish {
        probe: crate::cluster::fixture_probe,
        derive: terrain_sources_checking_derive,
    };
    convert(config, hooks).await.unwrap();
    let sources = TERRAIN_SOURCES_SEEN.lock().unwrap().take().unwrap();
    assert!(sources.ends_with("vfs"));
    assert!(
        sources
            .parent()
            .unwrap()
            .to_string_lossy()
            .contains(".terrain-sources-")
    );
    assert!(
        !sources.parent().unwrap().exists(),
        "the raw terrain textures outlive the derive that consumed them"
    );
    assert!(terrain_source_folders_beside(&world.output).is_empty());
    no_staging_beside(&world.output);
    assert_accepted_overlay(&world.output);
    let metadata_path = sole_overlay(&world.output).join("overlay.json");
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(metadata_path).unwrap()).unwrap();
    assert!(metadata.get("terrain_sources").is_none(), "{metadata:#}");
    fn assert_portable_strings(value: &serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                assert!(
                    !Path::new(text).is_absolute(),
                    "host path in overlay: {text}"
                );
            }
            serde_json::Value::Array(values) => values.iter().for_each(assert_portable_strings),
            serde_json::Value::Object(values) => values.values().for_each(assert_portable_strings),
            _ => {}
        }
    }
    assert_portable_strings(&metadata);
}

#[tokio::test]
async fn a_non_terrain_derive_failure_leaves_no_raw_sources_folder() {
    let world = world(true);
    convert(config(&world, LodMode::Auto), FAILING)
        .await
        .unwrap_err();
    assert!(terrain_source_folders_beside(&world.output).is_empty());
    no_staging_beside(&world.output);
}

#[tokio::test]
async fn unsupported_cluster_requests_fail_before_any_work() {
    let world = world(false);
    let mut baked_terrain = config(&world, LodMode::Baked);
    baked_terrain.cluster_terrain = true;
    let failure = convert(baked_terrain, PostPublish::FIXTURE)
        .await
        .unwrap_err();
    assert!(
        failure.to_string().contains("--cluster-terrain requires"),
        "{failure}"
    );
    let mut no_lod = config(&world, LodMode::Cluster);
    no_lod.no_lod = true;
    let failure = convert(no_lod, PostPublish::FIXTURE).await.unwrap_err();
    assert!(
        failure.to_string().contains("--no-lod contradicts"),
        "{failure}"
    );
    let mut extract_all = config(&world, LodMode::Cluster);
    extract_all.extract_all_archive_files = true;
    let failure = convert(extract_all, PostPublish::FIXTURE)
        .await
        .unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("--extract-all-archive-files contradicts"),
        "{failure}"
    );
    assert!(!world.output.exists());
}

#[tokio::test]
async fn metadata_rebuild_publishes_a_clusterless_base_then_derives_the_overlay() {
    let world = world(true);
    convert(config(&world, LodMode::Baked), PostPublish::FIXTURE)
        .await
        .unwrap();
    let rebuild = |name: &str, hooks: PostPublish| {
        let mut config = PipelineConfig::new(&world.data, world.output.with_file_name(name));
        config.lod_mode = LodMode::Cluster;
        config.cpu_jobs = 2;
        let source = world.output.clone();
        async move {
            let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
            let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
            let result =
                AssetPipeline::rebuild_metadata_with_post_publish(config, &source, sender, hooks)
                    .await;
            drain.await.unwrap();
            result
        }
    };
    let rebuilt = world.output.with_file_name("rebuilt");
    let report = rebuild("rebuilt", PostPublish::FIXTURE).await.unwrap();
    assert!(report.cluster.is_some());
    assert_clusterless_base(&rebuilt);
    assert_accepted_overlay(&rebuilt);
    assert!(
        !world.output.join("derived").exists(),
        "the source is untouched"
    );

    let error = rebuild("rebuilt-failed", FAILING).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("injected derive failure"), "{text}");
    assert!(text.contains("converter derive-clusters '"), "{text}");
    let failed = world.output.with_file_name("rebuilt-failed");
    assert_clusterless_base(&failed);
    assert!(!failed.join("derived").exists());
}

#[tokio::test]
async fn invalid_cluster_threshold_never_starts_base_conversion_or_metadata_rebuild() {
    for mode in [LodMode::Cluster, LodMode::Auto] {
        let world = world(false);
        let mut settings = config(&world, mode);
        settings.cluster_triangle_threshold = 0;
        let error = convert(settings.clone(), PostPublish::FIXTURE)
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("threshold"));
        assert!(!world.output.exists());
        assert!(!settings.ingestion_cache_dir().exists());
        no_staging_beside(&world.output);

        let missing_source = world.data.join("not-a-converted-base");
        let (sender, _receiver) = tokio::sync::mpsc::channel(8);
        let error = AssetPipeline::rebuild_metadata_with_post_publish(
            settings,
            &missing_source,
            sender,
            PostPublish::FIXTURE,
        )
        .await
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("threshold"), "{error:#}");
        assert!(!world.output.exists());
        no_staging_beside(&world.output);
    }
}
