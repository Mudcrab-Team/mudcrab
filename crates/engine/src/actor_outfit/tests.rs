use super::*;
use crate::actors::{ActorResolution, ActorSex};
use bevy::{
    asset::AssetPlugin,
    ecs::schedule::SingleThreadedExecutor,
    log::tracing_subscriber::{
        Registry,
        layer::{Layer, SubscriberExt},
    },
    mesh::skinning::SkinnedMeshInverseBindposes,
    world_serialization::WorldSerializationPlugin,
};
use shared::actor_outfits::{BIPED_BODY, BIPED_FEET, BIPED_HANDS};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tracing::{Event, Level, Subscriber};

const SPINE: &str = "NPC Spine [Spn0]";
const CALF: &str = "NPC L Calf [LClf]";

fn fixture_app() -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: directory.path().to_string_lossy().into_owned(),
            ..default()
        },
        WorldSerializationPlugin,
    ))
    .init_asset::<SkinnedMeshInverseBindposes>()
    .register_type::<ChildOf>()
    .register_type::<Children>()
    .register_type::<GlobalTransform>()
    .register_type::<Name>()
    .register_type::<Transform>()
    .register_type::<SkinnedMesh>()
    .register_type::<Visibility>()
    .add_plugins(ActorOutfitPlugin);
    (directory, app)
}

fn bindposes(app: &mut App, count: usize) -> Handle<SkinnedMeshInverseBindposes> {
    app.world_mut()
        .resource_mut::<Assets<SkinnedMeshInverseBindposes>>()
        .add(SkinnedMeshInverseBindposes::from(vec![
            Mat4::IDENTITY;
            count
        ]))
}

/// A composed body as the converter lays it out: the wrapper, two bones, and the four shapes of the
/// retail male body (underwear torso, upper body, hands, feet), each skinned to the bones.
fn body_world(inverse_bindposes: Handle<SkinnedMeshInverseBindposes>, root: bool) -> World {
    let mut world = World::new();
    let wrapper = world
        .spawn((
            Name::new(if root {
                crate::actor_animation::ANIMATION_ROOT_NODE
            } else {
                "not the root"
            }),
            Transform::default(),
        ))
        .id();
    let spine = world
        .spawn((Name::new(SPINE), Transform::default(), ChildOf(wrapper)))
        .id();
    let calf = world
        .spawn((Name::new(CALF), Transform::default(), ChildOf(spine)))
        .id();
    for shape in [
        "MaleUnderwear_1",
        "MaleUnderwearBody:0",
        "HandMaleBig3rd",
        "FootMale_Big",
    ] {
        world.spawn((
            Name::new(shape),
            SkinnedMesh {
                inverse_bindposes: inverse_bindposes.clone(),
                joints: vec![spine, calf],
            },
            Transform::default(),
            ChildOf(wrapper),
        ));
    }
    world
}

/// A worn part's scene: its own copy of the bones plus one skinned mesh.
fn part_world(
    inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    joint_names: &[&str],
) -> World {
    let mut world = World::new();
    let joints: Vec<Entity> = joint_names
        .iter()
        .map(|name| {
            world
                .spawn((Name::new((*name).to_owned()), Transform::default()))
                .id()
        })
        .collect();
    world.spawn((
        Name::new("Torso"),
        SkinnedMesh {
            inverse_bindposes,
            joints,
        },
        Transform::default(),
    ));
    world
}

fn skinless_part_world() -> World {
    let mut world = World::new();
    world.spawn((Name::new("Amulet"), Transform::default()));
    world
}

fn part(mask: u32) -> OutfitPart {
    OutfitPart {
        armo_id: 0x5b69f,
        armature_id: 0x5b6a2,
        biped_mask: mask,
        armature_race_id: 25,
        scene: "meshes/clothes/blacksmith/m/torso_1.glb".into(),
    }
}

/// An actor whose body is instantiated and whose skeleton the outfit system has captured.
fn actor_with_captured_body(app: &mut App, root: bool) -> Entity {
    let poses = bindposes(app, 2);
    let body = app
        .world()
        .resource::<AssetServer>()
        .add(WorldAsset::new(body_world(poses, root)));
    let actor = app
        .world_mut()
        .spawn((
            Actor::new(0x13475, 79686, ActorSex::Male, ActorResolution::BodyHead),
            // No parts: the system captures the skeleton and asks for nothing.
            ActorOutfit(Vec::new()),
            WorldAssetRoot(body),
        ))
        .id();
    app.update();
    app.world_mut().entity_mut(actor).insert(ActorBodyReady);
    app.update();
    actor
}

fn wear(app: &mut App, actor: Entity, part: OutfitPart, world: World) -> Entity {
    let asset = app
        .world()
        .resource::<AssetServer>()
        .add(WorldAsset::new(world));
    app.world_mut()
        .spawn((
            ActorOutfitInstance {
                actor,
                base_form_id: 0x13475,
                part,
            },
            WorldAssetRoot(asset),
            ChildOf(actor),
        ))
        .id()
}

fn body_shape(app: &mut App, name: &str) -> Entity {
    let mut query = app.world_mut().query::<(Entity, &Name)>();
    query
        .iter(app.world())
        .find(|(_, candidate)| candidate.as_str() == name)
        .map(|(entity, _)| entity)
        .unwrap_or_else(|| panic!("no entity named {name}"))
}

fn shape_hidden(app: &mut App, name: &str) -> bool {
    let entity = body_shape(app, name);
    app.world().get::<Visibility>(entity) == Some(&Visibility::Hidden)
}

fn instance_skins(app: &mut App, instance: Entity) -> Vec<SkinnedMesh> {
    let mut stack = vec![instance];
    let mut found = Vec::new();
    while let Some(entity) = stack.pop() {
        if let Some(skin) = app.world().get::<SkinnedMesh>(entity) {
            found.push(skin.clone());
        }
        if let Some(children) = app.world().get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
    found
}

#[derive(Clone, Default)]
struct WarningCount(Arc<AtomicUsize>);

impl<S: Subscriber> Layer<S> for WarningCount {
    fn on_event(
        &self,
        event: &Event<'_>,
        _context: bevy::log::tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        if *metadata.level() == Level::WARN && metadata.target() == "engine::actor_outfit" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn count_warnings(app: &mut App, frames: usize) -> usize {
    app.edit_schedule(Update, |schedule| {
        schedule.set_executor(SingleThreadedExecutor::new());
    });
    let warnings = WarningCount::default();
    let subscriber = Registry::default().with(warnings.clone());
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..frames {
            app.update();
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    warnings.0.load(Ordering::Relaxed)
}

#[test]
fn composed_body_shapes_map_to_their_regions_by_name() {
    for (name, region) in [
        ("MaleUnderwear_1", BodyRegion::Body),
        ("MaleUnderwearBody:0", BodyRegion::Body),
        ("FemaleUnderwear", BodyRegion::Body),
        ("HandMaleBig3rd", BodyRegion::Hands),
        ("HandFemale3rd", BodyRegion::Hands),
        ("FootMale_Big", BodyRegion::Feet),
        ("FootFemale_Big", BodyRegion::Feet),
    ] {
        assert_eq!(BodyRegion::of_shape(name), region, "{name}");
    }
}

#[test]
fn the_outfit_system_captures_the_body_bones_and_regions_once() {
    let (_directory, mut app) = fixture_app();
    let actor = actor_with_captured_body(&mut app, true);
    let skeleton = app.world().get::<ActorOutfitSkeleton>(actor).unwrap();
    assert!(skeleton.bones.contains_key(SPINE) && skeleton.bones.contains_key(CALF));
    let mut regions: Vec<_> = skeleton.body_meshes.iter().map(|(r, _)| *r).collect();
    regions.sort_by_key(|region| *region as u8);
    assert_eq!(
        regions,
        [
            BodyRegion::Body,
            BodyRegion::Body,
            BodyRegion::Hands,
            BodyRegion::Feet
        ]
    );
    assert!(app.world().get::<ActorOutfitResolved>(actor).is_some());
}

#[test]
fn an_actor_whose_body_has_no_skeleton_is_reported_once_and_keeps_its_skin() {
    let (_directory, mut app) = fixture_app();
    let poses = bindposes(&mut app, 2);
    let body = app
        .world()
        .resource::<AssetServer>()
        .add(WorldAsset::new(body_world(poses, false)));
    let actor = app
        .world_mut()
        .spawn((
            Actor::new(0x13475, 79686, ActorSex::Male, ActorResolution::BodyHead),
            ActorOutfit(vec![part(BIPED_BODY)]),
            WorldAssetRoot(body),
        ))
        .id();
    app.update();
    app.world_mut().entity_mut(actor).insert(ActorBodyReady);
    let warnings = count_warnings(&mut app, 4);
    assert_eq!(warnings, 1);
    assert!(app.world().get::<ActorOutfitRequests>(actor).is_none());
    assert!(app.world().get::<ActorOutfitResolved>(actor).is_some());
}

#[test]
fn a_torso_part_is_rehosted_onto_the_body_and_hides_only_the_body_shapes() {
    let (_directory, mut app) = fixture_app();
    let actor = actor_with_captured_body(&mut app, true);
    let (spine, calf) = {
        let bones = &app.world().get::<ActorOutfitSkeleton>(actor).unwrap().bones;
        (bones[SPINE], bones[CALF])
    };
    let poses = bindposes(&mut app, 2);
    let instance = wear(
        &mut app,
        actor,
        part(BIPED_BODY),
        part_world(poses.clone(), &[SPINE, CALF]),
    );
    for _ in 0..4 {
        app.update();
    }
    let skins = instance_skins(&mut app, instance);
    assert_eq!(skins.len(), 1);
    assert_eq!(
        skins[0].joints,
        vec![spine, calf],
        "joints are the body's bones"
    );
    assert_eq!(
        skins[0].inverse_bindposes, poses,
        "bind poses are untouched"
    );
    assert_eq!(
        app.world().get::<ChildOf>(instance).map(ChildOf::parent),
        Some(actor)
    );
    for hidden in ["MaleUnderwear_1", "MaleUnderwearBody:0"] {
        assert!(
            shape_hidden(&mut app, hidden),
            "{hidden} is covered by the torso"
        );
    }
    for shown in ["HandMaleBig3rd", "FootMale_Big"] {
        assert!(!shape_hidden(&mut app, shown), "{shown} is not covered");
    }
}

#[test]
fn hands_and_feet_parts_hide_their_own_regions() {
    let (_directory, mut app) = fixture_app();
    let actor = actor_with_captured_body(&mut app, true);
    let poses = bindposes(&mut app, 2);
    wear(
        &mut app,
        actor,
        part(BIPED_HANDS | BIPED_FEET),
        part_world(poses, &[SPINE, CALF]),
    );
    for _ in 0..4 {
        app.update();
    }
    assert!(shape_hidden(&mut app, "HandMaleBig3rd"));
    assert!(shape_hidden(&mut app, "FootMale_Big"));
    assert!(!shape_hidden(&mut app, "MaleUnderwear_1"));
}

#[test]
fn a_part_with_an_unknown_joint_is_reported_once_removed_and_hides_nothing() {
    let (_directory, mut app) = fixture_app();
    let actor = actor_with_captured_body(&mut app, true);
    let poses = bindposes(&mut app, 2);
    let instance = wear(
        &mut app,
        actor,
        part(BIPED_BODY),
        part_world(poses, &[SPINE, "NPC Missing [Bone]"]),
    );
    let warnings = count_warnings(&mut app, 4);
    assert_eq!(warnings, 1);
    assert!(
        app.world().get_entity(instance).is_err(),
        "the part is removed"
    );
    assert!(app.world().get_entity(actor).is_ok(), "the actor stays");
    assert!(!shape_hidden(&mut app, "MaleUnderwear_1"));
}

#[test]
fn a_skinless_part_is_reported_once_removed_and_hides_nothing() {
    let (_directory, mut app) = fixture_app();
    let actor = actor_with_captured_body(&mut app, true);
    let instance = wear(&mut app, actor, part(BIPED_BODY), skinless_part_world());
    let warnings = count_warnings(&mut app, 4);
    assert_eq!(warnings, 1);
    assert!(app.world().get_entity(instance).is_err());
    assert!(!shape_hidden(&mut app, "MaleUnderwear_1"));
}

#[test]
fn a_part_whose_scene_fails_to_load_is_reported_once_and_skin_stays() {
    let (_directory, mut app) = fixture_app();
    let poses = bindposes(&mut app, 2);
    let body = app
        .world()
        .resource::<AssetServer>()
        .add(WorldAsset::new(body_world(poses, true)));
    let actor = app
        .world_mut()
        .spawn((
            Actor::new(0x13475, 79686, ActorSex::Male, ActorResolution::BodyHead),
            ActorOutfit(vec![part(BIPED_BODY)]),
            WorldAssetRoot(body),
        ))
        .id();
    app.update();
    app.world_mut().entity_mut(actor).insert(ActorBodyReady);
    let warnings = count_warnings(&mut app, 200);
    assert_eq!(warnings, 1, "the missing scene is reported once");
    assert!(app.world().get::<ActorOutfitRequests>(actor).is_none());
    assert!(!shape_hidden(&mut app, "MaleUnderwear_1"));
}

#[test]
fn an_actor_without_an_outfit_is_never_touched() {
    let (_directory, mut app) = fixture_app();
    let poses = bindposes(&mut app, 2);
    let body = app
        .world()
        .resource::<AssetServer>()
        .add(WorldAsset::new(body_world(poses, true)));
    let actor = app
        .world_mut()
        .spawn((
            Actor::new(0x13475, 79686, ActorSex::Male, ActorResolution::BodyHead),
            WorldAssetRoot(body),
        ))
        .id();
    app.update();
    app.world_mut().entity_mut(actor).insert(ActorBodyReady);
    app.update();
    assert!(app.world().get::<ActorOutfitResolved>(actor).is_none());
    assert!(app.world().get::<ActorOutfitSkeleton>(actor).is_none());
}
