//! Real producer and loader inputs through source extraction and the fallback publisher.
use super::*;
use crate::cluster::{
    assets::{ClusterBlockAsset, ClusterMeshAsset},
    reader::{
        CertifiedPackage, RecordKey,
        producer_fixture_tests::{published_for_billboard_loader, published_for_loader},
    },
    render::{ClusterInstance, world_from_local},
    source_identity::actual_loader_tests::{loader_app, wait_for_loaded},
};
use bevy::{
    asset::AssetEvent,
    gltf::Gltf,
    mesh::UvChannel,
    world_serialization::{WorldAsset, WorldAssetRoot, WorldInstance},
};
use std::fs;

#[path = "template_qualification_tests.rs"]
mod template_qualification_tests;

struct Fixture {
    app: App,
    render: World,
    schedule: Schedule,
    certificate: Arc<CertifiedPackage>,
    refs: BTreeSet<u32>,
    source: Handle<WorldAsset>,
    material: Handle<StandardMaterial>,
    mesh: ClusterMeshAsset,
    _model: Handle<Gltf>,
    // Both worlds and their certificate/asset handles drop before the synthetic files.
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new(billboard: bool, normal_map: bool) -> Self {
        let directory = if billboard {
            published_for_billboard_loader()
        } else {
            published_for_loader()
        };
        Self::from_directory(directory, true, normal_map)
    }

    fn from_directory(directory: tempfile::TempDir, complete: bool, normal_map: bool) -> Self {
        let certificate = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let refs: BTreeSet<_> = certificate
            .records()
            .map(|(_, row)| row.reference)
            .collect();
        assert_eq!(refs.len(), 2);
        assert_eq!(
            certificate
                .records()
                .map(|(_, row)| row.worldspace)
                .collect::<BTreeSet<_>>()
                .len(),
            2,
            "the producer fixture covers parent and placed child worlds"
        );
        for reference in &refs {
            assert_eq!(certificate.reference_complete(*reference), complete);
        }
        let model_path = certificate
            .reference_model_path(*refs.first().unwrap())
            .unwrap()
            .to_owned();
        let db = crate::cluster::overlay_fixture::read_db(directory.path());
        let mesh_path: String = db
            .query_row(
                "SELECT payload_path FROM cluster_meshes WHERE model_path=?1 AND primitive_index=0",
                [&model_path],
                |row| row.get(0),
            )
            .unwrap();
        let mesh = ClusterMeshAsset::from_bytes(
            fs::read(
                crate::cluster::overlay_fixture::overlay_path(directory.path()).join(mesh_path),
            )
            .unwrap(),
        )
        .unwrap();
        let mut paths = db.prepare("SELECT payload_path FROM cluster_chunks ORDER BY worldspace_id,tier,anchor_x,anchor_y").unwrap();
        let mut instances = Vec::new();
        for path in paths.query_map([], |row| row.get::<_, String>(0)).unwrap() {
            let bytes = fs::read(
                crate::cluster::overlay_fixture::overlay_path(directory.path()).join(path.unwrap()),
            )
            .unwrap();
            let block = ClusterBlockAsset::from_bytes(&bytes).unwrap();
            assert_eq!(block.meshes.len(), 1);
            assert_eq!(block.meshes[0].mesh_hash, mesh.mesh_hash);
            for (ordinal, record) in block.instances.iter().enumerate() {
                assert_eq!(record.mesh, 0);
                let key = RecordKey {
                    worldspace: block.worldspace_id,
                    tier: block.tier,
                    anchor: [block.anchor_x, block.anchor_y],
                    ordinal: ordinal as u32,
                };
                let identity = certificate
                    .record(&key)
                    .expect("actual MCI record has authoritative identity");
                assert!(refs.contains(&identity.reference));
                instances.push(ClusterInstance {
                    identity: ClusterInstanceIdentity::Object(key),
                    mesh: 0,
                    batch: 0,
                    world_from_local: world_from_local(record),
                    source_worldspace: record.source_worldspace_id,
                });
            }
        }
        assert_eq!(instances.len(), refs.len());
        drop(paths);
        drop(db);
        let mut app = loader_app(directory.path());
        app.init_asset::<StandardMaterial>()
            .init_resource::<ClusterBatchHandles>()
            .init_resource::<SourceFallbackDemand>()
            .init_resource::<crate::cluster::ownership::OwnershipFacts>()
            .insert_resource(ValidatedPackage(certificate.clone()));
        let model: Handle<Gltf> = app
            .world()
            .resource::<AssetServer>()
            .load(model_path.clone());
        wait_for_loaded(&mut app, &model);
        let source = app
            .world()
            .resource::<Assets<Gltf>>()
            .get(&model)
            .unwrap()
            .scenes[0]
            .clone();
        let mut material = StandardMaterial::default();
        if normal_map {
            // Prepare the actual stock geometry for this normal-mapped test input.
            // Keep the compressed payload untouched so its asynchronous tangent
            // preparation remains pending; this is setup, not a loader E2E claim.
            let template = app
                .world()
                .resource::<Assets<WorldAsset>>()
                .get(&source)
                .unwrap();
            let mut query = template.world.try_query::<&Mesh3d>().unwrap();
            let meshes: Vec<_> = query
                .iter(&template.world)
                .map(|mesh| mesh.0.clone())
                .collect();
            assert!(!meshes.is_empty());
            for handle in meshes {
                let mut assets = app.world_mut().resource_mut::<Assets<Mesh>>();
                let mut stock = assets.get_mut(&handle).unwrap();
                stock.generate_tangents().unwrap();
                assert!(stock.contains_attribute(Mesh::ATTRIBUTE_TANGENT));
            }
            assert_eq!(mesh.prepared_tangents_usable(), None);
            material.normal_map_texture = Some(
                app.world_mut()
                    .resource_mut::<Assets<Image>>()
                    .add(Image::default()),
            );
        }
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(material);
        app.world_mut()
            .resource_mut::<ClusterBatchHandles>()
            .set(0, material.clone());
        let lazy = LazyBatches::for_source_test(&model_path, source.clone());
        assert_eq!(lazy.source_scene(0), Some(&source));
        assert!(lazy.source_scene(1).is_none());
        app.insert_resource(lazy).insert_resource(ClusterScene {
            ready_revision: default(),
            residency_revision: default(),
            certificate: Some(certificate.clone()),
            inventory: None,
            terrain_blocks: Vec::new(),
            meshes: vec![mesh.clone()],
            instances,
            origin_offset: Vec3::ZERO,
            blocks: Vec::new(),
        });
        let mut render = World::new();
        render.init_resource::<Sources>();
        let mut schedule = Schedule::new(ExtractSchedule);
        schedule.add_systems((super::super::extract_sources, publish).chain());
        Self {
            app,
            render,
            schedule,
            certificate,
            refs,
            source,
            material,
            mesh,
            _model: model,
            directory,
        }
    }

    fn extract(&mut self) {
        // Model the next main-world frame without spawning any stock scene.
        self.app.world_mut().increment_change_tick();
        let mut main = MainWorld::default();
        std::mem::swap(self.app.world_mut(), &mut *main);
        self.render.insert_resource(main);
        self.schedule.run(&mut self.render);
        let mut main = self.render.remove_resource::<MainWorld>().unwrap();
        std::mem::swap(self.app.world_mut(), &mut *main);
    }

    fn take(&mut self) -> Option<(u64, BTreeSet<u32>)> {
        self.app
            .world_mut()
            .resource_mut::<SourceFallbackDemand>()
            .consume_for_package(&self.certificate)
            .map(|(revision, refs)| (revision, refs.clone()))
    }

    fn edit_material(&mut self, edit: impl FnOnce(&mut StandardMaterial)) {
        edit(
            &mut self
                .app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .get_mut(&self.material)
                .unwrap(),
        );
        // Extraction runs directly, without the main app's asset event drain.
        self.app.world_mut().write_message(AssetEvent::Modified {
            id: self.material.id(),
        });
    }

    fn scene_tick(&self) -> Tick {
        self.app
            .world()
            .get_resource_ref::<ClusterScene>()
            .unwrap()
            .last_changed()
    }

    fn assert_no_stock_scene(&mut self) {
        let world = self.app.world_mut();
        assert_eq!(world.query::<&WorldAssetRoot>().iter(world).count(), 0);
        assert_eq!(world.query::<&WorldInstance>().iter(world).count(), 0);
    }
}

#[test]
fn ownership_retries_a_pending_template_that_becomes_valid_without_scene_changes() {
    use crate::cluster::ownership::OwnershipFacts;
    let mut fixture = Fixture::new(false, false);
    fixture.app.init_resource::<OwnershipFacts>();
    let templates = fixture
        .app
        .world_mut()
        .remove_resource::<Assets<WorldAsset>>()
        .unwrap();
    let tick = fixture.scene_tick();
    fixture.extract();
    for reference in &fixture.refs {
        assert!(
            !fixture
                .app
                .world()
                .resource::<OwnershipFacts>()
                .can_spawn(*reference)
        );
    }
    fixture.app.insert_resource(templates);
    fixture.extract();
    assert_eq!(fixture.scene_tick(), tick);
    for reference in &fixture.refs {
        assert!(
            fixture
                .app
                .world()
                .resource::<OwnershipFacts>()
                .can_spawn(*reference)
        );
    }
}

#[test]
fn pending_false_then_loaded_billboard_false_never_freezes_a_material_refusal() {
    let mut fixture = Fixture::new(true, false);
    let templates = fixture
        .app
        .world_mut()
        .remove_resource::<Assets<WorldAsset>>()
        .unwrap();
    fixture.extract();
    let sources = fixture.render.resource::<Sources>();
    assert_eq!(sources.templates, [false]);
    assert!(sources.source_fallback_batches.is_empty());
    assert!(sources.material_refused_batches.is_empty());
    let (pending_revision, pending) = fixture
        .take()
        .expect("fresh definitive-negative projection");
    // This report describes only definite refusals. Empty does not authorize
    // clusters, stock suppression, or template/pipeline readiness.
    assert!(
        pending.is_empty(),
        "a missing template is not a definitive refusal"
    );
    assert!(fixture.take().is_none(), "one delivery per extraction");
    fixture.app.insert_resource(templates);
    let template = fixture
        .app
        .world()
        .resource::<Assets<WorldAsset>>()
        .get(&fixture.source)
        .unwrap();
    let mut extras = template
        .world
        .try_query::<&bevy::gltf::GltfExtras>()
        .unwrap();
    assert!(extras.iter(&template.world).any(|extras| {
        crate::billboard::BillboardMode::from_extras(&extras.value)
            == Some(crate::billboard::BillboardMode::TurnAboutUp)
    }));
    let mut static_models = template.world.try_query::<&SourceStaticModel>().unwrap();
    assert_eq!(
        static_models
            .iter(&template.world)
            .map(|marker| marker.0)
            .collect::<Vec<_>>(),
        [true],
        "the actual loader tags a static primitive; its authored billboard is the refusal"
    );
    assert!(!template_valid(template, &HashSet::new()));
    fixture.extract();
    let sources = fixture.render.resource::<Sources>();
    assert_eq!(
        sources.templates,
        [false],
        "the positive vector itself did not change"
    );
    assert_eq!(sources.source_fallback_batches, BTreeSet::from([0]));
    assert!(sources.material_refused_batches.is_empty());
    let (revision, refs) = fixture.take().unwrap();
    assert_eq!(revision, pending_revision);
    assert!(
        refs.is_empty(),
        "invalid source structure is not a material refusal"
    );
    let facts = fixture.app.world().resource::<OwnershipFacts>();
    assert!(facts.projected_records().is_empty());
    assert!(facts.permanent_refused_records().is_empty());
    for reference in &fixture.refs {
        assert!(!facts.can_spawn(*reference));
    }
    assert!(fixture.take().is_none());
    fixture.extract();
    assert_eq!(fixture.take(), Some((revision, refs)));
    fixture.assert_no_stock_scene();
}

#[test]
fn late_tangent_failure_reprojects_cached_pending_inputs_without_a_scene_tick_change() {
    let mut fixture = Fixture::new(false, true);
    fixture.app.add_message::<AppExit>();
    let tick = fixture.scene_tick();
    assert_eq!(fixture.mesh.prepared_tangents_usable(), None);
    fixture.extract();
    let sources = fixture.render.resource::<Sources>();
    assert_eq!(sources.templates, [true]);
    assert_eq!(sources.normal_maps, [true]);
    assert!(sources.source_fallback_batches.is_empty());
    let (pending_revision, pending) = fixture.take().unwrap();
    assert!(pending.is_empty(), "pending tangents cannot demand stock");
    for reference in &fixture.refs {
        assert!(
            !fixture
                .app
                .world()
                .resource::<OwnershipFacts>()
                .can_spawn(*reference)
        );
    }
    fixture.extract();
    assert_eq!(fixture.take(), Some((pending_revision, pending)));
    assert_eq!(fixture.scene_tick(), tick);
    fixture.mesh.clone().fail_tangents_for_test();
    assert_eq!(fixture.mesh.prepared_tangents_usable(), Some(false));
    assert_eq!(fixture.scene_tick(), tick);
    fixture.extract();
    assert_eq!(fixture.scene_tick(), tick);
    assert!(
        fixture
            .render
            .resource::<Sources>()
            .source_fallback_batches
            .is_empty()
    );
    assert!(
        fixture.take().is_none(),
        "a defective package cannot demand stock"
    );
    assert!(matches!(
        fixture.app.world().get_resource::<crate::cluster::package::PackageValidationStatus>(),
        Some(crate::cluster::package::PackageValidationStatus::Failed(reason))
            if reason.contains("tangents")
    ));
    let exits = fixture.app.world().resource::<Messages<AppExit>>();
    assert_eq!(exits.len(), 1);
    assert!(
        fixture
            .app
            .world()
            .resource::<OwnershipFacts>()
            .permanent_refused_records()
            .is_empty()
    );
    for reference in &fixture.refs {
        assert!(
            !fixture
                .app
                .world()
                .resource::<OwnershipFacts>()
                .can_spawn(*reference)
        );
    }
    fixture.extract();
    assert!(fixture.take().is_none());
    assert_eq!(fixture.app.world().resource::<Messages<AppExit>>().len(), 1);
    fixture.assert_no_stock_scene();
}

#[test]
fn a_missing_normal_mapped_mesh_withdraws_previously_published_fallback_feedback() {
    let mut fixture = Fixture::new(false, true);
    fixture.mesh.prepare_tangents().unwrap();
    fixture.extract();
    assert!(fixture.take().unwrap().1.is_empty());
    let terminal_revision = fixture
        .app
        .world()
        .resource::<OwnershipFacts>()
        .terminal_revision();
    fixture
        .app
        .world_mut()
        .resource_mut::<ClusterScene>()
        .meshes
        .clear();
    fixture.extract();
    assert!(fixture.take().is_none());
    assert!(
        fixture
            .app
            .world()
            .resource::<SourceFallbackDemand>()
            .for_package(&fixture.certificate)
            .is_none()
    );
    let facts = fixture.app.world().resource::<OwnershipFacts>();
    assert!(facts.projected_records().is_empty());
    assert_eq!(facts.terminal_revision(), terminal_revision);
    assert!(facts.permanent_refused_references().is_empty());
    for reference in &fixture.refs {
        assert!(
            fixture
                .app
                .world()
                .resource::<crate::cluster::ownership::OwnershipFacts>()
                .can_spawn(*reference)
        );
    }
}

#[test]
fn absent_or_mismatched_source_scope_withdraws_previously_published_feedback() {
    for case in [
        "missing_scene",
        "missing_certificate",
        "different_certificate",
    ] {
        let mut fixture = Fixture::new(false, false);
        let texture = fixture
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(Image::default());
        fixture.edit_material(|material| {
            material.base_color_channel = UvChannel::Uv1;
            material.base_color_texture = Some(texture);
        });
        fixture.extract();
        assert_eq!(fixture.take().unwrap().1, fixture.refs);
        let terminal_revision = fixture
            .app
            .world()
            .resource::<OwnershipFacts>()
            .terminal_revision();
        match case {
            "missing_scene" => {
                fixture.app.world_mut().remove_resource::<ClusterScene>();
            }
            "missing_certificate" => {
                fixture
                    .app
                    .world_mut()
                    .remove_resource::<ValidatedPackage>();
            }
            "different_certificate" => {
                let other = Arc::new(
                    crate::cluster::overlay_fixture::open_fixture(fixture.directory.path(), false)
                        .unwrap(),
                );
                assert!(!Arc::ptr_eq(&fixture.certificate, &other));
                fixture
                    .app
                    .world_mut()
                    .resource_mut::<ClusterScene>()
                    .certificate = Some(other);
            }
            _ => unreachable!(),
        }
        fixture.extract();
        assert!(fixture.take().is_none(), "{case}");
        assert!(
            fixture
                .app
                .world()
                .resource::<SourceFallbackDemand>()
                .for_package(&fixture.certificate)
                .is_none(),
            "{case}"
        );
        let facts = fixture.app.world().resource::<OwnershipFacts>();
        assert!(facts.projected_records().is_empty(), "{case}");
        assert_eq!(facts.terminal_revision(), terminal_revision, "{case}");
        assert_eq!(facts.permanent_refused_references(), fixture.refs, "{case}");
        for reference in &fixture.refs {
            assert!(
                fixture
                    .app
                    .world()
                    .resource::<crate::cluster::ownership::OwnershipFacts>()
                    .can_spawn(*reference),
                "{case}"
            );
        }
    }
}

#[test]
fn initial_loader_failure_never_freezes_a_material_refusal_or_demands_stock() {
    let mut fixture = Fixture::new(false, false);
    fixture
        .app
        .world_mut()
        .resource_mut::<LazyBatches>()
        .mark_failed_for_test(0);
    let tick = fixture.scene_tick();
    fixture.extract();
    let sources = fixture.render.resource::<Sources>();
    assert_eq!(sources.templates, [false]);
    assert_eq!(sources.source_fallback_batches, BTreeSet::from([0]));
    assert!(sources.material_refused_batches.is_empty());
    let source_revision = sources.template_revision;
    let facts = fixture.app.world().resource::<OwnershipFacts>();
    assert!(facts.package_matches(&fixture.certificate));
    assert!(facts.projected_records().is_empty());
    assert!(facts.permanent_refused_records().is_empty());
    assert!(facts.permanent_refused_references().is_empty());
    let terminal_revision = facts.terminal_revision();
    for reference in &fixture.refs {
        assert!(!facts.can_spawn(*reference));
        assert!(!facts.reference_has_owned(*reference));
        assert!(facts.owned_ordinals(*reference).is_empty());
        assert!(facts.permanent_refused_ordinals(*reference).is_empty());
    }
    let (revision, refs) = fixture.take().unwrap();
    assert!(refs.is_empty());
    assert!(
        fixture.take().is_none(),
        "feedback is consumed once per extraction"
    );
    fixture.extract();
    assert_eq!(fixture.scene_tick(), tick);
    assert_eq!(
        fixture.render.resource::<Sources>().template_revision,
        source_revision
    );
    let facts = fixture.app.world().resource::<OwnershipFacts>();
    assert_eq!(facts.terminal_revision(), terminal_revision);
    assert!(facts.permanent_refused_records().is_empty());
    for reference in &fixture.refs {
        assert!(!facts.can_spawn(*reference));
    }
    assert_eq!(fixture.take(), Some((revision, refs)));
    assert!(fixture.take().is_none());
    fixture.assert_no_stock_scene();
}

#[test]
fn accepted_ownership_stays_frozen_after_material_or_resource_failure() {
    for case in ["unsupported_material", "loader_failure", "missing_source"] {
        let mut fixture = Fixture::new(false, false);
        let tick = fixture.scene_tick();
        fixture.extract();
        let accepted: BTreeMap<_, _> = fixture
            .certificate
            .records()
            .map(|(key, _)| (*key, false))
            .collect();
        let sources = fixture.render.resource::<Sources>();
        assert_eq!(sources.templates, [true], "{case}");
        assert!(sources.source_fallback_batches.is_empty(), "{case}");
        assert!(sources.material_refused_batches.is_empty(), "{case}");
        let source_revision = sources.template_revision;
        let facts = fixture.app.world().resource::<OwnershipFacts>();
        assert_eq!(facts.projected_records(), &accepted, "{case}");
        let terminal_revision = facts.terminal_revision();
        let (revision, refs) = fixture.take().unwrap();
        assert!(refs.is_empty(), "{case}");
        match case {
            "unsupported_material" => {
                let texture = fixture
                    .app
                    .world_mut()
                    .resource_mut::<Assets<Image>>()
                    .add(Image::default());
                fixture.edit_material(|material| {
                    material.base_color_channel = UvChannel::Uv1;
                    material.base_color_texture = Some(texture);
                    assert!(!crate::cluster::material_geometry::requirements(material).supported);
                });
            }
            "loader_failure" => fixture
                .app
                .world_mut()
                .resource_mut::<LazyBatches>()
                .mark_failed_for_test(0),
            "missing_source" => {
                assert!(
                    fixture
                        .app
                        .world_mut()
                        .resource_mut::<Assets<WorldAsset>>()
                        .remove(fixture.source.id())
                        .is_some()
                );
                fixture.app.world_mut().write_message(AssetEvent::Removed {
                    id: fixture.source.id(),
                });
            }
            _ => unreachable!(),
        }
        fixture.extract();
        assert_eq!(fixture.scene_tick(), tick, "{case}");
        let sources = fixture.render.resource::<Sources>();
        assert!(sources.template_revision > source_revision, "{case}");
        assert_eq!(sources.templates, [false], "{case}");
        assert_eq!(
            sources.source_fallback_batches,
            BTreeSet::from([0]),
            "{case}"
        );
        assert_eq!(
            sources.material_refused_batches,
            if case == "unsupported_material" {
                BTreeSet::from([0])
            } else {
                BTreeSet::new()
            },
            "{case}"
        );
        let facts = fixture.app.world().resource::<OwnershipFacts>();
        assert_eq!(facts.terminal_revision(), terminal_revision, "{case}");
        assert!(facts.permanent_refused_records().is_empty(), "{case}");
        assert!(facts.permanent_refused_references().is_empty(), "{case}");
        if case == "unsupported_material" {
            let refused = accepted.keys().map(|key| (*key, true)).collect();
            assert_eq!(facts.projected_records(), &refused, "{case}");
        } else {
            assert!(facts.projected_records().is_empty(), "{case}");
        }
        for reference in &fixture.refs {
            let ordinals = fixture
                .certificate
                .records()
                .filter_map(|(_, identity)| {
                    (identity.reference == *reference).then_some(identity.primitive)
                })
                .collect();
            assert!(facts.can_spawn(*reference), "{case}");
            assert!(facts.reference_has_owned(*reference), "{case}");
            assert_eq!(facts.owned_ordinals(*reference), ordinals, "{case}");
            assert!(
                facts.permanent_refused_ordinals(*reference).is_empty(),
                "{case}"
            );
        }
        assert_eq!(fixture.take(), Some((revision, refs.clone())), "{case}");
        assert!(fixture.take().is_none(), "{case}");
        fixture.extract();
        assert_eq!(fixture.take(), Some((revision, refs)), "{case}");
        assert!(fixture.take().is_none(), "{case}");
        fixture.assert_no_stock_scene();
    }
}

#[test]
fn initial_unsupported_material_remains_a_fixed_refusal_after_support_arrives() {
    let mut fixture = Fixture::new(false, false);
    let texture = fixture
        .app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    fixture.edit_material(|material| {
        material.base_color_channel = UvChannel::Uv1;
        material.base_color_texture = Some(texture);
        assert!(!crate::cluster::material_geometry::requirements(material).supported);
    });
    let tick = fixture.scene_tick();
    fixture.extract();
    let refused: BTreeMap<_, _> = fixture
        .certificate
        .records()
        .map(|(key, _)| (*key, true))
        .collect();
    let sources = fixture.render.resource::<Sources>();
    assert_eq!(sources.templates, [false]);
    assert_eq!(sources.source_fallback_batches, BTreeSet::from([0]));
    assert_eq!(sources.material_refused_batches, BTreeSet::from([0]));
    let source_revision = sources.template_revision;
    let facts = fixture.app.world().resource::<OwnershipFacts>();
    assert_eq!(facts.projected_records(), &refused);
    assert_eq!(
        facts.permanent_refused_records(),
        refused.keys().copied().collect()
    );
    assert_eq!(facts.permanent_refused_references(), fixture.refs);
    let terminal_revision = facts.terminal_revision();
    let (revision, refs) = fixture.take().unwrap();
    assert_eq!(refs, fixture.refs);
    assert!(fixture.take().is_none());
    fixture.edit_material(|material| {
        material.base_color_channel = UvChannel::Uv0;
        assert!(crate::cluster::material_geometry::requirements(material).supported);
    });
    fixture.extract();
    assert_eq!(fixture.scene_tick(), tick);
    let sources = fixture.render.resource::<Sources>();
    assert!(sources.template_revision > source_revision);
    assert_eq!(sources.templates, [true]);
    assert!(sources.source_fallback_batches.is_empty());
    assert!(sources.material_refused_batches.is_empty());
    let facts = fixture.app.world().resource::<OwnershipFacts>();
    let accepted = refused.keys().map(|key| (*key, false)).collect();
    assert_eq!(facts.projected_records(), &accepted);
    assert_eq!(facts.terminal_revision(), terminal_revision);
    assert_eq!(
        facts.permanent_refused_records(),
        refused.keys().copied().collect()
    );
    assert_eq!(facts.permanent_refused_references(), fixture.refs);
    for reference in &fixture.refs {
        let ordinals = fixture
            .certificate
            .records()
            .filter_map(|(_, identity)| {
                (identity.reference == *reference).then_some(identity.primitive)
            })
            .collect();
        assert!(facts.can_spawn(*reference));
        assert!(!facts.reference_has_owned(*reference));
        assert!(facts.owned_ordinals(*reference).is_empty());
        assert_eq!(facts.permanent_refused_ordinals(*reference), ordinals);
    }
    assert_eq!(fixture.take(), Some((revision, refs.clone())));
    assert!(fixture.take().is_none());
    fixture.extract();
    assert_eq!(fixture.take(), Some((revision, refs)));
    assert!(fixture.take().is_none());
    fixture.assert_no_stock_scene();
}

fn assert_incremental_matches_full(
    projection: &mut Projection,
    facts: &mut OwnershipFacts,
    certificate: &Arc<CertifiedPackage>,
    scene: &ClusterScene,
    tick: u32,
    sources: &Sources,
    visited: usize,
) {
    let update = projection
        .update(certificate, scene, Tick::new(tick), sources, true)
        .unwrap();
    assert_eq!(
        update.visited, visited,
        "only affected batches may be visited"
    );
    facts.publish_updates(certificate, update.reset, update.ownership);
    let full = references_requiring_source(
        certificate,
        scene,
        &sources.source_fallback_batches,
        &sources.material_refused_batches,
        &sources.normal_maps,
        Some(&sources.templates),
    )
    .unwrap();
    assert_eq!(
        projection
            .reference_counts
            .keys()
            .copied()
            .collect::<BTreeSet<_>>(),
        full.references
    );
    assert_eq!(
        projection
            .pending_tangents
            .keys()
            .copied()
            .collect::<BTreeSet<_>>(),
        full.pending_tangents
    );
    assert_eq!(facts.projected_records(), &full.ownership);
}

#[test]
fn incremental_projection_matches_full_across_readiness_scene_and_certificate_changes() {
    let fixture = Fixture::new(false, false);
    let mut scene = fixture.app.world().resource::<ClusterScene>().clone();
    scene.instances[1].batch = 1;
    let mut sources = Sources {
        templates: vec![false, false],
        normal_maps: vec![false, false],
        ..default()
    };
    let mut projection = Projection::default();
    let mut facts = OwnershipFacts::default();
    let package = &fixture.certificate;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 2);
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 0);
    sources.templates[0] = true;
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    sources.templates[1] = true;
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    sources.normal_maps[0] = true;
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 0);
    fixture.mesh.prepare_tangents().unwrap();
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    sources.source_fallback_batches.insert(1);
    sources.templates[1] = false;
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    sources.material_refused_batches.insert(1);
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    assert!(facts.permanent_refused_records().is_empty());
    sources.material_refused_batches.clear();
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    sources.source_fallback_batches.clear();
    sources.templates[1] = true;
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 1);
    sources.normal_maps[0] = false;
    sources.templates[1] = false;
    sources.template_revision += 1;
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 1, &sources, 2);
    scene.instances.pop();
    assert_incremental_matches_full(&mut projection, &mut facts, package, &scene, 2, &sources, 1);
    let replacement = Arc::new(
        crate::cluster::overlay_fixture::open_fixture(fixture.directory.path(), false).unwrap(),
    );
    assert_incremental_matches_full(
        &mut projection,
        &mut facts,
        &replacement,
        &scene,
        2,
        &sources,
        1,
    );
    sources.normal_maps[0] = true;
    sources.template_revision += 1;
    scene.meshes.clear();
    assert!(
        projection
            .update(&replacement, &scene, Tick::new(3), &sources, true)
            .is_err()
    );
    assert!(
        references_requiring_source(
            &replacement,
            &scene,
            &sources.source_fallback_batches,
            &sources.material_refused_batches,
            &sources.normal_maps,
            Some(&sources.templates)
        )
        .is_err()
    );
}

#[test]
fn fallback_reference_union_retains_a_reference_required_by_another_batch() {
    let fixture = Fixture::new(false, false);
    let mut scene = fixture.app.world().resource::<ClusterScene>().clone();
    // The union is by reference rather than batch. A repeated occurrence exercises
    // this property without inventing identity outside the real certificate.
    scene.instances.truncate(1);
    let mut repeated = scene.instances[0];
    repeated.batch = 1;
    scene.instances.push(repeated);
    let mut sources = Sources {
        source_fallback_batches: BTreeSet::from([0, 1]),
        ..default()
    };
    let mut projection = Projection::default();
    let first = projection
        .update(&fixture.certificate, &scene, Tick::new(1), &sources, false)
        .unwrap();
    let mut demand = SourceFallbackDemand::default();
    demand.update(Some(&fixture.certificate), first.added);
    let (revision, references) = demand.for_package(&fixture.certificate).unwrap();
    let references = references.clone();
    sources.source_fallback_batches.remove(&1);
    sources.template_revision += 1;
    let update = projection
        .update(&fixture.certificate, &scene, Tick::new(1), &sources, false)
        .unwrap();
    assert_eq!(update.visited, 1);
    assert!(update.added.is_empty());
    assert!(update.removed.is_empty());
    assert!(!demand.update_delta(&fixture.certificate, &update.added, &update.removed));
    assert_eq!(
        demand.consume_for_package(&fixture.certificate),
        Some((revision, &references))
    );
    sources.source_fallback_batches = BTreeSet::from([1]);
    sources.template_revision += 1;
    let update = projection
        .update(&fixture.certificate, &scene, Tick::new(1), &sources, false)
        .unwrap();
    assert_eq!(update.visited, 2);
    assert!(update.added.is_empty());
    assert!(update.removed.is_empty());
    assert!(!demand.update_delta(&fixture.certificate, &update.added, &update.removed));
    assert_eq!(
        demand.consume_for_package(&fixture.certificate),
        Some((revision, &references))
    );
    sources.source_fallback_batches.clear();
    sources.template_revision += 1;
    let update = projection
        .update(&fixture.certificate, &scene, Tick::new(1), &sources, false)
        .unwrap();
    assert_eq!(update.visited, 1);
    assert!(demand.update_delta(&fixture.certificate, &update.added, &update.removed));
    assert!(
        demand
            .for_package(&fixture.certificate)
            .unwrap()
            .1
            .is_empty()
    );
}

#[test]
fn unusable_normal_map_tangents_refuse_even_when_the_batch_already_requires_stock() {
    let fixture = Fixture::new(false, true);
    fixture.mesh.clone().fail_tangents_for_test();
    let scene = fixture.app.world().resource::<ClusterScene>();
    let result = references_requiring_source(
        &fixture.certificate,
        scene,
        &BTreeSet::from([0]),
        &BTreeSet::from([0]),
        &[true],
        Some(&[false]),
    );
    assert!(matches!(result, Err(reason) if reason.contains("tangents")));
}

#[test]
fn failed_unused_tangents_do_not_refuse_an_untextured_batch() {
    let mut fixture = Fixture::new(false, false);
    fixture.mesh.clone().fail_tangents_for_test();
    fixture.extract();
    assert!(fixture.take().unwrap().1.is_empty());
    assert!(
        fixture
            .app
            .world()
            .get_resource::<crate::cluster::package::PackageValidationStatus>()
            .is_none()
    );
    for reference in &fixture.refs {
        assert!(
            fixture
                .app
                .world()
                .resource::<OwnershipFacts>()
                .can_spawn(*reference)
        );
    }
}

#[test]
fn healthy_normal_map_tangents_complete_without_stock_or_terminal_refusal() {
    let mut fixture = Fixture::new(false, true);
    fixture.extract();
    assert!(fixture.take().unwrap().1.is_empty());
    let tick = fixture.scene_tick();
    fixture.mesh.prepare_tangents().unwrap();
    assert_eq!(fixture.mesh.prepared_tangents_usable(), Some(true));
    fixture.extract();
    assert_eq!(fixture.scene_tick(), tick);
    assert!(fixture.take().unwrap().1.is_empty());
    assert!(
        fixture
            .app
            .world()
            .get_resource::<crate::cluster::package::PackageValidationStatus>()
            .is_none()
    );
    for reference in &fixture.refs {
        assert!(
            fixture
                .app
                .world()
                .resource::<OwnershipFacts>()
                .can_spawn(*reference)
        );
    }
    fixture.assert_no_stock_scene();
}

fn assert_defective_tangent_publication_stops_once(fixture: &mut Fixture) {
    assert!(
        fixture.take().is_none(),
        "defective tangent geometry must never request stock"
    );
    assert!(
        matches!(fixture.app.world().get_resource::<crate::cluster::package::PackageValidationStatus>(),
        Some(crate::cluster::package::PackageValidationStatus::Failed(reason)) if reason.contains("tangents"))
    );
    assert_eq!(fixture.app.world().resource::<Messages<AppExit>>().len(), 1);
    fixture.extract();
    assert!(fixture.take().is_none());
    assert_eq!(fixture.app.world().resource::<Messages<AppExit>>().len(), 1);
    fixture.assert_no_stock_scene();
}

#[test]
fn actual_degenerate_mcl_tangent_generation_refuses_instead_of_proving_defaults() {
    let mut fixture = Fixture::new(false, true);
    fixture.app.add_message::<AppExit>();
    let mut bytes = fixture.mesh.snapshot().unwrap().to_vec();
    let (start, length) = {
        let view = shared::cluster::read_mcl(&bytes).unwrap();
        assert_eq!(view.flags & shared::cluster::FLAG_TANGENTS, 0);
        let uvs = view.section(shared::cluster::MclSection::Uvs);
        (uvs.as_ptr() as usize - bytes.as_ptr() as usize, uvs.len())
    };
    assert!(length > 0);
    bytes[start..start + length].fill(0);
    let malformed = ClusterMeshAsset::from_bytes(bytes).unwrap();
    fixture
        .app
        .world_mut()
        .resource_mut::<ClusterScene>()
        .meshes[0] = malformed.clone();
    fixture.mesh = malformed;
    fixture.mesh.prepare_tangents().unwrap();
    assert_eq!(
        fixture.mesh.prepared_tangents_usable(),
        Some(false),
        "finite default tangents are not real leaf-corner proof"
    );
    fixture.extract();
    assert_defective_tangent_publication_stops_once(&mut fixture);
}

fn remove_stock_template_tangents(fixture: &mut Fixture) {
    let handles: Vec<_> = {
        let template = fixture
            .app
            .world()
            .resource::<Assets<WorldAsset>>()
            .get(&fixture.source)
            .unwrap();
        let mut query = template.world.try_query::<&Mesh3d>().unwrap();
        query
            .iter(&template.world)
            .map(|mesh| mesh.0.clone())
            .collect()
    };
    assert!(!handles.is_empty());
    let mut meshes = fixture.app.world_mut().resource_mut::<Assets<Mesh>>();
    for handle in handles {
        assert!(
            meshes
                .get_mut(&handle)
                .unwrap()
                .remove_attribute(Mesh::ATTRIBUTE_TANGENT)
                .is_some()
        );
    }
}

#[test]
fn already_source_batch_observes_late_tangent_failure_without_source_or_scene_changes() {
    let mut fixture = Fixture::new(false, true);
    fixture.app.add_message::<AppExit>();
    remove_stock_template_tangents(&mut fixture);
    let tick = fixture.scene_tick();
    fixture.extract();
    assert_eq!(fixture.take().unwrap().1, fixture.refs);
    assert!(fixture.render.resource::<Sources>().normal_maps[0]);
    let mut revision = {
        let sources = fixture.render.resource::<Sources>();
        (sources.version, sources.template_revision)
    };
    let mut settled = false;
    for _ in 0..8 {
        fixture.extract();
        let next = {
            let sources = fixture.render.resource::<Sources>();
            (sources.version, sources.template_revision)
        };
        if next == revision {
            settled = true;
            break;
        }
        revision = next;
    }
    assert!(
        settled,
        "initial template/ownership feedback must settle before the completion experiment"
    );
    assert_eq!(fixture.scene_tick(), tick);
    assert_eq!(fixture.mesh.prepared_tangents_usable(), None);
    fixture.mesh.clone().fail_tangents_for_test();
    fixture.extract();
    assert_eq!(fixture.scene_tick(), tick);
    assert_eq!(
        {
            let sources = fixture.render.resource::<Sources>();
            (sources.version, sources.template_revision)
        },
        revision
    );
    assert_defective_tangent_publication_stops_once(&mut fixture);
}

#[test]
fn material_refusal_preserves_its_normal_map_requirement_and_refuses_failed_geometry() {
    let mut fixture = Fixture::new(false, true);
    fixture.app.add_message::<AppExit>();
    let texture = fixture
        .app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    fixture.edit_material(|material| {
        material.base_color_channel = UvChannel::Uv1;
        material.base_color_texture = Some(texture);
    });
    fixture.mesh.clone().fail_tangents_for_test();
    fixture.extract();
    assert!(
        fixture.render.resource::<Sources>().normal_maps[0],
        "unsupported channels must not erase a known tangent consumer"
    );
    assert_defective_tangent_publication_stops_once(&mut fixture);
}

#[test]
fn healthy_tangents_keep_genuine_material_refusal_on_the_stock_path() {
    let mut fixture = Fixture::new(false, true);
    fixture.app.add_message::<AppExit>();
    let texture = fixture
        .app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    fixture.edit_material(|material| {
        material.base_color_channel = UvChannel::Uv1;
        material.base_color_texture = Some(texture);
    });
    fixture.mesh.prepare_tangents().unwrap();
    assert_eq!(fixture.mesh.prepared_tangents_usable(), Some(true));
    fixture.extract();
    assert_eq!(fixture.take().unwrap().1, fixture.refs);
    assert!(
        fixture
            .app
            .world()
            .get_resource::<crate::cluster::package::PackageValidationStatus>()
            .is_none()
    );
    assert!(
        fixture
            .app
            .world()
            .resource::<Messages<AppExit>>()
            .is_empty()
    );
    fixture.assert_no_stock_scene();
}
