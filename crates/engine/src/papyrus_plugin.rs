//! Bevy glue for converted Papyrus scripts (`--papyrus-scripts`).
//!
//! The first running slice: when a placed reference spawns, the allowed scripts attached to
//! it (or to its base record) load and run `OnInit`, `OnCellAttach` and `OnLoad`. Pressing E
//! while looking at a scripted reference sends it `OnActivate`. Effects scripts queue
//! (Disable, Enable, ...) are applied to the scene each frame. Script time advances with
//! the frame clock.
//!
//! Not in this slice: trigger volumes (`OnTriggerEnter`), combat and death events, an
//! animation graph for `PlayAnimation`, and placing a reference that was never spawned.
//! Each gap logs when a script reaches it.
//!
//! Where retail differs (unverified): events fire when the reference spawns, before its model
//! has loaded (retail sends `OnLoad` once the 3D is loaded), and the E key reaches every
//! scripted reference whose model the ray hits (retail shows an activation prompt only for
//! activatable references). `docs/specs/engine/papyrus-host.md` lists the rest.

use crate::{
    door_crossing::{
        DOOR_BLOCKER_SLACK, DOOR_REACH, DoorCrossing, no_door_crossing, ray_hits_door,
    },
    papyrus_host::{Effect, HostConfig, PLAYER_REFERENCE, PapyrusHost, SLICE_SCRIPTS, SqliteForms},
    physics::{CursorCapture, PlayerBody},
    world::components::{ExpectedModelBounds, FormId, StreamingCamera},
};
use bevy::prelude::*;
use bevy_rapier3d::prelude::{Collider, ColliderDisabled, QueryFilter, ReadRapierContext};
use std::collections::HashMap;
use std::rc::Rc;

/// Marks a spawned reference that has at least one script attached.
#[derive(Component, Debug)]
pub struct ScriptedReference;

/// Marks a reference a script disabled: hidden, with its colliders off.
#[derive(Component, Debug)]
pub struct ScriptDisabled;

/// The host lives on the main thread only: a Luau VM is not `Send`.
struct PapyrusHostResource(PapyrusHost);

/// Spawned reference entities by form id, for applying effects. It holds every spawned
/// reference, not only scripted ones, because a script may disable one that has no script.
/// Entries can go stale when a cell unloads, so every use checks that the entity still
/// exists; the map is bounded by the number of distinct references seen.
#[derive(Resource, Default)]
struct ReferenceEntities(HashMap<u32, Entity>);

pub struct PapyrusScriptsPlugin;

impl Plugin for PapyrusScriptsPlugin {
    fn build(&self, app: &mut App) {
        let Some(config) = app.world().get_resource::<crate::config::EngineConfig>() else {
            warn!(target: "papyrus", "--papyrus-scripts needs a loaded world; scripts stay off");
            return;
        };
        // Off unless asked for: a pack loads exactly as before, whatever script ABI it has.
        if !config.papyrus_scripts {
            return;
        }
        let database = config.assets_dir.join("skyrim_world.db");
        let scripts = config.assets_dir.join("scripts");
        if !scripts.is_dir() {
            warn!(
                target: "papyrus",
                "--papyrus-scripts: no converted scripts folder at {}; scripts stay off",
                scripts.display()
            );
            return;
        }
        let forms = match SqliteForms::open(&database) {
            Ok(forms) => forms,
            Err(error) => {
                warn!(target: "papyrus", "--papyrus-scripts: {error}; scripts stay off");
                return;
            }
        };
        let host_config = HostConfig::new(scripts).allow_only(SLICE_SCRIPTS);
        let host = match PapyrusHost::new(host_config, Rc::new(forms)) {
            Ok(host) => host,
            Err(error) => {
                warn!(target: "papyrus", "--papyrus-scripts: the script VM did not start: {error}; scripts stay off");
                return;
            }
        };
        info!(
            target: "papyrus",
            "scripts on: attaching {} to the references that carry them",
            SLICE_SCRIPTS.join(", ")
        );
        install(app, host);
    }
}

/// Add the host and its systems to `app`. Split from [`PapyrusScriptsPlugin::build`] so tests
/// install exactly the systems the plugin installs.
pub(crate) fn install(app: &mut App, host: PapyrusHost) {
    app.insert_non_send(PapyrusHostResource(host))
        .init_resource::<ReferenceEntities>()
        .add_systems(
            Update,
            (
                begin_script_frame,
                attach_scripts,
                activate_scripted_reference,
                tick_scripts,
                apply_effects,
                keep_disabled_references_off,
            )
                .chain(),
        );
}

/// The per-frame script budget starts whole at the top of each frame's script systems.
fn begin_script_frame(host: NonSend<PapyrusHostResource>) {
    host.0.begin_frame();
}

/// Attach scripts to every reference the streamer just spawned.
fn attach_scripts(
    mut host: NonSendMut<PapyrusHostResource>,
    new: Query<(Entity, &FormId), Added<FormId>>,
    mut entities: ResMut<ReferenceEntities>,
    mut commands: Commands,
) {
    for (entity, form) in &new {
        entities.0.insert(form.0, entity);
        // The streamer spawns only enabled references, following enable parents, so a
        // spawned reference is enabled whatever its own header flag says. Tell the host
        // before any event runs, so a `Disable` from `OnCellAttach` is not ignored.
        {
            let mut world = host.0.world();
            if !world.disabled.contains(&form.0) {
                world.enabled.insert(form.0);
            }
        }
        let report = host.0.attach_reference(form.0);
        for (script, reason) in &report.skipped {
            warn!(target: "papyrus", "{:08X}: script {script} skipped: {reason}", form.0);
        }
        if !report.attached.is_empty() {
            info!(
                target: "papyrus",
                "attached {} to {:08X}",
                report.attached.join(", "),
                form.0
            );
        }
        if !report.attached.is_empty() || host.0.is_attached(form.0) {
            commands.entity(entity).insert(ScriptedReference);
        }
        // A reference that spawns again after its cell reloaded keeps what scripts did to it.
        if host.0.world().disabled.contains(&form.0) {
            commands
                .entity(entity)
                .insert((ScriptDisabled, Visibility::Hidden));
        }
    }
}

type ActivatableFields = (
    &'static FormId,
    &'static GlobalTransform,
    &'static ExpectedModelBounds,
);
type ActivatableFilter = (With<ScriptedReference>, Without<ScriptDisabled>);

/// The reference entity and everything below it. A static reference's collider sits on the
/// entity itself, a model's colliders on its descendants, so both must be reached.
fn entity_and_descendants<'a>(
    entity: Entity,
    children: &'a Query<&Children>,
) -> impl Iterator<Item = Entity> + 'a {
    std::iter::once(entity).chain(children.iter_descendants(entity))
}

/// E at a scripted reference: the same camera ray the load doors use, and the same
/// refusal when something solid is in the way.
fn activate_scripted_reference(
    (keyboard, capture, time): (
        Res<ButtonInput<KeyCode>>,
        Res<CursorCapture>,
        Res<Time<Virtual>>,
    ),
    crossing: Option<Res<DoorCrossing>>,
    camera: Query<&Transform, With<StreamingCamera>>,
    scripted: Query<ActivatableFields, ActivatableFilter>,
    (physics, player): (ReadRapierContext, Query<Entity, With<PlayerBody>>),
    host: NonSend<PapyrusHostResource>,
) {
    // The same guards as the door path: a press while the game is paused, or during a door
    // crossing's fade, is not an activation.
    if !keyboard.just_pressed(KeyCode::KeyE)
        || *capture != CursorCapture::Captured
        || time.is_paused()
        || !no_door_crossing(crossing)
    {
        return;
    }
    let Ok(view) = camera.single() else {
        return;
    };
    let direction = view.forward().as_vec3();
    let Some((form, distance)) = scripted
        .iter()
        .filter_map(|(form, transform, bounds)| {
            ray_hits_door(view.translation, direction, transform, bounds, DOOR_REACH)
                .map(|distance| (form.0, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return;
    };
    if let Ok(physics) = physics.single() {
        let filter = match player.single() {
            Ok(player) => QueryFilter::default().exclude_rigid_body(player),
            Err(_) => QueryFilter::default(),
        };
        let nearest = physics
            .cast_ray(view.translation, direction, DOOR_REACH, true, filter)
            .map(|(_, hit)| hit);
        if nearest.is_some_and(|hit| hit < distance - DOOR_BLOCKER_SLACK) {
            return;
        }
    }
    let handled = host.0.activate(form, PLAYER_REFERENCE);
    info!(
        target: "papyrus",
        "activated {form:08X}: {handled} script(s) handled OnActivate"
    );
}

/// Advance script time by the frame time.
fn tick_scripts(host: NonSend<PapyrusHostResource>, time: Res<Time>) {
    host.0.advance(time.delta_secs());
}

fn hide(
    entity: Entity,
    commands: &mut Commands,
    children: &Query<&Children>,
    colliders: &Query<(), With<Collider>>,
) {
    commands
        .entity(entity)
        .insert((ScriptDisabled, Visibility::Hidden));
    for target in entity_and_descendants(entity, children) {
        if colliders.contains(target) {
            commands.entity(target).insert(ColliderDisabled);
        }
    }
}

/// Apply what scripts queued this frame.
fn apply_effects(
    host: NonSend<PapyrusHostResource>,
    entities: Res<ReferenceEntities>,
    live: Query<(), With<FormId>>,
    children: Query<&Children>,
    colliders: Query<(), With<Collider>>,
    disabled_colliders: Query<(), With<ColliderDisabled>>,
    mut commands: Commands,
) {
    for effect in host.0.drain_effects() {
        let resident = |reference: u32| {
            entities
                .0
                .get(&reference)
                .copied()
                .filter(|entity| live.contains(*entity))
        };
        match effect {
            Effect::Disable { reference } => match resident(reference) {
                Some(entity) => {
                    info!(target: "papyrus", "script disabled {reference:08X}");
                    hide(entity, &mut commands, &children, &colliders);
                }
                None => debug!(
                    target: "papyrus",
                    "script disabled {reference:08X}, which is not loaded; it stays hidden when it spawns"
                ),
            },
            Effect::Enable { reference } => match resident(reference) {
                Some(entity) => {
                    info!(target: "papyrus", "script enabled {reference:08X}");
                    commands
                        .entity(entity)
                        .remove::<ScriptDisabled>()
                        .insert(Visibility::Inherited);
                    for target in entity_and_descendants(entity, &children) {
                        if disabled_colliders.contains(target) {
                            commands.entity(target).remove::<ColliderDisabled>();
                        }
                    }
                }
                None => warn!(
                    target: "papyrus",
                    "script enabled {reference:08X}, but the engine never spawned a reference that starts disabled; nothing appears yet"
                ),
            },
            Effect::Lock { reference, locked } => debug!(
                target: "papyrus",
                "lock state of {reference:08X} is now {locked}; the engine has no lock system yet"
            ),
            Effect::PlayAnimation { reference, event } => debug!(
                target: "papyrus",
                "animation {event} on {reference:08X} not played: no animation graph yet"
            ),
            Effect::AddToMap { reference } => debug!(
                target: "papyrus",
                "map marker {reference:08X} discovered; there is no map to show it on yet"
            ),
        }
    }
}

/// Streaming reveals a reference when its model finishes loading and adds colliders to
/// descendants as they spawn. Keep disabled references hidden and solid-free through that.
fn keep_disabled_references_off(
    disabled: Query<(Entity, &Visibility), With<ScriptDisabled>>,
    children: Query<&Children>,
    colliders: Query<Entity, (With<Collider>, Without<ColliderDisabled>)>,
    mut commands: Commands,
) {
    for (entity, visibility) in &disabled {
        if *visibility != Visibility::Hidden {
            commands.entity(entity).insert(Visibility::Hidden);
        }
        for target in entity_and_descendants(entity, &children) {
            if colliders.contains(target) {
                commands.entity(target).insert(ColliderDisabled);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::papyrus_host::MemoryForms;
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;
    use tempfile::TempDir;

    const REFERENCE: u32 = 0x1234;

    /// Minimal classes in the converter's shape: `ObjectReference.Disable` is a native
    /// wrapper, and the script disables its reference on activation and changes state.
    fn scripts() -> TempDir {
        let dir = TempDir::new().unwrap();
        let class = |name: &str, parent: &str, auto: &str, body: &str| {
            format!(
                "local Script = {{}}\nScript.__index = Script\nScript.__name = \"{name}\"\n\
                 Script.__parent = \"{parent}\"\nScript.__autoState = \"{auto}\"\n\
                 function Script.new(runtime) return setmetatable({{ __runtime = runtime }}, Script) end\n\
                 {body}\nreturn Script\n"
            )
        };
        std::fs::write(dir.path().join("form.luau"), class("Form", "", "", "")).unwrap();
        std::fs::write(
            dir.path().join("objectreference.luau"),
            class(
                "ObjectReference",
                "Form",
                "",
                "Script[\"Disable\"] = function(self, p0) local __rt = self.__runtime return __rt:native(self, \"Disable\", {p0}) end",
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("defaultdisableselfonactivate.luau"),
            class(
                "defaultDisableSelfOnActivate",
                "ObjectReference",
                "WaitingToBeActivated",
                "Script[\"WaitingToBeActivated::OnActivate\"] = function(self, p0) local __rt = self.__runtime \
                 __rt:call_method(self, \"Disable\", {false}) __rt:call_method(self, \"GotoState\", {\"Disabled\"}) end",
            ),
        )
        .unwrap();
        dir
    }

    /// `own_flag_disabled` sets the reference's own "initially disabled" header flag, as a
    /// reference has when it spawns only because its enable parent is enabled.
    fn host_with_flag(dir: &TempDir, own_flag_disabled: bool) -> PapyrusHost {
        let mut forms = MemoryForms::default();
        if own_flag_disabled {
            forms.initially_disabled.insert(REFERENCE);
        }
        forms
            .scripts
            .entry(REFERENCE)
            .or_default()
            .push(("defaultDisableSelfOnActivate".into(), vec![]));
        PapyrusHost::new(HostConfig::new(dir.path()), Rc::new(forms)).unwrap()
    }

    /// An app with the plugin's real systems, a camera at the origin looking down -Z, and a
    /// scripted reference 100 units ahead whose entity itself carries a collider, as a static
    /// reference's authored collision does. Each update advances script time by 0.1 s.
    fn live_app(dir: &TempDir) -> (App, Entity, Entity) {
        live_app_with(dir, false)
    }

    fn live_app_with(dir: &TempDir, own_flag_disabled: bool) -> (App, Entity, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                100,
            )))
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(CursorCapture::Captured);
        install(&mut app, host_with_flag(dir, own_flag_disabled));
        app.world_mut()
            .spawn((StreamingCamera, Transform::default()));
        let reference = app
            .world_mut()
            .spawn((
                FormId(REFERENCE),
                GlobalTransform::from(Transform::from_xyz(0.0, 0.0, -100.0)),
                ExpectedModelBounds {
                    min: Vec3::splat(-10.0),
                    max: Vec3::splat(10.0),
                },
                Visibility::Inherited,
                Collider::ball(10.0),
            ))
            .id();
        let bystander = app
            .world_mut()
            .spawn((FormId(0x9999), Visibility::Inherited))
            .id();
        (app, reference, bystander)
    }

    fn press_e(app: &mut App) {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(KeyCode::KeyE);
        keys.clear();
        keys.press(KeyCode::KeyE);
    }

    fn script_clock(app: &App) -> f64 {
        let host = &app.world().non_send::<PapyrusHostResource>().0;
        host.runtime().get::<f64>("clock").unwrap()
    }

    #[test]
    fn pressing_e_on_a_scripted_reference_hides_it_and_turns_its_own_collider_off() {
        let dir = scripts();
        let (mut app, reference, bystander) = live_app(&dir);
        app.update();
        assert!(app.world().get::<ScriptedReference>(reference).is_some());
        assert!(app.world().get::<ScriptedReference>(bystander).is_none());
        assert_eq!(
            app.world().get::<Visibility>(reference),
            Some(&Visibility::Inherited)
        );
        assert!(app.world().get::<ColliderDisabled>(reference).is_none());

        press_e(&mut app);
        app.update();
        assert!(app.world().get::<ScriptDisabled>(reference).is_some());
        assert_eq!(
            app.world().get::<Visibility>(reference),
            Some(&Visibility::Hidden)
        );
        assert!(
            app.world().get::<ColliderDisabled>(reference).is_some(),
            "the collider on the reference entity itself must be off"
        );
        assert_eq!(
            app.world().get::<Visibility>(bystander),
            Some(&Visibility::Inherited)
        );

        // Streaming reveals the model later and may add colliders; the plugin hides them again.
        app.world_mut()
            .entity_mut(reference)
            .insert(Visibility::Inherited);
        app.world_mut()
            .entity_mut(reference)
            .remove::<ColliderDisabled>();
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(reference),
            Some(&Visibility::Hidden)
        );
        assert!(app.world().get::<ColliderDisabled>(reference).is_some());
    }

    #[test]
    fn a_spawned_reference_hides_even_when_its_own_flag_says_disabled() {
        // The streamer spawned it, so an enable parent made it enabled; Disable must work.
        let dir = scripts();
        let (mut app, reference, _) = live_app_with(&dir, true);
        app.update();
        press_e(&mut app);
        app.update();
        assert!(app.world().get::<ScriptDisabled>(reference).is_some());
        assert_eq!(
            app.world().get::<Visibility>(reference),
            Some(&Visibility::Hidden)
        );
    }

    #[test]
    fn a_disabled_reference_cannot_be_activated_again() {
        let dir = scripts();
        let (mut app, reference, _) = live_app(&dir);
        app.update();
        press_e(&mut app);
        app.update();
        assert!(app.world().get::<ScriptDisabled>(reference).is_some());
        let before = app
            .world()
            .non_send::<PapyrusHostResource>()
            .0
            .log_lines()
            .len();
        press_e(&mut app);
        app.update();
        let host = &app.world().non_send::<PapyrusHostResource>().0;
        assert_eq!(host.log_lines().len(), before, "no second activation ran");
    }

    #[test]
    fn e_does_nothing_while_paused_during_a_door_crossing_or_off_target() {
        let dir = scripts();
        let (mut app, reference, _) = live_app(&dir);
        app.update();

        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        press_e(&mut app);
        app.update();
        assert!(
            app.world().get::<ScriptDisabled>(reference).is_none(),
            "paused"
        );
        app.world_mut().resource_mut::<Time<Virtual>>().unpause();

        app.insert_resource(DoorCrossing::holding_for_test());
        press_e(&mut app);
        app.update();
        assert!(
            app.world().get::<ScriptDisabled>(reference).is_none(),
            "door crossing"
        );
        app.world_mut().remove_resource::<DoorCrossing>();

        // Looking away from the reference: nothing is hit.
        let mut cameras = app
            .world_mut()
            .query_filtered::<Entity, With<StreamingCamera>>();
        let camera = cameras.single(app.world()).unwrap();
        app.world_mut()
            .entity_mut(camera)
            .insert(Transform::default().looking_to(Vec3::Z, Vec3::Y));
        press_e(&mut app);
        app.update();
        assert!(
            app.world().get::<ScriptDisabled>(reference).is_none(),
            "off target"
        );

        // Back on target with the cursor released: still nothing.
        app.world_mut()
            .entity_mut(camera)
            .insert(Transform::default());
        app.insert_resource(CursorCapture::Released);
        press_e(&mut app);
        app.update();
        assert!(
            app.world().get::<ScriptDisabled>(reference).is_none(),
            "cursor released"
        );

        // Everything clear: it works.
        app.insert_resource(CursorCapture::Captured);
        press_e(&mut app);
        app.update();
        assert!(app.world().get::<ScriptDisabled>(reference).is_some());
    }

    #[test]
    fn script_time_follows_the_frame_clock() {
        let dir = scripts();
        let (mut app, _, _) = live_app(&dir);
        for _ in 0..6 {
            app.update();
        }
        // The first update has no delta; each later one advances 0.1 s.
        let clock = script_clock(&app);
        assert!((clock - 0.5).abs() < 0.05, "script clock {clock}");
    }

    /// A pack-shaped folder: `skyrim_world.db` with a `scripts` table naming the script on
    /// `REFERENCE`, and a `scripts` folder holding the fixture classes plus a stale
    /// `papyrus_runtime.luau` copy, as a pack converted at an older script ABI would.
    fn old_abi_pack() -> TempDir {
        let pack = TempDir::new().unwrap();
        let scripts_dir = pack.path().join("scripts");
        std::fs::create_dir(&scripts_dir).unwrap();
        let fixtures = scripts();
        for entry in std::fs::read_dir(fixtures.path()).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), scripts_dir.join(entry.file_name())).unwrap();
        }
        std::fs::write(
            scripts_dir.join("papyrus_runtime.luau"),
            "this stale ABI 3 copy is not even valid Luau !!!",
        )
        .unwrap();
        let connection = rusqlite::Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scripts(form_id INTEGER NOT NULL, script_name TEXT NOT NULL, \
                 vmad BLOB NOT NULL, PRIMARY KEY(form_id, script_name)); \
                 CREATE TABLE script_properties(form_id INTEGER NOT NULL, script_name TEXT NOT NULL, \
                 property_index INTEGER NOT NULL, name TEXT NOT NULL, property_type INTEGER NOT NULL, \
                 status INTEGER NOT NULL, value_count INTEGER NOT NULL); \
                 CREATE TABLE script_property_values(form_id INTEGER NOT NULL, \
                 script_name TEXT NOT NULL, property_index INTEGER NOT NULL, \
                 value_index INTEGER NOT NULL, object_form_id INTEGER, alias_id INTEGER, \
                 string_value TEXT, integer_value INTEGER, float_bits INTEGER, boolean_value INTEGER);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO scripts VALUES (?1, 'defaultDisableSelfOnActivate', x'00')",
                [REFERENCE],
            )
            .unwrap();
        pack
    }

    fn app_for_pack(pack: &TempDir, papyrus_scripts: bool) -> App {
        let config = crate::config::EngineConfig {
            assets_dir: pack.path().to_path_buf(),
            papyrus_scripts,
            ..Default::default()
        };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(CursorCapture::Captured)
            .insert_resource(config)
            .add_plugins(PapyrusScriptsPlugin);
        app
    }

    #[test]
    fn without_the_flag_a_pack_loads_as_before_and_scripts_stay_off() {
        let pack = old_abi_pack();
        let mut app = app_for_pack(&pack, false);
        assert!(
            app.world().get_non_send::<PapyrusHostResource>().is_none(),
            "no host without --papyrus-scripts"
        );
        let entity = app
            .world_mut()
            .spawn((FormId(REFERENCE), Visibility::Inherited))
            .id();
        app.update();
        assert!(app.world().get::<ScriptedReference>(entity).is_none());
        assert_eq!(
            app.world().get::<Visibility>(entity),
            Some(&Visibility::Inherited)
        );
    }

    #[test]
    fn with_the_flag_an_old_abi_pack_runs_on_the_built_in_runtime() {
        let pack = old_abi_pack();
        let mut app = app_for_pack(&pack, true);
        assert!(app.world().get_non_send::<PapyrusHostResource>().is_some());
        let entity = app
            .world_mut()
            .spawn((FormId(REFERENCE), Visibility::Inherited))
            .id();
        app.update();
        // The stale runtime copy in the pack is never read: the script attached and ran.
        assert!(app.world().get::<ScriptedReference>(entity).is_some());
        let host = &app.world().non_send::<PapyrusHostResource>().0;
        assert!(host.is_attached(REFERENCE));
    }

    #[test]
    fn a_legacy_reader_pack_keeps_scripts_off_with_a_clear_message() {
        let pack = old_abi_pack();
        // A legacy-reader database has no script property tables.
        let connection = rusqlite::Connection::open(pack.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch("DROP TABLE script_properties; DROP TABLE script_property_values;")
            .unwrap();
        drop(connection);
        let error = SqliteForms::open(&pack.path().join("skyrim_world.db"))
            .err()
            .expect("a legacy database is refused");
        assert!(error.contains("legacy record reader"), "{error}");
        assert!(error.contains("Regenerate"), "{error}");
        let app = app_for_pack(&pack, true);
        assert!(
            app.world().get_non_send::<PapyrusHostResource>().is_none(),
            "scripts stay off rather than run without their properties"
        );
    }
}
