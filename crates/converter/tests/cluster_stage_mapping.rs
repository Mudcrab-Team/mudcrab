//! The stage writes the reference mapping and the finalizer certifies it against the dataset's own
//! inputs: a tiny synthetic dataset (core world database, converted GLBs, conversion manifest),
//! the real stage, the production authority and the certified finalizer. The data is invented; no
//! converted dataset is read.

use converter::{
    cache::{CacheEntry, ConversionManifest, hash_bytes},
    cluster::{
        LodMode,
        authority::CoreAuthority,
        probe::ProbeResult,
        publish::{PublishContext, finalize_base},
        stage::{StageSettings, run_stage},
    },
    esm::{exporter::create_tables, extractors::serialize_subrecords},
};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use shared::{
    asset_lock::AssetLock,
    cluster_mapping::{CoverageState, coverage_state},
};
use std::{fs, path::Path};

const WORLD: u32 = 0x3c;
/// A child worldspace of `WORLD`: its references join the parent's block grid.
const CHILD: u32 = 0x1f;
const CORE: &str = "cd";

fn glb(document: &Value, binary: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = binary.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

/// A 4x4 vertex grid (18 triangles) with normals, UVs and indices; with `broken` the model gets a
/// second primitive that has only POSITION, which the stage rejects.
fn grid_glb(broken: bool, uri: Option<&str>) -> Vec<u8> {
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
    let mut indices: Vec<u16> = Vec::new();
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
    let mut primitives = vec![json!({
        "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 },
        "indices": 3,
        "material": 0
    })];
    if broken {
        primitives.push(json!({ "attributes": { "POSITION": 0 } }));
    }
    let mut document = json!({
        "asset": { "version": "2.0" },
        "buffers": [{ "byteLength": binary.len() }],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 192 },
            { "buffer": 0, "byteOffset": 192, "byteLength": 192 },
            { "buffer": 0, "byteOffset": 384, "byteLength": 128 },
            { "buffer": 0, "byteOffset": 512, "byteLength": indices.len() * 2 }
        ],
        "accessors": [
            { "bufferView": 0, "componentType": 5126, "count": 16, "type": "VEC3",
              "min": [0.0, 0.0, 0.0], "max": [3.0, 3.0, 0.0] },
            { "bufferView": 1, "componentType": 5126, "count": 16, "type": "VEC3" },
            { "bufferView": 2, "componentType": 5126, "count": 16, "type": "VEC2" },
            { "bufferView": 3, "componentType": 5123, "count": indices.len(), "type": "SCALAR" }
        ],
        "materials": [{
            "extras": { "openSkyrim": { "shaderFamily": "lighting", "lightingShaderType": "default" } }
        }],
        "meshes": [{ "primitives": primitives }],
        "nodes": [{ "mesh": 0 }],
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
    });
    if let Some(uri) = uri {
        document["images"] = json!([{ "uri": uri }]);
        document["textures"] = json!([{ "source": 0 }]);
        document["materials"][0]["pbrMetallicRoughness"] =
            json!({ "baseColorTexture": { "index": 0 } });
    }
    glb(&document, &binary)
}

/// (id, base, cell, reference data, header flags)
type PlacedRow = (u32, u32, (i32, i32), Option<Vec<u8>>, u32);

fn clear() -> Vec<u8> {
    serialize_subrecords(&[(b"EDID".to_vec(), b"x\0".to_vec())])
}

struct Dataset {
    directory: tempfile::TempDir,
    manifest: ConversionManifest,
}

fn add_glb(dataset: &mut Dataset, path: &str, bytes: Vec<u8>) {
    let full = dataset.directory.path().join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(&full, &bytes).unwrap();
    dataset.manifest.entries.insert(
        path.to_owned(),
        CacheEntry {
            animation_diagnostics: None,
            source_hash: String::new(),
            output: path.to_owned(),
            output_size: bytes.len() as u64,
            output_hash: hash_bytes(&bytes),
        },
    );
}

/// A synthetic dataset: one worldspace, three models, and references that cover every verdict.
fn dataset() -> Dataset {
    let mut dataset = Dataset {
        directory: tempfile::tempdir().unwrap(),
        manifest: ConversionManifest::default(),
    };
    add_glb(
        &mut dataset,
        "textures/rock.ktx2",
        b"texture bytes".to_vec(),
    );
    add_glb(
        &mut dataset,
        "meshes/arch/rock.glb",
        grid_glb(false, Some("../../textures/rock.ktx2")),
    );
    add_glb(&mut dataset, "meshes/arch/pair.glb", grid_glb(true, None));
    // The same bytes in a deeper folder: the rock resolves its relative texture to another
    // published file (a conflict), the pair names no texture (a valid share).
    add_glb(
        &mut dataset,
        "meshes/textures/rock.ktx2",
        b"other texture bytes".to_vec(),
    );
    add_glb(
        &mut dataset,
        "meshes/arch/sub/rock.glb",
        grid_glb(false, Some("../../textures/rock.ktx2")),
    );
    add_glb(
        &mut dataset,
        "meshes/arch/sub/pair.glb",
        grid_glb(true, None),
    );
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    create_tables(&connection).unwrap();
    connection
        .execute_batch(&format!(
            "INSERT INTO lod_build VALUES (1, '{}');
             INSERT INTO worldspaces(id,editor_id,parent_world,flags,lod_origin_x,lod_origin_y) VALUES ({WORLD}, 'Tamriel', NULL, 0, -32, -32);
             INSERT INTO worldspaces(id,editor_id,parent_world,flags,lod_origin_x,lod_origin_y) VALUES ({CHILD}, 'Child', {WORLD}, 0, NULL, NULL);",
            CORE.repeat(32)
        ))
        .unwrap();
    // Base records: (id, record type, MODL, record subrecords).
    let alternate = serialize_subrecords(&[(b"MODS".to_vec(), vec![1, 0, 0, 0])]);
    let bases: [(u32, &str, &str, Vec<u8>); 8] = [
        (0x11, "STAT", "Arch\\Rock.nif", clear()),
        (0x12, "STAT", "Arch\\Rock.nif", alternate),
        (0x13, "NPC_", "Arch\\Rock.nif", clear()),
        (0x14, "STAT", "Missing\\Gone.nif", clear()),
        (0x15, "STAT", "Arch\\Pair.nif", clear()),
        (0x16, "STAT", "Arch\\Rock.nif", b"junk".to_vec()),
        (0x17, "STAT", "Arch\\Sub\\Rock.nif", clear()),
        (0x18, "STAT", "Arch\\Sub\\Pair.nif", clear()),
    ];
    for (id, kind, model, data) in &bases {
        connection
            .execute(
                "INSERT INTO records(form_id, record_type, cell_id, worldspace_id, load_order, data)
                 VALUES (?1, ?2, NULL, NULL, 0, ?3)",
                params![id, kind, data],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO statics(id, editor_id, model_path, flags) VALUES (?1, 'x', ?2, 0)",
                params![id, model],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO scripts(form_id, script_name, vmad, properties_json)
             VALUES (?1, 'Quest', x'00', '{}')",
            [0x106],
        )
        .unwrap();
    // References: (id, base, cell, data, flags).
    let xalp = serialize_subrecords(&[(b"XALP".to_vec(), vec![1, 0])]);
    let references: [PlacedRow; 11] = [
        (0x100, 0x11, (0, 0), Some(clear()), 0),
        (0x101, 0x15, (1, 0), Some(clear()), 0),
        (0x102, 0x12, (0, 0), Some(clear()), 0),
        (0x103, 0x13, (0, 0), Some(clear()), 0),
        (0x104, 0x11, (0, 0), Some(xalp), 0),
        (0x105, 0x14, (0, 0), Some(clear()), 0),
        (0x106, 0x11, (0, 0), Some(clear()), 0),
        (0x107, 0x11, (0, 0), None, 0),
        (0x108, 0x16, (0, 0), Some(clear()), 0),
        (0x109, 0x17, (0, 0), Some(clear()), 0),
        (0x10a, 0x18, (1, 0), Some(clear()), 0),
    ];
    // A reference placed in the child worldspace, standing in cell (0, 0) of the parent's grid.
    connection
        .execute(
            "INSERT INTO \"references\"(id, cell_id, worldspace_id, base_form_id, is_exterior,
                 pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale, header_flags, data)
             VALUES (0x10b, 0x31, ?1, 0x11, 1, 100.0, 100.0, 10.0, 0.0, 0.0, 0.0, 1.0, 0, ?2)",
            params![CHILD, clear()],
        )
        .unwrap();
    for (id, base, cell, data, flags) in references {
        connection
            .execute(
                "INSERT INTO \"references\"(id, cell_id, worldspace_id, base_form_id, is_exterior,
                     pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale, header_flags, data)
                 VALUES (?1, 0x30, ?2, ?3, 1, ?4, ?5, 10.0, 0.0, 0.0, 0.0, 1.0, ?6, ?7)",
                params![
                    id,
                    WORLD,
                    base,
                    cell.0 as f32 * 4096.0 + 100.0,
                    cell.1 as f32 * 4096.0 + 100.0,
                    flags,
                    data
                ],
            )
            .unwrap();
    }
    dataset
}

fn settings() -> StageSettings {
    StageSettings {
        mode: LodMode::Cluster,
        triangle_threshold: 1,

        jobs: 2,
        assets_root: None,

        read_database: None,
        area: None,
        child_worldspaces: vec![CHILD],
        include_trees: false,

        map_references: true,
    }
}

fn probe() -> ProbeResult {
    ProbeResult {
        passed: true,
        missing: Vec::new(),
        backend: "test".into(),
        adapter_name: "test".into(),
        json: json!({ "test": true }),
    }
}

/// Runs the stage and returns its builder identity.
fn run(dataset: &Dataset, settings: &StageSettings) -> String {
    let none = dataset.directory.path().join("no-previous");
    let (report, _) = run_stage(
        settings,
        Some(probe()),
        dataset.directory.path(),
        &none,
        &dataset.manifest,
    )
    .unwrap();
    report.builder_identity
}

fn authority(dataset: &Dataset, settings: &StageSettings, builder: String) -> CoreAuthority {
    CoreAuthority::open(
        Connection::open_with_flags(
            dataset.directory.path().join("skyrim_world.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
        dataset.directory.path(),
        &dataset.manifest,
        settings,
        builder,
    )
    .unwrap()
}

fn finalize(
    dataset: &Dataset,
    authority: &CoreAuthority,
) -> color_eyre::Result<converter::cluster::publish::BaseFinal> {
    let root: &Path = dataset.directory.path();
    let lock = AssetLock::acquire_exclusive(root)?;
    let context = PublishContext::new(&lock, root, root)?;
    finalize_base(&context, Some(authority))
}

#[test]
fn the_stage_writes_a_mapping_the_authority_certifies() {
    let dataset = dataset();
    let settings = settings();
    let builder = run(&dataset, &settings);
    let authority = authority(&dataset, &settings, builder);
    let done = finalize(&dataset, &authority).unwrap();
    assert_eq!(done.extension.built_from, CORE.repeat(32));
    assert_eq!(done.extension.stats["instance_map_rows"], 5);

    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    let state = |id| coverage_state(&connection, id).unwrap();
    assert_eq!(state(0x100), CoverageState::Complete);
    // The pair model has a second primitive the builder rejected.
    assert_eq!(state(0x101), CoverageState::Partial);
    // Refused as wholes, with their reasons; never drawn.
    for (id, status, reason) in [
        (0x104, "not_eligible", "unresolved_reference_override:XALP"),
        (0x105, "unknown", "model_not_converted"),
        (0x107, "unknown", "unparsed_reference_data"),
        (0x108, "unknown", "unparsed_base_record"),
        (0x109, "unknown", "model_material_path_conflict"),
    ] {
        let (found_status, found_reason): (String, String) = connection
            .query_row(
                "SELECT status, reason FROM cluster_reference_coverage
                 WHERE reference_id = ?1 AND scope = 'reference'",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|_| panic!("no coverage row for {id:#x}"));
        assert_eq!(
            (found_status.as_str(), found_reason.as_str()),
            (status, reason),
            "{id:#x}"
        );
    }
    // The NPC is not in the declared scope at all.
    assert_eq!(state(0x103), CoverageState::Absent);
    let mapped: Vec<u32> = connection
        .prepare("SELECT DISTINCT reference_id FROM cluster_instance_map ORDER BY 1")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // The shared pair model is drawn for the reference in the deeper folder too.
    assert_eq!(mapped, [0x100, 0x101, 0x102, 0x10a, 0x10b]);
    assert_eq!(state(0x102), CoverageState::Complete);
    assert_eq!(state(0x106), CoverageState::NotEligible);
    assert_eq!(state(0x10b), CoverageState::Complete);
    assert_eq!(state(0x10a), CoverageState::Partial);
}

#[test]
fn a_change_in_the_core_after_the_stage_is_not_certified() {
    let dataset = dataset();
    let settings = settings();
    let builder = run(&dataset, &settings);
    // The base record of 0x100 gains alternate textures after the mapping was written: the stage's
    // tables still say it is drawn, the authority's recomputation says it is refused.
    {
        let connection =
            Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "UPDATE records SET data = ?1 WHERE form_id = 0x11",
                [serialize_subrecords(&[(
                    b"MODS".to_vec(),
                    vec![1, 0, 0, 0],
                )])],
            )
            .unwrap();
    }
    let authority = authority(&dataset, &settings, builder);
    let error = format!("{:#}", finalize(&dataset, &authority).unwrap_err());
    assert!(error.contains("DigestNotAuthoritative"), "{error}");
    assert!(
        !dataset
            .directory
            .path()
            .join("cluster-manifest.json")
            .exists()
    );
}

#[test]
fn a_changed_scope_or_model_is_not_certified() {
    let dataset = dataset();
    let settings = settings();
    let builder = run(&dataset, &settings);
    // The authority examines trees as well: the declared scope no longer matches.
    let mut other = settings.clone();
    other.include_trees = true;
    let error = format!(
        "{:#}",
        finalize(&dataset, &authority(&dataset, &other, builder.clone())).unwrap_err()
    );
    assert!(error.contains("ScopeDeclarationMismatch"), "{error}");
    // A model whose published hash changed since the stage ran.
    let mut changed = dataset;
    let entry = changed
        .manifest
        .entries
        .get_mut("meshes/arch/rock.glb")
        .unwrap();
    entry.output_hash = "ee".repeat(32);
    let error = format!(
        "{:#}",
        finalize(&changed, &authority(&changed, &settings, builder)).unwrap_err()
    );
    assert!(error.contains("DigestNotAuthoritative"), "{error}");
}

/// Edits a published file after the stage ran, leaving the conversion manifest untouched.
fn corrupt(dataset: &Dataset, path: &str, edit: impl Fn(Vec<u8>) -> Option<Vec<u8>>) {
    let full = dataset.directory.path().join(path);
    match edit(fs::read(&full).unwrap()) {
        Some(bytes) => fs::write(&full, bytes).unwrap(),
        None => fs::remove_file(&full).unwrap(),
    }
}

#[test]
fn published_content_changed_after_the_stage_is_not_certified() {
    // Every case keeps the manifest, the sizes where it can, and the primitive count; only the
    // authority reading the current files can see it.
    type Edit = fn(Vec<u8>) -> Option<Vec<u8>>;
    let cases: [(&str, &str, Edit); 6] = [
        (
            "geometry in the binary chunk",
            "meshes/arch/rock.glb",
            |mut bytes| {
                let last = bytes.len() - 1;
                bytes[last] ^= 0xff;
                Some(bytes)
            },
        ),
        (
            "material in the JSON chunk",
            "meshes/arch/rock.glb",
            |mut bytes| {
                // The same length and still valid JSON: only the shader family differs.
                let needle = b"\"lighting\"";
                let at = bytes
                    .windows(needle.len())
                    .position(|window| window == needle)
                    .expect("the material names its shader family");
                bytes[at + 1] = b'L';
                Some(bytes)
            },
        ),
        ("the GLB deleted", "meshes/arch/rock.glb", |_| None),
        ("texture bytes", "textures/rock.ktx2", |mut bytes| {
            bytes[0] ^= 0xff;
            Some(bytes)
        }),
        ("texture deleted", "textures/rock.ktx2", |_| None),
        (
            "texture emptied",
            "textures/rock.ktx2",
            |_| Some(Vec::new()),
        ),
    ];
    for (name, path, edit) in cases {
        let dataset = dataset();
        let settings = settings();
        let builder = run(&dataset, &settings);
        corrupt(&dataset, path, edit);
        let error = format!(
            "{:#}",
            finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap_err()
        );
        assert!(
            error.contains("DigestNotAuthoritative") || error.contains("EligibilityMismatch"),
            "{name}: {error}"
        );
        assert!(
            !dataset
                .directory
                .path()
                .join("cluster-manifest.json")
                .exists(),
            "{name}"
        );
    }
}

#[test]
fn an_unpublished_texture_makes_the_reference_unknown_not_eligible() {
    let mut dataset = dataset();
    // The conversion did not publish the texture the model names.
    dataset.manifest.entries.remove("textures/rock.ktx2");
    let settings = settings();
    let builder = run(&dataset, &settings);
    let authority = authority(&dataset, &settings, builder);
    finalize(&dataset, &authority).unwrap();
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    assert_eq!(
        coverage_state(&connection, 0x100).unwrap(),
        CoverageState::Unknown
    );
    let reason: String = connection
        .query_row(
            "SELECT reason FROM cluster_reference_coverage WHERE reference_id = 256 AND scope = 'reference'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reason, "texture_not_converted");
    // The pair model has no texture and stays eligible.
    assert_eq!(
        coverage_state(&connection, 0x101).unwrap(),
        CoverageState::Partial
    );
}

#[test]
fn an_emitted_material_that_is_not_the_canonical_paths_is_not_certified() {
    let dataset = dataset();
    let settings = settings();
    let builder = run(&dataset, &settings);
    // The rock mesh row claims the material key of another path's texture resolution.
    Connection::open(dataset.directory.path().join("skyrim_world.db"))
        .unwrap()
        .execute(
            "UPDATE cluster_meshes SET material_key = 'from-the-other-folder'
             WHERE model_path = 'meshes/arch/rock.glb'",
            [],
        )
        .unwrap();
    let error = format!(
        "{:#}",
        finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap_err()
    );
    assert!(error.contains("MaterialMismatch"), "{error}");
}

#[test]
fn a_block_that_the_lod_origin_does_not_put_there_is_not_certified() {
    let dataset = dataset();
    let settings = settings();
    let builder = run(&dataset, &settings);
    // The package keeps its blocks, rows and hashes consistent, but the block worldspace's LOD
    // origin is not the one they were built on: every object block sits on the wrong anchor, for
    // the references of the world and of its child alike.
    Connection::open(dataset.directory.path().join("skyrim_world.db"))
        .unwrap()
        .execute(
            "UPDATE worldspaces SET lod_origin_x = lod_origin_x + 16 WHERE id = ?1",
            [WORLD],
        )
        .unwrap();
    let error = format!(
        "{:#}",
        finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap_err()
    );
    assert!(error.contains("SnapshotMismatch"), "{error}");
    assert!(error.contains("block"), "{error}");
}

#[test]
fn a_mesh_row_must_name_the_canonical_glb_slot_and_flags_not_only_the_key() {
    let positive = dataset();
    let settings = settings();
    let builder = run(&positive, &settings);
    let connection = Connection::open(positive.directory.path().join("skyrim_world.db")).unwrap();
    // The stage writes the canonical path and the real slot.
    let (path, slot): (String, Option<u32>) = connection
        .query_row(
            "SELECT model_path, material_index FROM cluster_meshes
             WHERE model_path LIKE '%rock.glb' AND primitive_index = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((path.as_str(), slot), ("meshes/arch/rock.glb", Some(0)));
    drop(connection);
    finalize(&positive, &authority(&positive, &settings, builder)).unwrap();

    for sql in [
        "UPDATE cluster_meshes SET material_index = 1 WHERE model_path = 'meshes/arch/rock.glb'",
        "UPDATE cluster_meshes SET material_index = NULL WHERE model_path = 'meshes/arch/rock.glb'",
        "UPDATE cluster_meshes SET model_path = 'meshes/arch/sub/rock.glb'
         WHERE model_path = 'meshes/arch/rock.glb'",
        "UPDATE cluster_meshes SET alpha_mode = 'mask', alpha_cutoff = 0.5
         WHERE model_path = 'meshes/arch/rock.glb'",
        "UPDATE cluster_meshes SET double_sided = 1 WHERE model_path = 'meshes/arch/rock.glb'",
    ] {
        let dataset = dataset();
        let builder = run(&dataset, &settings);
        Connection::open(dataset.directory.path().join("skyrim_world.db"))
            .unwrap()
            .execute(sql, [])
            .unwrap();
        let error = format!(
            "{:#}",
            finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap_err()
        );
        assert!(error.contains("MaterialMismatch"), "{sql}: {error}");
    }
}

/// Runs SQL on the dataset's core database before the stage.
fn core(dataset: &Dataset, sql: &str) {
    Connection::open(dataset.directory.path().join("skyrim_world.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

/// The snow material the instance record of `reference` carries, read from its `.mci` block.
fn recorded_snow(dataset: &Dataset, reference: u32) -> u32 {
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    let (path, ordinal): (String, u32) = connection
        .query_row(
            "SELECT c.payload_path, m.ordinal
             FROM cluster_instance_map m JOIN cluster_chunks c
               ON c.worldspace_id = m.worldspace_id AND c.tier = m.tier
              AND c.anchor_x = m.anchor_x AND c.anchor_y = m.anchor_y
             WHERE m.reference_id = ?1 LIMIT 1",
            [reference],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let bytes = fs::read(dataset.directory.path().join(path)).unwrap();
    shared::cluster::read_mci(&bytes)
        .unwrap()
        .instance(ordinal as usize)
        .snow_material
}

fn declared_snow_source(dataset: &Dataset) -> String {
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    let scope: String = connection
        .query_row(
            "SELECT classifier_scope FROM cluster_provenance WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let scope: Value = serde_json::from_str(&scope).unwrap();
    scope["snow_material_source"].as_str().unwrap().to_owned()
}

const MATO: u32 = 0x25129;
const EXTENSION_TABLE: &str = "CREATE TABLE IF NOT EXISTS static_materials (
        id INTEGER PRIMARY KEY, material_object INTEGER, material_max_angle REAL);
    INSERT INTO static_materials(id, material_object, material_max_angle) VALUES (0x11, 0x25129, 45.0);";

#[test]
fn snow_comes_from_the_directional_snow_extension_table() {
    // The current core `statics` has no material_object column: the extension owns it.
    let dataset = dataset();
    core(&dataset, EXTENSION_TABLE);
    let settings = settings();
    let builder = run(&dataset, &settings);
    finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap();
    assert_eq!(declared_snow_source(&dataset), "static_materials");
    // Both references of base 0x11 (one in the child worldspace) record the real MATO; a base
    // with no row in the extension table records 0.
    assert_eq!(recorded_snow(&dataset, 0x100), MATO);
    assert_eq!(recorded_snow(&dataset, 0x10b), MATO);
    assert_eq!(recorded_snow(&dataset, 0x101), 0);
}

#[test]
fn without_the_extension_snow_is_zero_by_declared_policy() {
    let dataset = dataset();
    core(&dataset, "DROP TABLE IF EXISTS static_materials");
    let settings = settings();
    let builder = run(&dataset, &settings);
    finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap();
    assert_eq!(declared_snow_source(&dataset), "none");
    assert_eq!(recorded_snow(&dataset, 0x100), 0);
}

#[test]
fn a_changed_snow_source_after_the_stage_is_not_certified() {
    let settings = settings();
    // The MATO of a base changes after the stage ran: the reference inputs differ.
    let dataset = dataset();
    core(&dataset, EXTENSION_TABLE);
    let builder = run(&dataset, &settings);
    core(
        &dataset,
        "UPDATE static_materials SET material_object = 0x25130 WHERE id = 0x11",
    );
    let error = format!(
        "{:#}",
        finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap_err()
    );
    assert!(error.contains("DigestNotAuthoritative"), "{error}");
    // The extension table disappears after the stage: the declared snow source no longer holds.
    let dataset = self::dataset();
    core(&dataset, EXTENSION_TABLE);
    let builder = run(&dataset, &settings);
    core(&dataset, "DROP TABLE static_materials");
    let error = format!(
        "{:#}",
        finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap_err()
    );
    assert!(error.contains("ScopeDeclarationMismatch"), "{error}");
}

#[test]
fn the_legacy_column_is_read_only_when_the_extension_table_is_absent() {
    let settings = settings();
    let legacy = "DROP TABLE IF EXISTS static_materials;
                  ALTER TABLE statics ADD COLUMN material_object INTEGER;
                  UPDATE statics SET material_object = 0x777 WHERE id = 0x11;";
    let dataset = dataset();
    core(&dataset, legacy);
    let builder = run(&dataset, &settings);
    finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap();
    assert_eq!(declared_snow_source(&dataset), "statics.material_object");
    assert_eq!(recorded_snow(&dataset, 0x100), 0x777);
    // With both present the extension table wins.
    let dataset = self::dataset();
    core(&dataset, legacy);
    core(&dataset, EXTENSION_TABLE);
    let builder = run(&dataset, &settings);
    finalize(&dataset, &authority(&dataset, &settings, builder)).unwrap();
    assert_eq!(declared_snow_source(&dataset), "static_materials");
    assert_eq!(recorded_snow(&dataset, 0x100), MATO);
}

/// A GLB with a JSON chunk and nothing else: a scene with no nodes, as the converter writes for a
/// NIF that has no drawable geometry.
fn empty_glb() -> Vec<u8> {
    let mut json = serde_json::to_vec(&json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [] }],
    }))
    .unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total = 12 + 8 + json.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out
}

#[test]
fn a_model_a_mod_emptied_to_hide_an_object_is_certified_with_nothing_to_draw() {
    // Mods hide a vanilla object by replacing its model with a NIF that has no geometry. The
    // reference stays eligible and expanded; its model simply has no primitives, so it has no
    // primitive rows, no mesh or reject rows and no instances, and nothing is left to draw.
    let mut dataset = dataset();
    add_glb(&mut dataset, "meshes/arch/hidden.glb", empty_glb());
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    connection
        .execute(
            "INSERT INTO records(form_id, record_type, cell_id, worldspace_id, load_order, data)
             VALUES (0x19, 'STAT', NULL, NULL, 0, ?1)",
            [clear()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO statics(id, editor_id, model_path, flags)
             VALUES (0x19, 'x', ?1, 0)",
            [r"Arch\Hidden.nif"],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO \"references\"(id, cell_id, worldspace_id, base_form_id, is_exterior,
                 pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale, header_flags, data)
             VALUES (0x10c, 0x30, ?1, 0x19, 1, 100.0, 100.0, 10.0, 0.0, 0.0, 0.0, 1.0, 0, ?2)",
            params![WORLD, clear()],
        )
        .unwrap();
    drop(connection);

    let settings = settings();
    let builder = run(&dataset, &settings);
    let authority = authority(&dataset, &settings, builder);
    finalize(&dataset, &authority).unwrap();

    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    let (status, primitives, instances): (String, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT status FROM cluster_reference_coverage
                  WHERE reference_id = 0x10c AND scope = 'reference'),
                 (SELECT COUNT(*) FROM cluster_reference_coverage
                  WHERE reference_id = 0x10c AND scope = 'primitive'),
                 (SELECT COUNT(*) FROM cluster_instance_map WHERE reference_id = 0x10c)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((status.as_str(), primitives, instances), ("expanded", 0, 0));
    // Not complete (nothing is clustered), and nothing unclustered for the native path to draw.
    assert_eq!(
        coverage_state(&connection, 0x10c).unwrap(),
        CoverageState::Partial
    );
}

#[test]
fn a_loose_item_is_refused_until_production_state_writers_exist() {
    let dataset = dataset();
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    connection
        .execute(
            "UPDATE records SET record_type='MISC' WHERE form_id=0x11",
            [],
        )
        .unwrap();
    drop(connection);
    let settings = settings();
    let builder = run(&dataset, &settings);
    let authority = authority(&dataset, &settings, builder);
    let done = finalize(&dataset, &authority).unwrap();
    assert_eq!(done.extension.stats["instance_map_rows"], 3);
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    assert_eq!(
        coverage_state(&connection, 0x100).unwrap(),
        CoverageState::NotEligible
    );
    assert_eq!(
        coverage_state(&connection, 0x106).unwrap(),
        CoverageState::NotEligible
    );
}

#[test]
fn scripts_on_either_reference_or_base_prevent_cluster_replacement() {
    for (scripted_form, rows) in [(0x100, 4), (0x11, 3)] {
        let dataset = dataset();
        let connection =
            Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute("INSERT INTO scripts (form_id, script_name, vmad, properties_json) VALUES (?1, 'Quest', x'00', '{}')", [scripted_form])
            .unwrap();
        drop(connection);
        let settings = settings();
        let builder = run(&dataset, &settings);
        let authority = authority(&dataset, &settings, builder);
        let done = finalize(&dataset, &authority).unwrap();
        // A scripted reference loses its own rows; a scripted base loses every reference to it.
        assert_eq!(done.extension.stats["instance_map_rows"], rows);
        let connection =
            Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
        assert_eq!(
            coverage_state(&connection, 0x100).unwrap(),
            CoverageState::NotEligible
        );
    }
}

#[test]
fn unproven_shader_features_are_refused_without_losing_source_material_identity() {
    let bytes = grid_glb(false, None);
    let source = gltf::Gltf::from_slice(&bytes).unwrap();
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let document: serde_json::Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
    for kind in [
        "environment_map",
        "multi_layer_parallax",
        "glow",
        "parallax",
        "sparkle_snow",
    ] {
        let mut material_document = document.clone();
        material_document["materials"][0]["extras"]["openSkyrim"]["lightingShaderType"] =
            json!(kind);
        material_document["materials"][0]["emissiveFactor"] = json!([0.2, 0.4, 0.1]);
        let converted = glb(&material_document, source.blob.as_ref().unwrap());
        let loaded =
            converter::cluster::glb_input::load_model(&converted, "meshes/test.glb").unwrap();
        let facts = shared::cluster_recipe::model_facts::model_facts_from_json(
            &serde_json::to_vec(&material_document).unwrap(),
            "meshes/test.glb",
        )
        .unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].reject.as_ref().unwrap().reason,
            converter::cluster::model::RejectReason::UnsupportedMaterialFeature,
            "{kind}"
        );
        assert_eq!(loaded[0].material_key, facts.materials[&0].material_key);
        material_document["materials"][0]["extras"]["openSkyrim"]["shaderFlags1"] =
            json!(shared::material_depth::DEPTH_TEST | shared::material_depth::DECAL);
        material_document["materials"][0]["extras"]["openSkyrim"]["shaderFlags2"] =
            json!(shared::material_depth::DEPTH_WRITE);
        let converted = glb(&material_document, source.blob.as_ref().unwrap());
        let loaded =
            converter::cluster::glb_input::load_model(&converted, "meshes/test.glb").unwrap();
        assert_eq!(
            loaded[0].reject.as_ref().unwrap().reason,
            converter::cluster::model::RejectReason::NotOpaque
        );
        assert!(
            loaded[0]
                .reject
                .as_ref()
                .unwrap()
                .detail
                .contains("sorted color pass")
        );
    }
}

#[test]
fn changing_mods_rekeys_only_references_of_that_base_without_changing_model_identity() {
    use shared::cluster_mapping::AuthoritativeInputs;
    let dataset = dataset();
    let settings = settings();
    let builder = run(&dataset, &settings);
    let before = authority(&dataset, &settings, builder.clone());
    let modified = before.reference(0x102).unwrap();
    let untouched = before.reference(0x100).unwrap();
    assert_eq!(
        modified.eligibility,
        shared::cluster_mapping::Eligibility::Eligible
    );
    drop(before);
    let connection = Connection::open(dataset.directory.path().join("skyrim_world.db")).unwrap();
    let changed = serialize_subrecords(&[(b"MODS".to_vec(), vec![2, 0, 0, 0])]);
    connection
        .execute("UPDATE records SET data=?1 WHERE form_id=0x12", [changed])
        .unwrap();
    drop(connection);
    let after = authority(&dataset, &settings, builder);
    let modified_after = after.reference(0x102).unwrap();
    assert_ne!(modified.digest, modified_after.digest);
    assert_eq!(modified.model, modified_after.model);
    assert_eq!(untouched.digest, after.reference(0x100).unwrap().digest);
}
