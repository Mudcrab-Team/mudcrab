//! Exact stock terrain proof input used by current-view cluster clipping.
use bevy::render::{Extract, ExtractSchedule};
use bevy::{prelude::*, render::sync_world::MainEntity};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SurfaceKey {
    pub world: u32,
    pub grid: IVec2,
    pub quadrant: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Surface {
    pub keys: Vec<SurfaceKey>,
    pub(crate) mesh: bevy::asset::AssetId<Mesh>,
}

#[derive(Resource, Default)]
pub(crate) struct RenderTerrain {
    pub surfaces: HashMap<MainEntity, Surface>,
}

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TerrainHandoffSystems {
    Cluster,
    Filter,
}

/// Installs the selected package's full-detail input to existing per-view clipping.
pub(super) struct TerrainHandoffPlugin;

impl Plugin for TerrainHandoffPlugin {
    fn build(&self, app: &mut App) {
        let enabled = app
            .world()
            .get_resource::<crate::config::EngineConfig>()
            .is_some_and(|config| config.clusters.is_some() && config.cluster_terrain);
        if !enabled {
            return;
        }
        super::identity_changes::install_terrain(app.world_mut());
        if let Some(render) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render
                .init_resource::<RenderTerrain>()
                .add_systems(ExtractSchedule, extract);
        }
    }
}

type ChangedMergedTerrain = (
    With<super::MergedTerrainCoverage>,
    Or<(
        Changed<super::MergedTerrainCoverage>,
        Changed<super::TerrainWorldspace>,
    )>,
);

/// Full-detail entities retain bounded change-tick checks each Extract. The existing
/// identity service owns its ancestry/invalidation work. Current-view mesh and pass
/// qualification stays in the renderer's clipping consumer.
#[derive(bevy::ecs::system::SystemParam)]
struct DetailInputs<'w, 's> {
    world: &'w World,
    visibility: Query<'w, 's, Ref<'static, InheritedVisibility>>,
    meshes: Query<'w, 's, Ref<'static, Mesh3d>>,
    merged: Query<
        'w,
        's,
        (
            Entity,
            Ref<'static, super::MergedTerrainCoverage>,
            Ref<'static, super::TerrainWorldspace>,
        ),
    >,
    changed_merged: Query<'w, 's, Entity, ChangedMergedTerrain>,
    events: Option<Res<'w, Messages<super::identity_changes::TerrainNodeChanged>>>,
}

#[derive(Default)]
struct DetailInputsSeen {
    initialized: bool,
    detail: HashSet<Entity>,
    merged: HashSet<Entity>,
    cursor: bevy::ecs::message::MessageCursor<super::identity_changes::TerrainNodeChanged>,
}

impl DetailInputs<'_, '_> {
    fn visible_mesh(&self, entity: Entity) -> Option<bevy::asset::AssetId<Mesh>> {
        self.visibility
            .get(entity)
            .ok()
            .filter(|visible| visible.get())?;
        Some(self.meshes.get(entity).ok()?.0.id())
    }

    fn changed(&self, entity: Entity, previous: Option<&Surface>) -> bool {
        let current_mesh = self.visible_mesh(entity);
        if previous.map(|surface| surface.mesh) != current_mesh {
            return true;
        }
        self.visibility
            .get(entity)
            .is_ok_and(|visible| visible.is_changed())
            || self.meshes.get(entity).is_ok_and(|mesh| mesh.is_changed())
            || self
                .merged
                .get(entity)
                .is_ok_and(|(_, coverage, world)| coverage.is_changed() || world.is_changed())
    }

    fn surface(
        &self,
        entity: Entity,
        identity: Option<&super::TerrainPrimitiveIdentity>,
    ) -> Option<Surface> {
        let mesh = self.visible_mesh(entity)?;
        if let Ok((_, coverage, world)) = self.merged.get(entity) {
            return Some(Surface {
                mesh,
                keys: (0..4)
                    .map(|quadrant| SurfaceKey {
                        world: world.0,
                        grid: coverage.grid,
                        quadrant,
                    })
                    .collect(),
            });
        }
        let identity = identity.filter(|identity| identity.tier.is_none())?;
        Some(Surface {
            mesh,
            keys: vec![SurfaceKey {
                world: identity.worldspace_id,
                grid: identity.grid,
                quadrant: identity.quadrant,
            }],
        })
    }
}

fn extract(
    mut out: ResMut<RenderTerrain>,
    mut identities: Extract<super::TerrainIdentityAccess>,
    inputs: Extract<DetailInputs>,
    mut seen: Local<DetailInputsSeen>,
) {
    let seen = &mut *seen;
    // Drop this snapshot at the end of the fetch. Keeping a prior Arc would force
    // a full identity-map copy when the source owner applies its next local delta.
    let snapshot = identities.identities_shared_with_changes();
    let mut dirty = HashSet::new();
    let mut recover = !seen.initialized || identities.recovered();
    if let Some(events) = &inputs.events {
        recover |= seen.cursor.missed_messages(events) != 0;
        for event in seen.cursor.read(events) {
            if seen.merged.contains(&event.0) || inputs.merged.contains(event.0) {
                dirty.insert(event.0);
            }
        }
    }
    if recover {
        seen.initialized = true;
        dirty.extend(out.surfaces.keys().map(|main| main.id()));
        seen.detail = snapshot
            .iter()
            .filter(|(_, identity)| identity.tier.is_none())
            .map(|(main, _)| main.id())
            .collect();
        seen.merged = inputs.merged.iter().map(|(entity, ..)| entity).collect();
        dirty.extend(seen.detail.iter().copied());
        dirty.extend(seen.merged.iter().copied());
    } else {
        for main in identities.changed_identities() {
            if snapshot
                .get(main)
                .is_some_and(|identity| identity.tier.is_none())
            {
                seen.detail.insert(main.id());
                dirty.insert(main.id());
            } else if seen.detail.remove(&main.id()) {
                dirty.insert(main.id());
            }
        }
    }
    dirty.extend(inputs.changed_merged.iter());
    for entity in &dirty {
        if inputs.merged.contains(*entity) {
            seen.merged.insert(*entity);
        }
    }
    seen.merged.retain(|entity| {
        // Raw existence preserves disabled members until re-enabled, while exact
        // default-filtered queries below withdraw their render inputs meanwhile.
        if inputs
            .world
            .get::<super::MergedTerrainCoverage>(*entity)
            .is_some()
        {
            true
        } else {
            dirty.insert(*entity);
            false
        }
    });
    for entity in seen.detail.iter().chain(&seen.merged) {
        if inputs.changed(*entity, out.surfaces.get(&MainEntity::from(*entity))) {
            dirty.insert(*entity);
        }
    }
    for entity in dirty {
        let main = MainEntity::from(entity);
        let current = inputs.surface(entity, snapshot.get(&main));
        match current {
            Some(surface) if out.surfaces.get(&main) != Some(&surface) => {
                out.surfaces.insert(main, surface);
            }
            None if out.surfaces.contains_key(&main) => {
                out.surfaces.remove(&main);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod detail_extraction_tests {
    use super::*;
    use crate::streaming::{MergedTerrainCoverage, TerrainCoverage, TerrainWorldspace};
    use bevy::render::{ExtractSchedule, MainWorld, RenderApp};

    struct Fixture {
        app: App,
    }

    impl Fixture {
        fn new(enabled: bool) -> Self {
            let mut app = App::new();
            app.insert_resource(crate::config::EngineConfig {
                clusters: enabled.then(|| "selected".to_owned()),
                cluster_terrain: enabled,
                ..default()
            });
            crate::streaming::identity_changes::install_terrain(app.world_mut());
            let mut render = bevy::app::SubApp::new();
            render.add_schedule(Schedule::new(ExtractSchedule));
            app.insert_sub_app(RenderApp, render);
            app.add_plugins(TerrainHandoffPlugin);
            Self { app }
        }

        fn step(&mut self) {
            let mut main = MainWorld::default();
            std::mem::swap(self.app.world_mut(), &mut *main);
            let render = self.app.sub_app_mut(RenderApp).world_mut();
            render.insert_resource(main);
            render.run_schedule(ExtractSchedule);
            let mut main = render.remove_resource::<MainWorld>().unwrap();
            std::mem::swap(self.app.world_mut(), &mut *main);
        }

        fn terrain(&self) -> &RenderTerrain {
            self.app
                .sub_app(RenderApp)
                .world()
                .resource::<RenderTerrain>()
        }

        fn detail(&mut self, grid: IVec2, quadrant: u8) -> (Entity, Entity) {
            let patch = self
                .app
                .world_mut()
                .spawn((
                    TerrainCoverage {
                        grid,
                        quadrant,
                        tier: None,
                    },
                    TerrainWorldspace(7),
                ))
                .id();
            let primitive = self
                .app
                .world_mut()
                .spawn((
                    Mesh3d::default(),
                    InheritedVisibility::VISIBLE,
                    ChildOf(patch),
                ))
                .id();
            (patch, primitive)
        }

        fn merged(&mut self, grid: IVec2) -> Entity {
            self.app
                .world_mut()
                .spawn((
                    MergedTerrainCoverage { grid },
                    TerrainWorldspace(7),
                    Mesh3d::default(),
                    InheritedVisibility::VISIBLE,
                ))
                .id()
        }
    }

    #[test]
    fn selected_detail_extraction_supplies_exact_quadrants_to_existing_renderer() {
        let mut fixture = Fixture::new(true);
        let (_, detail) = fixture.detail(IVec2::new(2, -3), 2);
        let merged = fixture.merged(IVec2::new(-1, 4));
        fixture.step();
        assert_eq!(fixture.terrain().surfaces.len(), 2);
        assert_eq!(
            fixture.terrain().surfaces[&detail.into()].keys,
            vec![SurfaceKey {
                world: 7,
                grid: IVec2::new(2, -3),
                quadrant: 2
            }]
        );
        assert_eq!(
            fixture.terrain().surfaces[&merged.into()].keys,
            (0..4)
                .map(|quadrant| SurfaceKey {
                    world: 7,
                    grid: IVec2::new(-1, 4),
                    quadrant
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn detail_visibility_payload_and_world_edits_replace_current_input() {
        let mut fixture = Fixture::new(true);
        let (patch, primitive) = fixture.detail(IVec2::ZERO, 0);
        fixture.step();
        assert!(fixture.terrain().surfaces.contains_key(&primitive.into()));
        *fixture
            .app
            .world_mut()
            .get_mut::<InheritedVisibility>(primitive)
            .unwrap() = InheritedVisibility::HIDDEN;
        fixture.step();
        assert!(!fixture.terrain().surfaces.contains_key(&primitive.into()));
        *fixture
            .app
            .world_mut()
            .get_mut::<InheritedVisibility>(primitive)
            .unwrap() = InheritedVisibility::VISIBLE;
        fixture
            .app
            .world_mut()
            .get_mut::<TerrainWorldspace>(patch)
            .unwrap()
            .0 = 11;
        fixture.step();
        assert_eq!(
            fixture.terrain().surfaces[&primitive.into()].keys[0].world,
            11
        );
        fixture
            .app
            .world_mut()
            .entity_mut(primitive)
            .remove::<Mesh3d>();
        fixture.step();
        assert!(!fixture.terrain().surfaces.contains_key(&primitive.into()));
        fixture
            .app
            .world_mut()
            .entity_mut(primitive)
            .insert(Mesh3d::default());
        fixture.step();
        assert!(fixture.terrain().surfaces.contains_key(&primitive.into()));
    }

    #[test]
    fn late_merged_membership_and_in_place_keys_are_not_stale() {
        let mut fixture = Fixture::new(true);
        let merged = fixture
            .app
            .world_mut()
            .spawn((
                Mesh3d::default(),
                TerrainWorldspace(7),
                InheritedVisibility::VISIBLE,
            ))
            .id();
        fixture.step();
        fixture
            .app
            .world_mut()
            .entity_mut(merged)
            .insert(MergedTerrainCoverage { grid: IVec2::ZERO });
        fixture.step();
        assert_eq!(fixture.terrain().surfaces[&merged.into()].keys.len(), 4);
        fixture
            .app
            .world_mut()
            .get_mut::<MergedTerrainCoverage>(merged)
            .unwrap()
            .grid = IVec2::new(4, 5);
        fixture.step();
        assert!(
            fixture.terrain().surfaces[&merged.into()]
                .keys
                .iter()
                .all(|key| key.grid == IVec2::new(4, 5))
        );
        fixture
            .app
            .world_mut()
            .entity_mut(merged)
            .remove::<MergedTerrainCoverage>();
        fixture.step();
        assert!(!fixture.terrain().surfaces.contains_key(&merged.into()));
    }

    #[test]
    fn full_detail_extraction_never_adopts_baked_native_terrain() {
        let mut fixture = Fixture::new(true);
        let (_, detail) = fixture.detail(IVec2::ZERO, 0);
        fixture
            .app
            .world_mut()
            .init_resource::<crate::streaming::lod::LodStreaming>();
        let root = crate::streaming::lod::LodStreaming::spawn_ready_chunk_for_test(
            fixture.app.world_mut(),
            7,
            shared::lod::ChunkAnchor::new(0, 0),
            shared::lod::LodOrigin::new(0, 0),
        );
        let patch = fixture
            .app
            .world_mut()
            .spawn((
                TerrainCoverage {
                    grid: IVec2::ZERO,
                    quadrant: 1,
                    tier: Some(shared::lod::LodTier::Tier16),
                },
                ChildOf(root),
            ))
            .id();
        let native = fixture
            .app
            .world_mut()
            .spawn((
                Mesh3d::default(),
                InheritedVisibility::VISIBLE,
                ChildOf(patch),
            ))
            .id();
        fixture.step();
        assert!(fixture.terrain().surfaces.contains_key(&detail.into()));
        assert!(!fixture.terrain().surfaces.contains_key(&native.into()));
        assert!(
            fixture
                .app
                .world()
                .get::<crate::swap_bridge::SwapSurface>(native)
                .is_none()
        );
        assert!(
            fixture
                .app
                .world()
                .get::<crate::swap_bridge::SwapWaiting>(native)
                .is_none()
        );
    }

    #[test]
    fn ordinary_launch_installs_no_detail_complement_work() {
        let fixture = Fixture::new(false);
        assert!(
            !fixture
                .app
                .sub_app(RenderApp)
                .world()
                .contains_resource::<RenderTerrain>()
        );
    }

    #[test]
    fn removing_merged_world_withdraws_proof_until_current_identity_returns() {
        let mut fixture = Fixture::new(true);
        let merged = fixture.merged(IVec2::new(-2, 3));
        fixture.step();
        assert_eq!(fixture.terrain().surfaces[&merged.into()].keys[0].world, 7);
        fixture
            .app
            .world_mut()
            .entity_mut(merged)
            .remove::<TerrainWorldspace>();
        fixture.step();
        assert!(
            !fixture.terrain().surfaces.contains_key(&merged.into()),
            "a mesh without its current world identity cannot clip certified terrain"
        );
        fixture
            .app
            .world_mut()
            .entity_mut(merged)
            .insert(TerrainWorldspace(19));
        fixture.step();
        assert!(
            fixture.terrain().surfaces[&merged.into()]
                .keys
                .iter()
                .all(|key| key.world == 19)
        );
    }
}
