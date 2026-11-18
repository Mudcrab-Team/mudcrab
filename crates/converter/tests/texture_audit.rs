//! The texture audit finds each way a surface can lose its texture, and a baseline ratchets the
//! count down. The CI tests build a tiny game-data directory, a pack and a world database; the
//! ignored test audits a real pack against the real game data.

use converter::{
    material::NifMaterialDisposition,
    mesh::MeshConverter,
    texture_audit::{
        AuditOptions, Baseline, Cause, Report, SourceCatalog, audit_pack, compare_to_baseline,
        improvements,
    },
};
use dummy_content::nif::{StaticShape, static_shape};
use std::{
    fs,
    path::{Path, PathBuf},
};

const POSITIONS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
const NORMALS: [[f32; 3]; 3] = [[0.0, 0.0, 1.0]; 3];
const UVS: [[f32; 2]; 3] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];

/// Writes a source NIF and returns the shape block of its one shape, which the converter records
/// in the GLB material so the audit can find the material of a shape.
fn write_nif(data: &Path, name: &str, diffuse: &str, normal: &str) -> u32 {
    let bytes = static_shape(&StaticShape {
        name,
        positions: &POSITIONS,
        normals: &NORMALS,
        uvs: &UVS,
        indices: &[[0, 1, 2]],
        diffuse,
        normal_texture: normal,
    })
    .unwrap();
    let path = data.join("meshes/fix").join(format!("{name}.nif"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    MeshConverter::inspect_nif_materials(&path)
        .unwrap()
        .into_iter()
        .find(|shape| matches!(shape.disposition, NifMaterialDisposition::Validated { .. }))
        .expect("the fixture NIF has one validated shape")
        .shape_block
}

/// One material of a fixture GLB. `diffuse` and `normal` index `images`; an image that is listed
/// and not bound reaches no slot.
#[derive(Clone, Copy)]
struct Mat {
    shape_block: Option<u32>,
    diffuse: Option<usize>,
    normal: Option<usize>,
}

/// What a fixture GLB holds: images, materials, and one primitive per entry of `primitives`, each
/// naming the material it uses. A material no primitive names is unused.
struct Glb<'a> {
    images: &'a [&'a str],
    materials: Vec<Mat>,
    primitives: Vec<usize>,
    has_uvs: bool,
    /// The converter's `texturePathRepairs`: an original URI and the URI that replaced it.
    repairs: Option<(&'a str, &'a str)>,
}

impl<'a> Glb<'a> {
    /// One material, used by one primitive: the usual converted shape.
    fn single(
        images: &'a [&'a str],
        diffuse: Option<usize>,
        normal: Option<usize>,
        has_uvs: bool,
        shape_block: Option<u32>,
    ) -> Self {
        Self {
            images,
            materials: vec![Mat {
                shape_block,
                diffuse,
                normal,
            }],
            primitives: vec![0],
            has_uvs,
            repairs: None,
        }
    }
}

/// A GLB with only a JSON chunk: the audit reads nothing else.
fn write_glb(pack: &Path, name: &str, spec: &Glb<'_>) {
    let textures: Vec<_> = (0..spec.images.len())
        .map(|index| serde_json::json!({"source": index}))
        .collect();
    let mut attributes = serde_json::json!({"POSITION": 0});
    if spec.has_uvs {
        attributes["TEXCOORD_0"] = serde_json::json!(1);
    }
    let materials: Vec<_> = spec
        .materials
        .iter()
        .map(|mat| {
            let mut material = serde_json::json!({
                "name": "m",
                "pbrMetallicRoughness": {},
                "extras": {"openSkyrim": {"shaderFamily": "lighting"}}
            });
            if let Some(block) = mat.shape_block {
                material["extras"]["openSkyrim"]["shapeBlock"] = serde_json::json!(block);
            }
            if let Some(index) = mat.diffuse {
                material["pbrMetallicRoughness"]["baseColorTexture"] =
                    serde_json::json!({"index": index});
            }
            if let Some(index) = mat.normal {
                material["normalTexture"] = serde_json::json!({"index": index});
            }
            material
        })
        .collect();
    let primitives: Vec<_> = spec
        .primitives
        .iter()
        .map(|material| serde_json::json!({"attributes": attributes, "material": material}))
        .collect();
    let mut document = serde_json::json!({
        "asset": {"version": "2.0"},
        "images": spec.images.iter().map(|uri| serde_json::json!({"uri": uri})).collect::<Vec<_>>(),
        "textures": textures,
        "materials": materials,
        "meshes": [{"name": name, "primitives": primitives}]
    });
    if let Some((original, replacement)) = spec.repairs {
        document["extras"] = serde_json::json!({
            "mudcrab": {"texturePathRepairs": {original: replacement}}
        });
    }
    let mut json = serde_json::to_vec(&document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bytes = b"glTF".to_vec();
    bytes.extend(2u32.to_le_bytes());
    bytes.extend((12 + 8 + json.len() as u32).to_le_bytes());
    bytes.extend((json.len() as u32).to_le_bytes());
    bytes.extend(b"JSON");
    bytes.extend(json);
    let path = pack.join("meshes/fix").join(format!("{name}.glb"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn touch(root: &Path, relative: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

const PRESENT: &str = "../../textures/fix/present.opensky-srgb.ktx2";
const PRESENT_N: &str = "../../textures/fix/present_n.ktx2";
const REPAIR_N: &str = "../../textures/fix/repair_n.ktx2";
const REPAIRED_N: &str = "../../textures/dlc/repair_n.ktx2";

/// Twelve meshes. Each source NIF wants a diffuse and a normal texture.
///
/// * `pruned`: wants `absent_n.dds`, which the game data does not hold; the converter pruned it.
/// * `lost`: wants `lost.dds`, which the game data holds but the pack never converted.
/// * `dropped`: wants `present_n.dds`; the pack holds it and the GLB lists the image, but no
///   material binds it to the normal slot.
/// * `unused`: two materials carry the shape block. One binds both images but no primitive uses
///   it; the used one binds only the diffuse. The unused material must not clear the normal.
/// * `aliased`: the GLB binds `present_n.opensky-srgb.ktx2`, a file the pack does not hold, though
///   the pack holds `present_n.ktx2` under the same base name. The engine requests the exact file.
/// * `repaired`: wants `repair_n.dds`, which the game data does not hold; the converter replaced
///   the image with `textures/dlc/repair_n.ktx2` and published the repair, so nothing is missing.
/// * `repaired_unbound`: the same repair is published, but the material carries no shape block, so
///   nothing shows that the shape binds the replacement. It must not count as resolved.
/// * `nometa`: the only material has no shape block, so the audit cannot tell it belongs to the
///   shape.
/// * `dup`: two used materials carry the same shape block.
/// * `flat`: every texture resolves, but the primitive has no UVs.
/// * `generated`: a GLB with no source NIF that references a texture that exists nowhere.
/// * `refused`: a GLB with no source NIF whose image URI has an invalid escape.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    let pack = root.path().join("pack");
    let scratch = root.path().join("scratch");
    let block = |name, diffuse, normal| write_nif(&data, name, diffuse, normal);
    const D: &str = "textures/fix/present.dds";
    const N: &str = "textures/fix/present_n.dds";
    let pruned = block("pruned", D, "textures/fix/absent_n.dds");
    let lost = block("lost", "textures/fix/lost.dds", N);
    let dropped = block("dropped", D, N);
    let unused = block("unused", D, N);
    let aliased = block("aliased", D, N);
    let flat = block("flat", D, N);
    let repaired = block("repaired", D, "textures/fix/repair_n.dds");
    block("repaired_unbound", D, "textures/fix/repair_n.dds");
    block("nometa", D, N);
    let dup = block("dup", D, N);
    for dds in ["present", "present_n", "lost"] {
        touch(&data, &format!("textures/fix/{dds}.dds"));
    }
    touch(&pack, "textures/fix/present.opensky-srgb.ktx2");
    touch(&pack, "textures/fix/present_n.ktx2");
    touch(&pack, "textures/fix/set_ok.ktx2");
    touch(&pack, "textures/dlc/repair_n.ktx2");
    let lost_image = "../../textures/fix/lost.opensky-srgb.ktx2";
    let aliased_image = "../../textures/fix/present_n.opensky-srgb.ktx2";
    fn single<'a>(
        images: &'a [&'a str],
        diffuse: Option<usize>,
        normal: Option<usize>,
        has_uvs: bool,
        shape_block: u32,
    ) -> Glb<'a> {
        Glb::single(images, diffuse, normal, has_uvs, Some(shape_block))
    }
    write_glb(
        &pack,
        "pruned",
        &single(&[PRESENT], Some(0), None, true, pruned),
    );
    write_glb(
        &pack,
        "lost",
        &single(&[lost_image, PRESENT_N], Some(0), Some(1), true, lost),
    );
    write_glb(
        &pack,
        "dropped",
        &single(&[PRESENT, PRESENT_N], Some(0), None, true, dropped),
    );
    write_glb(
        &pack,
        "unused",
        &Glb {
            images: &[PRESENT, PRESENT_N],
            materials: vec![
                Mat {
                    shape_block: Some(unused),
                    diffuse: Some(0),
                    normal: Some(1),
                },
                Mat {
                    shape_block: Some(unused),
                    diffuse: Some(0),
                    normal: None,
                },
            ],
            primitives: vec![1],
            has_uvs: true,
            repairs: None,
        },
    );
    write_glb(
        &pack,
        "aliased",
        &single(&[PRESENT, aliased_image], Some(0), Some(1), true, aliased),
    );
    write_glb(
        &pack,
        "flat",
        &single(&[PRESENT, PRESENT_N], Some(0), Some(1), false, flat),
    );
    write_glb(
        &pack,
        "repaired",
        &Glb {
            repairs: Some((REPAIR_N, REPAIRED_N)),
            ..Glb::single(
                &[PRESENT, REPAIRED_N],
                Some(0),
                Some(1),
                true,
                Some(repaired),
            )
        },
    );
    write_glb(
        &pack,
        "repaired_unbound",
        &Glb {
            repairs: Some((REPAIR_N, REPAIRED_N)),
            ..Glb::single(&[PRESENT, REPAIRED_N], Some(0), Some(1), true, None)
        },
    );
    write_glb(
        &pack,
        "nometa",
        &Glb::single(&[PRESENT, PRESENT_N], Some(0), Some(1), true, None),
    );
    let both = Mat {
        shape_block: Some(dup),
        diffuse: Some(0),
        normal: Some(1),
    };
    write_glb(
        &pack,
        "dup",
        &Glb {
            images: &[PRESENT, PRESENT_N],
            materials: vec![both, both],
            primitives: vec![0, 1],
            has_uvs: true,
            repairs: None,
        },
    );
    write_glb(
        &pack,
        "generated",
        &Glb::single(
            &["../../textures/fix/gone.opensky-srgb.ktx2"],
            Some(0),
            None,
            true,
            None,
        ),
    );
    write_glb(
        &pack,
        "refused",
        &Glb::single(
            &["../../textures/fix/stone%zz.ktx2"],
            Some(0),
            None,
            true,
            None,
        ),
    );
    (root, data, pack, scratch)
}

fn audit(
    data: &Path,
    pack: &Path,
    scratch: &Path,
    database: Option<&Path>,
) -> Result<Report, color_eyre::Report> {
    let sources = SourceCatalog::build(data, &[])?;
    audit_pack(&AuditOptions {
        pack,
        sources: &sources,
        scratch,
        database,
        mesh_filter: None,
    })
}

fn audit_fixture() -> Report {
    let (_root, data, pack, scratch) = fixture();
    audit(&data, &pack, &scratch, None).unwrap()
}

fn baseline(name: &str) -> Baseline {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/texture-audit")
        .join(name);
    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap()
}

fn of(report: &Report, cause: Cause) -> Vec<String> {
    report
        .findings
        .iter()
        .filter(|finding| finding.cause == cause)
        .map(|finding| format!("{} {} {}", finding.mesh, finding.slot, finding.wanted_path))
        .collect()
}

#[test]
fn each_cause_is_found_with_its_slot_and_source() {
    let report = audit_fixture();
    assert_eq!(
        of(&report, Cause::SourceTextureAbsent),
        [
            "meshes/fix/generated.glb image textures/fix/gone.dds",
            "meshes/fix/pruned.glb normal textures/fix/absent_n.dds",
            // The replacement is listed, but nothing shows that the shape binds it.
            "meshes/fix/repaired_unbound.glb normal textures/fix/repair_n.dds",
        ]
    );
    assert_eq!(
        of(&report, Cause::ConvertedTextureMissing),
        [
            "meshes/fix/aliased.glb normal textures/fix/present_n.dds",
            "meshes/fix/lost.glb diffuse textures/fix/lost.dds",
        ]
    );
    assert_eq!(
        of(&report, Cause::SlotUnreferenced),
        [
            "meshes/fix/dropped.glb normal textures/fix/present_n.dds",
            // The unused material that binds the normal does not clear the used one.
            "meshes/fix/unused.glb normal textures/fix/present_n.dds",
        ]
    );
    // No single used material can be tied to these shapes: none carries the shape block, or two
    // do. The textures exist, but the slots are not proven, so they are not resolved.
    assert_eq!(
        of(&report, Cause::BindingAmbiguous),
        [
            "meshes/fix/dup.glb diffuse textures/fix/present.dds",
            "meshes/fix/dup.glb normal textures/fix/present_n.dds",
            "meshes/fix/nometa.glb diffuse textures/fix/present.dds",
            "meshes/fix/nometa.glb normal textures/fix/present_n.dds",
            "meshes/fix/repaired_unbound.glb diffuse textures/fix/present.dds",
        ]
    );
    assert_eq!(
        of(&report, Cause::UnparseablePath),
        ["meshes/fix/refused.glb image ../../textures/fix/stone%zz.ktx2"]
    );
    assert_eq!(of(&report, Cause::PrimitiveWithoutUvs).len(), 1);
    assert_eq!(report.summary.unresolved_slots, 14);
    assert_eq!(report.summary.glb_files_audited_without_source, 2);
    let lost = report
        .findings
        .iter()
        .find(|finding| finding.mesh.ends_with("lost.glb"))
        .unwrap();
    assert!(lost.source_has_dds && !lost.pack_has_ktx2 && lost.glb_references_it);
    let pruned = report
        .findings
        .iter()
        .find(|finding| finding.mesh.ends_with("pruned.glb"))
        .unwrap();
    assert!(!pruned.source_has_dds && !pruned.glb_references_it);
    // The pack holds the base texture, but the exact file the GLB requests is absent.
    let aliased = report
        .findings
        .iter()
        .find(|finding| finding.mesh.ends_with("aliased.glb"))
        .unwrap();
    assert!(aliased.glb_references_it && !aliased.pack_has_ktx2);
}

#[test]
fn the_checked_in_baseline_holds_and_a_lower_one_fails() {
    let report = audit_fixture();
    assert_eq!(
        compare_to_baseline(&report, &baseline("baseline.json")),
        Vec::<String>::new()
    );
    assert_eq!(
        improvements(&report, &baseline("baseline.json")),
        Vec::<String>::new()
    );
    let mut stricter = baseline("baseline.json");
    stricter.max_by_cause.insert(Cause::SourceTextureAbsent, 0);
    let regressions = compare_to_baseline(&report, &stricter);
    assert_eq!(regressions.len(), 1);
    assert!(regressions[0].contains("SourceTextureAbsent"));
    // A baseline above the count is stale: the ratchet reports the value to lower it to.
    let mut stale = baseline("baseline.json");
    stale.max_by_cause.insert(Cause::SlotUnreferenced, 5);
    assert_eq!(
        improvements(&report, &stale),
        ["SlotUnreferenced: lower the baseline from 5 to 2"]
    );
}

#[test]
fn an_unreadable_mesh_makes_the_audit_incomplete_instead_of_better() {
    let (_root, data, pack, scratch) = fixture();
    // Truncate a GLB that has a finding: its finding disappears and only the failure count grows.
    let glb = pack.join("meshes/fix/pruned.glb");
    let bytes = fs::read(&glb).unwrap();
    fs::write(&glb, &bytes[..bytes.len() / 2]).unwrap();
    let report = audit(&data, &pack, &scratch, None).unwrap();
    assert_eq!(report.summary.nif_files_failed, 1);
    assert!(of(&report, Cause::SourceTextureAbsent).len() < 3);
    let problems = compare_to_baseline(&report, &baseline("baseline.json"));
    assert!(
        problems
            .iter()
            .any(|line| line.starts_with("incomplete audit: 1 meshes could not be read")),
        "{problems:?}"
    );
}

#[test]
fn a_relative_pack_path_gives_the_same_report() {
    let (_root, data, pack, scratch) = fixture();
    let absolute = audit(&data, &pack, &scratch, None).unwrap();
    // `pack/../pack` names the same directory through a path the resolver would otherwise
    // compare as text.
    let alias = pack.join("..").join("pack");
    let aliased = audit(&data, &alias, &scratch, None).unwrap();
    assert_eq!(
        absolute.summary.counts_by_cause,
        aliased.summary.counts_by_cause
    );
}

#[test]
fn loose_folders_with_capital_letters_are_read() {
    let (_root, data, pack, scratch) = fixture();
    // A case-sensitive filesystem can hold `Meshes`/`Textures`; the converter reads them.
    fs::rename(data.join("meshes"), data.join("Meshes_tmp")).unwrap();
    fs::rename(data.join("Meshes_tmp"), data.join("Meshes")).unwrap();
    fs::rename(data.join("textures"), data.join("Textures_tmp")).unwrap();
    fs::rename(data.join("Textures_tmp"), data.join("Textures")).unwrap();
    let report = audit(&data, &pack, &scratch, None).unwrap();
    assert_eq!(report.summary.glb_files_with_source_nif, 10);
    assert_eq!(of(&report, Cause::SourceTextureAbsent).len(), 3);
}

#[test]
fn an_archive_the_lister_cannot_read_is_recorded() {
    let (_root, data, pack, scratch) = fixture();
    fs::write(data.join("Broken.bsa"), b"not an archive").unwrap();
    let report = audit(&data, &pack, &scratch, None).unwrap();
    assert_eq!(report.summary.skipped_archives.len(), 1);
    assert!(report.summary.skipped_archives[0].starts_with("Broken.bsa"));
    let problems = compare_to_baseline(&report, &baseline("baseline.json"));
    assert!(
        problems
            .iter()
            .any(|line| line.contains("archive could not be listed"))
    );
}

#[test]
fn a_published_texture_repair_is_not_a_missing_texture() {
    let report = audit_fixture();
    assert!(
        report
            .findings
            .iter()
            .all(|finding| !finding.mesh.ends_with("repaired.glb")),
        "{:?}",
        report.findings
    );
}

#[test]
fn an_archive_of_an_inactive_plugin_is_left_out_and_an_unowned_one_is_kept() {
    use dummy_content::{Entry, bsa};
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir_all(&data).unwrap();
    let archive =
        |entry: &str| bsa::v105(&[Entry::new(entry, b"dds")], bsa::Compression::None).unwrap();
    // `Disabled.esp` is installed but not active, so the converter leaves `Disabled.bsa` out.
    fs::write(data.join("Disabled.esp"), b"plugin").unwrap();
    fs::write(
        data.join("Disabled.bsa"),
        archive("textures/fix/only_in_disabled.dds"),
    )
    .unwrap();
    // No plugin owns `Standalone.bsa`, so it stays eligible.
    fs::write(
        data.join("Standalone.bsa"),
        archive("textures/fix/standalone.dds"),
    )
    .unwrap();
    let inactive = SourceCatalog::build(&data, &[]).unwrap();
    assert_eq!(inactive.inactive_archives(), ["Disabled.bsa"]);
    assert!(
        inactive
            .texture("textures/fix/only_in_disabled.dds")
            .is_none()
    );
    assert!(inactive.texture("textures/fix/standalone.dds").is_some());
    assert!(inactive.skipped_archives().is_empty());
    // Once the plugin is active its archive counts.
    let active = SourceCatalog::build(&data, &["Disabled.esp".to_owned()]).unwrap();
    assert!(active.inactive_archives().is_empty());
    assert!(
        active
            .texture("textures/fix/only_in_disabled.dds")
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_mesh_folder_is_an_error_not_a_smaller_audit() {
    use std::os::unix::fs::PermissionsExt;
    let (_root, data, pack, scratch) = fixture();
    let locked = pack.join("meshes/locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("hidden.glb"), b"x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let result = audit(&data, &pack, &scratch, None);
    // Ask the filesystem whether the folder really is unreadable; a process that ignores
    // permissions (root) can read it.
    let unreadable = fs::read_dir(&locked).is_err();
    // Restore access first so the temporary folder can be removed even if an assert fails.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if unreadable {
        let error = result.expect_err("an unreadable mesh folder must fail the audit");
        assert!(format!("{error:#}").contains("failed to scan"), "{error:#}");
    }
}

/// A pack database with every table the audit reads, and one row per record-side cause.
fn write_database(pack: &Path) -> PathBuf {
    let path = pack.join("skyrim_world.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE plugins(id INTEGER PRIMARY KEY, name TEXT, priority INTEGER, checksum BLOB);
             CREATE TABLE texture_sets(id INTEGER PRIMARY KEY, editor_id TEXT, diffuse_path TEXT,
                normal_path TEXT, glow_path TEXT, height_path TEXT, environment_path TEXT,
                mask_path TEXT, specular_path TEXT, detail_path TEXT);
             CREATE TABLE waters(id INTEGER PRIMARY KEY, flow_normal_path TEXT);
             CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY, editor_id TEXT, texture_set_id INTEGER);
             CREATE TABLE inhouse_unresolved_terrain_textures(form_id INTEGER, texture_form_id INTEGER,
                PRIMARY KEY(form_id, texture_form_id));
             CREATE TABLE formid_map(form_id INTEGER PRIMARY KEY, plugin_name TEXT, internal_id INTEGER,
                record_type TEXT);
             CREATE TABLE base_texture_swaps(base_id INTEGER, position INTEGER, shape_name TEXT,
                shape_index INTEGER, texture_set_id INTEGER, disposition TEXT,
                PRIMARY KEY(base_id, position));
             CREATE TABLE statics(id INTEGER PRIMARY KEY, model_path TEXT);
             CREATE TABLE \"references\"(id INTEGER PRIMARY KEY, base_form_id INTEGER, cell_id INTEGER);
             CREATE TABLE cells(id INTEGER PRIMARY KEY, interior_name TEXT, full_name TEXT,
                grid_x INTEGER, grid_y INTEGER, worldspace_id INTEGER);
             CREATE TABLE worldspaces(id INTEGER PRIMARY KEY, editor_id TEXT);

             INSERT INTO plugins VALUES (0, 'Fix.esm', 0, x'00');
             -- 10 resolves; 11 names a texture that exists nowhere; 12 spells a `.` segment the
             -- engine refuses even though the source-path rules would accept it.
             INSERT INTO texture_sets(id, diffuse_path) VALUES
                (10, 'textures/fix/set_ok.dds'), (11, 'textures/fix/set_gone.dds'),
                (12, 'textures/./set_ok.dds');
             INSERT INTO waters VALUES (20, 'textures/fix/water_gone.dds');
             INSERT INTO landscape_textures VALUES (30, 'LtexOk', 10), (31, 'LtexNoSet', NULL);
             INSERT INTO inhouse_unresolved_terrain_textures VALUES (1000, 2116), (1001, 2116);
             INSERT INTO formid_map VALUES (1, 'fix.esm', 1, 'STAT');
             -- a live set, the NullTextureSet, a set no plugin defines, and a set that names a
             -- missing texture (still a live set).
             INSERT INTO base_texture_swaps VALUES
                (1, 0, 'a', 0, 10, 'texture_set'), (1, 1, 'b', 1, 0, 'null_texture_set'),
                (1, 2, 'c', 2, 99, 'missing_texture_set'), (1, 3, 'd', 3, 11, 'texture_set');
             INSERT INTO statics VALUES (1, 'fix\\flat.nif');
             INSERT INTO \"references\" VALUES (100, 1, 7), (101, 1, 7);
             INSERT INTO cells VALUES (7, 'FixCell', NULL, NULL, NULL, NULL);",
        )
        .unwrap();
    path
}

#[test]
fn record_side_causes_are_found_in_ci_through_a_database_fixture() {
    let (_root, data, pack, scratch) = fixture();
    let database = write_database(&pack);
    let report = audit(&data, &pack, &scratch, Some(&database)).unwrap();
    assert!(report.summary.records_audited);
    // Three TXST paths, one water path, one LTEX without a diffuse, one undefined LTEX.
    assert_eq!(report.summary.texture_set_paths, 3 + 1 + 1 + 1);
    assert_eq!(
        of(&report, Cause::TextureSetUnresolved),
        [
            "TXST 0000000B diffuse textures/fix/set_gone.dds",
            "WATR 00000014 flow_normal textures/fix/water_gone.dds",
        ]
    );
    // One refused image URI of a GLB and one TXST path the engine refuses.
    assert_eq!(
        of(&report, Cause::UnparseablePath),
        [
            "TXST 0000000C diffuse textures/./set_ok.dds",
            "meshes/fix/refused.glb image ../../textures/fix/stone%zz.ktx2",
        ]
    );
    assert_eq!(
        of(&report, Cause::LandTextureUnresolved),
        ["LTEX 0000001F diffuse ", "LTEX 00000844 diffuse "]
    );
    assert_eq!(
        of(&report, Cause::OverrideTargetUnresolved),
        ["STAT 00000001 3D index 2 TXST 00000063"]
    );
    let overrides = &report.overrides;
    assert!(overrides.engine_applies_entries);
    assert_eq!(overrides.entries, 4);
    assert_eq!(overrides.hide_shape_entries, 1);
    assert_eq!(overrides.entries_with_texture_paths, 2);
    assert_eq!(overrides.unresolved_target_entries, 1);
    // `used_by` follows the static placements: `flat` is placed twice in one cell.
    let flat = report
        .findings
        .iter()
        .find(|finding| finding.cause == Cause::PrimitiveWithoutUvs)
        .unwrap();
    assert_eq!(flat.used_by.references, 2);
    assert_eq!(flat.used_by.cells[0].label, "interior FixCell");
}

/// Audits a real pack against the real game data and compares with `full-pack-baseline.json`.
///
/// Run locally with the pack and the game data in place:
///
/// ```text
/// MUDCRAB_TEXTURE_AUDIT_PACK=<pack> MUDCRAB_GAME_DATA=<Skyrim Data> \
///   cargo test -p converter --test texture_audit -- --ignored full_pack
/// ```
///
/// `MUDCRAB_TEXTURE_AUDIT_OUT` names a folder for `report.json`. The test holds the pack's shared
/// asset lock while it reads. It fails when any cause grows, when the audit is incomplete (an
/// unreadable mesh or archive, no texture slot or texture set read), and when a count has fallen
/// below the baseline: lower the baseline then, so a later regression cannot hide under the old
/// ceiling.
#[test]
#[ignore = "needs a converted pack and the game's Data folder"]
fn full_pack_stays_within_its_baseline() {
    let pack = PathBuf::from(
        std::env::var_os("MUDCRAB_TEXTURE_AUDIT_PACK")
            .expect("set MUDCRAB_TEXTURE_AUDIT_PACK to a converted pack"),
    );
    let data = PathBuf::from(
        std::env::var_os("MUDCRAB_GAME_DATA").expect("set MUDCRAB_GAME_DATA to the Data folder"),
    );
    let _lock = converter::texture_audit::lock_pack(&pack).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let database = pack.join("skyrim_world.db");
    assert!(database.is_file(), "the pack has no skyrim_world.db");
    let plugin_names = converter::texture_audit::plugin_names(&database).unwrap();
    let sources = SourceCatalog::build(&data, &plugin_names).unwrap();
    let report = audit_pack(&AuditOptions {
        pack: &pack,
        sources: &sources,
        scratch: scratch.path(),
        database: Some(&database),
        mesh_filter: None,
    })
    .unwrap();
    if let Some(out) = std::env::var_os("MUDCRAB_TEXTURE_AUDIT_OUT") {
        let out = PathBuf::from(out);
        fs::create_dir_all(&out).unwrap();
        fs::write(
            out.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        fs::write(
            out.join("report.csv"),
            converter::texture_audit::findings_csv(&report),
        )
        .unwrap();
    }
    let baseline = baseline("full-pack-baseline.json");
    let regressions = compare_to_baseline(&report, &baseline);
    assert!(
        regressions.is_empty(),
        "unresolved texture slots grew or the audit is incomplete: {regressions:#?}\nsummary: {}",
        serde_json::to_string_pretty(&report.summary).unwrap()
    );
    let lower = improvements(&report, &baseline);
    assert!(
        lower.is_empty(),
        "the baseline is above the count; lower it in full-pack-baseline.json: {lower:#?}"
    );
}
