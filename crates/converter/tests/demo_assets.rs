//! Materialises the minimal converted actor tree the engine retail tests load.
//!
//! The engine's retail tests are `#[ignore]`d because they need a converted tree on disk. This
//! test builds that tree from retail Skyrim data: both composed default bodies, the three gait
//! clips per sex over the composed bone tree, and the FaceGen heads.

use converter::{
    animation::{decode_clip_with_skeleton, write_clip_glb},
    compose::{BodyPart, BodyPartKind, compose_body, write_composed_body},
    mesh::MeshConverter,
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

const DEFAULT_ACTOR_NIF_DIR: &str =
    "/Users/dev/Documents/fun/rust/skyrust/scratch/assets/bsa-meshes/meshes/actors";
const DEFAULT_ANIMATION_DIR: &str = "/Users/dev/Documents/fun/rust/skyrust/scratch/assets/animations-extract/meshes/actors/character";

const GAITS: [&str; 3] = ["idle", "walkforward", "runforward"];
const HEADS: [&str; 2] = ["00013475", "0010f5a1"];

fn actor_nif_dir() -> PathBuf {
    std::env::var_os("MUDCRAB_ACTOR_NIF_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ACTOR_NIF_DIR))
}

fn animation_dir() -> PathBuf {
    std::env::var_os("MUDCRAB_ANIMATION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ANIMATION_DIR))
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
    GlbSummary {
        nodes,
        skins,
        skinned_meshes,
        channels,
    }
}

#[test]
#[ignore = "materialises the converted actor assets the engine retail tests load"]
fn materialises_the_converted_actor_assets() {
    let Some(output) = std::env::var_os("MUDCRAB_DEMO_ASSETS").map(PathBuf::from) else {
        return;
    };
    let actor_root = actor_nif_dir();
    let animation_root = animation_dir();
    let character_assets = actor_root.join("character/character assets");
    let skeleton_nif = character_assets.join("skeleton.nif");
    let skeleton_hkx = animation_root.join("character assets/skeleton.hkx");
    let skeleton_bytes = fs::read(&skeleton_hkx)
        .unwrap_or_else(|error| panic!("read {}: {error}", skeleton_hkx.display()));

    assert!(skeleton_nif.is_file(), "missing {}", skeleton_nif.display());

    for sex in ["male", "female"] {
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
                BodyPart { path, kind: *kind }
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
        let body = read_glb_summary(&body_path);
        println!(
            "body sex={sex} nodes={} skins={} skinned_meshes={} bones={}",
            body.nodes,
            body.skins,
            body.skinned_meshes,
            bones.bones.len()
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

    for head in HEADS {
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

    println!("converted actor tree written to {}", output.display());
}
