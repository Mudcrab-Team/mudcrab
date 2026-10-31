//! Qualification belongs to the certified occurrence, not every mesh in its GLB.
use super::*;
use crate::cluster::ownership::OwnershipFacts;
use converter::{
    cache::{CacheEntry, ConversionManifest, hash_bytes},
    cluster::{
        LodMode,
        authority::CoreAuthority,
        probe::ProbeResult,
        publish::{PublishContext, finalize_base},
        stage::{StageSettings, run_stage},
    },
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use shared::asset_lock::AssetLock;

/// Keep the existing fixture's invented core records and grid geometry, then
/// publish a fresh certificate for a separately authored, refused flame sibling.
/// No certified source, payload, coverage row or hash is patched after publication.
fn mixed_fixture() -> Fixture {
    let original = published_for_loader();
    let model_path = "meshes/arch/rock.glb";
    let bytes = fs::read(original.path().join(model_path)).unwrap();
    let mut glb = gltf::binary::Glb::from_slice(&bytes).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&glb.json).unwrap();
    let mut flame_material = document["materials"][0].clone();
    flame_material["alphaMode"] = json!("BLEND");
    document["materials"]
        .as_array_mut()
        .unwrap()
        .push(flame_material);
    let mut flame_mesh = document["meshes"][0].clone();
    flame_mesh["primitives"][0]["material"] = json!(1);
    document["meshes"].as_array_mut().unwrap().push(flame_mesh);
    document["nodes"] = json!([
        {"name":"opaque body", "mesh":0, "children":[3]},
        {"name":"authored flame pivot", "children":[2],
         "extras":{"openSkyrim":{"billboard":"rotateAboutUp"}}},
        {"name":"refused flame", "mesh":1},
        {"name":"body gameplay anchor", "translation":[0,2,0]}
    ]);
    document["scenes"][0]["nodes"] = json!([0, 1]);
    glb.json = serde_json::to_vec(&document).unwrap().into();
    let bytes = glb.to_vec().unwrap();
    let primitives = converter::cluster::glb_input::load_model(&bytes, model_path).unwrap();
    assert_eq!(primitives.len(), 2);
    assert!(primitives[0].reject.is_none());
    assert!(
        primitives[1].reject.is_some(),
        "the sibling is converter-refused"
    );

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join("OWNER.txt"),
        "template qualification regression fixture\n",
    )
    .unwrap();
    fs::copy(
        original.path().join("skyrim_world.db"),
        root.join("skyrim_world.db"),
    )
    .unwrap();
    fs::create_dir_all(root.join("meshes/arch")).unwrap();
    fs::write(root.join(model_path), &bytes).unwrap();
    let lock = AssetLock::acquire_exclusive(root).unwrap();
    let mut manifest = ConversionManifest::default();
    manifest.entries.insert(
        model_path.to_owned(),
        CacheEntry {
            source_hash: hash_bytes(&bytes),
            output: model_path.to_owned(),
            output_size: bytes.len() as u64,
            output_hash: hash_bytes(&bytes),
            animation_diagnostics: None,
        },
    );
    let db = Connection::open_with_flags(
        root.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let child_worlds = db
        .prepare("SELECT id FROM worldspaces WHERE parent_world IS NOT NULL")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<u32>, _>>()
        .unwrap();
    let settings = StageSettings {
        mode: LodMode::Cluster,
        triangle_threshold: 1,
        jobs: 1,
        assets_root: None,
        read_database: None,
        area: None,
        child_worldspaces: child_worlds,
        include_trees: false,
        map_references: true,
    };
    let probe = ProbeResult {
        passed: true,
        missing: Vec::new(),
        backend: "test".into(),
        adapter_name: "invented mixed template fixture, no GPU".into(),
        json: json!({"test":true}),
    };
    let (report, _) = run_stage(
        &settings,
        Some(probe),
        root,
        &root.join("no-previous"),
        &manifest,
    )
    .unwrap();
    let authority =
        CoreAuthority::open(db, root, &manifest, &settings, report.builder_identity).unwrap();
    let context = PublishContext::new(&lock, root, root).unwrap();
    let base = finalize_base(&context, Some(&authority)).unwrap();
    converter::extensions::refresh_manifest_with_finalized(&mut manifest, root, &[base.extension])
        .unwrap();
    manifest.complete = true;
    manifest
        .save(&root.join("conversion-manifest.json"))
        .unwrap();
    drop(authority);
    drop(lock);
    crate::cluster::overlay_fixture::split_full_copy(root);
    Fixture::from_directory(directory, false, false)
}

fn occurrence(fixture: &Fixture, ordinal: u32) -> (Entity, Handle<Mesh>) {
    let source = fixture
        .app
        .world()
        .resource::<Assets<WorldAsset>>()
        .get(&fixture.source)
        .unwrap();
    let mut meshes = source.world.try_query::<(Entity, &Mesh3d)>().unwrap();
    meshes
        .iter(&source.world)
        .find_map(|(entity, mesh)| {
            (source.world.get::<SourcePrimitive>(entity).unwrap().ordinal == ordinal)
                .then(|| (entity, mesh.0.clone()))
        })
        .unwrap()
}

#[test]
fn mixed_template_rejects_malformed_or_missing_selected_occurrence() {
    for case in [
        "missing ordinal",
        "missing tag",
        "duplicate ordinal",
        "wrong source mesh",
        "missing baseline",
        "wrong source transform",
        "nondefault scene",
        "nonstatic model",
    ] {
        let mut fixture = mixed_fixture();
        let (body, _) = occurrence(&fixture, 0);
        let (flame, flame_mesh) = occurrence(&fixture, 1);
        {
            let mut scenes = fixture.app.world_mut().resource_mut::<Assets<WorldAsset>>();
            let world = &mut scenes.get_mut(&fixture.source).unwrap().world;
            match case {
                "missing ordinal" => world.get_mut::<SourcePrimitive>(body).unwrap().ordinal = 7,
                "missing tag" => {
                    world.entity_mut(body).remove::<SourcePrimitive>();
                }
                "duplicate ordinal" => world.get_mut::<SourcePrimitive>(flame).unwrap().ordinal = 0,
                "wrong source mesh" => {
                    world
                        .entity_mut(body)
                        .insert(SourcePrimitiveMesh(flame_mesh));
                }
                "missing baseline" => {
                    world.entity_mut(body).remove::<SourceNodePose>();
                }
                "wrong source transform" => {
                    world
                        .entity_mut(body)
                        .insert(SourcePrimitiveTransform(Mat4::from_translation(Vec3::X)));
                }
                "nondefault scene" => {
                    world
                        .get_mut::<SourcePrimitive>(body)
                        .unwrap()
                        .matches_default_scene = false;
                }
                "nonstatic model" => world.get_mut::<SourceStaticModel>(body).unwrap().0 = false,
                _ => unreachable!(),
            }
        }
        fixture.extract();
        let sources = fixture.render.resource::<Sources>();
        assert_eq!(sources.templates, [false], "{case}");
        assert_eq!(
            sources.source_fallback_batches,
            BTreeSet::from([0]),
            "{case}"
        );
        assert!(sources.material_refused_batches.is_empty(), "{case}");
        let facts = fixture.app.world().resource::<OwnershipFacts>();
        assert!(facts.projected_records().is_empty(), "{case}");
        assert!(facts.permanent_refused_records().is_empty(), "{case}");
        assert!(
            fixture
                .refs
                .iter()
                .all(|reference| !facts.can_spawn(*reference)),
            "{case}"
        );
        fixture.assert_no_stock_scene();
    }
}

#[test]
fn mixed_template_mesh_events_requalify_only_the_selected_occurrence_and_keep_frozen_ownership() {
    let mut fixture = mixed_fixture();
    let (_, body_mesh) = occurrence(&fixture, 0);
    let (_, flame_mesh) = occurrence(&fixture, 1);
    fixture.extract();
    assert_eq!(fixture.render.resource::<Sources>().templates, [true]);
    let terminal_revision = fixture
        .app
        .world()
        .resource::<OwnershipFacts>()
        .terminal_revision();
    let scene_tick = fixture.scene_tick();
    fixture
        .app
        .world_mut()
        .write_message(AssetEvent::<Mesh>::Modified {
            id: flame_mesh.id(),
        });
    fixture.extract();
    assert_eq!(fixture.scene_tick(), scene_tick);
    assert_eq!(
        fixture.render.resource::<Sources>().templates,
        [true],
        "mutating the refused flame cannot revoke the unrelated certified body"
    );
    fixture
        .app
        .world_mut()
        .write_message(AssetEvent::<Mesh>::Modified { id: body_mesh.id() });
    fixture.extract();
    let sources = fixture.render.resource::<Sources>();
    assert_eq!(sources.templates, [false]);
    assert_eq!(sources.source_fallback_batches, BTreeSet::from([0]));
    assert!(sources.material_refused_batches.is_empty());
    let facts = fixture.app.world().resource::<OwnershipFacts>();
    assert_eq!(facts.terminal_revision(), terminal_revision);
    assert!(facts.projected_records().is_empty());
    assert!(facts.permanent_refused_records().is_empty());
    for reference in &fixture.refs {
        assert_eq!(facts.owned_ordinals(*reference), BTreeSet::from([0]));
        assert!(facts.permanent_refused_ordinals(*reference).is_empty());
    }
    fixture.assert_no_stock_scene();
}
