//! Filter owned render bodies in a private template before WorldAssetRoot can spawn them.

use super::{OwnershipDeferred, OwnershipFacts, OwnershipReference};
use crate::cluster::{
    flat_source::OriginalStockScene,
    source_identity::{SourcePrimitive, SourcePrimitiveMesh},
};
use bevy::{asset::AssetId, prelude::*, world_serialization::WorldAssetRoot};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Nonrender source data for strict bounds/material checks and physics.
/// The embedded values are not ECS render components and are never extracted.
#[derive(Component, Clone, Reflect)]
#[reflect(Component)]
pub(crate) struct OwnedPrimitiveSource {
    pub(crate) mesh: Mesh3d,
    pub(crate) material: Option<MeshMaterial3d<StandardMaterial>>,
}

struct FilteredTemplate {
    scene: Handle<WorldAsset>,
    failure: Option<String>,
}

#[derive(Resource, Default)]
struct Templates(HashMap<(AssetId<WorldAsset>, Vec<u32>), FilteredTemplate>);

pub(super) fn install(app: &mut App) {
    app.register_type::<OwnedPrimitiveSource>()
        .register_type::<SourcePrimitiveMesh>()
        .init_resource::<Templates>()
        .add_message::<AssetEvent<WorldAsset>>()
        .add_systems(Update, source_changes);
}

/// Clone all gameplay/source data, granting render bodies only to the fixed
/// certified refusal set. New source ordinals never acquire stock permission.
pub(crate) fn clone_with_stock_ordinals(
    source: &WorldAsset,
    stock: &BTreeSet<u32>,
    registry: &AppTypeRegistry,
) -> Result<WorldAsset, String> {
    let mut scene = source
        .clone_with(registry)
        .map_err(|error| format!("cannot clone owned source template: {error}"))?;
    let mut remove = Vec::new();
    let mut seen = HashSet::new();
    for node in scene.world.iter_entities() {
        if !node.contains::<Mesh3d>() {
            continue;
        }
        let tag = node
            .get::<SourcePrimitive>()
            .ok_or("owned source mesh is missing occurrence identity")?;
        if !tag.matches_default_scene || !seen.insert(tag.ordinal) {
            return Err("owned source occurrence identity is invalid or duplicated".to_owned());
        }
        if !stock.contains(&tag.ordinal) {
            remove.push(node.id());
        }
    }
    if !stock.iter().all(|ordinal| seen.contains(ordinal)) {
        return Err("owned source lost a permanently refused ordinal".to_owned());
    }
    for entity in remove {
        let mut node = scene.world.entity_mut(entity);
        let mesh = node
            .take::<Mesh3d>()
            .ok_or("owned source mesh disappeared")?;
        let material = node.take::<MeshMaterial3d<StandardMaterial>>();
        node.insert((
            SourcePrimitiveMesh(mesh.0.clone()),
            OwnedPrimitiveSource { mesh, material },
        ))
        .remove::<(
            MeshMaterial3d<crate::nif_depth::NifDepthMaterial>,
            bevy::light::NotShadowCaster,
            bevy::light::NotShadowReceiver,
            bevy::camera::visibility::VisibilityClass,
            bevy::camera::visibility::ViewVisibility,
            bevy::camera::primitives::Aabb,
        )>();
    }
    Ok(scene)
}

/// A source/stock-allowlist pair shares one filtered asset across its placements.
/// An empty allowlist retains gameplay/source nodes with no render bodies.
/// The source asset remains complete and keeps the original dependency lifetime.
pub(crate) fn filtered_scene(
    world: &mut World,
    source: &Handle<WorldAsset>,
    stock: &BTreeSet<u32>,
) -> Result<Handle<WorldAsset>, String> {
    world.init_resource::<Templates>();
    let key = (source.id(), stock.iter().copied().collect::<Vec<_>>());
    if let Some(cached) = world.resource::<Templates>().0.get(&key) {
        return match &cached.failure {
            Some(reason) => Err(reason.clone()),
            None => Ok(cached.scene.clone()),
        };
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    // Partial promotion also uses this helper in apps without ownership systems.
    registry.write().register::<OwnedPrimitiveSource>();
    registry.write().register::<SourcePrimitiveMesh>();
    let scene = {
        let assets = world.resource::<Assets<WorldAsset>>();
        let original = assets
            .get(source)
            .ok_or("owned source scene is unavailable")?;
        clone_with_stock_ordinals(original, stock, &registry)?
    };
    let scene = world.resource_mut::<Assets<WorldAsset>>().add(scene);
    world.resource_mut::<Templates>().0.insert(
        key,
        FilteredTemplate {
            scene: scene.clone(),
            failure: None,
        },
    );
    Ok(scene)
}

/// Keep the ordinary readiness failure path, but never give a raw source to the
/// spawner. A later AssetServer retry cannot instantiate an owned render body.
pub(crate) fn fail_closed(world: &mut World, root: Entity, source: Handle<WorldAsset>) {
    let empty = world
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    world
        .entity_mut(root)
        .insert((WorldAssetRoot(empty), OriginalStockScene(source)))
        .remove::<(crate::streaming::PendingModel, OwnershipDeferred)>();
}

/// Render role is independent of runtime query disablers, including custom ones.
pub(super) fn has_render_body(template: &WorldAsset) -> bool {
    template
        .world
        .iter_entities()
        .any(|entity| entity.contains::<Mesh3d>())
}

pub(crate) fn arm(world: &mut World, root: Entity, source: Handle<WorldAsset>) {
    let has_render_body = world
        .resource::<Assets<WorldAsset>>()
        .get(&source)
        .map(has_render_body);
    if has_render_body == Some(false) {
        // The ownership gate admits loaded gameplay-only sources before a certificate.
        // They have no render body to filter or suppress, but must retain their data.
        world
            .entity_mut(root)
            .insert(WorldAssetRoot(source))
            .remove::<(crate::streaming::PendingModel, OwnershipDeferred)>();
        return;
    }
    let reference = world
        .get::<OwnershipReference>(root)
        .map(|reference| reference.0);
    let selection = reference.and_then(|reference| {
        world.get_resource::<OwnershipFacts>().map(|facts| {
            let mut stock = facts
                .certificate
                .as_ref()
                .and_then(|package| package.placed_reference(reference))
                .and_then(|placed| placed.stock_primitives.clone())
                .unwrap_or_default();
            stock.extend(facts.permanent_refused_ordinals(reference));
            (
                facts.can_spawn(reference),
                facts.references.contains_key(&reference),
                stock,
            )
        })
    });
    let Some((decided, mapped, stock)) = selection else {
        world
            .entity_mut(root)
            .insert(WorldAssetRoot(source))
            .remove::<crate::streaming::PendingModel>();
        return;
    };
    if !decided {
        let failed = world.get_resource::<AssetServer>().is_some_and(|server| {
            server
                .get_load_states(source.id())
                .is_some_and(|(load, _, recursive)| {
                    matches!(load, bevy::asset::LoadState::Failed(_))
                        || matches!(
                            recursive,
                            bevy::asset::RecursiveDependencyLoadState::Failed(_)
                        )
                })
        });
        if has_render_body == Some(true) && !failed {
            // A queued arming command must preserve the gate's undecided state.
            world.entity_mut(root).insert(OwnershipDeferred);
            if let Some(reference) = reference
                && let Some(mut demand) = world.get_resource_mut::<super::OwnershipBatchDemand>()
            {
                demand.push(reference);
            }
            return;
        }
        // Missing/failed source loads still reach ordinary readiness/error accounting.
        // Retain their original handle without permitting a later raw owned retry.
        fail_closed(world, root, source);
        return;
    }
    if !mapped {
        // Wholly convert-refused and out-of-scope references keep the ordinary
        // loader. A mapped reference always uses its fixed stock allowlist,
        // including when every mapped record was initially material-refused.
        world
            .entity_mut(root)
            .insert(WorldAssetRoot(source))
            .remove::<(crate::streaming::PendingModel, OwnershipDeferred)>();
        return;
    }
    match filtered_scene(world, &source, &stock) {
        Ok(scene) => {
            let filtered = scene != source;
            let mut node = world.entity_mut(root);
            node.insert(WorldAssetRoot(scene))
                .remove::<(crate::streaming::PendingModel, OwnershipDeferred)>();
            if filtered {
                node.insert(OriginalStockScene(source));
            }
        }
        Err(reason) => {
            error!(?root, %reason, "owned source instancing failed; no stock body is permitted");
            fail_closed(world, root, source);
            refuse_source_instance(
                world,
                root,
                format!("owned source instancing failed: {reason}"),
            );
        }
    }
}

/// Record a real instance failure before publishing the shared process-terminal outcome.
/// Keeping these effects outside the once-only carrier preserves later producer evidence.
pub(crate) fn refuse_source_instance(world: &mut World, root: Entity, reason: String) {
    if let Ok(mut root) = world.get_entity_mut(root) {
        root.insert(crate::streaming::AssetFailed);
    }
    crate::streaming::failure::refuse_loading(world, reason);
}

fn source_changes(mut events: MessageReader<AssetEvent<WorldAsset>>, mut commands: Commands) {
    let changed: HashSet<_> = events
        .read()
        .filter_map(|event| match event {
            AssetEvent::Added { id } | AssetEvent::Modified { id } | AssetEvent::Removed { id } => {
                Some(*id)
            }
            _ => None,
        })
        .collect();
    if changed.is_empty() {
        return;
    }
    commands.queue(move |world: &mut World| {
        let refresh: Vec<_> = world.resource::<Templates>().0.iter()
            .filter(|((source, _), _)| changed.contains(source))
            .map(|((source, stock), cached)| ((*source, stock.clone()), cached.scene.clone()))
            .collect();
        let registry = world.resource::<AppTypeRegistry>().clone();
        for (key, handle) in refresh {
            let stock = key.1.iter().copied().collect::<BTreeSet<_>>();
            let result = world.resource::<Assets<WorldAsset>>().get(key.0)
                .ok_or_else(|| "owned source scene was removed".to_owned())
                .and_then(|scene| clone_with_stock_ordinals(scene, &stock, &registry));
            let (scene, mut failure) = match result {
                Ok(scene) => (scene, None),
                Err(reason) => (WorldAsset::new(World::new()), Some(reason)),
            };
            if let Err(error) = world.resource_mut::<Assets<WorldAsset>>().insert(handle.id(), scene) {
                let reason = format!("cannot refresh filtered owned source: {error}");
                failure = Some(match failure { Some(first) => format!("{first}; {reason}"), None => reason });
            }
            if let Some(cached) = world.resource_mut::<Templates>().0.get_mut(&key) {
                cached.failure = failure.clone();
            }
            if let Some(reason) = failure {
                error!(%reason, source = ?key.0, "edited owned source refused; render bodies remain absent");
                // Only the failure path of an actual source mutation inspects live bindings.
                // Iterate unfiltered so hidden/disabled retained roots keep their obligation.
                // A cached unused variant does not make a process-terminal claim, but its
                // retained failure above prevents a later caller accepting an empty substitute.
                let roots: Vec<_> = world.iter_entities().filter_map(|node| {
                    node.get::<WorldAssetRoot>()
                        .is_some_and(|root| root.0.id() == handle.id())
                        .then_some(node.id())
                }).collect();
                for root in roots {
                    refuse_source_instance(world, root, format!("edited owned source refused: {reason}"));
                }
            }
        }
    });
}

#[cfg(test)]
mod terminal_source_tests {
    use super::*;
    use crate::streaming::failure::{LoadingRefusal, refuse_loading};

    fn app(package: &std::sync::Arc<crate::cluster::reader::CertifiedPackage>) -> App {
        let mut app = App::new();
        app.init_resource::<Assets<WorldAsset>>()
            .init_resource::<OwnershipFacts>()
            .register_type::<Mesh3d>()
            .register_type::<SourcePrimitive>();
        install(&mut app);
        app.world_mut().resource_mut::<OwnershipFacts>().publish(
            package,
            package.records().map(|(key, _)| (*key, false)).collect(),
        );
        app
    }

    fn original(app: &mut App, tag: Option<SourcePrimitive>) -> (Handle<WorldAsset>, Entity) {
        let mut source = World::new();
        let mut node = source.spawn(Mesh3d(Handle::default()));
        if let Some(tag) = tag {
            node.insert(tag);
        }
        let node = node.id();
        (
            app.world_mut()
                .resource_mut::<Assets<WorldAsset>>()
                .add(WorldAsset::new(source)),
            node,
        )
    }

    fn assert_refusal(
        app: &App,
        earlier: Option<&str>,
        exits: &mut bevy::ecs::message::MessageCursor<AppExit>,
    ) {
        let failure = app.world().get_resource::<LoadingRefusal>();
        assert!(
            failure.is_some(),
            "actual owned-source failure must reach the terminal owner"
        );
        if let Some(reason) = earlier {
            assert_eq!(failure.unwrap().0, reason);
        } else {
            assert!(failure.unwrap().0.contains("occurrence identity"));
        }
        // The first failure may precede app.update(); count both retained buffers.
        let emitted: Vec<_> = exits
            .read(app.world().resource::<Messages<AppExit>>())
            .cloned()
            .collect();
        assert_eq!(emitted, [AppExit::error()]);
    }

    #[test]
    fn malformed_owned_instancing_refuses_and_never_releases_raw_geometry() {
        for earlier in [None, Some("original full-cell failure")] {
            let (_directory, package, reference, _tag, _key) = super::super::tests::fixture();
            let mut app = app(&package);
            let mut exits = app
                .world()
                .resource::<Messages<AppExit>>()
                .get_cursor_current();
            let (source, _) = original(&mut app, None);
            let reason = filtered_scene(app.world_mut(), &source, &BTreeSet::new()).unwrap_err();
            assert!(
                reason.contains("occurrence identity"),
                "fixture must reach identity validation: {reason}"
            );
            let root = app
                .world_mut()
                .spawn((OwnershipReference(reference), OwnershipDeferred))
                .id();
            if let Some(reason) = earlier {
                refuse_loading(app.world_mut(), reason.into());
            }
            arm(app.world_mut(), root, source.clone());
            let filtered = &app.world().get::<WorldAssetRoot>(root).unwrap().0;
            assert_ne!(filtered.id(), source.id());
            assert!(app.world().get::<OwnershipDeferred>(root).is_none());
            assert!(
                app.world()
                    .get::<crate::streaming::AssetFailed>(root)
                    .is_some()
            );
            assert!(
                !app.world()
                    .resource::<Assets<WorldAsset>>()
                    .get(filtered)
                    .unwrap()
                    .world
                    .iter_entities()
                    .any(|node| node.contains::<Mesh3d>())
            );
            assert_refusal(&app, earlier, &mut exits);
        }
    }

    #[test]
    fn active_owned_source_refresh_failure_refuses_after_clearing_its_render_body() {
        for earlier in [None, Some("original renderer failure")] {
            let (_directory, package, reference, tag, _key) = super::super::tests::fixture();
            let mut app = app(&package);
            let mut exits = app
                .world()
                .resource::<Messages<AppExit>>()
                .get_cursor_current();
            let (source, node) = original(&mut app, Some(tag));
            let root = app.world_mut().spawn(OwnershipReference(reference)).id();
            arm(app.world_mut(), root, source.clone());
            let filtered = app.world().get::<WorldAssetRoot>(root).unwrap().0.clone();
            assert_ne!(filtered.id(), source.id());
            assert!(app.world().get::<OriginalStockScene>(root).is_some());
            app.world_mut()
                .resource_mut::<Assets<WorldAsset>>()
                .get_mut(&source)
                .unwrap()
                .world
                .entity_mut(node)
                .remove::<SourcePrimitive>();
            if let Some(reason) = earlier {
                refuse_loading(app.world_mut(), reason.into());
            }
            app.world_mut()
                .write_message(AssetEvent::<WorldAsset>::Modified { id: source.id() });
            app.update();
            assert_eq!(
                app.world().get::<WorldAssetRoot>(root).unwrap().0.id(),
                filtered.id()
            );
            assert!(
                !app.world()
                    .resource::<Assets<WorldAsset>>()
                    .get(&filtered)
                    .unwrap()
                    .world
                    .iter_entities()
                    .any(|node| node.contains::<Mesh3d>())
            );
            assert!(
                app.world()
                    .get::<crate::streaming::AssetFailed>(root)
                    .is_some()
            );
            assert_refusal(&app, earlier, &mut exits);
        }
    }

    #[test]
    fn inactive_cached_owned_template_retains_failure_for_next_use_without_exiting() {
        let (_directory, package, _reference, tag, _key) = super::super::tests::fixture();
        let mut app = app(&package);
        let (source, node) = original(&mut app, Some(tag));
        let filtered = filtered_scene(app.world_mut(), &source, &BTreeSet::new()).unwrap();
        app.world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .get_mut(&source)
            .unwrap()
            .world
            .entity_mut(node)
            .remove::<SourcePrimitive>();
        app.world_mut()
            .write_message(AssetEvent::<WorldAsset>::Modified { id: source.id() });
        app.update();
        assert!(
            !app.world().contains_resource::<LoadingRefusal>(),
            "an unused cached variant is not a current process-terminal source obligation"
        );
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
        assert!(
            !app.world()
                .resource::<Assets<WorldAsset>>()
                .get(&filtered)
                .unwrap()
                .world
                .iter_entities()
                .any(|node| node.contains::<Mesh3d>())
        );
        let result = filtered_scene(app.world_mut(), &source, &BTreeSet::new());
        assert!(
            result.is_err(),
            "a cached empty substitute must not hide a failed source on its next use"
        );
        assert!(result.unwrap_err().contains("occurrence identity"));
    }

    #[test]
    fn inactive_owned_variant_recovers_only_after_a_valid_source_refresh() {
        let (_directory, package, _reference, tag, _key) = super::super::tests::fixture();
        let mut app = app(&package);
        let (source, node) = original(&mut app, Some(tag));
        let filtered = filtered_scene(app.world_mut(), &source, &BTreeSet::new()).unwrap();
        app.world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .get_mut(&source)
            .unwrap()
            .world
            .entity_mut(node)
            .remove::<SourcePrimitive>();
        app.world_mut()
            .write_message(AssetEvent::<WorldAsset>::Modified { id: source.id() });
        app.update();
        assert!(filtered_scene(app.world_mut(), &source, &BTreeSet::new()).is_err());
        app.world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .get_mut(&source)
            .unwrap()
            .world
            .entity_mut(node)
            .insert(tag);
        app.world_mut()
            .write_message(AssetEvent::<WorldAsset>::Modified { id: source.id() });
        app.update();
        let repaired = filtered_scene(app.world_mut(), &source, &BTreeSet::new())
            .expect("valid refresh must recover an unused cached variant");
        assert_eq!(repaired.id(), filtered.id());
        let assets = app.world().resource::<Assets<WorldAsset>>();
        let repaired = &assets.get(&repaired).unwrap().world;
        assert!(
            !repaired
                .iter_entities()
                .any(|node| node.contains::<Mesh3d>())
        );
        assert!(
            repaired
                .iter_entities()
                .any(|node| node.contains::<OwnedPrimitiveSource>())
        );
        assert!(!app.world().contains_resource::<LoadingRefusal>());
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
    }

    #[test]
    fn inactive_removed_owned_source_recovers_when_reinserted_with_added() {
        let (_directory, package, _reference, tag, _key) = super::super::tests::fixture();
        let mut app = app(&package);
        let (source, _) = original(&mut app, Some(tag));
        let filtered = filtered_scene(app.world_mut(), &source, &BTreeSet::new()).unwrap();
        let removed = app
            .world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .remove(source.id())
            .expect("fixture source was resident");
        app.world_mut()
            .write_message(AssetEvent::<WorldAsset>::Removed { id: source.id() });
        app.update();
        let error = filtered_scene(app.world_mut(), &source, &BTreeSet::new()).unwrap_err();
        assert!(error.contains("removed"));
        assert!(!app.world().contains_resource::<LoadingRefusal>());
        // Assets::remove keeps a still-live ID; inserting the value back emits Added.
        // This fixture explicitly publishes that native event because it owns plain Assets.
        app.world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .insert(source.id(), removed)
            .unwrap();
        app.world_mut()
            .write_message(AssetEvent::<WorldAsset>::Added { id: source.id() });
        app.update();
        let repaired = filtered_scene(app.world_mut(), &source, &BTreeSet::new()).expect(
            "native Added must rebuild a recovered source rather than retain removal failure",
        );
        assert_eq!(repaired.id(), filtered.id());
        let assets = app.world().resource::<Assets<WorldAsset>>();
        let repaired = &assets.get(&repaired).unwrap().world;
        assert!(
            !repaired
                .iter_entities()
                .any(|node| node.contains::<Mesh3d>())
        );
        assert!(
            repaired
                .iter_entities()
                .any(|node| node.contains::<OwnedPrimitiveSource>())
        );
        assert!(!app.world().contains_resource::<LoadingRefusal>());
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
    }
}
