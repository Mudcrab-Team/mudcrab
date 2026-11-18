//! The texture audit finds each way a surface can lose its texture, and a baseline ratchets the
//! count down. The CI test builds a tiny game-data directory and pack; the ignored test audits a
//! real pack against the real game data.

use converter::texture_audit::{
    AuditOptions, Baseline, Cause, SourceCatalog, audit_pack, compare_to_baseline,
};
use dummy_content::nif::{StaticShape, static_shape};
use std::{
    fs,
    path::{Path, PathBuf},
};

const POSITIONS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
const NORMALS: [[f32; 3]; 3] = [[0.0, 0.0, 1.0]; 3];
const UVS: [[f32; 2]; 3] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];

fn write_nif(data: &Path, name: &str, diffuse: &str, normal: &str) {
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
    fs::write(path, bytes).unwrap();
}

/// A GLB with only a JSON chunk: the audit reads nothing else.
fn write_glb(pack: &Path, name: &str, images: &[&str], has_uvs: bool) {
    let textures: Vec<_> = (0..images.len())
        .map(|index| serde_json::json!({"source": index}))
        .collect();
    let mut attributes = serde_json::json!({"POSITION": 0});
    if has_uvs {
        attributes["TEXCOORD_0"] = serde_json::json!(1);
    }
    let mut material = serde_json::json!({
        "name": "m",
        "pbrMetallicRoughness": {},
        "extras": {"openSkyrim": {"shaderFamily": "lighting"}}
    });
    if !images.is_empty() {
        material["pbrMetallicRoughness"]["baseColorTexture"] = serde_json::json!({"index": 0});
    }
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "images": images.iter().map(|uri| serde_json::json!({"uri": uri})).collect::<Vec<_>>(),
        "textures": textures,
        "materials": [material],
        "meshes": [{"name": name, "primitives": [{"attributes": attributes, "material": 0}]}]
    });
    let mut json = serde_json::to_vec(&document).unwrap();
    while json.len() % 4 != 0 {
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

/// Four meshes, one per cause:
///
/// * `pruned`: wants `absent_n.dds`, which the game data does not hold; the converter pruned it.
/// * `lost`: wants `lost.dds`, which the game data holds but the pack never converted.
/// * `dropped`: wants `present_n.dds`, which the pack holds but the GLB does not reference.
/// * `flat`: every texture resolves, but the primitive has no UVs.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    let pack = root.path().join("pack");
    let scratch = root.path().join("scratch");
    write_nif(&data, "pruned", "textures/fix/present.dds", "textures/fix/absent_n.dds");
    write_nif(&data, "lost", "textures/fix/lost.dds", "textures/fix/present_n.dds");
    write_nif(&data, "dropped", "textures/fix/present.dds", "textures/fix/present_n.dds");
    write_nif(&data, "flat", "textures/fix/present.dds", "textures/fix/present_n.dds");
    for dds in ["present", "present_n", "lost"] {
        touch(&data, &format!("textures/fix/{dds}.dds"));
    }
    touch(&pack, "textures/fix/present.opensky-srgb.ktx2");
    touch(&pack, "textures/fix/present_n.ktx2");
    let present = "../../textures/fix/present.opensky-srgb.ktx2";
    let present_n = "../../textures/fix/present_n.ktx2";
    let lost = "../../textures/fix/lost.opensky-srgb.ktx2";
    write_glb(&pack, "pruned", &[present], true);
    write_glb(&pack, "lost", &[lost, present_n], true);
    write_glb(&pack, "dropped", &[present], true);
    write_glb(&pack, "flat", &[present, present_n], false);
    (root, data, pack, scratch)
}

fn audit_fixture() -> converter::texture_audit::Report {
    let (_root, data, pack, scratch) = fixture();
    let sources = SourceCatalog::build(&data, &[]).unwrap();
    audit_pack(&AuditOptions {
        pack: &pack,
        sources: &sources,
        scratch: &scratch,
        database: None,
        mesh_filter: None,
    })
    .unwrap()
}

fn baseline(name: &str) -> Baseline {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/texture-audit")
        .join(name);
    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap()
}

#[test]
fn each_cause_is_found_with_its_slot_and_source() {
    let report = audit_fixture();
    let of = |cause: Cause| -> Vec<String> {
        report
            .findings
            .iter()
            .filter(|finding| finding.cause == cause)
            .map(|finding| format!("{} {} {}", finding.mesh, finding.slot, finding.wanted_path))
            .collect()
    };
    assert_eq!(
        of(Cause::SourceTextureAbsent),
        ["meshes/fix/pruned.glb normal textures/fix/absent_n.dds"]
    );
    assert_eq!(
        of(Cause::ConvertedTextureMissing),
        ["meshes/fix/lost.glb diffuse textures/fix/lost.dds"]
    );
    assert_eq!(
        of(Cause::SlotUnreferenced),
        ["meshes/fix/dropped.glb normal textures/fix/present_n.dds"]
    );
    assert_eq!(of(Cause::PrimitiveWithoutUvs).len(), 1);
    assert_eq!(report.summary.unresolved_slots, 4);
    let lost = report
        .findings
        .iter()
        .find(|finding| finding.cause == Cause::ConvertedTextureMissing)
        .unwrap();
    assert!(lost.source_has_dds && !lost.pack_has_ktx2 && lost.glb_references_it);
    let pruned = report
        .findings
        .iter()
        .find(|finding| finding.cause == Cause::SourceTextureAbsent)
        .unwrap();
    assert!(!pruned.source_has_dds && !pruned.glb_references_it);
}

#[test]
fn the_checked_in_baseline_holds_and_a_lower_one_fails() {
    let report = audit_fixture();
    assert_eq!(
        compare_to_baseline(&report, &baseline("baseline.json")),
        Vec::<String>::new()
    );
    let mut stricter = baseline("baseline.json");
    stricter.max_by_cause.insert(Cause::SourceTextureAbsent, 0);
    let regressions = compare_to_baseline(&report, &stricter);
    assert_eq!(regressions.len(), 1);
    assert!(regressions[0].contains("SourceTextureAbsent"));
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
/// `MUDCRAB_TEXTURE_AUDIT_OUT` names a folder for `report.json`. Ratchet the baseline down when
/// causes are fixed; the test fails when any cause grows.
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
    let scratch = tempfile::tempdir().unwrap();
    let database = pack.join("skyrim_world.db");
    let plugin_names = {
        let connection = rusqlite::Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = connection
            .prepare("SELECT name FROM plugins ORDER BY priority")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let sources = SourceCatalog::build(&data, &plugin_names).unwrap();
    let report = audit_pack(&AuditOptions {
        pack: &pack,
        sources: &sources,
        scratch: scratch.path(),
        database: database.is_file().then_some(database.as_path()),
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
    let regressions = compare_to_baseline(&report, &baseline("full-pack-baseline.json"));
    assert!(
        regressions.is_empty(),
        "unresolved texture slots grew: {regressions:#?}\nsummary: {}",
        serde_json::to_string_pretty(&report.summary).unwrap()
    );
}
