//! One crosshair target drives both the action prompt and E activation.

use crate::{
    console::ConsoleState,
    door_crossing::{DOOR_REACH, DoorCrossing, door_view_blocked, fallback_bounds, ray_hits_door},
    doors::LoadDoor,
    physics::{CursorCapture, DebugTankard, DynamicClutter, HeldTankard, PlayerBody},
    world::components::{ExpectedModelBounds, StreamingCamera},
};
use bevy::prelude::*;
use bevy_rapier3d::prelude::{QueryFilter, ReadRapierContext};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionAction {
    Pickup(Entity),
    Drop,
    Door(Entity),
}

#[derive(Resource, Default)]
pub(crate) struct InteractionTarget {
    pub action: Option<InteractionAction>,
}

pub(crate) type PickupFilter = Or<(With<DebugTankard>, With<DynamicClutter>)>;

#[derive(Resource, Default)]
struct DoorLabels(HashMap<u32, String>);

#[derive(Component)]
struct InteractionPrompt;

pub(crate) struct InteractionPlugin;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum InteractionSet {
    Target,
    Pickup,
}

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InteractionTarget>()
            .init_resource::<DoorLabels>()
            .add_systems(Startup, setup_prompt)
            .add_systems(
                PostUpdate,
                update_interaction_target
                    .after(bevy::transform::TransformSystems::Propagate)
                    .in_set(InteractionSet::Target),
            );
    }
}

fn setup_prompt(
    mut commands: Commands,
    config: Res<crate::config::EngineConfig>,
    mut labels: ResMut<DoorLabels>,
) {
    // A small projection, read once; no SQLite access in the frame loop.
    let path = config.assets_dir.join("skyrim_world.db");
    if let Ok(conn) =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        && let Ok(mut query) = conn.prepare(
            "SELECT ref_id,destination_name FROM door_links WHERE destination_name IS NOT NULL",
        )
        && let Ok(rows) = query.query_map([], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
        })
    {
        labels.0.extend(rows.filter_map(Result::ok));
    }
    commands
        .spawn((
            Name::new("Interaction prompt"),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Percent(54.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
        ))
        .with_children(|parent| {
            parent.spawn((
                InteractionPrompt,
                Text::new(""),
                TextFont::from_font_size(22.0),
                TextColor(Color::WHITE),
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.75)),
                Node {
                    padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                    display: Display::None,
                    ..default()
                },
            ));
        });
}

/// A collider may be a child of the authored rigid-body reference.
pub(crate) fn pickup_root(
    mut entity: Entity,
    parents: &Query<&ChildOf>,
    objects: &Query<(), PickupFilter>,
) -> Option<Entity> {
    loop {
        if objects.get(entity).is_ok() {
            return Some(entity);
        }
        entity = parents.get(entity).ok()?.parent();
    }
}

fn choose_action(
    held: bool,
    pickup: Option<(Entity, f32)>,
    door: Option<(Entity, f32)>,
) -> Option<InteractionAction> {
    if held {
        return Some(InteractionAction::Drop);
    }
    match (pickup, door) {
        (Some((_, distance)), Some((door, door_distance))) if distance >= door_distance => {
            Some(InteractionAction::Door(door))
        }
        (Some((object, _)), _) => Some(InteractionAction::Pickup(object)),
        (_, Some((door, _))) => Some(InteractionAction::Door(door)),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_interaction_target(
    capture: Res<CursorCapture>,
    console: Option<Res<ConsoleState>>,
    crossing: Option<Res<DoorCrossing>>,
    held: Res<HeldTankard>,
    camera: Query<&Transform, With<StreamingCamera>>,
    doors: Query<(
        Entity,
        &LoadDoor,
        &GlobalTransform,
        Option<&ExpectedModelBounds>,
    )>,
    objects: Query<(), PickupFilter>,
    parents: Query<&ChildOf>,
    (context, player): (ReadRapierContext, Query<Entity, With<PlayerBody>>),
    labels: Res<DoorLabels>,
    mut target: ResMut<InteractionTarget>,
    mut prompt: Query<(&mut Text, &mut Node), With<InteractionPrompt>>,
) {
    let action = if *capture != CursorCapture::Captured
        || console.is_some_and(|console| console.open)
        || crossing.is_some_and(|crossing| crossing.is_active())
    {
        None
    } else if let Ok(camera) = camera.single() {
        let direction = camera.forward().as_vec3();
        let hit = context.single().ok().and_then(|context| {
            let filter = player.single().map_or_else(
                |_| QueryFilter::default(),
                |player| QueryFilter::default().exclude_rigid_body(player),
            );
            context.cast_ray(camera.translation, direction, 240.0, true, filter)
        });
        let pickup = hit.and_then(|(entity, distance)| {
            pickup_root(entity, &parents, &objects).map(|root| (root, distance))
        });
        let door = doors
            .iter()
            .filter_map(|(entity, _, pose, bounds)| {
                let distance = ray_hits_door(
                    camera.translation,
                    direction,
                    pose,
                    &bounds.copied().unwrap_or_else(fallback_bounds),
                    DOOR_REACH,
                )?;
                (!door_view_blocked(hit.map(|(_, distance)| distance), distance))
                    .then_some((entity, distance))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        choose_action(
            held.0.is_some_and(|entity| objects.get(entity).is_ok()),
            pickup,
            door,
        )
    } else {
        None
    };
    target.action = action;
    let text = match action {
        Some(InteractionAction::Drop) => "E · Drop".to_owned(),
        Some(InteractionAction::Pickup(_)) => "E · Pick up".to_owned(),
        Some(InteractionAction::Door(entity)) => doors
            .get(entity)
            .ok()
            .map(|(_, door, _, _)| {
                let name = labels
                    .0
                    .get(&door.ref_id)
                    .map(String::as_str)
                    .unwrap_or("interior");
                if door.destination.interior_cell_id.is_some() {
                    format!("E · Enter {name}")
                } else {
                    format!(
                        "E · Exit to {}",
                        labels
                            .0
                            .get(&door.ref_id)
                            .map(String::as_str)
                            .unwrap_or("outside")
                    )
                }
            })
            .unwrap_or_default(),
        None => String::new(),
    };
    if let Ok((mut label, mut node)) = prompt.single_mut() {
        if label.0 != text {
            label.0 = text;
        }
        let display = if action.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn door_prompt_uses_its_destination_and_disappears_behind_a_wall() {
        let mut app = crate::physics::headless::fixture_app();
        app.init_resource::<HeldTankard>()
            .init_resource::<InteractionTarget>();
        app.insert_resource(DoorLabels(HashMap::from([(
            7,
            "Sleeping Giant Inn".to_owned(),
        )])));
        let prompt = app
            .world_mut()
            .spawn((InteractionPrompt, Text::new(""), Node::default()))
            .id();
        let door = app
            .world_mut()
            .spawn((
                LoadDoor {
                    ref_id: 7,
                    destination: crate::doors::DoorDestination {
                        destination_ref_id: 8,
                        interior_cell_id: Some(9),
                        worldspace_id: None,
                        arrival_position: [0.0; 3],
                        arrival_rotation: [0.0; 3],
                    },
                },
                GlobalTransform::from_translation(Vec3::new(500.0, 100.0, -160.0)),
                ExpectedModelBounds::new(Vec3::splat(-10.0), Vec3::splat(10.0)).unwrap(),
            ))
            .id();
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<StreamingCamera>>()
            .single(app.world())
            .unwrap();
        let pose = Transform::from_xyz(500.0, 100.0, 0.0);
        app.update();
        *app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap() = pose;
        app.insert_resource(CursorCapture::Captured);
        app.world_mut()
            .run_system_once(update_interaction_target)
            .unwrap();
        assert_eq!(
            app.world().resource::<InteractionTarget>().action,
            Some(InteractionAction::Door(door))
        );
        assert_eq!(
            app.world().get::<Text>(prompt).unwrap().0,
            "E · Enter Sleeping Giant Inn"
        );
        app.insert_resource(CursorCapture::Released);
        app.world_mut()
            .run_system_once(update_interaction_target)
            .unwrap();
        assert_eq!(app.world().resource::<InteractionTarget>().action, None);
        assert_eq!(
            app.world().get::<Node>(prompt).unwrap().display,
            Display::None
        );
        app.insert_resource(CursorCapture::Captured);
        app.insert_resource(ConsoleState {
            open: true,
            ..default()
        });
        app.world_mut()
            .run_system_once(update_interaction_target)
            .unwrap();
        assert_eq!(app.world().resource::<InteractionTarget>().action, None);
        app.world_mut().remove_resource::<ConsoleState>();
        *app.world_mut()
            .entity_mut(door)
            .get_mut::<GlobalTransform>()
            .unwrap() = GlobalTransform::from_translation(Vec3::new(500.0, 100.0, -350.0));
        app.world_mut()
            .run_system_once(update_interaction_target)
            .unwrap();
        assert_eq!(app.world().resource::<InteractionTarget>().action, None);
        *app.world_mut()
            .entity_mut(door)
            .get_mut::<GlobalTransform>()
            .unwrap() = GlobalTransform::from_translation(Vec3::new(500.0, 100.0, -160.0));

        app.world_mut().spawn((
            bevy_rapier3d::prelude::RigidBody::Fixed,
            bevy_rapier3d::prelude::Collider::cuboid(20.0, 20.0, 10.0),
            crate::physics::world_collision_groups(),
            Transform::from_xyz(500.0, 100.0, -50.0),
        ));
        app.update();
        *app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap() = pose;
        app.insert_resource(CursorCapture::Captured);
        app.world_mut()
            .run_system_once(update_interaction_target)
            .unwrap();
        assert_eq!(app.world().resource::<InteractionTarget>().action, None);
        assert!(app.world().get::<Text>(prompt).unwrap().0.is_empty());
    }

    #[test]
    fn held_objects_and_nearest_target_determine_the_action() {
        let pickup = Entity::from_raw_u32(1).unwrap();
        let door = Entity::from_raw_u32(2).unwrap();
        assert_eq!(
            choose_action(true, Some((pickup, 20.0)), Some((door, 10.0))),
            Some(InteractionAction::Drop)
        );
        assert_eq!(
            choose_action(false, Some((pickup, 20.0)), Some((door, 10.0))),
            Some(InteractionAction::Door(door))
        );
        assert_eq!(
            choose_action(false, Some((pickup, 10.0)), Some((door, 20.0))),
            Some(InteractionAction::Pickup(pickup))
        );
        assert_eq!(choose_action(false, None, None), None);
    }
}
