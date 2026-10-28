//! Materialises the minimal converted actor tree the engine retail tests load.
//!
//! The engine's retail tests are `#[ignore]`d because they need a converted tree on disk. This
//! test builds that tree from retail Skyrim data: both composed default bodies, the three gait
//! clips per sex over the composed bone tree, the FaceGen heads, and the DDS textures those GLBs
//! reference, converted to KTX2 from the retail `Data` folder. When that folder holds
//! `Skyrim.esm`, its actor catalog supplies the bodies' armature skin texture sets.
//!
//! Inputs come from the environment and the test does nothing unless all four are set:
//! `MUDCRAB_DEMO_ASSETS` (output tree), `MUDCRAB_ACTOR_NIF_DIR` (extracted `meshes/actors`),
//! `MUDCRAB_ANIMATION_DIR` (extracted `meshes/actors/character`) and `MUDCRAB_TEXTURE_DIR` (the
//! retail `Data` folder holding the texture archives).
//!
//! `MUDCRAB_DEMO_EXTRA_HEADS` optionally names more `Skyrim.esm` FaceGen heads to convert into the
//! same tree, as comma-separated FormIDs (`00013bbf,0001bdb1`), for previewing other NPCs.

use converter::{
    animation::{decode_clip_with_skeleton, write_clip_glb},
    archive::ArchiveExtractor,
    asset_path::{resolve_asset_uri, runtime_texture_source},
    compose::{
        BodyPart, BodyPartKind, compose_body, default_skin_texture_sets, skin_texture_set_for,
        write_composed_body,
    },
    esm::EsmParser,
    mesh::MeshConverter,
    texture::{TextureConverter, TextureEncoding, TextureSemantic},
};
use dummy_content::{Entry, ba2, bsa};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const GAITS: [&str; 3] = ["idle", "walkforward", "runforward"];
/// Alvor's and Sigrid's FaceGen heads, the pair `actor_preview` renders by default.
const HEADS: [&str; 2] = ["00013475", "00013476"];
/// Converted alongside `HEADS` because the engine's `retail_head_follows_the_animated_body_bone`
/// loads this head from the same tree.
const RETAIL_TEST_HEADS: [&str; 1] = ["0010f5a1"];

/// The FormIDs listed in `MUDCRAB_DEMO_EXTRA_HEADS`, lower-cased like the converted head paths.
fn extra_heads() -> Vec<String> {
    let Some(value) = std::env::var_os("MUDCRAB_DEMO_EXTRA_HEADS") else {
        return Vec::new();
    };
    value
        .to_string_lossy()
        .split(',')
        .map(|form| form.trim().trim_start_matches("0x").to_ascii_lowercase())
        .filter(|form| !form.is_empty())
        .map(|form| {
            assert!(
                form.len() == 8 && form.chars().all(|c| c.is_ascii_hexdigit()),
                "MUDCRAB_DEMO_EXTRA_HEADS entry `{form}` is not an 8-digit hexadecimal FormID"
            );
            form
        })
        .collect()
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

struct TextureRequest {
    aliases: BTreeSet<String>,
    semantics: BTreeSet<TextureSemantic>,
}

fn relative_key(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or_else(|_| panic!("{} is outside {}", path.display(), root.display()))
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn texture_requests(output: &Path, glbs: &[PathBuf]) -> BTreeMap<String, TextureRequest> {
    let mut requests: BTreeMap<String, TextureRequest> = BTreeMap::new();
    for glb in glbs {
        let dependencies = MeshConverter::glb_texture_dependencies(glb)
            .unwrap_or_else(|error| panic!("read textures of {}: {error:#}", glb.display()));
        for dependency in dependencies {
            let runtime_path =
                resolve_asset_uri(output, glb, &dependency.uri).unwrap_or_else(|error| {
                    panic!(
                        "resolve {} from {}: {error:#}",
                        dependency.uri,
                        glb.display()
                    )
                });
            let runtime_key = relative_key(output, &runtime_path);
            let canonical = runtime_texture_source(&runtime_key)
                .unwrap_or_else(|| panic!("{runtime_key} is not a KTX2 texture alias"));
            let request = requests.entry(canonical).or_insert_with(|| TextureRequest {
                aliases: BTreeSet::new(),
                semantics: BTreeSet::new(),
            });
            request.aliases.insert(runtime_key);
            request.semantics.insert(dependency.semantic);
        }
    }
    requests
}

fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::symlink_metadata(to) {
        Ok(_) => {
            // Removing the source itself would lose its bytes; a dangling destination symlink
            // must still be removed before the copy fallback can follow it.
            if same_path(from, to) {
                return Ok(());
            }
            fs::remove_file(to)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::hard_link(from, to).or_else(|_| fs::copy(from, to).map(|_| ()))
}

/// Whether `from` and `to` name the same file, so removing `to` cannot delete `from`.
fn same_path(from: &Path, to: &Path) -> bool {
    match fs::canonicalize(from) {
        Ok(from) => fs::canonicalize(to).is_ok_and(|to| to == from),
        Err(_) => false,
    }
}

#[test]
fn link_or_copy_retries_an_existing_hard_link_without_changing_source_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dds");
    let destination = directory.path().join(".texture-source/source.dds");
    fs::write(&source, b"synthetic texture bytes").unwrap();
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::hard_link(&source, &destination).unwrap();

    link_or_copy(&source, &destination).unwrap();
    link_or_copy(&source, &destination).unwrap();

    assert_eq!(fs::read(&source).unwrap(), b"synthetic texture bytes");
    assert_eq!(fs::read(&destination).unwrap(), b"synthetic texture bytes");
}

#[test]
fn link_or_copy_replaces_stale_regular_destination() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dds");
    let destination = directory.path().join("destination.dds");
    fs::write(&source, b"synthetic texture bytes").unwrap();
    fs::write(&destination, b"stale bytes").unwrap();

    link_or_copy(&source, &destination).unwrap();

    assert_eq!(fs::read(&source).unwrap(), b"synthetic texture bytes");
    assert_eq!(fs::read(&destination).unwrap(), b"synthetic texture bytes");
}

#[cfg(unix)]
#[test]
fn link_or_copy_replaces_destination_symlink_without_changing_target() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dds");
    let target = directory.path().join("symlink-target.dds");
    let destination = directory.path().join("destination.dds");
    fs::write(&source, b"synthetic texture bytes").unwrap();
    fs::write(&target, b"symlink target bytes").unwrap();
    symlink(&target, &destination).unwrap();

    link_or_copy(&source, &destination).unwrap();

    assert_eq!(fs::read(&source).unwrap(), b"synthetic texture bytes");
    assert_eq!(fs::read(&destination).unwrap(), b"synthetic texture bytes");
    assert_eq!(fs::read(&target).unwrap(), b"symlink target bytes");
}

#[test]
fn link_or_copy_keeps_directory_destination_and_returns_cleanup_error() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dds");
    let destination = directory.path().join("destination.dds");
    fs::write(&source, b"synthetic texture bytes").unwrap();
    fs::create_dir(&destination).unwrap();

    assert!(link_or_copy(&source, &destination).is_err());
    assert!(destination.is_dir());
    assert_eq!(fs::read(&source).unwrap(), b"synthetic texture bytes");
}

/// Orders the retail archives so texture banks are read first; the rest are only needed if a
/// texture lives outside them.
fn archive_paths(root: &Path) -> Vec<PathBuf> {
    let mut archives: Vec<PathBuf> = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            matches!(
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("bsa" | "ba2")
            )
        })
        .collect();
    archives.sort_by_key(|path| {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        (!name.contains("texture"), name)
    });
    archives
}

fn stage_textures(source: &Path, staging: &Path, needed: &mut BTreeSet<PathBuf>) {
    let archives = archive_paths(source);
    for archive in archives {
        if needed.is_empty() {
            break;
        }
        let found = ArchiveExtractor::extract_paths(&archive, staging, needed)
            .unwrap_or_else(|error| panic!("extract from {}: {error:#}", archive.display()));
        for file in found {
            needed.remove(&file.path);
        }
    }

    let loose_files: Vec<_> = needed
        .iter()
        .filter(|relative| source.join(relative).is_file())
        .cloned()
        .collect();
    for relative in loose_files {
        let from = source.join(&relative);
        let to = staging.join(&relative);
        link_or_copy(&from, &to).unwrap_or_else(|error| panic!("write {}: {error}", to.display()));
        needed.remove(&relative);
    }
}

#[test]
fn stage_textures_falls_back_to_loose_files_after_bsa_and_ba2() {
    let archived_path = PathBuf::from("textures/archive.dds");
    let loose_path = PathBuf::from("textures/loose.dds");
    let missing_path = PathBuf::from("textures/missing.dds");
    let archived_bytes = b"archive texture bytes";
    let loose_bytes = b"loose texture bytes";
    let archive_entries = [Entry::new("textures/archive.dds", archived_bytes)];
    let fixtures = [
        (
            "texture-bank.bsa",
            bsa::v105(&archive_entries, bsa::Compression::None).unwrap(),
        ),
        (
            "texture-bank.ba2",
            ba2::general(&archive_entries, ba2::Compression::None).unwrap(),
        ),
    ];

    for (archive_name, archive_bytes) in fixtures {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staging = directory.path().join("staging");
        fs::create_dir_all(source.join("textures")).unwrap();
        fs::write(source.join(archive_name), archive_bytes).unwrap();
        fs::write(source.join(&archived_path), b"conflicting loose bytes").unwrap();
        fs::write(source.join(&loose_path), loose_bytes).unwrap();
        let mut needed = BTreeSet::from([
            archived_path.clone(),
            loose_path.clone(),
            missing_path.clone(),
        ]);

        stage_textures(&source, &staging, &mut needed);

        assert_eq!(
            fs::read(staging.join(&archived_path)).unwrap(),
            archived_bytes
        );
        assert_eq!(fs::read(staging.join(&loose_path)).unwrap(), loose_bytes);
        assert!(!staging.join(&missing_path).exists());
        assert_eq!(needed, BTreeSet::from([missing_path.clone()]));
    }
}

#[test]
fn stage_textures_stages_loose_files_without_archives() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let staging = directory.path().join("staging");
    let relative = PathBuf::from("textures/loose.dds");
    fs::create_dir_all(source.join("textures")).unwrap();
    fs::write(source.join(&relative), b"loose texture bytes").unwrap();
    let mut needed = BTreeSet::from([relative.clone()]);

    stage_textures(&source, &staging, &mut needed);

    assert_eq!(
        fs::read(staging.join(&relative)).unwrap(),
        b"loose texture bytes"
    );
    assert!(needed.is_empty());
}

#[test]
fn stage_textures_keeps_missing_path_needed_when_staging_has_stale_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let staging = directory.path().join("staging");
    let relative = PathBuf::from("textures/missing.dds");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(staging.join("textures")).unwrap();
    fs::write(staging.join(&relative), b"stale bytes from interrupted run").unwrap();
    let mut needed = BTreeSet::from([relative.clone()]);

    stage_textures(&source, &staging, &mut needed);

    assert_eq!(needed, BTreeSet::from([relative.clone()]));
    assert_eq!(
        fs::read(staging.join(&relative)).unwrap(),
        b"stale bytes from interrupted run"
    );
}

fn materialise_textures(source: &Path, output: &Path, glbs: &[PathBuf]) {
    let requests = texture_requests(output, glbs);
    assert!(
        !requests.is_empty(),
        "no written GLB references a texture; the demo would stay untextured"
    );
    let staging = output.join(".texture-source");
    let mut needed: BTreeSet<PathBuf> = requests
        .keys()
        .map(|canonical| PathBuf::from(canonical).with_extension("dds"))
        .collect();
    stage_textures(source, &staging, &mut needed);
    assert!(
        needed.is_empty(),
        "textures missing from {}: {needed:?}",
        source.display()
    );

    let mut converted = 0usize;
    let mut aliases = 0usize;
    for (canonical, request) in &requests {
        let source_dds = staging.join(PathBuf::from(canonical).with_extension("dds"));
        let ktx2 = output.join(canonical);
        let encoding = TextureEncoding::from_semantics(&request.semantics)
            .unwrap_or_else(|error| panic!("choose encoding for {canonical}: {error:#}"));
        TextureConverter::convert_dds_to_ktx2(&source_dds, &ktx2, encoding)
            .unwrap_or_else(|error| panic!("convert {canonical}: {error:#}"));
        assert_written(&ktx2);
        converted += 1;
        for alias in &request.aliases {
            let alias_path = output.join(alias);
            if alias_path == ktx2 {
                continue;
            }
            link_or_copy(&ktx2, &alias_path)
                .unwrap_or_else(|error| panic!("write {}: {error}", alias_path.display()));
            assert_written(&alias_path);
            aliases += 1;
        }
    }
    fs::remove_dir_all(&staging).ok();
    println!(
        "textures: {converted} converted, {aliases} aliases, source {}",
        source.display()
    );
}

fn glb_path(output: &Path, relative: &str) -> PathBuf {
    output.join(relative)
}

fn assert_written(path: &Path) {
    let metadata = fs::metadata(path)
        .unwrap_or_else(|error| panic!("{} was not written: {error}", path.display()));
    assert!(metadata.len() > 0, "{} must not be empty", path.display());
}

struct GlbSummary {
    nodes: usize,
    skins: usize,
    skinned_meshes: usize,
    channels: usize,
    base_color_materials: usize,
}

fn read_glb_summary(path: &Path) -> GlbSummary {
    let bytes = fs::read(path).expect("read the written GLB");
    assert_eq!(&bytes[..4], b"glTF", "{} is not a GLB", path.display());
    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(
        &bytes[16..20],
        b"JSON",
        "{} has no JSON chunk",
        path.display()
    );
    let json: Value =
        serde_json::from_slice(&bytes[20..20 + json_length]).expect("parse the GLB JSON chunk");
    let nodes = json["nodes"].as_array().map_or(0, Vec::len);
    let skins = json["skins"].as_array().map_or(0, Vec::len);
    let skinned_meshes = json["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["skin"].is_number() && node["mesh"].is_number())
        .count();
    let channels = json["animations"]
        .as_array()
        .and_then(|animations| animations.first())
        .and_then(|animation| animation["channels"].as_array())
        .map_or(0, Vec::len);
    let base_color_materials = json["materials"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|material| {
            material["pbrMetallicRoughness"]
                .get("baseColorTexture")
                .is_some()
        })
        .count();
    GlbSummary {
        nodes,
        skins,
        skinned_meshes,
        channels,
        base_color_materials,
    }
}

#[test]
#[ignore = "materialises the converted actor assets the engine retail tests load"]
fn materialises_the_converted_actor_assets() {
    let (Some(output), Some(actor_root), Some(animation_root), Some(texture_root)) = (
        env_dir("MUDCRAB_DEMO_ASSETS"),
        env_dir("MUDCRAB_ACTOR_NIF_DIR"),
        env_dir("MUDCRAB_ANIMATION_DIR"),
        env_dir("MUDCRAB_TEXTURE_DIR"),
    ) else {
        return;
    };
    assert!(
        !output.as_os_str().is_empty(),
        "MUDCRAB_DEMO_ASSETS must not be empty"
    );
    let character_assets = actor_root.join("character/character assets");
    let skeleton_nif = character_assets.join("skeleton.nif");
    let skeleton_hkx = animation_root.join("character assets/skeleton.hkx");
    let skeleton_bytes = fs::read(&skeleton_hkx)
        .unwrap_or_else(|error| panic!("read {}: {error}", skeleton_hkx.display()));
    let mut written_glbs = Vec::new();

    assert!(skeleton_nif.is_file(), "missing {}", skeleton_nif.display());

    // The armature skin texture sets come from the actor catalog, as in a full conversion.
    let catalog_dir = tempfile::tempdir().unwrap();
    let catalog = catalog_dir.path().join("skyrim_world.db");
    let esm = texture_root.join("Skyrim.esm");
    let has_catalog = esm.is_file();
    if has_catalog {
        EsmParser::convert_plugins(std::slice::from_ref(&esm), &catalog)
            .unwrap_or_else(|error| panic!("export {}: {error:#}", esm.display()));
    } else {
        println!(
            "no {}: composed bodies keep the NIF textures",
            esm.display()
        );
    }

    for sex in ["male", "female"] {
        let skins = if has_catalog {
            default_skin_texture_sets(&catalog, sex)
                .unwrap_or_else(|error| panic!("read the {sex} skin texture sets: {error:#}"))
        } else {
            Vec::new()
        };
        for skin in &skins {
            println!(
                "skin sex={sex} slot={} texture_set={:08X} model={}",
                skin.kind, skin.textures.form_id, skin.model_path
            );
        }
        let parts = [
            (
                character_assets.join(format!("{sex}body_1.nif")),
                BodyPartKind::Body,
            ),
            (
                character_assets.join(format!("{sex}hands_1.nif")),
                BodyPartKind::Hands,
            ),
            (
                character_assets.join(format!("{sex}feet_1.nif")),
                BodyPartKind::Feet,
            ),
        ];
        let body_parts: Vec<BodyPart<'_>> = parts
            .iter()
            .map(|(path, kind)| {
                assert!(path.is_file(), "missing {}", path.display());
                BodyPart {
                    path,
                    kind: *kind,
                    skin_textures: skin_texture_set_for(&skins, *kind, path),
                }
            })
            .collect();
        let model = compose_body(&body_parts, &skeleton_nif)
            .unwrap_or_else(|error| panic!("compose the {sex} body: {error:#}"));
        let bones = model
            .skeletal_meshes
            .first()
            .expect("a composed body carries skeletal meshes")
            .bones
            .clone();
        assert_eq!(bones.bones.len(), 99, "{sex} composed bone count");

        let body_relative = format!("meshes/actors/character/composed/default/{sex}/body.glb");
        let body_path = glb_path(&output, &body_relative);
        write_composed_body(&model, &body_path)
            .unwrap_or_else(|error| panic!("write {body_relative}: {error:#}"));
        assert_written(&body_path);
        written_glbs.push(body_path.clone());
        let body = read_glb_summary(&body_path);
        assert!(
            body.base_color_materials > 0,
            "{sex} composed body must publish a baseColorTexture"
        );
        println!(
            "body sex={sex} nodes={} skins={} skinned_meshes={} bones={} base_color_materials={}",
            body.nodes,
            body.skins,
            body.skinned_meshes,
            bones.bones.len(),
            body.base_color_materials
        );

        for gait in GAITS {
            let clip_path = animation_root.join(format!("animations/{sex}/mt_{gait}.hkx"));
            let clip_bytes = fs::read(&clip_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", clip_path.display()));
            let mut clip = decode_clip_with_skeleton(&clip_bytes, &skeleton_bytes)
                .unwrap_or_else(|error| panic!("decode {}: {error:#}", clip_path.display()));
            let clip_name = format!("mt_{gait}");
            let clip_relative = format!("meshes/actors/character/animations/{sex}/{clip_name}.glb");
            let clip_output = glb_path(&output, &clip_relative);
            let report = write_clip_glb(&mut clip, &bones, &clip_name, &clip_output)
                .unwrap_or_else(|error| panic!("write {clip_relative}: {error:#}"));
            assert_written(&clip_output);
            assert!(
                report.bound() > 0,
                "{clip_name} for {sex} binds no body bone"
            );
            let summary = read_glb_summary(&clip_output);
            println!(
                "clip sex={sex} name={clip_name} sampled={:.6}s frames={} bound={} aliased={} skipped={} nodes={} channels={}",
                clip.duration_seconds,
                clip.frame_count,
                report.bound(),
                report.aliased.len(),
                report.skipped.len(),
                summary.nodes,
                summary.channels,
            );
        }
    }

    let mut heads: Vec<String> = HEADS
        .into_iter()
        .chain(RETAIL_TEST_HEADS)
        .map(str::to_owned)
        .collect();
    for head in extra_heads() {
        if !heads.contains(&head) {
            heads.push(head);
        }
    }
    for head in &heads {
        let head_nif = actor_root
            .join("character/facegendata/facegeom/skyrim.esm")
            .join(format!("{head}.nif"));
        assert!(head_nif.is_file(), "missing {}", head_nif.display());
        let head_relative =
            format!("meshes/actors/character/facegendata/facegeom/skyrim.esm/{head}.glb");
        let head_path = glb_path(&output, &head_relative);
        MeshConverter::convert_nif_to_glb(&head_nif, &head_path)
            .unwrap_or_else(|error| panic!("convert {}: {error:#}", head_nif.display()));
        assert_written(&head_path);
        written_glbs.push(head_path.clone());
        let summary = read_glb_summary(&head_path);
        assert!(
            summary.skins > 0 && summary.skinned_meshes > 0,
            "{head} must carry a skinned mesh"
        );
        println!(
            "head form={head} nodes={} skins={} skinned_meshes={}",
            summary.nodes, summary.skins, summary.skinned_meshes
        );
    }

    materialise_textures(&texture_root, &output, &written_glbs);
    println!("converted actor tree written to {}", output.display());
}
