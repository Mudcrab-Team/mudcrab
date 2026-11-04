//! Shared reference creation. Demand, residency and ownership remain caller-owned.
use super::{
    PendingAssetProfile, PendingModel, creation_rotation_to_bevy, creation_to_bevy,
    spawned_model_path,
};
use crate::world::{
    components::{
        CellRef, ExpectedModelBounds, FormId, InstanceBounds, MeshHandle, WorldPosition,
        WorldTransform,
    },
    database::{CellKey, ReferenceRow},
};
use bevy::prelude::*;
use std::time::Instant;

/// Inputs already selected by the caller. This helper does not establish residency or readiness.
pub(crate) struct ReferenceSpawnContext<'a> {
    pub(crate) key: CellKey,
    pub(crate) model_sequence: &'a mut u64,
    pub(crate) asset_server: &'a AssetServer,
    pub(crate) actor_catalog: &'a crate::world::actor_catalog::ActorCatalog,
    pub(crate) world_physics: bool,
    pub(crate) lights: bool,
    pub(crate) schedule_scenes: bool,
}

/// The ordinary loader's exact placement, also used when a retained source changes cell owner.
pub(crate) fn reference_placement(
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
    key: CellKey,
) -> (WorldPosition, Transform) {
    let creation_position = Vec3::from_array(position);
    let world_position = WorldPosition::from_creation_units(creation_position);
    let translation = match key {
        CellKey::Exterior { grid_x, grid_y, .. } => {
            creation_to_bevy(world_position.relative_to(IVec2::new(grid_x, grid_y)))
        }
        CellKey::Interior(_) => creation_to_bevy(creation_position),
    };
    let transform = Transform::from_translation(translation)
        .with_rotation(creation_rotation_to_bevy(rotation))
        .with_scale(Vec3::splat(scale));
    (world_position, transform)
}

/// Spawn under the caller's cell-space parent; return the root and whether a converted model
/// was requested. A disabled reference or an unresolved actor creates no entity or request.
/// Candidate identity does not confer ownership or certify readiness.
pub(crate) fn spawn_reference(
    parent: &mut ChildSpawnerCommands<'_>,
    reference: ReferenceRow,
    context: &mut ReferenceSpawnContext<'_>,
) -> Option<(Entity, bool)> {
    let key = context.key;
    let model_sequence = &mut *context.model_sequence;
    let asset_server = context.asset_server;
    let actor_catalog = context.actor_catalog;
    let world_physics = context.world_physics;
    let lights = context.lights;
    let schedule_scenes = context.schedule_scenes;
    let mut model_requested = false;
    if !reference.initially_enabled {
        return None;
    }
    let (world_position, transform) =
        reference_placement(reference.position, reference.rotation, reference.scale, key);
    let model_bounds = reference.bounds_valid.then(|| {
        ExpectedModelBounds::new(
            Vec3::from_array(reference.bounds_min),
            Vec3::from_array(reference.bounds_max),
        )
    });
    let model_bounds = model_bounds.flatten();
    let bounds = model_bounds
        .map(|bounds| InstanceBounds::transformed(bounds.min, bounds.max, transform.to_matrix()));
    // A referenced `NPC_` with a catalog row becomes one actor entity. A row the catalog
    // could not resolve, or one with no body to draw, skips the reference entirely: no
    // placeholder, no static mesh, and exactly one diagnostic naming the base FormID.
    let actor_plan = actor_catalog
        .get(reference.base_form_id)
        .map(|model| actor_catalog.spawn_plan(model));
    let actor_plan = match actor_plan {
        Some(Ok(plan)) => Some(plan),
        Some(Err(diagnostic)) => {
            warn!(
                form_id = format_args!("{:08X}", diagnostic.base_form_id),
                reason = diagnostic.reason.as_str(),
                "actor spawn skipped: {diagnostic}"
            );
            return None;
        }
        None => None,
    };
    let mut entity = parent.spawn((
        Name::new(format!("Reference {:08X}", reference.form_id)),
        FormId(reference.form_id),
        CellRef(reference.cell_id),
        world_position,
        WorldTransform(transform.to_matrix()),
        transform,
        // Enable-state writers must reach models still waiting for scene admission.
        Visibility::default(),
        // Every mesh this reference draws belongs to the placed-object layer, so the water
        // reflection pass - which renders the world layer only - leaves it out. The meshes
        // arrive later as descendants from the converted glb, so the layer is propagated
        // instead of inserted here; see `render::add_placed_object_layer_propagation`.
        bevy::app::Propagate(super::PLACED_OBJECT_RENDER_LAYERS),
    ));
    if let Some(bounds) = bounds.zip(model_bounds) {
        entity.insert(bounds);
    }
    if let Some(plan) = &actor_plan {
        entity.insert(plan.actor);
        if !plan.parts.is_empty() {
            entity.insert(plan.parts.clone());
        }
        if let Some(head) = &plan.head {
            entity.insert(head.clone());
        }
        if let Some(tone) = plan.skin_tone {
            entity.insert(tone);
        }
        if let Some(weight) = plan.weight {
            entity.insert(crate::actor_body_weight::ActorBodyWeight(
                (1.0 - weight / 100.0).clamp(0.0, 1.0),
            ));
        }
        if plan.uses_composed_body() {
            entity.insert((
                crate::actor_animation::ActorClips::load(asset_server, plan.actor.sex),
                crate::actor_drive::ActorDrive::default(),
            ));
        }
    }
    // The colour the reference's `XEMI` source lends its model's `External_Emittance`
    // surfaces once they are instantiated below it; see `crate::emittance`.
    if let Some(rgb) = reference.emittance {
        entity.insert(crate::emittance::ReferenceEmittance::from_srgb(rgb));
    }
    // A child of the reference, so the light sits where the reference is and follows it
    // through a render-origin rebase - and, because it is a descendant of the cell root,
    // through the cell unload that despawns the reference.
    if let Some(light) = reference
        .light
        .as_ref()
        .and_then(|row| crate::lights::point_light(row, reference.light_radius_override))
    {
        let marker = crate::lights::SkyrimLight {
            form_id: reference.form_id,
            cell_id: reference.cell_id,
        };
        if !lights {
            entity.insert(crate::lights::DeferredPointLight { light, marker });
        } else {
            // Keep the ordinary light visibility source and dedicated light render layers.
            entity.insert(Visibility::default());
            entity.with_child((
                Name::new(format!("Light {:08X}", reference.form_id)),
                light,
                marker,
                // Not the reference's propagated placed-object layer: a light keeps
                // `LIGHT_LAYERS` so it reaches the world layer and both cameras.
                crate::render::light_render_layers(),
            ));
        }
    }
    if let Some(door) = crate::doors::load_door(reference.form_id, reference.door.as_ref()) {
        entity.insert(door);
    }
    // An actor draws its composed body (or its catalog body part before one exists); every
    // other reference keeps the model path it always used.
    let model_path = match &actor_plan {
        Some(plan) => Some(plan.scene_path.clone()),
        None => spawned_model_path(reference.model_path, reference.base_is_marker),
    };
    if let Some(path) = model_path {
        // This is candidate identity, not ownership permission. The certificate
        // and occurrence ordinal decide ownership on the instantiated children.
        if matches!(key, CellKey::Exterior { .. })
            && reference.base_record_type.as_deref().is_some_and(|kind| {
                shared::cluster_recipe::classify::base_type_in_scope(kind, true)
            })
        {
            entity.insert((
                crate::cluster::ownership::OwnershipReference(reference.form_id),
                crate::cluster::runtime_state::RestingReferencePose(transform),
            ));
        }
        model_requested = true;
        let sequence = *model_sequence;
        *model_sequence = model_sequence.saturating_add(1);
        if schedule_scenes {
            let (center, radius) =
                super::requests::reference_geometry(world_position, &transform, bounds);
            entity.insert(super::requests::SceneRequest {
                path: path.clone(),
                cell: Some(key),
                coarse_terrain: false,
                retry_cleanup: None,
                content_identity: None,
                sequence,
                center,
                radius,
                collision_candidate: world_physics
                    && super::fixed_collision_record_eligible(
                        reference.base_record_type.as_deref(),
                    ),
                handle: None,
                started: Instant::now(),
                allow_retry: false,
            });
        } else {
            let handle = asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
            entity.insert(PendingModel { handle, sequence });
        }
        entity.insert((
            MeshHandle(path.clone()),
            PendingAssetProfile {
                started: Instant::now(),
                scene_spawned: false,
                path,
                form_id: reference.form_id,
                base_form_id: reference.base_form_id,
                base_record_type: reference.base_record_type,
                static_physics: world_physics,
                cell_id: reference.cell_id,
                actor_scene: actor_plan.is_some(),
            },
        ));
    }
    Some((entity.id(), model_requested))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{
        actor_catalog::ActorCatalog,
        database::{DoorLinkRow, LightRow},
    };
    use bevy::{asset::AssetPlugin, ecs::world::CommandQueue};

    fn row() -> ReferenceRow {
        ReferenceRow {
            form_id: 17,
            initially_enabled: true,
            cell_id: 31,
            base_form_id: 43,
            base_record_type: Some("STAT".to_owned()),
            base_is_marker: false,
            model_path: Some("Architecture/Wall.nif".to_owned()),
            position: [100.0, 50.0, -200.0],
            rotation: [0.0; 3],
            scale: 2.0,
            bounds_min: [-1.0, -2.0, -3.0],
            bounds_max: [1.0, 2.0, 3.0],
            bounds_valid: true,
            light: None,
            light_radius_override: None,
            emittance: None,
            door: None,
        }
    }

    fn exterior() -> CellKey {
        CellKey::Exterior {
            worldspace_id: 59,
            grid_x: -2,
            grid_y: 1,
        }
    }

    struct Fixture {
        app: App,
        parent: Entity,
        spawned: Vec<Option<(Entity, bool)>>,
        next_sequence: u64,
        initial_entities: u32,
    }

    fn spawn(
        rows: Vec<ReferenceRow>,
        key: CellKey,
        scheduled: bool,
        physics: bool,
        lights: bool,
        sequence: u64,
        catalog: ActorCatalog,
    ) -> Fixture {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            // Keep the production scene-asset prerequisite even for queued requests.
            .init_asset::<WorldAsset>()
            .init_asset::<bevy::gltf::Gltf>();
        assert!(app.world().contains_resource::<Assets<WorldAsset>>());
        let initial_entities = app.world().entities().count_spawned();
        let server = app.world().resource::<AssetServer>().clone();
        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, app.world());
        let mut spawned = Vec::new();
        let mut next_sequence = sequence;
        let parent = commands.spawn_empty().id();
        commands.entity(parent).with_children(|parent| {
            let mut context = ReferenceSpawnContext {
                key,
                model_sequence: &mut next_sequence,
                asset_server: &server,
                actor_catalog: &catalog,
                world_physics: physics,
                lights,
                schedule_scenes: scheduled,
            };
            for reference in rows {
                spawned.push(spawn_reference(parent, reference, &mut context));
            }
        });
        queue.apply(app.world_mut());
        Fixture {
            app,
            parent,
            spawned,
            next_sequence,
            initial_entities,
        }
    }

    #[test]
    fn disabled_and_model_free_references_do_not_consume_scene_sequence() {
        let mut disabled = row();
        disabled.initially_enabled = false;
        let mut marker = row();
        marker.base_is_marker = true;
        let mut no_model = row();
        no_model.model_path = None;
        let fixture = spawn(
            vec![disabled, marker, no_model],
            exterior(),
            true,
            true,
            false,
            71,
            ActorCatalog::default(),
        );
        assert_eq!(fixture.spawned[0], None);
        assert_eq!(fixture.next_sequence, 71);
        assert_eq!(
            fixture
                .app
                .world()
                .get::<Children>(fixture.parent)
                .unwrap()
                .len(),
            2
        );
        for result in &fixture.spawned[1..] {
            let (entity, requested) = result.unwrap();
            assert!(!requested);
            let entity = fixture.app.world().entity(entity);
            assert_eq!(entity.get::<Visibility>(), Some(&Visibility::Inherited));
            assert!(entity.get::<MeshHandle>().is_none());
            assert!(
                entity
                    .get::<super::super::requests::SceneRequest>()
                    .is_none()
            );
            assert!(
                entity
                    .get::<crate::cluster::ownership::OwnershipReference>()
                    .is_none()
            );
        }
    }

    #[test]
    fn candidate_identity_requires_exterior_in_scope_and_requested_model() {
        for (key, kind, expected) in [
            (exterior(), Some("STAT"), true),
            (exterior(), Some("TREE"), true),
            (exterior(), Some("MISC"), true),
            (exterior(), Some("NPC_"), false),
            (exterior(), Some("LIGH"), false),
            (exterior(), None, false),
            (CellKey::Interior(31), Some("STAT"), false),
        ] {
            let mut reference = row();
            reference.base_record_type = kind.map(str::to_owned);
            let fixture = spawn(
                vec![reference],
                key,
                true,
                false,
                false,
                0,
                ActorCatalog::default(),
            );
            let (entity, requested) = fixture.spawned[0].unwrap();
            assert!(requested);
            let entity = fixture.app.world().entity(entity);
            let identity = entity.get::<crate::cluster::ownership::OwnershipReference>();
            assert_eq!(identity.map(|identity| identity.0), expected.then_some(17));
            let pose = entity.get::<crate::cluster::runtime_state::RestingReferencePose>();
            assert_eq!(
                pose.map(|pose| pose.0),
                expected.then(|| *entity.get::<Transform>().unwrap())
            );
            assert_eq!(
                entity
                    .get::<PendingAssetProfile>()
                    .unwrap()
                    .base_record_type
                    .as_deref(),
                kind
            );
        }
    }

    #[test]
    fn scheduled_creation_retains_placement_bounds_identity_physics_and_sequence() {
        let mut reference = row();
        let cell_size = crate::world::components::CELL_SIZE;
        reference.position = [-cell_size - 8.0, cell_size + 12.0, 20.0];
        let fixture = spawn(
            vec![reference],
            exterior(),
            true,
            true,
            false,
            79,
            ActorCatalog::default(),
        );
        let (entity, requested) = fixture.spawned[0].unwrap();
        assert!(requested);
        assert_eq!(fixture.next_sequence, 80);
        let entity = fixture.app.world().entity(entity);
        assert_eq!(entity.get::<FormId>(), Some(&FormId(17)));
        assert_eq!(entity.get::<CellRef>(), Some(&CellRef(31)));
        assert_eq!(entity.get::<ChildOf>().unwrap().parent(), fixture.parent);
        assert_eq!(
            entity.get::<WorldPosition>().unwrap().grid,
            IVec2::new(-2, 1)
        );
        let transform = entity.get::<Transform>().unwrap();
        assert_eq!(
            transform.translation,
            Vec3::new(cell_size - 8.0, 20.0, -12.0)
        );
        assert_eq!(transform.scale, Vec3::splat(2.0));
        assert_eq!(
            entity.get::<WorldTransform>().unwrap().0,
            transform.to_matrix()
        );
        let expected = entity.get::<ExpectedModelBounds>().unwrap();
        assert_eq!(
            (expected.min, expected.max),
            (Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0))
        );
        let bounds = entity.get::<InstanceBounds>().unwrap();
        assert_eq!(
            bounds.min,
            transform.translation + Vec3::new(-2.0, -4.0, -6.0)
        );
        assert_eq!(bounds.max, transform.translation + Vec3::new(2.0, 4.0, 6.0));
        let request = entity
            .get::<super::super::requests::SceneRequest>()
            .unwrap();
        assert_eq!(request.path, "meshes/architecture/wall.glb");
        assert_eq!(request.cell, Some(exterior()));
        assert_eq!(request.sequence, 79);
        assert!(request.collision_candidate);
        assert!(!request.coarse_terrain && !request.allow_retry);
        assert!(
            request.handle.is_none()
                && request.retry_cleanup.is_none()
                && request.content_identity.is_none()
        );
        assert!(entity.get::<PendingModel>().is_none());
        assert!(entity.get::<PendingAssetProfile>().unwrap().static_physics);
        assert!(
            entity
                .get::<bevy::app::Propagate<bevy::camera::visibility::RenderLayers>>()
                .is_some()
        );
    }

    #[test]
    fn collision_request_follows_type_and_caller_physics_without_inventing_bounds() {
        for (kind, physics, expected) in [
            (Some("FURN"), true, true),
            (Some("MISC"), true, false),
            (Some("STAT"), false, false),
            (None, true, false),
        ] {
            let mut reference = row();
            reference.base_record_type = kind.map(str::to_owned);
            reference.bounds_max = [f32::NAN; 3];
            let fixture = spawn(
                vec![reference],
                CellKey::Interior(31),
                true,
                physics,
                false,
                u64::MAX,
                ActorCatalog::default(),
            );
            let entity = fixture.app.world().entity(fixture.spawned[0].unwrap().0);
            assert_eq!(fixture.next_sequence, u64::MAX);
            assert!(entity.get::<ExpectedModelBounds>().is_none());
            assert!(entity.get::<InstanceBounds>().is_none());
            assert_eq!(
                entity
                    .get::<super::super::requests::SceneRequest>()
                    .unwrap()
                    .collision_candidate,
                expected
            );
            assert_eq!(
                entity.get::<PendingAssetProfile>().unwrap().static_physics,
                physics
            );
        }
    }

    #[test]
    fn immediate_creation_retains_pending_scene_handle_and_profile() {
        let fixture = spawn(
            vec![row()],
            CellKey::Interior(31),
            false,
            false,
            false,
            83,
            ActorCatalog::default(),
        );
        let entity = fixture.app.world().entity(fixture.spawned[0].unwrap().0);
        let pending = entity.get::<PendingModel>().unwrap();
        assert_eq!(pending.sequence, 83);
        assert_eq!(fixture.next_sequence, 84);
        let path = fixture
            .app
            .world()
            .resource::<AssetServer>()
            .get_path(pending.handle.id())
            .unwrap();
        assert_eq!(
            path.path(),
            std::path::Path::new("meshes/architecture/wall.glb")
        );
        assert_eq!(path.label(), Some("Scene0"));
        assert!(
            entity
                .get::<super::super::requests::SceneRequest>()
                .is_none()
        );
        let profile = entity.get::<PendingAssetProfile>().unwrap();
        assert_eq!(
            (profile.form_id, profile.base_form_id, profile.cell_id),
            (17, 43, 31)
        );
        assert!(!profile.scene_spawned && !profile.actor_scene && !profile.static_physics);
        assert_eq!(entity.get::<MeshHandle>().unwrap().0, profile.path);
    }

    #[test]
    fn model_free_light_door_and_emittance_keep_the_reference_parent() {
        for lights in [false, true] {
            let mut reference = row();
            reference.model_path = None;
            reference.light = Some(LightRow {
                radius: 64.0,
                color: [255, 128, 64],
                flags: 0,
            });
            reference.light_radius_override = Some(96.0);
            reference.emittance = Some([48, 96, 192]);
            reference.door = Some(DoorLinkRow {
                destination_ref_id: 101,
                destination_cell_id: Some(103),
                destination_worldspace_id: None,
                arrival_position: [1.0, 2.0, 3.0],
                arrival_rotation: [0.0; 3],
            });
            let fixture = spawn(
                vec![reference],
                CellKey::Interior(31),
                true,
                false,
                lights,
                0,
                ActorCatalog::default(),
            );
            let (id, requested) = fixture.spawned[0].unwrap();
            assert!(!requested);
            let entity = fixture.app.world().entity(id);
            assert_eq!(entity.get::<Visibility>(), Some(&Visibility::Inherited));
            assert!(
                entity
                    .get::<crate::emittance::ReferenceEmittance>()
                    .is_some()
            );
            let door = entity.get::<crate::doors::LoadDoor>().unwrap();
            assert_eq!(door.ref_id, 17);
            assert_eq!(door.destination.interior_cell_id, Some(103));
            assert_eq!(door.destination.arrival_position, [1.0, 2.0, 3.0]);
            assert_eq!(
                entity.get::<crate::lights::DeferredPointLight>().is_some(),
                !lights
            );
            if lights {
                let children = entity.get::<Children>().unwrap();
                assert_eq!(children.len(), 1);
                let light = fixture.app.world().entity(children[0]);
                assert_eq!(light.get::<ChildOf>().unwrap().parent(), id);
                assert_eq!(light.get::<PointLight>().unwrap().range, 96.0);
                let marker = light.get::<crate::lights::SkyrimLight>().unwrap();
                assert_eq!((marker.form_id, marker.cell_id), (17, 31));
                assert_eq!(
                    light.get::<bevy::camera::visibility::RenderLayers>(),
                    Some(&crate::render::light_render_layers().0)
                );
            }
        }
    }

    #[test]
    fn unresolved_actor_creates_no_reference_or_model_request() {
        let directory = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE actor_models (id INTEGER PRIMARY KEY, race_id INTEGER NOT NULL,
             sex TEXT NOT NULL, skin_armo_id INTEGER, head_model TEXT, head_source TEXT,
             resolution TEXT NOT NULL);
             CREATE TABLE actor_model_parts (npc_id INTEGER NOT NULL, slot TEXT NOT NULL,
             model_path TEXT NOT NULL, PRIMARY KEY (npc_id, slot));
             INSERT INTO actor_models(id, race_id, sex, resolution) VALUES (43, 47, 'male', 'unresolved');"
        ).unwrap();
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());
        let fixture = spawn(
            vec![row()],
            CellKey::Interior(31),
            true,
            true,
            true,
            89,
            catalog,
        );
        assert_eq!(fixture.spawned, vec![None]);
        assert_eq!(fixture.next_sequence, 89);
        assert!(
            fixture
                .app
                .world()
                .get::<Children>(fixture.parent)
                .is_none()
        );
        assert_eq!(
            fixture.app.world().entities().count_spawned(),
            fixture.initial_entities + 1
        );
    }
}
