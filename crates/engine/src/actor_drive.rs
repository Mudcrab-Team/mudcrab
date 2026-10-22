//! Gait selection and engine-supplied locomotion for actors.
//!
//! The gait clips carry no root motion: `NPC Root` is identity in every clip and the converter's
//! `extractedMotion` is null. A clip therefore supplies only the pose, and the actor's speed
//! supplies the displacement. [`select_actor_gait`] picks the clip from [`ActorDrive::speed`] and
//! [`translate_driven_actors`] moves the actor along its facing by `speed * dt`.
//!
//! [`ActorDrive`] is the only thing a caller sets. Every resolved actor starts at speed zero and
//! keeps the idle clip until something drives it; no AI, navigation, or input feeds this module.

use bevy::{
    animation::{
        AnimationPlayer,
        prelude::{AnimationNodeIndex, AnimationTransitions},
    },
    prelude::*,
};
use std::time::Duration;

/// The gait an actor plays, in ascending speed order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gait {
    Idle,
    Walk,
    Run,
}

impl Gait {
    /// Every gait, in the order [`MovementTuning`] distinguishes them.
    pub const ALL: [Self; 3] = [Self::Idle, Self::Walk, Self::Run];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Walk => "walk",
            Self::Run => "run",
        }
    }
}

/// Speed thresholds for [`gait_for`], in creation units per second.
///
/// The defaults are the plan's authored walk and run speeds, so each clip's cadence roughly
/// matches the actor's ground speed. This is the actor's gait tuning, separate from the player's
/// `crate::physics::MovementTuning`; nothing here reads the player's movement state.
#[derive(Debug, Clone, Copy, Resource)]
pub struct MovementTuning {
    /// Speeds below this are unambiguously walking.
    pub walk_below: f32,
    /// Speeds at or above this play the run clip.
    pub run_at_or_above: f32,
}

impl Default for MovementTuning {
    fn default() -> Self {
        Self {
            walk_below: 160.0,
            run_at_or_above: 300.0,
        }
    }
}

/// The only component a caller sets to drive an actor.
#[derive(Debug, Clone, Copy, PartialEq, Component, Default)]
pub struct ActorDrive {
    /// Creation units per second along the actor's facing; zero holds the actor still.
    pub speed: f32,
}

impl ActorDrive {
    pub fn new(speed: f32) -> Self {
        Self { speed }
    }
}

/// The gait for `speed`.
///
/// Idle covers zero and every non-finite or negative speed. Between [`MovementTuning::walk_below`]
/// and [`MovementTuning::run_at_or_above`] there is no third gait, so the walk clip stays selected
/// until the actor reaches run speed; no speed is left without a gait.
pub fn gait_for(speed: f32, tuning: &MovementTuning) -> Gait {
    match speed {
        speed if !speed.is_finite() || speed <= 0.0 => Gait::Idle,
        speed if speed >= tuning.run_at_or_above => Gait::Run,
        speed if speed < tuning.walk_below => Gait::Walk,
        _ => Gait::Walk,
    }
}

/// The graph node each gait plays on one actor.
///
/// A missing walk clip maps to the idle node and a missing run clip to the walk node, so the actor
/// keeps animating instead of failing; the loader reports the missing clip once.
#[derive(Debug, Clone, Copy, Component)]
pub struct ActorGaitNodes {
    idle: AnimationNodeIndex,
    walk: AnimationNodeIndex,
    run: AnimationNodeIndex,
}

impl ActorGaitNodes {
    pub(crate) fn new(
        idle: AnimationNodeIndex,
        walk: AnimationNodeIndex,
        run: AnimationNodeIndex,
    ) -> Self {
        Self { idle, walk, run }
    }

    pub fn node(&self, gait: Gait) -> AnimationNodeIndex {
        match gait {
            Gait::Idle => self.idle,
            Gait::Walk => self.walk,
            Gait::Run => self.run,
        }
    }
}

/// The gait the actor is currently playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub struct ActorGait(pub Gait);

/// Crossfade length when the gait changes.
pub const GAIT_TRANSITION: Duration = Duration::from_millis(200);

/// Selects the gait clip from [`ActorDrive::speed`] and crossfades only when the gait changes.
///
/// Selecting the gait the actor already plays is a no-op, so equal-speed frames never restart the
/// clip.
fn select_actor_gait(
    tuning: Res<MovementTuning>,
    mut actors: Query<(
        &ActorDrive,
        &mut ActorGait,
        &ActorGaitNodes,
        &mut AnimationPlayer,
        &mut AnimationTransitions,
    )>,
) {
    for (drive, mut gait, nodes, mut player, mut transitions) in &mut actors {
        let wanted = gait_for(drive.speed, &tuning);
        if gait.0 == wanted {
            continue;
        }
        gait.0 = wanted;
        let node = nodes.node(wanted);
        // A missing clip maps to an already-playing gait; do not replay it on every change.
        if transitions.get_main_animation() == Some(node) {
            continue;
        }
        transitions.play(&mut player, node, GAIT_TRANSITION);
    }
}

/// Moves each driven actor along its facing by `speed * dt`.
fn translate_driven_actors(time: Res<Time>, mut actors: Query<(&ActorDrive, &mut Transform)>) {
    let delta = time.delta_secs();
    for (drive, mut transform) in &mut actors {
        advance_actor(&mut transform, drive.speed, delta);
    }
}

/// Advances one actor transform by `speed * delta` along its local forward.
///
/// No clip carries displacement, so this is the only source of actor translation.
pub(crate) fn advance_actor(transform: &mut Transform, speed: f32, delta: f32) {
    if speed == 0.0 || !speed.is_finite() || !delta.is_finite() {
        return;
    }
    transform.translation += *transform.forward() * (speed * delta);
}

/// Drives actor gait and locomotion from [`ActorDrive`].
pub struct ActorDrivePlugin;

impl Plugin for ActorDrivePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementTuning>()
            .add_systems(Update, (select_actor_gait, translate_driven_actors));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_animation::{ANIMATION_ROOT_NODE, ActorAnimationPlugin, ActorClips};
    use crate::actors::{Actor, ActorResolution, ActorSex};
    use bevy::{
        asset::AssetPlugin,
        ecs::system::RunSystemOnce,
        gltf::Gltf,
        platform::collections::HashMap,
        world_serialization::{WorldAsset, WorldAssetRoot, WorldSerializationPlugin},
    };
    use std::time::Duration;

    #[test]
    fn gait_for_buckets_every_speed() {
        let tuning = MovementTuning::default();
        assert_eq!(tuning.walk_below, 160.0);
        assert_eq!(tuning.run_at_or_above, 300.0);

        assert_eq!(gait_for(0.0, &tuning), Gait::Idle);
        assert_eq!(gait_for(f32::EPSILON, &tuning), Gait::Walk);
        assert_eq!(gait_for(-1.0, &tuning), Gait::Idle);
        assert_eq!(gait_for(f32::NAN, &tuning), Gait::Idle);
        assert_eq!(gait_for(160.0, &tuning), Gait::Walk);
        assert_eq!(gait_for(160.0 + f32::EPSILON, &tuning), Gait::Walk);
        assert_eq!(gait_for(200.0, &tuning), Gait::Walk);
        assert_eq!(gait_for(300.0, &tuning), Gait::Run);
        assert_eq!(gait_for(300.0 + f32::EPSILON, &tuning), Gait::Run);
        assert_eq!(gait_for(10_000.0, &tuning), Gait::Run);
        assert_eq!(gait_for(0.0, &tuning), Gait::Idle);
    }

    #[test]
    fn custom_thresholds_move_the_run_boundary() {
        let tuning = MovementTuning {
            walk_below: 50.0,
            run_at_or_above: 70.0,
        };
        assert_eq!(gait_for(49.0, &tuning), Gait::Walk);
        assert_eq!(gait_for(70.0, &tuning), Gait::Run);
    }

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
        .init_asset::<Gltf>()
        .init_asset::<AnimationClip>()
        .init_asset::<AnimationGraph>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<GlobalTransform>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .add_plugins((ActorAnimationPlugin, ActorDrivePlugin));
        (directory, app)
    }

    fn fixture_body_world() -> World {
        let mut world = World::new();
        let wrapper = world
            .spawn((Name::new(ANIMATION_ROOT_NODE), Transform::default()))
            .id();
        let spine = world
            .spawn((
                Name::new("NPC Spine [Spn0]"),
                Transform::default(),
                ChildOf(wrapper),
            ))
            .id();
        world.spawn((
            Name::new("NPC Head [Head]"),
            Transform::default(),
            ChildOf(spine),
        ));
        world
    }

    fn gltf_with_animation(name: &str, clip: Handle<AnimationClip>) -> Gltf {
        Gltf {
            scenes: Vec::new(),
            named_scenes: HashMap::new(),
            meshes: Vec::new(),
            named_meshes: HashMap::new(),
            materials: Vec::new(),
            named_materials: HashMap::new(),
            nodes: Vec::new(),
            named_nodes: HashMap::new(),
            skins: Vec::new(),
            named_skins: HashMap::new(),
            default_scene: None,
            animations: vec![clip.clone()],
            named_animations: HashMap::from([(name.into(), clip)]),
            source: None,
        }
    }

    /// Installs a player whose graph holds the three fixture clips and returns the actor.
    fn spawn_animated_actor(app: &mut App) -> Entity {
        let idle = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(AnimationClip::default());
        let walk = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(AnimationClip::default());
        let run = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(AnimationClip::default());
        let asset_server = app.world().resource::<AssetServer>().clone();
        let idle_gltf = asset_server.add(gltf_with_animation("mt_idle", idle));
        let walk_gltf = asset_server.add(gltf_with_animation("mt_walkforward", walk));
        let run_gltf = asset_server.add(gltf_with_animation("mt_runforward", run));
        let scene = asset_server.add(WorldAsset::new(fixture_body_world()));
        app.update();

        let actor = app
            .world_mut()
            .spawn((
                Actor::new(0x13475, 79686, ActorSex::Male, ActorResolution::Body),
                ActorClips {
                    idle: idle_gltf,
                    walk: walk_gltf,
                    run: run_gltf,
                },
                ActorDrive::default(),
                WorldAssetRoot(scene),
            ))
            .id();
        for _ in 0..8 {
            app.update();
        }
        actor
    }

    fn actor_gait(app: &App, actor: Entity) -> Gait {
        app.world().get::<ActorGait>(actor).expect("the gait").0
    }

    fn main_node(app: &App, actor: Entity) -> AnimationNodeIndex {
        app.world()
            .get::<AnimationTransitions>(actor)
            .expect("the transitions")
            .get_main_animation()
            .expect("a main animation")
    }

    fn nodes(app: &App, actor: Entity) -> ActorGaitNodes {
        *app.world().get::<ActorGaitNodes>(actor).expect("the nodes")
    }

    #[test]
    fn the_graph_holds_all_three_clips_and_starts_idle() {
        let (_directory, mut app) = fixture_app();
        let actor = spawn_animated_actor(&mut app);
        let graph = app
            .world()
            .get::<AnimationGraphHandle>(actor)
            .and_then(|handle| {
                app.world()
                    .resource::<Assets<AnimationGraph>>()
                    .get(&handle.0)
            })
            .expect("the graph asset");
        assert_eq!(
            graph.graph.node_count(),
            4,
            "the root and the three gait clips"
        );
        assert_eq!(actor_gait(&app, actor), Gait::Idle);
        assert_eq!(main_node(&app, actor), nodes(&app, actor).node(Gait::Idle));
    }

    #[test]
    fn actor_drive_ramp_selects_idle_walk_run_idle() {
        let (_directory, mut app) = fixture_app();
        let actor = spawn_animated_actor(&mut app);
        let gait_nodes = nodes(&app, actor);

        let step = |app: &mut App, speed: f32, gait: Gait| {
            app.world_mut()
                .entity_mut(actor)
                .get_mut::<ActorDrive>()
                .expect("the drive")
                .speed = speed;
            app.update();
            assert_eq!(actor_gait(app, actor), gait, "speed {speed}");
            assert_eq!(
                main_node(app, actor),
                gait_nodes.node(gait),
                "speed {speed} plays the {gait:?} node"
            );
            assert!(
                app.world()
                    .get::<AnimationPlayer>(actor)
                    .expect("the player")
                    .is_playing_animation(gait_nodes.node(gait)),
                "speed {speed} starts the {gait:?} clip"
            );
        };

        step(&mut app, 0.0, Gait::Idle);
        step(&mut app, 80.0, Gait::Walk);
        step(&mut app, 160.0, Gait::Walk);
        step(&mut app, 300.0, Gait::Run);
        step(&mut app, 0.0, Gait::Idle);
    }

    #[test]
    fn an_unchanged_gait_does_not_restart_the_clip() {
        let (_directory, mut app) = fixture_app();
        let actor = spawn_animated_actor(&mut app);
        let walk = nodes(&app, actor).node(Gait::Walk);

        app.world_mut()
            .entity_mut(actor)
            .get_mut::<ActorDrive>()
            .expect("the drive")
            .speed = 200.0;
        app.update();
        assert_eq!(actor_gait(&app, actor), Gait::Walk);
        assert_eq!(main_node(&app, actor), walk);

        app.world_mut()
            .entity_mut(actor)
            .get_mut::<AnimationPlayer>()
            .expect("the player")
            .animation_mut(walk)
            .expect("the walk animation")
            .set_seek_time(0.5);

        for _ in 0..2 {
            app.update();
            let seek = app
                .world()
                .get::<AnimationPlayer>(actor)
                .expect("the player")
                .animation(walk)
                .expect("the walk animation")
                .seek_time();
            assert_eq!(seek, 0.5, "the unchanged gait keeps its clip time");
        }
    }

    #[test]
    fn a_driven_actor_moves_along_its_facing() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(ActorDrivePlugin);
        let actor = app
            .world_mut()
            .spawn((ActorDrive::new(200.0), Transform::default()))
            .id();
        let still = app
            .world_mut()
            .spawn((ActorDrive::default(), Transform::default()))
            .id();

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.world_mut()
            .run_system_once(translate_driven_actors)
            .unwrap();

        let moved = app.world().get::<Transform>(actor).expect("the transform");
        assert!(
            (moved.translation.length() - 200.0).abs() < 0.01,
            "one second at 200 moves 200 units, got {}",
            moved.translation.length()
        );
        assert!(
            moved.translation.z < -199.9,
            "the actor moves along its local forward"
        );
        let unmoved = app.world().get::<Transform>(still).expect("the transform");
        assert_eq!(unmoved.translation, Vec3::ZERO, "zero speed does not move");
    }

    #[test]
    fn translation_follows_the_actors_rotation() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(ActorDrivePlugin);
        let actor = app
            .world_mut()
            .spawn((
                ActorDrive::new(100.0),
                Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
            ))
            .id();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.world_mut()
            .run_system_once(translate_driven_actors)
            .unwrap();
        let moved = app.world().get::<Transform>(actor).expect("the transform");
        assert!(
            moved.translation.x < -99.9,
            "a quarter turn faces local forward down -X, got {:?}",
            moved.translation
        );
    }
}
