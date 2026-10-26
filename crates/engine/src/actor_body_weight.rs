//! An actor's body weight (`NPC_/NAM7`) on the `thin` morph target of its composed body.
//!
//! The composed body GLB carries one morph target named `thin` holding the weight-0 deltas, with
//! morph weight 0 leaving the heavy body the converter wrote before weights existed. The catalog
//! stores NAM7 in 0..100, so the morph value is `1 - NAM7 / 100`: weight 20 gives 0.8. A pack or a
//! body without the target is a no-op, because every older body is heavy.

use bevy::{app::SceneSpawnerSystems, mesh::morph::MorphWeights, prelude::*};

/// Register after `NifMaterialPlugin` and the scene spawner that gives actors their children.
pub struct ActorBodyWeightPlugin;

impl Plugin for ActorBodyWeightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            SpawnScene,
            apply_actor_body_weight.after(SceneSpawnerSystems::WorldInstanceSpawn),
        );
    }
}

/// The morph weight for an actor's `thin` body target: `1 - NPC_/NAM7 / 100`, in 0..1.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ActorBodyWeight(pub f32);

/// Marks a weighted actor whose morph mesh had not loaded when its scene was instantiated, so
/// [`apply_actor_body_weight`] tries again once a mesh arrives.
#[derive(Component)]
struct AwaitingBodyWeightMesh;

type WeightedActors<'world, 'state> = Query<
    'world,
    'state,
    (
        Entity,
        &'static ActorBodyWeight,
        Ref<'static, Children>,
        Has<AwaitingBodyWeightMesh>,
    ),
    Or<(Changed<Children>, With<AwaitingBodyWeightMesh>)>,
>;

/// Sets the `thin` weight on every [`MorphWeights`] node below a newly instantiated actor.
///
/// Bevy puts `MorphWeights` on the glTF node entity and `MeshMorphWeights::Reference(node)` on the
/// primitives, so the systems walks descendants for the node component rather than the meshes. The
/// target index comes from `mesh.extras.targetNames` through [`Mesh::morph_target_names`].
fn apply_actor_body_weight(
    mut commands: Commands,
    actors: WeightedActors,
    descendants: Query<&Children>,
    mut morphs: Query<&mut MorphWeights>,
    meshes: Res<Assets<Mesh>>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
) {
    let mesh_arrived = mesh_events.read().any(|event| {
        matches!(
            event,
            AssetEvent::Added { .. } | AssetEvent::LoadedWithDependencies { .. }
        )
    });
    for (actor, weight, children, awaiting) in &actors {
        if !children.is_changed() && !mesh_arrived {
            continue;
        }
        let mut missing = false;
        for descendant in descendants.iter_descendants(actor) {
            let Ok(mut morph) = morphs.get_mut(descendant) else {
                continue;
            };
            let first_mesh = morph.first_mesh().cloned();
            let Some(mesh) = first_mesh.as_ref().and_then(|handle| meshes.get(handle)) else {
                missing = true;
                continue;
            };
            let Some(index) = mesh
                .morph_target_names()
                .and_then(|names| names.iter().position(|name| name == "thin"))
            else {
                continue;
            };
            if let Some(slot) = morph.weights_mut().get_mut(index) {
                *slot = weight.0.clamp(0.0, 1.0);
            }
        }
        match (missing, awaiting) {
            (true, false) => {
                commands.entity(actor).insert(AwaitingBodyWeightMesh);
            }
            (false, true) => {
                commands.entity(actor).remove::<AwaitingBodyWeightMesh>();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{asset::RenderAssetUsages, mesh::PrimitiveTopology};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .add_systems(Update, apply_actor_body_weight);
        app
    }

    fn mesh(app: &mut App, names: &[&str]) -> Handle<Mesh> {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.set_morph_target_names(names.iter().map(|name| (*name).to_owned()).collect());
        app.world_mut().resource_mut::<Assets<Mesh>>().add(mesh)
    }

    fn actor(
        app: &mut App,
        weight: Option<f32>,
        mesh: &Handle<Mesh>,
        target_count: usize,
    ) -> (Entity, Entity) {
        let world = app.world_mut();
        let node = world
            .spawn(MorphWeights::new(vec![0.0; target_count], Some(mesh.clone())).unwrap())
            .id();
        let mut actor = world.spawn_empty();
        if let Some(weight) = weight {
            actor.insert(ActorBodyWeight(weight));
        }
        (actor.add_child(node).id(), node)
    }

    fn thin_weight(app: &App, node: Entity) -> f32 {
        app.world().get::<MorphWeights>(node).unwrap().weights()[0]
    }

    #[test]
    fn the_weight_reaches_the_thin_target() {
        let mut app = app();
        let mesh = mesh(&mut app, &["thin"]);
        let (_, node) = actor(&mut app, Some(0.8), &mesh, 1);
        app.update();
        assert_eq!(thin_weight(&app, node), 0.8);
    }

    #[test]
    fn the_target_is_found_by_name_not_position() {
        let mut app = app();
        let mesh = mesh(&mut app, &["other", "thin"]);
        let (_, node) = actor(&mut app, Some(0.5), &mesh, 2);
        app.update();
        let weights = app.world().get::<MorphWeights>(node).unwrap().weights();
        assert_eq!(weights, [0.0, 0.5]);
    }

    #[test]
    fn an_actor_without_a_weight_keeps_the_heavy_body() {
        let mut app = app();
        let mesh = mesh(&mut app, &["thin"]);
        let (_, node) = actor(&mut app, None, &mesh, 1);
        app.update();
        assert_eq!(thin_weight(&app, node), 0.0);
    }

    #[test]
    fn a_body_without_the_thin_target_is_untouched() {
        let mut app = app();
        let mesh = mesh(&mut app, &["other"]);
        let (_, node) = actor(&mut app, Some(0.8), &mesh, 1);
        app.update();
        assert_eq!(thin_weight(&app, node), 0.0);
    }

    #[test]
    fn a_late_mesh_is_applied_when_it_arrives() {
        let mut app = app();
        let late = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .reserve_handle();
        let (actor, node) = actor(&mut app, Some(0.8), &late, 1);
        app.update();
        assert_eq!(thin_weight(&app, node), 0.0);
        assert!(app.world().get::<AwaitingBodyWeightMesh>(actor).is_some());

        let mut loaded = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        loaded.set_morph_target_names(vec!["thin".to_owned()]);
        app.world_mut()
            .resource_mut::<Assets<Mesh>>()
            .insert(&late, loaded)
            .unwrap();
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(thin_weight(&app, node), 0.8);
        assert!(app.world().get::<AwaitingBodyWeightMesh>(actor).is_none());
    }
}
