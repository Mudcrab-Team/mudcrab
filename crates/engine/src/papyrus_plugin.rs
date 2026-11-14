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

use crate::{
    door_crossing::{DOOR_BLOCKER_SLACK, DOOR_REACH, ray_hits_door},
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

/// Spawned reference entities by form id, for applying effects. Entries can go stale when
/// a cell unloads, so every use checks that the entity still exists.
#[derive(Resource, Default)]
struct ReferenceEntities(HashMap<u32, Entity>);

pub struct PapyrusScriptsPlugin;

impl Plugin for PapyrusScriptsPlugin {
    fn build(&self, app: &mut App) {
        let Some(config) = app.world().get_resource::<crate::config::EngineConfig>() else {
            warn!(target: "papyrus", "--papyrus-scripts needs a loaded world; scripts stay off");
            return;
        };
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
        app.insert_non_send(PapyrusHostResource(host))
            .init_resource::<ReferenceEntities>()
            .add_systems(
                Update,
                (
                    attach_scripts,
                    activate_scripted_reference,
                    tick_scripts,
                    apply_effects,
                    keep_disabled_references_off,
                )
                    .chain(),
            );
    }
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
        let report = host.0.attach_reference(form.0);
        for (script, reason) in &report.skipped {
            warn!(target: "papyrus", "{:08X}: script {script} skipped: {reason}", form.0);
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

/// E at a scripted reference: the same camera ray the load doors use, and the same
/// refusal when something solid is in the way.
fn activate_scripted_reference(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Res<CursorCapture>,
    camera: Query<&Transform, With<StreamingCamera>>,
    scripted: Query<ActivatableFields, ActivatableFilter>,
    (physics, player): (ReadRapierContext, Query<Entity, With<PlayerBody>>),
    host: NonSend<PapyrusHostResource>,
) {
    if !keyboard.just_pressed(KeyCode::KeyE) || *capture != CursorCapture::Captured {
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
    for descendant in children.iter_descendants(entity) {
        if colliders.contains(descendant) {
            commands.entity(descendant).insert(ColliderDisabled);
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
                    for descendant in children.iter_descendants(entity) {
                        if disabled_colliders.contains(descendant) {
                            commands.entity(descendant).remove::<ColliderDisabled>();
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
        for descendant in children.iter_descendants(entity) {
            if colliders.contains(descendant) {
                commands.entity(descendant).insert(ColliderDisabled);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::papyrus_host::{MemoryForms, PLAYER_REFERENCE};
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

    #[test]
    fn a_spawned_scripted_reference_hides_when_its_script_disables_it() {
        let dir = scripts();
        let mut forms = MemoryForms::default();
        forms
            .scripts
            .entry(REFERENCE)
            .or_default()
            .push(("defaultDisableSelfOnActivate".into(), vec![]));
        let host = PapyrusHost::new(HostConfig::new(dir.path()), Rc::new(forms)).unwrap();

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_non_send(PapyrusHostResource(host))
            .init_resource::<ReferenceEntities>()
            .add_systems(
                Update,
                (attach_scripts, apply_effects, keep_disabled_references_off).chain(),
            );
        let entity = app
            .world_mut()
            .spawn((FormId(REFERENCE), Visibility::Inherited))
            .id();
        let bystander = app
            .world_mut()
            .spawn((FormId(0x9999), Visibility::Inherited))
            .id();
        app.update();
        assert!(app.world().get::<ScriptedReference>(entity).is_some());
        assert!(app.world().get::<ScriptedReference>(bystander).is_none());
        assert_eq!(
            app.world().get::<Visibility>(entity),
            Some(&Visibility::Inherited)
        );

        // The player activates it; the next frame applies the Disable effect.
        let handled = app
            .world()
            .non_send::<PapyrusHostResource>()
            .0
            .activate(REFERENCE, PLAYER_REFERENCE);
        assert_eq!(handled, 1);
        app.update();
        assert!(app.world().get::<ScriptDisabled>(entity).is_some());
        assert_eq!(
            app.world().get::<Visibility>(entity),
            Some(&Visibility::Hidden)
        );
        assert_eq!(
            app.world().get::<Visibility>(bystander),
            Some(&Visibility::Inherited)
        );

        // Streaming reveals the model later; the plugin hides the reference again.
        app.world_mut()
            .entity_mut(entity)
            .insert(Visibility::Inherited);
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(entity),
            Some(&Visibility::Hidden)
        );
    }
}
