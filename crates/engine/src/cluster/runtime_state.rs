//! Runtime suppression of the converted resting representation of a reference.
//!
//! Logical state writers trigger `ReferenceStateChanged` before mutating a reference.
//! A changed reference stays on the ordinary stock path for this session, including
//! after streaming unload/reload. Restoring a resting proxy requires a new certified
//! representation, rather than assuming that a later entity spawn resets game state.
//! Explicit enable transitions use `SetReferenceEnabled` instead: their suppression is reversible
//! and cannot erase a separate pose, harvest, or other permanent representation revocation.

use bevy::{
    camera::visibility::VisibilitySystems,
    prelude::*,
    render::extract_resource::{ExtractResource, ExtractResourcePlugin},
    transform::TransformSystems,
};
use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use super::{
    ownership::{OwnedPrimitiveSource, OwnershipFacts},
    source_identity::{SourceNodePose, SourcePrimitive, SourcePrimitiveMesh},
};
use crate::world::components::FormId;

/// Emitted by gameplay when a reference is moved, disabled, harvested or otherwise changed.
/// This identifies the placed reference, never its shared mesh or base record.
#[derive(Event, Clone, Copy, Debug)]
pub struct ReferenceStateChanged {
    pub reference: u32,
}

/// The source reference's cell-local resting pose, unaffected by render-origin rebases.
#[derive(Component)]
pub(crate) struct RestingReferencePose(pub(crate) Transform);

/// Suppression is independent of entity lifetime, view, batch and mesh sharing.
#[derive(Resource, Clone, Default, ExtractResource)]
pub(crate) struct RuntimeReferenceState {
    pub(crate) revision: u64,
    references: Arc<BTreeSet<u32>>,
    permanent: Arc<BTreeSet<u32>>,
    disabled: Arc<BTreeSet<u32>>,
}

impl RuntimeReferenceState {
    pub(crate) fn contains(&self, reference: u32) -> bool {
        self.references.contains(&reference)
    }

    pub(crate) fn references(&self) -> impl Iterator<Item = u32> + '_ {
        self.references.iter().copied()
    }

    fn suppress(&mut self, reference: u32) {
        if !Arc::make_mut(&mut self.permanent).insert(reference) {
            return;
        }
        if Arc::make_mut(&mut self.references).insert(reference) {
            self.revision = self.revision.wrapping_add(1);
        }
        info!(
            reference,
            suppressed_references = self.references.len(),
            "cluster resting reference revoked by runtime state change"
        );
    }

    /// Enable overrides are reversible; pose and gameplay revocations remain permanent.
    fn set_enabled(&mut self, reference: u32, enabled: bool, mapped: bool) {
        let changed = if enabled {
            Arc::make_mut(&mut self.disabled).remove(&reference);
            !self.permanent.contains(&reference)
                && Arc::make_mut(&mut self.references).remove(&reference)
        } else {
            Arc::make_mut(&mut self.disabled).insert(reference);
            if mapped {
                Arc::make_mut(&mut self.references).insert(reference)
            } else {
                !self.permanent.contains(&reference)
                    && Arc::make_mut(&mut self.references).remove(&reference)
            }
        };
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RuntimeStateSystems {
    Observe,
}

pub(crate) fn install(app: &mut App) {
    app.init_resource::<RuntimeReferenceState>()
        .add_observer(record_state_change)
        .add_observer(record_enable_change)
        .add_observer(record_disabled_reference)
        .add_systems(
            PostUpdate,
            observe_changed_reference_poses
                .in_set(RuntimeStateSystems::Observe)
                .after(bevy::app::AnimationSystems)
                .before(TransformSystems::Propagate)
                .before(VisibilitySystems::VisibilityPropagate),
        )
        .add_plugins(ExtractResourcePlugin::<RuntimeReferenceState>::default());
}

fn record_state_change(event: On<ReferenceStateChanged>, mut state: ResMut<RuntimeReferenceState>) {
    state.suppress(event.reference);
}

fn record_enable_change(
    event: On<crate::world::reference_state::ReferenceEnableChanged>,
    mut state: ResMut<RuntimeReferenceState>,
    package: Option<Res<super::package::ValidatedPackage>>,
) {
    // Membership includes partial and not-yet-resident instances. Without a certificate,
    // preserve the existing conservative suppression policy. Disabled tracking remains
    // global so a hidden stock root cannot become a permanent pose revocation.
    let mapped = package.is_none_or(|package| package.0.reference(event.reference).is_some());
    state.set_enabled(event.reference, event.enabled, mapped);
}

fn record_disabled_reference(
    event: On<Add, bevy::ecs::entity_disabling::Disabled>,
    references: Query<
        &FormId,
        (
            With<RestingReferencePose>,
            bevy::ecs::query::Allow<bevy::ecs::entity_disabling::Disabled>,
        ),
    >,
    mut state: ResMut<RuntimeReferenceState>,
) {
    if let Ok(reference) = references.get(event.entity) {
        state.suppress(reference.0);
    }
}

type ChangedRestingRoots<'w, 's> = Query<
    'w,
    's,
    (
        &'static FormId,
        &'static RestingReferencePose,
        &'static Transform,
        Option<&'static Visibility>,
    ),
    Or<(Changed<Transform>, Changed<Visibility>)>,
>;

type SourceSubtreeNodes<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static Children>,
        Option<&'static SourcePrimitive>,
        Has<OwnedPrimitiveSource>,
        Has<SourcePrimitiveMesh>,
        Has<Mesh3d>,
    ),
>;

/// Only descend from the changed node: a gameplay anchor below an owned mesh
/// cannot move that mesh, while moving any of its ancestors does affect it.
/// Missing source identities and broken relationships cannot prove independence.
fn subtree_affects_owned_source(
    changed: Entity,
    owned: &BTreeSet<u32>,
    nodes: &SourceSubtreeNodes,
    parents: &Query<&ChildOf>,
) -> bool {
    let mut pending = vec![changed];
    let mut visited = HashSet::new();
    while let Some(entity) = pending.pop() {
        if !visited.insert(entity) {
            return true;
        }
        let Ok((children, tag, owned_source, source_mesh, mesh)) = nodes.get(entity) else {
            return true;
        };
        // The retained owned-source marker also protects a lost or edited ordinal.
        if owned_source {
            return true;
        }
        if let Some(tag) = tag {
            if !tag.matches_default_scene || owned.contains(&tag.ordinal) {
                return true;
            }
        } else if source_mesh || mesh {
            return true;
        }
        if let Some(children) = children {
            for child in children.iter() {
                if parents.get(child).ok().map(ChildOf::parent) != Some(entity) {
                    return true;
                }
                pending.push(child);
            }
        }
    }
    false
}

/// Stock transform/visibility writers that predate the logical state event still revoke
/// the resting instance. Cell retirement and rebasing change its parent, not this pose.
fn observe_changed_reference_poses(
    roots: ChangedRestingRoots,
    changed_nodes: Query<(Entity, &Transform, &SourceNodePose), Changed<Transform>>,
    parents: Query<&ChildOf>,
    identities: Query<&FormId, With<RestingReferencePose>>,
    subtree_nodes: SourceSubtreeNodes,
    facts: Option<Res<OwnershipFacts>>,
    mut state: ResMut<RuntimeReferenceState>,
) {
    for (form, resting, current, visible) in &roots {
        if *current != resting.0
            || (visible == Some(&Visibility::Hidden) && !state.disabled.contains(&form.0))
        {
            state.suppress(form.0);
        }
    }
    for (entity, current, resting) in &changed_nodes {
        if *current == resting.0 {
            continue;
        }
        let mut at = Some(entity);
        let mut visited = HashSet::new();
        while let Some(node) = at {
            if !visited.insert(node) {
                break;
            }
            if let Ok(reference) = identities.get(node) {
                if state.contains(reference.0) {
                    break;
                }
                // Logical/root state remains global. Descendants may be independent
                // only after all certificate records have frozen terminal ownership.
                let affects_reference = node == entity
                    || facts
                        .as_deref()
                        .filter(|facts| facts.can_spawn(reference.0))
                        .map(|facts| facts.owned_ordinals(reference.0))
                        .filter(|owned| !owned.is_empty())
                        .is_none_or(|owned| {
                            subtree_affects_owned_source(entity, &owned, &subtree_nodes, &parents)
                        });
                if affects_reference {
                    state.suppress(reference.0);
                }
                break;
            }
            at = parents.get(node).ok().map(ChildOf::parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod certified_descendants {
        use super::*;
        use crate::cluster::{
            ownership::{OwnedPrimitiveSource, OwnershipFacts},
            reader::{
                CertifiedPackage,
                producer_fixture_tests::{published, published_for_loader},
            },
            source_identity::{SourcePrimitive, SourcePrimitiveMesh},
        };

        struct Fixture {
            app: App,
            reference: u32,
            root: Entity,
            shared: Entity,
            body: Entity,
            flame: Entity,
            anchor: Entity,
            _directory: tempfile::TempDir,
        }

        impl Fixture {
            fn new() -> Self {
                // Real producer certification supplies both an accepted occurrence and
                // an independently refused occurrence. No certificate bytes are patched.
                let directory = published();
                let package = Arc::new(
                    crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
                );
                let (_, identity) = package.records().next().unwrap();
                let reference = identity.reference;
                let body_ordinal = identity.primitive;
                let flame_ordinal = *package
                    .unclustered_primitives(reference)
                    .unwrap()
                    .first()
                    .expect("the producer fixture includes a refused occurrence");
                let mut facts = OwnershipFacts::default();
                facts.publish(
                    &package,
                    package.records().map(|(key, _)| (*key, false)).collect(),
                );
                // Runtime withdrawal must not erase the first terminal ownership.
                facts.revoke();
                assert!(facts.can_spawn(reference));
                assert_eq!(
                    facts.owned_ordinals(reference),
                    BTreeSet::from([body_ordinal])
                );
                let mut app = app();
                app.insert_resource(facts);
                let root = app
                    .world_mut()
                    .spawn((
                        FormId(reference),
                        RestingReferencePose(Transform::default()),
                        Transform::default(),
                        Visibility::Inherited,
                    ))
                    .id();
                let shared = node(&mut app, root);
                let body = node(&mut app, shared);
                let tag = SourcePrimitive {
                    scene_index: 0,
                    ordinal: body_ordinal,
                    mesh_index: 0,
                    primitive_index: 0,
                    matches_default_scene: true,
                };
                app.world_mut().entity_mut(body).insert((
                    tag,
                    OwnedPrimitiveSource {
                        mesh: Mesh3d(Handle::default()),
                        material: None,
                    },
                ));
                let anchor = node(&mut app, body);
                let flame = node(&mut app, shared);
                app.world_mut()
                    .entity_mut(flame)
                    .insert(crate::billboard::Billboard {
                        mode: crate::billboard::BillboardMode::TurnAboutUp,
                        authored: Quat::IDENTITY,
                    });
                let flame_mesh = node(&mut app, flame);
                app.world_mut().entity_mut(flame_mesh).insert((
                    SourcePrimitive {
                        ordinal: flame_ordinal,
                        primitive_index: 1,
                        ..tag
                    },
                    Mesh3d(Handle::default()),
                ));
                app.update();
                assert_eq!(app.world().resource::<RuntimeReferenceState>().revision, 0);
                Self {
                    app,
                    reference,
                    root,
                    shared,
                    body,
                    flame,
                    anchor,
                    _directory: directory,
                }
            }
        }

        fn node(app: &mut App, parent: Entity) -> Entity {
            app.world_mut()
                .spawn((
                    ChildOf(parent),
                    SourceNodePose(Transform::default()),
                    Transform::default(),
                ))
                .id()
        }

        fn replacement_certificate() -> (tempfile::TempDir, Arc<CertifiedPackage>) {
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
            use shared::asset_lock::AssetLock;
            use std::fs;

            let original = published_for_loader();
            let original_package =
                crate::cluster::overlay_fixture::open_fixture(original.path(), false).unwrap();
            let reference = original_package.records().next().unwrap().1.reference;
            let model_path = original_package.reference_model_path(reference).unwrap();
            let bytes = fs::read(original.path().join(model_path)).unwrap();
            let mut glb = gltf::binary::Glb::from_slice(&bytes).unwrap();
            let mut document: serde_json::Value = serde_json::from_slice(&glb.json).unwrap();
            let primitive = document["meshes"][0]["primitives"][0].clone();
            document["meshes"][0]["primitives"]
                .as_array_mut()
                .unwrap()
                .push(primitive);
            glb.json = serde_json::to_vec(&document).unwrap().into();
            let bytes = glb.to_vec().unwrap();

            // Author the extra occurrence before staging. The producer writes every
            // replacement certificate, coverage row and payload hash itself.
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            fs::write(
                root.join("OWNER.txt"),
                "runtime certificate replacement fixture\n",
            )
            .unwrap();
            fs::copy(
                original.path().join("skyrim_world.db"),
                root.join("skyrim_world.db"),
            )
            .unwrap();
            fs::create_dir_all(root.join(model_path).parent().unwrap()).unwrap();
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
            let child_worldspaces = db
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
                child_worldspaces,
                include_trees: false,
                map_references: true,
            };
            let probe = ProbeResult {
                passed: true,
                missing: Vec::new(),
                backend: "test".into(),
                adapter_name: "invented runtime replacement fixture, no GPU".into(),
                json: serde_json::json!({"test":true}),
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
                CoreAuthority::open(db, root, &manifest, &settings, report.builder_identity)
                    .unwrap();
            let context = PublishContext::new(&lock, root, root).unwrap();
            let base = finalize_base(&context, Some(&authority)).unwrap();
            converter::extensions::refresh_manifest_with_finalized(
                &mut manifest,
                root,
                &[base.extension],
            )
            .unwrap();
            manifest.complete = true;
            manifest
                .save(&root.join("conversion-manifest.json"))
                .unwrap();
            drop(authority);
            drop(lock);
            crate::cluster::overlay_fixture::split_full_copy(root);
            let package =
                Arc::new(crate::cluster::overlay_fixture::open_fixture(root, false).unwrap());
            (directory, package)
        }

        #[test]
        fn refused_billboard_pose_preserves_certified_body() {
            let mut fixture = Fixture::new();
            fixture
                .app
                .world_mut()
                .get_mut::<Transform>(fixture.flame)
                .unwrap()
                .rotation = Quat::from_rotation_y(0.5);
            fixture.app.update();
            assert_eq!(
                fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .revision,
                0,
                "a refused sibling's billboard pose must not revoke the owned body"
            );
        }

        #[test]
        fn refused_animation_write_preserves_certified_body_in_same_frame() {
            let mut fixture = Fixture::new();
            fixture.app.world_mut().entity_mut(fixture.flame).insert(
                bevy::animation::AnimationTargetId::from_name(&Name::new("refused flame")),
            );
            fixture.app.add_systems(
                PostUpdate,
                (|mut nodes: Query<&mut Transform, With<bevy::animation::AnimationTargetId>>| {
                    for mut pose in &mut nodes {
                        pose.translation.x += 1.0;
                    }
                })
                .in_set(bevy::app::AnimationSystems),
            );
            fixture.app.update();
            assert!(
                !fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .contains(fixture.reference)
            );
        }

        #[test]
        fn gameplay_anchor_below_owned_mesh_preserves_certified_body() {
            let mut fixture = Fixture::new();
            fixture
                .app
                .world_mut()
                .get_mut::<Transform>(fixture.anchor)
                .unwrap()
                .translation
                .y += 1.0;
            fixture.app.update();
            assert_eq!(
                fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .revision,
                0
            );
        }

        #[test]
        fn owned_body_or_shared_ancestor_pose_revokes_whole_reference() {
            for change_body in [true, false] {
                let mut fixture = Fixture::new();
                let changed = if change_body {
                    fixture.body
                } else {
                    fixture.shared
                };
                fixture
                    .app
                    .world_mut()
                    .get_mut::<Transform>(changed)
                    .unwrap()
                    .translation
                    .x += 1.0;
                fixture.app.update();
                assert!(
                    fixture
                        .app
                        .world()
                        .resource::<RuntimeReferenceState>()
                        .contains(fixture.reference)
                );
            }
        }

        #[test]
        fn terminal_facts_do_not_exempt_logical_or_root_changes() {
            let mut fixture = Fixture::new();
            fixture.app.world_mut().trigger(ReferenceStateChanged {
                reference: fixture.reference,
            });
            assert!(
                fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .contains(fixture.reference)
            );

            for change in ["transform", "visibility", "disabled"] {
                let mut fixture = Fixture::new();
                match change {
                    "transform" => {
                        fixture
                            .app
                            .world_mut()
                            .get_mut::<Transform>(fixture.root)
                            .unwrap()
                            .translation
                            .x += 1.0
                    }
                    "visibility" => {
                        *fixture
                            .app
                            .world_mut()
                            .get_mut::<Visibility>(fixture.root)
                            .unwrap() = Visibility::Hidden;
                    }
                    _ => {
                        fixture
                            .app
                            .world_mut()
                            .entity_mut(fixture.root)
                            .insert(bevy::ecs::entity_disabling::Disabled);
                    }
                }
                fixture.app.update();
                assert!(
                    fixture
                        .app
                        .world()
                        .resource::<RuntimeReferenceState>()
                        .contains(fixture.reference),
                    "root {change} must revoke"
                );
            }
        }

        #[test]
        fn unresolved_ownership_keeps_conservative_descendant_revocation() {
            let mut fixture = Fixture::new();
            fixture.app.insert_resource(OwnershipFacts::default());
            fixture
                .app
                .world_mut()
                .get_mut::<Transform>(fixture.flame)
                .unwrap()
                .translation
                .x += 1.0;
            fixture.app.update();
            assert!(
                fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .contains(fixture.reference)
            );
        }

        #[test]
        fn missing_owned_source_identity_keeps_conservative_revocation() {
            for marker in ["owned", "mesh", "source_mesh"] {
                let mut fixture = Fixture::new();
                {
                    let mut body = fixture.app.world_mut().entity_mut(fixture.body);
                    body.remove::<SourcePrimitive>();
                    match marker {
                        "mesh" => {
                            body.remove::<OwnedPrimitiveSource>()
                                .insert(Mesh3d(Handle::default()));
                        }
                        "source_mesh" => {
                            body.remove::<OwnedPrimitiveSource>()
                                .insert(SourcePrimitiveMesh(Handle::default()));
                        }
                        _ => {}
                    }
                }
                fixture
                    .app
                    .world_mut()
                    .get_mut::<Transform>(fixture.body)
                    .unwrap()
                    .translation
                    .x += 1.0;
                fixture.app.update();
                assert!(
                    fixture
                        .app
                        .world()
                        .resource::<RuntimeReferenceState>()
                        .contains(fixture.reference),
                    "untagged {marker} source must revoke"
                );
            }
        }

        #[test]
        fn replaced_certificate_keeps_old_owned_source_conservative() {
            let mut fixture = Fixture::new();
            let old_ordinal = fixture
                .app
                .world()
                .get::<SourcePrimitive>(fixture.body)
                .unwrap()
                .ordinal;
            assert_eq!(old_ordinal, 0);
            let (_replacement_directory, package) = replacement_certificate();
            {
                let mut facts = fixture.app.world_mut().resource_mut::<OwnershipFacts>();
                assert!(!facts.package_matches(&package));
                facts.publish(
                    &package,
                    package
                        .records()
                        .map(|(key, identity)| (*key, identity.primitive == old_ordinal))
                        .collect(),
                );
                assert!(facts.can_spawn(fixture.reference));
                assert_eq!(facts.owned_ordinals(fixture.reference), BTreeSet::from([1]));
            }
            fixture.app.update();
            assert!(
                !fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .contains(fixture.reference)
            );
            fixture
                .app
                .world()
                .get::<OwnedPrimitiveSource>(fixture.body)
                .expect("the old owned-source marker remains on the live occurrence");
            fixture
                .app
                .world_mut()
                .get_mut::<Transform>(fixture.body)
                .unwrap()
                .translation
                .x += 1.0;
            fixture.app.update();
            assert!(
                fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .contains(fixture.reference)
            );
        }
    }

    fn app() -> App {
        let mut app = App::new();
        install(&mut app);
        app
    }

    #[test]
    fn logical_changes_are_immediate_idempotent_and_reference_local() {
        let mut app = app();
        app.world_mut()
            .trigger(ReferenceStateChanged { reference: 7 });
        let state = app.world().resource::<RuntimeReferenceState>();
        assert!(state.contains(7));
        assert!(!state.contains(8));
        let snapshot = state.clone();
        app.world_mut()
            .trigger(ReferenceStateChanged { reference: 7 });
        assert_eq!(app.world().resource::<RuntimeReferenceState>().revision, 1);
        app.world_mut()
            .trigger(ReferenceStateChanged { reference: 8 });
        assert!(!snapshot.contains(8));
        assert_eq!(
            app.world()
                .resource::<RuntimeReferenceState>()
                .references()
                .collect::<Vec<_>>(),
            [7, 8]
        );
    }

    #[test]
    fn enable_changes_are_reversible_without_erasing_permanent_revocations() {
        use crate::world::reference_state::ReferenceEnableChanged;
        let mut app = app();
        app.world_mut().trigger(ReferenceEnableChanged {
            reference: 7,
            enabled: false,
        });
        assert!(app.world().resource::<RuntimeReferenceState>().contains(7));
        app.world_mut().trigger(ReferenceEnableChanged {
            reference: 7,
            enabled: true,
        });
        assert!(!app.world().resource::<RuntimeReferenceState>().contains(7));
        app.world_mut()
            .trigger(ReferenceStateChanged { reference: 7 });
        app.world_mut().trigger(ReferenceEnableChanged {
            reference: 7,
            enabled: false,
        });
        app.world_mut().trigger(ReferenceEnableChanged {
            reference: 7,
            enabled: true,
        });
        assert!(app.world().resource::<RuntimeReferenceState>().contains(7));
    }

    #[test]
    fn changed_pose_stays_revoked_after_streaming_unload_and_respawn() {
        let mut app = app();
        let resting = Transform::from_xyz(1.0, 2.0, 3.0);
        let root = app
            .world_mut()
            .spawn((FormId(7), RestingReferencePose(resting), resting))
            .id();
        app.update();
        assert!(!app.world().resource::<RuntimeReferenceState>().contains(7));
        app.world_mut()
            .get_mut::<Transform>(root)
            .unwrap()
            .translation
            .x += 1.0;
        app.update();
        assert!(app.world().resource::<RuntimeReferenceState>().contains(7));
        app.world_mut().despawn(root);
        app.world_mut()
            .spawn((FormId(7), RestingReferencePose(resting), resting));
        app.update();
        assert!(app.world().resource::<RuntimeReferenceState>().contains(7));
    }

    #[test]
    fn parent_rebase_does_not_revoke_but_explicit_reference_hide_does() {
        let mut app = app();
        let parent = app.world_mut().spawn(Transform::default()).id();
        let root = app
            .world_mut()
            .spawn((
                FormId(7),
                RestingReferencePose(Transform::default()),
                Transform::default(),
                Visibility::Inherited,
                ChildOf(parent),
            ))
            .id();
        app.update();
        app.world_mut()
            .get_mut::<Transform>(parent)
            .unwrap()
            .translation
            .x = 4096.0;
        app.update();
        assert!(!app.world().resource::<RuntimeReferenceState>().contains(7));
        *app.world_mut().get_mut::<Visibility>(root).unwrap() = Visibility::Hidden;
        app.update();
        assert!(app.world().resource::<RuntimeReferenceState>().contains(7));
    }

    #[test]
    fn disabled_root_and_animated_child_revoke_their_own_resting_references() {
        let mut app = app();
        let root = app
            .world_mut()
            .spawn((
                FormId(7),
                RestingReferencePose(Transform::default()),
                Transform::default(),
            ))
            .id();
        let other = app
            .world_mut()
            .spawn((
                FormId(8),
                RestingReferencePose(Transform::default()),
                Transform::default(),
            ))
            .id();
        let child = app
            .world_mut()
            .spawn((
                ChildOf(other),
                SourceNodePose(Transform::default()),
                Transform::default(),
            ))
            .id();
        app.update();
        assert_eq!(app.world().resource::<RuntimeReferenceState>().revision, 0);
        app.world_mut()
            .entity_mut(root)
            .insert(bevy::ecs::entity_disabling::Disabled);
        assert!(app.world().resource::<RuntimeReferenceState>().contains(7));
        assert!(!app.world().resource::<RuntimeReferenceState>().contains(8));
        app.world_mut()
            .get_mut::<Transform>(child)
            .unwrap()
            .rotation = Quat::from_rotation_y(0.5);
        app.update();
        assert!(app.world().resource::<RuntimeReferenceState>().contains(8));
    }

    #[test]
    fn animation_writes_revoke_the_resting_reference_in_the_same_frame() {
        let mut app = app();
        let root = app
            .world_mut()
            .spawn((
                FormId(7),
                RestingReferencePose(Transform::default()),
                Transform::default(),
            ))
            .id();
        app.world_mut().spawn((
            ChildOf(root),
            SourceNodePose(Transform::default()),
            Transform::default(),
            bevy::animation::AnimationTargetId::from_name(&Name::new("moving")),
        ));
        app.add_systems(
            PostUpdate,
            (|mut nodes: Query<&mut Transform, With<bevy::animation::AnimationTargetId>>| {
                for mut pose in &mut nodes {
                    pose.translation.x += 1.0;
                }
            })
            .in_set(bevy::app::AnimationSystems),
        );
        app.update();
        assert!(app.world().resource::<RuntimeReferenceState>().contains(7));
    }
}
