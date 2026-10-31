//! A small synthetic, complete base conversion for tests in other crates (feature `test-support`).
//!
//! The engine's producer/consumer proof needs a base the real [`crate::derive_extensions::run`] accepts:
//! a world database, a converted model, a terrain recipe with its raw DDS, a cell cache and a sealed
//! conversion manifest whose extension entries and file hashes pass the same preflight a converted
//! Skyrim base does. This module writes only that base. It writes no overlay: the overlay always comes
//! from the real derivation, so a test sees exactly what a user's `derive-clusters` produces. The
//! panics are test-fixture assertions; nothing here is reachable from a shipped binary.
//!
//! Layout (invented data only): one worldspace, one object reference to `stone.glb`, three land cells
//! forming one terrain block, and the raw landscape DDS under the base's own `vfs/`, which is the
//! terrain input root of a base that kept its VFS.

use crate::{
    cache::{CONVERTER_SCHEMA_VERSION, CacheEntry, ConversionManifest, hash_bytes},
    cluster::test_support::glb,
    derive_extensions::DeriveOptions,
    esm::{exporter::create_tables, extractors::serialize_subrecords},
};
use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
use rusqlite::{Connection, params};
use serde_json::json;
use shared::{CachedLand, CellCache, TerrainLayer};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

/// The core build identity every synthetic base records.
pub const CORE_IDENTITY: &str = "c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7";
/// The asset key [`SyntheticBase::with_failed_asset`] records as a failed conversion.
pub const FAILED_ASSET: &str = "textures/failed.dds";

const WORLD: u32 = 0x52;
const ORIGIN: [i32; 2] = [-24, 40];
const CELLS: [(u32, i32, i32); 3] = [(0x31, 8, 24), (0x32, 9, 24), (0x33, 8, 25)];
const MODEL: &str = "meshes/invented/stone.glb";
const TERRAIN_DDS: &str = "textures/landscape/invented.dds";

/// A tempfile-owned base conversion folder.
pub struct SyntheticBase {
    directory: TempDir,
    base: PathBuf,
}

impl SyntheticBase {
    /// A finished conversion: `complete`, no recorded failure.
    pub fn complete() -> Self {
        Self::build(false)
    }

    /// What the converter leaves after one skipped asset: `complete: false`, one entry in
    /// `failures`, record data otherwise complete.
    pub fn with_failed_asset() -> Self {
        Self::build(true)
    }

    /// The base conversion folder (the `--assets` root).
    pub fn path(&self) -> &Path {
        &self.base
    }

    /// The temporary folder that owns the base; kept alive by `self`.
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Options for the real derivation of this base. Scheduling and the label are fixed; the
    /// threshold of 1 clusters the 18-triangle model.
    pub fn derive_options(&self, terrain: bool) -> DeriveOptions {
        DeriveOptions {
            source: self.base.clone(),
            label: "default".into(),
            replace: false,
            keep_failed_staging: false,
            terrain,
            terrain_sources: None,
            jobs: 1,
            triangle_threshold: 1,
            include_trees: false,
        }
    }

    fn build(failed_asset: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("OWNER.txt"),
            "owner: converter test-support synthetic base; tempfile-owned invented data\n",
        )
        .unwrap();
        let base = directory.path().join("base");
        fs::create_dir(&base).unwrap();
        fs::write(
            base.join("OWNER.txt"),
            "owner: synthetic complete base conversion\n",
        )
        .unwrap();
        let mut manifest = ConversionManifest {
            schema_version: CONVERTER_SCHEMA_VERSION,
            complete: !failed_asset,
            record_data_complete: Some(true),
            ..Default::default()
        };

        let model = stone_glb();
        add_output(
            &mut manifest,
            &base,
            MODEL,
            MODEL,
            &model,
            hash_bytes(&model),
        );
        let dds = landscape_dds();
        let dds_hash = hash_bytes(&dds);
        add_output(
            &mut manifest,
            &base,
            TERRAIN_DDS,
            &format!("vfs/{TERRAIN_DDS}"),
            &dds,
            format!("{dds_hash}:texture-encoding:ColorSrgb"),
        );
        if failed_asset {
            manifest
                .failures
                .insert(FAILED_ASSET.into(), "invented conversion failure".into());
        }

        write_world(&base);
        fs::write(
            base.join("lod-manifest.json"),
            serde_json::to_vec(&json!({
                "build_identity": CORE_IDENTITY,
                "converter_schema": CONVERTER_SCHEMA_VERSION,
                "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
                "chunks": 0,
                "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
                "terrain_sources": { (TERRAIN_DDS): dds_hash },
            }))
            .unwrap(),
        )
        .unwrap();

        manifest.extensions = crate::extensions::build_entries(&base).unwrap();
        manifest
            .save(&base.join("conversion-manifest.json"))
            .unwrap();
        Self { directory, base }
    }
}

fn add_output(
    manifest: &mut ConversionManifest,
    base: &Path,
    key: &str,
    output: &str,
    bytes: &[u8],
    source_hash: String,
) {
    let path = base.join(output);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    manifest.entries.insert(
        key.into(),
        CacheEntry {
            animation_diagnostics: None,
            source_hash,
            output: output.into(),
            output_size: bytes.len() as u64,
            output_hash: hash_bytes(bytes),
        },
    );
}

/// The world database and the cell cache: one object reference and three land cells.
fn write_world(base: &Path) {
    let db = Connection::open(base.join("skyrim_world.db")).unwrap();
    create_tables(&db).unwrap();
    db.execute("INSERT INTO lod_build VALUES (1, ?1)", [CORE_IDENTITY])
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
    db.execute(
        "INSERT INTO statics(id,editor_id,model_path,flags) \
         VALUES (0x11,'InventedStone','Invented\\Stone.nif',0)",
        [],
    )
    .unwrap();
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
    // The object stands in the first land cell (cell size 4096 world units).
    db.execute(
        "INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,\
         rot_x,rot_y,rot_z,scale,header_flags,data) VALUES (0x100,0x31,?1,0x11,1,?2,?3,20,0,0,0,1,0,?4)",
        params![WORLD, 8.0f32 * 4096.0 + 100.0, 24.0f32 * 4096.0 + 100.0, clear],
    )
    .unwrap();
    drop(db);
    let cache = CellCache {
        version: shared::CELL_CACHE_VERSION,
        cells: lands,
    };
    fs::write(
        base.join("cell_cache.rkyv"),
        rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap(),
    )
    .unwrap();
}

/// A complete 4x4-vertex grid (18 triangles) with normals and UVs, which the cluster stage accepts.
fn stone_glb() -> Vec<u8> {
    let mut binary = Vec::new();
    let (mut positions, mut normals, mut uvs) = (Vec::new(), Vec::new(), Vec::new());
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
    for value in positions.iter().chain(&normals).chain(&uvs) {
        binary.extend(value.to_le_bytes());
    }
    for value in &indices {
        binary.extend(value.to_le_bytes());
    }
    glb(
        &json!({
            "asset": {"version": "2.0"}, "buffers": [{"byteLength": binary.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 192},
                {"buffer": 0, "byteOffset": 192, "byteLength": 192},
                {"buffer": 0, "byteOffset": 384, "byteLength": 128},
                {"buffer": 0, "byteOffset": 512, "byteLength": indices.len() * 2}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 16, "type": "VEC3",
                 "min": [0, 0, 0], "max": [3, 3, 0]},
                {"bufferView": 1, "componentType": 5126, "count": 16, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5126, "count": 16, "type": "VEC2"},
                {"bufferView": 3, "componentType": 5123, "count": indices.len(), "type": "SCALAR"}
            ],
            "materials": [{"extras": {"openSkyrim": {
                "shaderFamily": "lighting", "lightingShaderType": "default"}}}],
            "meshes": [{"primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                "indices": 3, "material": 0}]}],
            "nodes": [{"mesh": 0}], "scene": 0, "scenes": [{"nodes": [0]}]
        }),
        &binary,
    )
}

/// The raw landscape texture the terrain recipe records: 4x4 RGBA, one mip.
fn landscape_dds() -> Vec<u8> {
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
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    bytes
}
