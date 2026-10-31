//! Terrain metadata publication, dependent on the final shared geometry extension.

use super::{
    publish::{BaseFinal, PublishContext},
    terrain_atlas,
    terrain_stage::{TERRAIN_RECIPE_VERSION, TERRAIN_TABLES_VERSION},
};
use crate::{
    cache::{hash_bytes, hash_file},
    lod::albedo::TerrainAtlas,
};
use color_eyre::{
    Result,
    eyre::{ensure, eyre},
};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use shared::{
    cluster::{BLOCK_SIDE_CELLS, PAYLOAD_FORMAT_VERSION, canonical_json, parse_hash},
    extensions::{CLUSTER_TERRAIN, CLUSTERS, ExtensionEntry, ExtensionHasher, ExtensionOwns},
    lod::LodTier,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

pub const MANIFEST: &str = "cluster-terrain-manifest.json";
pub const VERSION: u32 = 1;
pub const TABLES: [&str; 3] = [
    "cluster_terrain",
    "cluster_terrain_heights",
    "cluster_terrain_build",
];

/// A snapshot made by the terrain stage after source verification and its row transaction.
/// Kept in memory; callers cannot use an old dataset's rows to manufacture certification.
#[derive(Debug, Clone)]
pub struct TerrainProof {
    core_identity: String,
    cache_hash: String,
    builder: String,
    tables_hash: String,
    blocks: Vec<BlockProof>,
}

#[derive(Debug, Clone, Serialize)]
struct BlockProof {
    worldspace: u32,
    anchor: [i32; 2],
    side_cells: u32,
    /// The stage's source-recipe input key, also carried as the generated mesh identity.
    input_key: String,
    source_cells: Vec<[i32; 2]>,
    atlas_path: String,
    atlas_sha256: String,
    atlas_side: u32,
}

/// The cluster store, read-only (for an overlay with the core attached as `core`).
fn connection(context: &PublishContext<'_>) -> Result<Connection> {
    context.open_store_read_only()
}

fn tables_hash(connection: &Connection) -> Result<String> {
    let mut hasher = ExtensionHasher::new(CLUSTER_TERRAIN, VERSION);
    for table in TABLES {
        hasher.add_table(connection, table)?;
    }
    Ok(hasher.finish())
}

impl TerrainProof {
    /// Only the source-reading stage calls this, after verifying its cache/texture inputs.
    pub(super) fn capture(
        context: &PublishContext<'_>,
        cache_hash: &str,
        builder: &str,
    ) -> Result<Self> {
        ensure!(
            parse_hash(cache_hash).is_some() && parse_hash(builder).is_some(),
            "invalid terrain source identity"
        );
        let connection = connection(context)?;
        let core_identity = context
            .core_identity()?
            .ok_or_else(|| eyre!("terrain proof requires a current core build identity"))?;
        let mut statement = connection.prepare(
            "SELECT mesh_hash, worldspace_id, anchor_x, anchor_y, side_cells, atlas_path, source_cells
             FROM cluster_terrain ORDER BY worldspace_id, anchor_x, anchor_y, mesh_hash"
        )?;
        let mut rows = statement.query([])?;
        let mut blocks = Vec::new();
        let mut keys = BTreeSet::new();
        while let Some(row) = rows.next()? {
            let input_key: String = row.get(0)?;
            let worldspace: u32 = row.get(1)?;
            let anchor = [row.get::<_, i32>(2)?, row.get::<_, i32>(3)?];
            let side_cells: u32 = row.get(4)?;
            let atlas_path: String = row.get(5)?;
            let source_cells: Vec<[i32; 2]> = serde_json::from_str(&row.get::<_, String>(6)?)?;
            ensure!(
                parse_hash(&input_key).is_some(),
                "invalid terrain input key"
            );
            ensure!(
                side_cells == BLOCK_SIDE_CELLS,
                "unsupported terrain block side"
            );
            ensure!(keys.insert((worldspace, anchor)), "duplicate terrain block");
            ensure!(
                !source_cells.is_empty()
                    && source_cells.len() <= (side_cells * side_cells) as usize,
                "invalid terrain source-cell count"
            );
            ensure!(
                source_cells.windows(2).all(|pair| pair[0] < pair[1]),
                "terrain source cells are not unique and ordered"
            );
            for cell in &source_cells {
                ensure!(
                    (0..i64::from(side_cells))
                        .contains(&(i64::from(cell[0]) - i64::from(anchor[0])))
                        && (0..i64::from(side_cells))
                            .contains(&(i64::from(cell[1]) - i64::from(anchor[1]))),
                    "terrain source cell is outside its block"
                );
            }
            let atlas_side = u32::try_from(TerrainAtlas::size_for_cells(
                LodTier::Tier16,
                source_cells.len(),
            )?)?;
            let atlas_bytes = fs::read(context.staging().join(&atlas_path))?;
            terrain_atlas::validate(&atlas_bytes, atlas_side)?;
            blocks.push(BlockProof {
                worldspace,
                anchor,
                side_cells,
                input_key,
                source_cells,
                atlas_sha256: hash_bytes(&atlas_bytes),
                atlas_path,
                atlas_side,
            });
        }
        ensure!(!blocks.is_empty(), "cannot certify empty terrain metadata");
        Ok(Self {
            core_identity,
            cache_hash: cache_hash.to_owned(),
            builder: builder.to_owned(),
            tables_hash: tables_hash(&connection)?,
            blocks,
        })
    }

    /// Rechecks the stage's expected metadata and assets before base finalization/publication.
    pub fn validate(&self, context: &PublishContext<'_>) -> Result<()> {
        let connection = connection(context)?;
        ensure!(
            context.core_identity()?.as_deref() == Some(self.core_identity.as_str()),
            "terrain proof belongs to another core build"
        );
        let (version, recorded): (u32, String) = connection.query_row(
            "SELECT version, cell_cache_hash FROM cluster_terrain_build WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            version == TERRAIN_TABLES_VERSION && recorded == self.cache_hash,
            "terrain cache provenance changed"
        );
        ensure!(
            hash_file(&context.core_root().join("cell_cache.rkyv"))? == self.cache_hash,
            "terrain does not match the current core cell cache"
        );
        let builder: String = connection.query_row(
            "SELECT builder_identity FROM cluster_build WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        ensure!(builder == self.builder, "terrain builder identity changed");
        ensure!(
            tables_hash(&connection)? == self.tables_hash,
            "terrain metadata changed after source verification"
        );
        for block in &self.blocks {
            // Capture decoded these exact bytes against the independently derived recipe.
            // Equality to that digest preserves the proof without decoding again.
            ensure!(
                hash_file(&context.staging().join(&block.atlas_path))? == block.atlas_sha256,
                "terrain atlas changed after source verification"
            );
        }
        Ok(())
    }
}

/// Called after object AND terrain geometry are complete and base finalization returned its hash.
pub fn finalize(
    context: &PublishContext<'_>,
    base: &BaseFinal,
    proof: &TerrainProof,
) -> Result<ExtensionEntry> {
    proof.validate(context)?;
    let connection = connection(context)?;
    let core = context
        .core_identity()?
        .ok_or_else(|| eyre!("terrain publication requires a core build identity"))?;
    ensure!(
        base.extension.id == CLUSTERS
            && base.extension.version == 1
            && base.extension.built_from == core
            && parse_hash(&base.extension.sha256).is_some(),
        "terrain dependency is not the final compatible base extension"
    );
    let inputs = json!({"recipe":TERRAIN_RECIPE_VERSION,"builder":proof.builder,
        "cell_cache":proof.cache_hash,"blocks":proof.blocks});
    let bytes = canonical_json(&json!({
        "version":VERSION,"payload_format_version":PAYLOAD_FORMAT_VERSION,
        "terrain_tables_version":TERRAIN_TABLES_VERSION,"terrain_recipe_version":TERRAIN_RECIPE_VERSION,
        "built_from":core,"builder_identity":proof.builder,
        "cell_cache_sha256":proof.cache_hash,"input_identity":hash_bytes(&canonical_json(&inputs)),
        "requires":[{"id":CLUSTERS,"version":1,"sha256":base.extension.sha256}],
        "blocks":proof.blocks
    }));
    let mut hasher = ExtensionHasher::new(CLUSTER_TERRAIN, VERSION);
    for table in TABLES {
        hasher.add_table(&connection, table)?;
    }
    hasher.add_file(MANIFEST, &bytes);
    let extension = ExtensionEntry {
        id: CLUSTER_TERRAIN.to_owned(),
        version: VERSION,
        owns: ExtensionOwns {
            tables: TABLES.iter().map(|table| (*table).to_owned()).collect(),
            files: vec![MANIFEST.to_owned()],
            ..Default::default()
        },
        sha256: hasher.finish(),
        built_from: core,
        stats: BTreeMap::from([("blocks".to_owned(), proof.blocks.len() as u64)]),
    };
    context.replace_file(&context.staging().join(MANIFEST), &bytes)?;
    Ok(extension)
}

#[cfg(test)]
mod tests {
    // Invented-data joint publication proof. No game installation or GPU is used.

    use super::*;
    use crate::{
        cache::{CacheEntry, ConversionManifest},
        cluster::{
            LodMode,
            authority::CoreAuthority,
            probe::ProbeResult,
            publish::{PublishContext, finalize_base},
            stage::{StageSettings, run_stage},
            terrain_stage,
            test_support::glb,
        },
        esm::{exporter::create_tables, extractors::serialize_subrecords},
    };
    use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
    use rusqlite::{OpenFlags, params};
    use shared::{
        CachedLand, CellCache, TerrainLayer,
        asset_lock::AssetLock,
        cluster::read_mci,
        extensions::{ExtensionEntry, ExtensionHasher, ownership_problems},
    };
    use std::path::Path;

    const WORLD: u32 = 0x52;
    const CORE: &str = "c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7";
    const ORIGIN: [i32; 2] = [-24, 40];
    const ANCHOR: [i32; 2] = [8, 24];
    const CELLS: [(u32, i32, i32); 3] = [(0x31, 8, 24), (0x32, 9, 24), (0x33, 8, 25)];

    /// A real, eligible 4x4 vertex grid (18 triangles), rather than a hand-built MCL.
    fn object_glb() -> Vec<u8> {
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut uvs = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                positions.extend([x as f32, y as f32, 0.0]);
                normals.extend([0.0f32, 0.0, 1.0]);
                uvs.extend([x as f32 / 3.0, y as f32 / 3.0]);
            }
        }
        let mut indices = Vec::<u16>::new();
        for y in 0..3u16 {
            for x in 0..3u16 {
                let a = y * 4 + x;
                indices.extend([a, a + 1, a + 4, a + 1, a + 5, a + 4]);
            }
        }
        let mut binary = Vec::new();
        for value in positions.iter().chain(&normals).chain(&uvs) {
            binary.extend(value.to_le_bytes());
        }
        for value in &indices {
            binary.extend(value.to_le_bytes());
        }
        glb(
            &json!({
                "asset":{"version":"2.0"}, "buffers":[{"byteLength":binary.len()}],
                "bufferViews":[
                    {"buffer":0,"byteOffset":0,"byteLength":192},
                    {"buffer":0,"byteOffset":192,"byteLength":192},
                    {"buffer":0,"byteOffset":384,"byteLength":128},
                    {"buffer":0,"byteOffset":512,"byteLength":indices.len()*2}
                ],
                "accessors":[
                    {"bufferView":0,"componentType":5126,"count":16,"type":"VEC3","min":[0,0,0],"max":[3,3,0]},
                    {"bufferView":1,"componentType":5126,"count":16,"type":"VEC3"},
                    {"bufferView":2,"componentType":5126,"count":16,"type":"VEC2"},
                    {"bufferView":3,"componentType":5123,"count":indices.len(),"type":"SCALAR"}
                ],
                "materials":[{"extras":{"openSkyrim":{"shaderFamily":"lighting","lightingShaderType":"default"}}}],
                "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":3,"material":0}]}],
                "nodes":[{"mesh":0}], "scene":0, "scenes":[{"nodes":[0]}]
            }),
            &binary,
        )
    }

    fn settings() -> StageSettings {
        StageSettings {
            mode: LodMode::Cluster,
            triangle_threshold: 1,

            jobs: 1,
            assets_root: None,

            read_database: None,
            area: None,
            child_worldspaces: Vec::new(),
            include_trees: false,

            map_references: true,
        }
    }

    /// Inputs and staging share a directory in this fixture; all source reads still use the real
    /// stages. The destination is separate, so publication exercises the rename under its lock.
    fn seed(staging: &Path) -> ConversionManifest {
        fs::create_dir_all(staging.join("meshes/invented")).unwrap();
        fs::create_dir_all(staging.join("vfs/textures/landscape")).unwrap();
        let bytes = object_glb();
        let model_path = "meshes/invented/stone.glb";
        fs::write(staging.join(model_path), &bytes).unwrap();
        let mut manifest = ConversionManifest {
            schema_version: crate::cache::CONVERTER_SCHEMA_VERSION,
            ..Default::default()
        };
        manifest.entries.insert(
            model_path.to_owned(),
            CacheEntry {
                animation_diagnostics: None,
                source_hash: String::new(),
                output: model_path.to_owned(),
                output_size: bytes.len() as u64,
                output_hash: hash_bytes(&bytes),
            },
        );
        let db = Connection::open(staging.join("skyrim_world.db")).unwrap();
        create_tables(&db).unwrap();
        db.execute("INSERT INTO lod_build VALUES (1, ?1)", [CORE])
            .unwrap();
        db.execute(
            "INSERT INTO worldspaces(id,editor_id,parent_world,flags,lod_origin_x,lod_origin_y) VALUES (?1,'Invented',NULL,0,?2,?3)",
            params![WORLD, ORIGIN[0], ORIGIN[1]],
        )
        .unwrap();
        let clear = serialize_subrecords(&[(b"EDID".to_vec(), b"invented\0".to_vec())]);
        db.execute(
            "INSERT INTO records(form_id,record_type,load_order,data) VALUES (0x11,'STAT',0,?1)",
            [&clear],
        )
        .unwrap();
        db.execute("INSERT INTO statics(id,editor_id,model_path,flags) VALUES (0x11,'InventedStone','Invented\\Stone.nif',0)", []).unwrap();
        db.execute("INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,data)
        VALUES (0x100,0x31,?1,0x11,1,?2,?3,20,0,0,0,1,0,?4)",
        params![WORLD, 8.0f32 * 4096.0 + 100.0, 24.0f32 * 4096.0 + 100.0, clear]).unwrap();
        db.execute(
            "INSERT INTO texture_sets(id,diffuse_path) VALUES (0x72,'Landscape\\Invented.dds')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO landscape_textures(id,texture_set_id) VALUES (0x71,0x72)",
            [],
        )
        .unwrap();
        let mut lands = Vec::new();
        for (cell_id, x, y) in CELLS {
            db.execute(
                "INSERT INTO cells(id,worldspace_id,grid_x,grid_y,flags) VALUES (?1,?2,?3,?4,0)",
                params![cell_id, WORLD, x, y],
            )
            .unwrap();
            db.execute(
                "INSERT INTO land(cell_id,heightmap) VALUES (?1,x'00')",
                [cell_id],
            )
            .unwrap();
            lands.push(CachedLand {
                cell_id,
                width: 33,
                height: 33,
                // One globally continuous slope; shared corners are exactly identical.
                heights: (0..33)
                    .flat_map(|row| {
                        (0..33).map(move |column| ((x * 32 + column) + (y * 32 + row)) as f32)
                    })
                    .collect(),
                normals: [0i8, 0, 127].repeat(33 * 33),
                vertex_colors: vec![255; 33 * 33 * 3],
                layers: (0..4)
                    .map(|quadrant| TerrainLayer {
                        texture_form_id: 0x71,
                        quadrant,
                        layer: 0,
                        is_base: true,
                        weights: Vec::new(),
                    })
                    .collect(),
                water_height: None,
                water_type_form_id: None,
            });
        }
        drop(db);
        let cache = CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: lands,
        };
        fs::write(
            staging.join("cell_cache.rkyv"),
            rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap(),
        )
        .unwrap();
        let mut dds = Dds::new_dxgi(NewDxgiParams {
            width: 4,
            height: 4,
            depth: None,
            format: DxgiFormat::R8G8B8A8_UNorm,
            mipmap_levels: Some(1),
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Straight,
        })
        .unwrap();
        dds.data.copy_from_slice(&[100, 150, 70, 255].repeat(16));
        let mut dds_bytes = Vec::new();
        dds.write(&mut dds_bytes).unwrap();
        let dds_hash = hash_bytes(&dds_bytes);
        fs::write(
            staging.join("vfs/textures/landscape/invented.dds"),
            &dds_bytes,
        )
        .unwrap();
        manifest.entries.insert(
            "textures/landscape/invented.dds".into(),
            CacheEntry {
                animation_diagnostics: None,
                source_hash: format!("{dds_hash}:texture-encoding:ColorSrgb"),
                output: "vfs/textures/landscape/invented.dds".into(),
                output_size: dds_bytes.len() as u64,
                output_hash: dds_hash.clone(),
            },
        );
        fs::write(
            staging.join("conversion-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            staging.join("lod-manifest.json"),
            serde_json::to_vec(&json!({
                "build_identity": CORE,
                "converter_schema": crate::cache::CONVERTER_SCHEMA_VERSION,
                "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
                "chunks": 0,
                "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
                "terrain_sources": {"textures/landscape/invented.dds": dds_hash},
            }))
            .unwrap(),
        )
        .unwrap();
        manifest
    }

    fn stage(
        context: &PublishContext<'_>,
        manifest: &ConversionManifest,
    ) -> (TerrainProof, CoreAuthority) {
        let staging = context.staging();
        let settings = settings();
        let probe = ProbeResult {
            passed: true,
            missing: Vec::new(),
            backend: "synthetic".into(),
            adapter_name: "no GPU".into(),
            json: json!({"synthetic":true}),
        };
        let (report, _) = run_stage(
            &settings,
            Some(probe),
            staging,
            &staging.join("no-previous"),
            manifest,
        )
        .unwrap();
        let database = Connection::open(staging.join("skyrim_world.db")).unwrap();
        let mapped: i64 = database
            .query_row("SELECT count(*) FROM cluster_instance_map", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            mapped > 0,
            "the joint proof must contain actual object mappings"
        );
        drop(database);
        let terrain = terrain_stage::run_with_report(staging, context, None).unwrap();
        assert_eq!(terrain.blocks, 1);
        assert_eq!(terrain.cells, 3);
        let proof = terrain
            .publication
            .expect("real terrain stage must capture its proof");
        let authority = CoreAuthority::open(
            Connection::open_with_flags(
                staging.join("skyrim_world.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap(),
            staging,
            manifest,
            &settings,
            report.builder_identity,
        )
        .unwrap();
        (proof, authority)
    }

    fn enumerate_files(root: &Path, relative: &Path, files: &mut Vec<String>) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            let child = relative.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                enumerate_files(root, &child, files);
            } else {
                files.push(child.to_str().unwrap().replace('\\', "/"));
            }
        }
    }

    /// Enumerate actual geometry files, rather than trusting only the entry's declared file list.
    fn recompute(root: &Path, entry: &ExtensionEntry) -> String {
        let db = Connection::open_with_flags(
            root.join("skyrim_world.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut hasher = ExtensionHasher::new(&entry.id, entry.version);
        for table in &entry.owns.tables {
            hasher.add_table(&db, table).unwrap();
        }
        for index in &entry.owns.indexes {
            hasher.add_index(&db, index).unwrap();
        }
        let mut files = entry.owns.files.clone();
        if entry.id == CLUSTERS {
            files = vec!["cluster-manifest.json".to_owned()];
            enumerate_files(root, Path::new("clusters"), &mut files);
            files.sort();
            assert_eq!(entry.owns.files, ["cluster-manifest.json", "clusters/"]);
        }
        for file in files {
            hasher.add_file(&file, &fs::read(root.join(&file)).unwrap());
        }
        hasher.finish()
    }

    #[test]
    fn certified_terrain_uses_requested_jobs_instead_of_the_ambient_pool() {
        let ambient = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        for jobs in [1, 2] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("published");
            let staging = root.path().join("staging");
            let lock = AssetLock::acquire_exclusive(&destination).unwrap();
            let manifest = seed(&staging);
            let context = PublishContext::new(&lock, &destination, &staging).unwrap();
            let settings = StageSettings { jobs, ..settings() };
            let (objects, _) = run_stage(
                &settings,
                Some(synthetic_probe()),
                &staging,
                &staging.join("no-previous"),
                &manifest,
            )
            .unwrap();
            ambient.install(|| {
                crate::cluster::finalize_certified_extensions(
                    &settings,
                    &context,
                    &manifest,
                    &objects.builder_identity,
                    true,
                )
                .unwrap();
            });
            assert_eq!(
                terrain_stage::worker_pool_observer::take(&staging),
                BTreeSet::from([jobs])
            );
        }
    }

    #[test]
    fn publication_decodes_each_atlas_once_and_still_rejects_mutation() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("published");
        let staging = root.path().join("staging");
        let lock = AssetLock::acquire_exclusive(&destination).unwrap();
        let manifest = seed(&staging);
        let context = PublishContext::new(&lock, &destination, &staging).unwrap();
        terrain_atlas::PUBLICATION_DECODES.with(|count| count.set(0));
        let (proof, authority) = stage(&context, &manifest);
        proof.validate(&context).unwrap();
        let base = finalize_base(&context, Some(&authority)).unwrap();
        let first = finalize(&context, &base, &proof).unwrap();
        let manifest_bytes = fs::read(staging.join(MANIFEST)).unwrap();
        let repeated = finalize(&context, &base, &proof).unwrap();
        assert_eq!(first.sha256, repeated.sha256);
        assert_eq!(fs::read(staging.join(MANIFEST)).unwrap(), manifest_bytes);
        terrain_atlas::PUBLICATION_DECODES.with(|count| assert_eq!(count.get(), 1));
        let atlas = staging.join(&proof.blocks[0].atlas_path);
        let mut bytes = fs::read(&atlas).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&atlas, bytes).unwrap();
        assert!(proof.validate(&context).is_err());
        assert!(finalize(&context, &base, &proof).is_err());
    }

    #[test]
    fn capture_rejects_an_atlas_that_was_not_successfully_decoded() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("published");
        let staging = root.path().join("staging");
        let lock = AssetLock::acquire_exclusive(&destination).unwrap();
        let manifest = seed(&staging);
        let context = PublishContext::new(&lock, &destination, &staging).unwrap();
        let (proof, _authority) = stage(&context, &manifest);
        fs::write(
            staging.join(&proof.blocks[0].atlas_path),
            b"not a KTX2 atlas",
        )
        .unwrap();
        assert!(TerrainProof::capture(&context, &proof.cache_hash, &proof.builder).is_err());
    }

    #[test]
    fn real_object_and_three_land_cells_publish_with_one_owner_and_final_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("published");
        let staging = root.path().join("staging");
        // Retain this guard through all stages, both finalizers, envelope save and rename.
        let lock = AssetLock::acquire_exclusive(&destination).unwrap();
        let mut manifest = seed(&staging);
        let context = PublishContext::new(&lock, &destination, &staging).unwrap();
        let (proof, authority) = stage(&context, &manifest);
        proof.validate(&context).unwrap();
        let db = Connection::open(staging.join("skyrim_world.db")).unwrap();
        let (x, y, side, path): (i32, i32, u32, String) = db
            .query_row(
                "SELECT anchor_x,anchor_y,side_cells,instance_path FROM cluster_terrain",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(([x, y], side), (ANCHOR, 16));
        let payload_bytes = fs::read(staging.join(path)).unwrap();
        let payload = read_mci(&payload_bytes).unwrap();
        assert_eq!((payload.anchor_x, payload.anchor_y), (2, -1));
        drop(db);
        let base = finalize_base(&context, Some(&authority)).unwrap();
        let terrain = finalize(&context, &base, &proof).unwrap();
        assert_eq!(base.extension.built_from, CORE);
        assert_eq!(terrain.built_from, CORE);
        assert_eq!(terrain.owns.tables.len(), 3);
        assert_eq!(terrain.owns.files, [MANIFEST]);
        assert!(terrain.owns.indexes.is_empty());
        crate::extensions::refresh_manifest_with_finalized(
            &mut manifest,
            &staging,
            &[base.extension.clone(), terrain.clone()],
        )
        .unwrap();
        assert!(ownership_problems(&manifest.extensions).is_empty());
        let dependency: serde_json::Value =
            serde_json::from_slice(&fs::read(staging.join(MANIFEST)).unwrap()).unwrap();
        assert_eq!(
            dependency["requires"],
            json!([{"id":CLUSTERS,"version":1,"sha256":base.extension.sha256}])
        );
        assert_eq!(
            dependency["cell_cache_sha256"],
            hash_bytes(&fs::read(staging.join("cell_cache.rkyv")).unwrap())
        );
        manifest
            .save(&staging.join("conversion-manifest.json"))
            .unwrap();
        drop(authority);
        crate::pipeline::publish_new_directory(&staging, &destination, &lock).unwrap();
        assert_eq!(
            recompute(&destination, &base.extension),
            base.extension.sha256
        );
        assert_eq!(recompute(&destination, &terrain), terrain.sha256);
        assert!(!staging.exists());
        assert!(destination.join("conversion-manifest.json").is_file());
    }

    fn synthetic_probe() -> ProbeResult {
        ProbeResult {
            passed: true,
            missing: Vec::new(),
            backend: "synthetic".into(),
            adapter_name: "no GPU".into(),
            json: json!({"synthetic":true}),
        }
    }

    /// Path and SHA-256 of every file under `root`.
    fn snapshot(root: &Path) -> BTreeMap<String, String> {
        let mut files = Vec::new();
        enumerate_files(root, Path::new(""), &mut files);
        files
            .into_iter()
            .map(|file| {
                let hash = hash_bytes(&fs::read(root.join(&file)).unwrap());
                (file, hash)
            })
            .collect()
    }

    #[test]
    fn an_overlay_publishes_the_envelopes_of_a_full_copy_and_copies_no_core() {
        let root = tempfile::tempdir().unwrap();

        // Reference: today's full-copy layout, core and clusters in one folder.
        let full_destination = root.path().join("full-published");
        let full_staging = root.path().join("full-staging");
        let full_lock = AssetLock::acquire_exclusive(&full_destination).unwrap();
        let manifest = seed(&full_staging);
        let full_context =
            PublishContext::new(&full_lock, &full_destination, &full_staging).unwrap();
        let (report, _) = run_stage(
            &settings(),
            Some(synthetic_probe()),
            &full_staging,
            &full_staging.join("no-previous"),
            &manifest,
        )
        .unwrap();
        let (full, _) = crate::cluster::finalize_certified_extensions(
            &settings(),
            &full_context,
            &manifest,
            &report.builder_identity,
            true,
        )
        .unwrap();

        // The same data as a finished base conversion, and an overlay staged beside it.
        let base = root.path().join("base");
        let staging = root.path().join("overlay-staging");
        let destination = root.path().join("overlay");
        let manifest = seed(&base);
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("clusters.db"), b"").unwrap();
        let before = snapshot(&base);
        let lock = AssetLock::acquire_exclusive(&destination).unwrap();
        let context =
            PublishContext::new_overlay(&lock, &destination, &staging, &base, None).unwrap();
        let overlay_settings = StageSettings {
            assets_root: Some(base.clone()),
            read_database: Some(base.join("skyrim_world.db")),
            ..settings()
        };
        // The overlay stage reads the base in place: without it there is nothing to read.
        let detached = StageSettings {
            assets_root: None,
            ..overlay_settings.clone()
        };
        assert!(
            run_stage(
                &detached,
                Some(synthetic_probe()),
                &staging,
                &destination,
                &manifest,
            )
            .is_err()
        );
        let (report, _) = run_stage(
            &overlay_settings,
            Some(synthetic_probe()),
            &staging,
            &destination,
            &manifest,
        )
        .unwrap();
        let (overlay, _) = crate::cluster::finalize_certified_extensions(
            &overlay_settings,
            &context,
            &manifest,
            &report.builder_identity,
            true,
        )
        .unwrap();

        let describe = |entries: &[ExtensionEntry]| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry.id.clone(),
                        entry.sha256.clone(),
                        entry.built_from.clone(),
                        entry.stats.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(overlay.len(), 2);
        assert_eq!(
            describe(&overlay),
            describe(&full),
            "same data, same envelopes, either layout"
        );
        assert!(overlay.iter().all(|entry| entry.built_from == CORE));

        // Only cluster files were staged: no core database, GLB, texture, cache or manifest.
        let mut top: Vec<String> = fs::read_dir(&staging)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        top.sort();
        assert_eq!(
            top,
            [
                "cluster-manifest.json",
                "cluster-terrain-manifest.json",
                "clusters",
                "clusters.db"
            ]
        );
        assert_eq!(snapshot(&base), before, "the base is byte-identical");
        let core = Connection::open(base.join("skyrim_world.db")).unwrap();
        let cluster_tables: i64 = core
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'cluster_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cluster_tables, 0);
        let store = Connection::open(staging.join("clusters.db")).unwrap();
        let core_tables: i64 = store
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'cluster_%'
                 AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(core_tables, 0, "clusters.db carries no core table");
    }

    #[test]
    fn mutations_after_the_real_stage_cannot_mint_a_terrain_manifest() {
        // One fixture per independent attack; no mutation is accidentally masked by an earlier one.
        for attack in ["core", "cache", "source_cells", "heights", "atlas"] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("published");
            let staging = root.path().join("staging");
            let lock = AssetLock::acquire_exclusive(&destination).unwrap();
            let manifest = seed(&staging);
            let context = PublishContext::new(&lock, &destination, &staging).unwrap();
            let (proof, authority) = stage(&context, &manifest);
            // Final base is genuine and predates the attack, so terrain must reject stale proof even
            // when supplied a formerly-valid base entry rather than a fabricated test double.
            let base = finalize_base(&context, Some(&authority)).unwrap();
            drop(authority);
            let db = Connection::open(staging.join("skyrim_world.db")).unwrap();
            match attack {
                "core" => {
                    db.execute("UPDATE lod_build SET build_identity=?1", ["ab".repeat(32)])
                        .unwrap();
                }
                "cache" => {
                    let path = staging.join("cell_cache.rkyv");
                    let mut bytes = fs::read(&path).unwrap();
                    bytes[0] ^= 1;
                    fs::write(path, bytes).unwrap();
                }
                "source_cells" => {
                    db.execute("UPDATE cluster_terrain SET source_cells='[[8,24]]'", [])
                        .unwrap();
                }
                "heights" => {
                    db.execute("UPDATE cluster_terrain_heights SET heights=zeroblob(length(heights)) WHERE grid_x=8 AND grid_y=24",[]).unwrap();
                }
                "atlas" => {
                    let path: String = db
                        .query_row("SELECT atlas_path FROM cluster_terrain", [], |row| {
                            row.get(0)
                        })
                        .unwrap();
                    let path = staging.join(path);
                    let mut bytes = fs::read(&path).unwrap();
                    let last = bytes.len() - 1;
                    bytes[last] ^= 1;
                    fs::write(path, bytes).unwrap();
                }
                _ => unreachable!(),
            }
            drop(db);
            assert!(proof.validate(&context).is_err(), "attack {attack}");
            assert!(
                finalize(&context, &base, &proof).is_err(),
                "attack {attack}"
            );
            assert!(!staging.join(MANIFEST).exists(), "attack {attack}");
            assert!(!destination.exists(), "attack {attack}");
        }
    }
    #[test]
    fn certified_pipeline_terrain_reads_current_not_missing_or_stale_staged_sources() {
        for stale in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("published");
            let staging = root.path().join("staging");
            let lock = AssetLock::acquire_exclusive(&destination).unwrap();
            let manifest = seed(&staging);
            let path = staging.join("conversion-manifest.json");
            if stale {
                let mut old = manifest.clone();
                old.entries
                    .get_mut("textures/landscape/invented.dds")
                    .unwrap()
                    .source_hash = "ff".repeat(32);
                old.save(&path).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
            let context = PublishContext::new(&lock, &destination, &staging).unwrap();
            let settings = settings();
            let probe = ProbeResult {
                passed: true,
                missing: Vec::new(),
                backend: "synthetic".into(),
                adapter_name: "no GPU".into(),
                json: json!({"synthetic":true}),
            };
            let (objects, _) = run_stage(
                &settings,
                Some(probe),
                &staging,
                &staging.join("no-previous"),
                &manifest,
            )
            .unwrap();
            let (entries, _) = crate::cluster::finalize_certified_extensions(
                &settings,
                &context,
                &manifest,
                &objects.builder_identity,
                true,
            )
            .unwrap();
            assert_eq!(entries.len(), 2);
            assert!(entries.iter().any(|entry| entry.id == CLUSTERS));
            assert!(entries.iter().any(|entry| entry.id == CLUSTER_TERRAIN));
            assert!(staging.join(MANIFEST).is_file());
            let saved = ConversionManifest::load(&path).unwrap();
            assert_eq!(
                saved.entries["textures/landscape/invented.dds"].source_hash,
                manifest.entries["textures/landscape/invented.dds"].source_hash
            );
            assert!(!destination.exists());
        }
    }

    #[test]
    fn standalone_terrain_benchmark_uses_origins_but_cannot_certify_mapping() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("published");
        let staging = root.path().join("staging");
        let lock = AssetLock::acquire_exclusive(&destination).unwrap();
        let manifest = seed(&staging);
        // The benchmark intentionally has no certified producer/source manifests.
        fs::remove_file(staging.join("conversion-manifest.json")).unwrap();
        fs::remove_file(staging.join("lod-manifest.json")).unwrap();
        let context = PublishContext::new(&lock, &destination, &staging).unwrap();
        let mut settings = settings();
        settings.map_references = false;
        let probe = ProbeResult {
            passed: true,
            missing: Vec::new(),
            backend: "synthetic".into(),
            adapter_name: "no GPU".into(),
            json: json!({"synthetic":true}),
        };
        run_stage(
            &settings,
            Some(probe),
            &staging,
            &staging.join("no-previous"),
            &manifest,
        )
        .unwrap();
        let report = terrain_stage::run_benchmark_with_report(&staging, &context, None).unwrap();
        assert_eq!(report.blocks, 1);
        assert!(report.publication.is_none());
        assert!(crate::cluster::publish::finalize_benchmark(&context).is_err());
        assert!(
            !staging
                .join(crate::cluster::publish::CLUSTER_MANIFEST)
                .exists()
        );
        let origins = std::collections::HashMap::from([(WORLD, (ORIGIN[0], ORIGIN[1]))]);
        crate::cluster::publish::finalize_benchmark_with_origins(&context, &origins).unwrap();
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(staging.join(crate::cluster::publish::CLUSTER_MANIFEST)).unwrap(),
        )
        .unwrap();
        assert_eq!(value["uncertified"], true);
        assert!(!staging.join(MANIFEST).exists());
        assert!(!staging.join("conversion-manifest.json").exists());
        let database = Connection::open(staging.join("skyrim_world.db")).unwrap();
        database
            .execute_batch(shared::cluster_mapping::CLUSTER_MAPPING_DDL)
            .unwrap();
        assert!(
            crate::cluster::publish::finalize_benchmark_with_origins(&context, &origins).is_err()
        );
    }
}
