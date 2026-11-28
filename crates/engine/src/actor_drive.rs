//! Gait selection and engine-supplied locomotion for actors.
//!
//! The gait clips carry no root motion: `NPC Root` is identity in every clip and the converter's
//! `extractedMotion` is null. A clip therefore supplies only the pose, and the actor's speed
//! supplies the displacement. [`select_actor_gait`] picks the clip from [`ActorDrive::speed`] and
//! [`translate_driven_actors`] moves the actor along its facing by `speed * dt`.
//!
//! [`ActorDrive`] is the only thing a caller sets. Every resolved actor starts at speed zero and
//! keeps the idle clip until something drives it; no AI, navigation, or input feeds this module.
//!
//! An actor whose records resolve carries [`ActorMovementSpeeds`]: its race's `WKMV` and `RNMV`
//! movement types and its `NPC_.ACBS` speed multiplier (#1065, #1066). [`ActorDrive::walking`]
//! and [`ActorDrive::running`] take their speed from it, and the run clip starts at the actor's
//! own run speed. An actor without those records keeps the stand-in [`MovementTuning`].

use crate::actors::Actor;
use crate::movement_speeds::ActorMovementSpeeds;
use crate::streaming::PendingAssetProfile;
use bevy::{
    animation::{
        AnimationPlayer,
        prelude::{AnimationNodeIndex, AnimationTransitions},
    },
    prelude::*,
};
use std::{collections::HashSet, time::Duration};

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

/// Stand-in speed thresholds for [`gait_for`], in creation units per second.
///
/// An actor with [`ActorMovementSpeeds`] does not use these: its own run speed is the run
/// threshold. This is the gait tuning of an actor whose records did not resolve (a world
/// database without movement types, or an actor whose movement type is missing from it). It is
/// separate from the player's `crate::physics::MovementTuning`; nothing here reads the player's
/// movement state.
#[derive(Debug, Clone, Copy, Resource)]
pub struct MovementTuning {
    /// Speeds below this are unambiguously walking.
    pub walk_below: f32,
    /// Speeds at or above this play the run clip.
    pub run_at_or_above: f32,
}

impl Default for MovementTuning {
    fn default() -> Self {
        // STAND-IN(#1066): an actor without resolved record speeds has no run speed to compare
        // with, so the earlier project thresholds stay for that actor only.
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

    /// Drives the actor at its own record walk speed.
    pub fn walking(speeds: &ActorMovementSpeeds) -> Self {
        Self::new(speeds.walk)
    }

    /// Drives the actor at its own record run speed.
    pub fn running(speeds: &ActorMovementSpeeds) -> Self {
        Self::new(speeds.run)
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

/// The gait for `speed` when the actor's own record speeds are known.
///
/// Idle covers zero and every non-finite or negative speed. A speed at or above the actor's own
/// run speed plays the run clip. Every other speed plays the walk clip.
///
/// STAND-IN(#1066): no record says which clip an actor plays at a speed between its walk and
/// run speeds, so this keeps the earlier rule that only the run threshold changes the clip.
pub fn gait_for_actor(speed: f32, speeds: &ActorMovementSpeeds) -> Gait {
    match speed {
        speed if !speed.is_finite() || speed <= 0.0 => Gait::Idle,
        speed if speeds.run > 0.0 && speed >= speeds.run => Gait::Run,
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

type ActorGaitComponents<'a> = (
    Entity,
    Option<&'a Actor>,
    &'a ActorDrive,
    Option<&'a ActorMovementSpeeds>,
    &'a mut ActorGait,
    &'a ActorGaitNodes,
    &'a mut AnimationPlayer,
    &'a mut AnimationTransitions,
);

/// Selects the gait clip from [`ActorDrive::speed`] and crossfades only when the gait changes.
///
/// Selecting the gait the actor already plays is a no-op, so equal-speed frames never restart the
/// clip.
fn select_actor_gait(
    tuning: Res<MovementTuning>,
    mut logged_zero_run: Local<HashSet<Entity>>,
    mut actors: Query<ActorGaitComponents<'_>>,
) {
    for (entity, actor, drive, record_speeds, mut gait, nodes, mut player, mut transitions) in
        &mut actors
    {
        let wanted = match record_speeds {
            Some(speeds) => {
                if drive.speed.is_finite()
                    && drive.speed > 0.0
                    && speeds.run <= 0.0
                    && logged_zero_run.insert(entity)
                {
                    if let Some(actor) = actor {
                        warn!(
                            entity = ?entity,
                            base_form_id = format_args!("{:08X}", actor.base_form_id),
                            race_id = format_args!("{:08X}", actor.race_id),
                            speed = drive.speed,
                            run_speed = speeds.run,
                            "actor driven with non-positive record run speed; keeping walk gait"
                        );
                    } else {
                        warn!(
                            entity = ?entity,
                            speed = drive.speed,
                            run_speed = speeds.run,
                            "actor driven with non-positive record run speed; keeping walk gait"
                        );
                    }
                }
                gait_for_actor(drive.speed, speeds)
            }
            None => gait_for(drive.speed, &tuning),
        };
        if gait.0 == wanted {
            continue;
        }
        gait.0 = wanted;
        let node = nodes.node(wanted);
        // A missing clip maps to an already-playing gait; do not replay it on every change.
        if transitions.get_main_animation() == Some(node) {
            continue;
        }
        // A transition starts a fresh animation in single-play mode; a held gait must loop.
        transitions
            .play(&mut player, node, GAIT_TRANSITION)
            .repeat();
    }
}

/// Moves each driven actor along its facing by `speed * dt`.
fn translate_driven_actors(
    time: Res<Time>,
    mut actors: Query<(&ActorDrive, &mut Transform), Without<PendingAssetProfile>>,
) {
    let delta = time.delta_secs();
    for (drive, mut transform) in &mut actors {
        advance_actor(&mut transform, drive.speed, delta);
    }
}

/// Advances one actor transform by `speed * delta` along its local forward.
///
/// No clip carries displacement, so this is the only source of actor translation. Speeds
/// [`gait_for`] treats as idle leave the actor in place.
pub(crate) fn advance_actor(transform: &mut Transform, speed: f32, delta: f32) {
    if speed <= 0.0 || !speed.is_finite() || !delta.is_finite() {
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
        animation::RepeatAnimation,
        asset::AssetPlugin,
        ecs::system::RunSystemOnce,
        gltf::Gltf,
        platform::collections::HashMap,
        world_serialization::{WorldAsset, WorldAssetRoot, WorldSerializationPlugin},
    };
    use std::time::Duration;

    #[test]
    fn advance_actor_leaves_an_idle_speed_in_place() {
        for speed in [0.0, -100.0, f32::NAN] {
            let mut transform = Transform::default();
            advance_actor(&mut transform, speed, 0.5);
            assert_eq!(transform.translation, Vec3::ZERO, "speed {speed}");
        }
        let mut transform = Transform::default();
        advance_actor(&mut transform, 10.0, 0.5);
        assert_eq!(transform.translation, Vec3::NEG_Z * 5.0);
    }

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

    fn speeds(percent: u16) -> ActorMovementSpeeds {
        ActorMovementSpeeds::new(
            80.1,
            370.0,
            crate::movement_speeds::SpeedMultiplier::new(percent),
        )
    }

    #[test]
    fn an_actors_own_run_speed_is_its_run_threshold() {
        let plain = speeds(100);
        assert_eq!(gait_for_actor(0.0, &plain), Gait::Idle);
        assert_eq!(gait_for_actor(f32::NAN, &plain), Gait::Idle);
        assert_eq!(gait_for_actor(-1.0, &plain), Gait::Idle);
        assert_eq!(gait_for_actor(plain.walk, &plain), Gait::Walk);
        assert_eq!(gait_for_actor(369.9, &plain), Gait::Walk);
        assert_eq!(gait_for_actor(plain.run, &plain), Gait::Run);
        // The stand-in 300 threshold no longer decides: this actor runs at 370.
        assert_eq!(gait_for_actor(300.0, &plain), Gait::Walk);

        // A 150 percent actor runs at 555, so 400 is still a walk clip for it.
        let fast = speeds(150);
        assert_eq!(fast.run, 555.0);
        assert_eq!(gait_for_actor(400.0, &fast), Gait::Walk);
        assert_eq!(gait_for_actor(555.0, &fast), Gait::Run);

        // A zero run speed does not turn a positive walk speed into a run clip.
        let walk_only =
            ActorMovementSpeeds::new(80.1, 0.0, crate::movement_speeds::SpeedMultiplier::NEUTRAL);
        assert_eq!(gait_for_actor(0.0, &walk_only), Gait::Idle);
        assert_eq!(gait_for_actor(80.1, &walk_only), Gait::Walk);
    }

    #[test]
    fn walking_and_running_drives_take_the_actors_record_speeds() {
        let fast = speeds(150);
        assert_eq!(ActorDrive::walking(&fast).speed, fast.walk);
        assert_eq!(ActorDrive::running(&fast).speed, fast.run);
        assert_eq!(
            gait_for_actor(ActorDrive::walking(&fast).speed, &fast),
            Gait::Walk
        );
        assert_eq!(
            gait_for_actor(ActorDrive::running(&fast).speed, &fast),
            Gait::Run
        );
    }

    #[test]
    fn a_150_percent_actor_covers_one_and_a_half_times_the_distance() {
        let mut slow = Transform::default();
        let mut fast = Transform::default();
        advance_actor(&mut slow, ActorDrive::running(&speeds(100)).speed, 1.0);
        advance_actor(&mut fast, ActorDrive::running(&speeds(150)).speed, 1.0);
        assert!((slow.translation.length() - 370.0).abs() < 0.01);
        assert!((fast.translation.length() - 555.0).abs() < 0.01);
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
            assert_eq!(
                app.world()
                    .get::<AnimationPlayer>(actor)
                    .expect("the player")
                    .animation(gait_nodes.node(gait))
                    .expect("the gait animation")
                    .repeat_mode(),
                RepeatAnimation::Forever,
                "speed {speed} keeps the {gait:?} clip repeating"
            );
        };

        step(&mut app, 0.0, Gait::Idle);
        step(&mut app, 80.0, Gait::Walk);
        step(&mut app, 160.0, Gait::Walk);
        step(&mut app, 300.0, Gait::Run);
        step(&mut app, 0.0, Gait::Idle);
    }

    #[test]
    fn record_speeds_replace_the_stand_in_run_threshold_in_the_graph() {
        let (_directory, mut app) = fixture_app();
        let actor = spawn_animated_actor(&mut app);
        app.world_mut().entity_mut(actor).insert(speeds(100));
        let gait_nodes = nodes(&app, actor);

        let mut step = |speed: f32, gait: Gait| {
            app.world_mut()
                .entity_mut(actor)
                .get_mut::<ActorDrive>()
                .expect("the drive")
                .speed = speed;
            app.update();
            assert_eq!(actor_gait(&app, actor), gait, "speed {speed}");
            assert_eq!(
                main_node(&app, actor),
                gait_nodes.node(gait),
                "speed {speed}"
            );
        };
        step(0.0, Gait::Idle);
        // 300 is the stand-in run threshold. This actor's own run speed is 370.
        step(300.0, Gait::Walk);
        step(370.0, Gait::Run);
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

    #[test]
    fn driven_streaming_actor_waits_for_asset_readiness_before_moving() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(ActorDrivePlugin);
        let authored = Transform::from_translation(Vec3::new(3.0, -4.0, 5.0));
        let actor = app
            .world_mut()
            .spawn((
                ActorDrive::new(200.0),
                authored,
                crate::world::components::WorldTransform(authored.to_matrix()),
                PendingAssetProfile::test_actor(),
            ))
            .id();
        let idle = app
            .world_mut()
            .spawn((ActorDrive::default(), Transform::from_xyz(7.0, 8.0, 9.0)))
            .id();
        let nonfinite = app
            .world_mut()
            .spawn((
                ActorDrive::new(f32::INFINITY),
                Transform::from_xyz(-2.0, 6.0, 4.0),
            ))
            .id();
        let pending = app.world().get::<PendingAssetProfile>(actor).unwrap();
        assert!(
            !pending.instance_spawned_for_test(),
            "unarmed/loading reference"
        );

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.world_mut()
            .run_system_once(translate_driven_actors)
            .unwrap();
        assert_eq!(app.world().get::<Transform>(actor), Some(&authored));
        assert_eq!(
            app.world()
                .get::<crate::world::components::WorldTransform>(actor)
                .unwrap()
                .0,
            authored.to_matrix(),
            "authored placement remains valid during loading"
        );

        // Instance exists, but track_asset_readiness has not validated it or removed the marker.
        app.world_mut()
            .entity_mut(actor)
            .get_mut::<PendingAssetProfile>()
            .unwrap()
            .mark_instance_spawned_for_test();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.world_mut()
            .run_system_once(translate_driven_actors)
            .unwrap();
        assert_eq!(app.world().get::<Transform>(actor), Some(&authored));

        // Readiness completion removes PendingAssetProfile; ordinary driving resumes next run.
        app.world_mut()
            .entity_mut(actor)
            .remove::<PendingAssetProfile>();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.world_mut()
            .run_system_once(translate_driven_actors)
            .unwrap();
        let moved = app.world().get::<Transform>(actor).unwrap();
        assert!((moved.translation - (authored.translation + Vec3::NEG_Z * 200.0)).length() < 0.01);
        assert_eq!(
            app.world().get::<Transform>(idle).unwrap().translation,
            Vec3::new(7.0, 8.0, 9.0),
            "zero speed remains still"
        );
        assert_eq!(
            app.world().get::<Transform>(nonfinite).unwrap().translation,
            Vec3::new(-2.0, 6.0, 4.0),
            "non-finite speed remains still"
        );
    }
}
