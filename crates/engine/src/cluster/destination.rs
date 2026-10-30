//! Candidate changes revoke destination evidence for every observed request scope.
use super::*;
use crate::swap_bridge::{destination::*, systems::extract_destination_protocol};
use bevy::render::MainWorld;
pub(super) fn install(app: &mut App) {
    let Some(render) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render.init_resource::<CandidateObserver>().add_systems(
        ExtractSchedule,
        observe_candidates
            .after(extract_sources)
            .after(extract_batch_materials)
            .after(extract_destination_protocol),
    );
}

/// Eligibility/composition changes invalidate old reports; becoming drawable does
/// not invalidate its own new Ready report on every pipeline-preparation step.
#[path = "destination_scope.rs"]
mod destination_scope;

#[derive(Default, Resource)]
struct CandidateObserver {
    versions: Option<(u64, u64)>,
    /// The request set last recorded: (the resource's insertion tick, its revision).
    requests: Option<(bevy::ecs::change_detection::Tick, u64)>,
    certificate: Option<Arc<crate::cluster::reader::CertifiedPackage>>,
    scene_present: bool,
    index: destination_scope::BatchCells,
    worlds: HashSet<WorldId>,
    // Keep the dependency scope of stored reports after their request is withdrawn.
    // A later change must revoke those reports as well as pending requests.
    scopes: HashMap<DestinationKey, Arc<[CellKey]>>,
    /// Walks over the pending requests or stored scopes; a settled frame adds none.
    #[cfg(test)]
    walks: usize,
}

fn observe_candidates(
    sources: Res<Sources>,
    materials: Res<ClusterBatchMaterials>,
    scene: Option<Res<ClusterScene>>,
    mut main: ResMut<MainWorld>,
    frame: Option<Res<RenderFrame>>,
    invalidations: Option<ResMut<RenderInvalidations>>,
    mut previous: ResMut<CandidateObserver>,
) {
    let previous = &mut *previous;
    // Record each request's scope when the request set changes: the scope must outlive a
    // withdrawn request whose report is still stored. An unchanged set costs nothing.
    let requests = main
        .get_resource_change_ticks::<DestinationRequests>()
        .zip(main.get_resource::<DestinationRequests>())
        .map(|(ticks, requests)| (ticks.added, requests));
    let stamp = requests
        .as_ref()
        .map(|(added, requests)| (*added, requests.revision()));
    if previous.requests != stamp {
        previous.requests = stamp;
        #[cfg(test)]
        {
            previous.walks += 1;
        }
        for (_, request) in requests.iter().flat_map(|(_, requests)| requests.iter()) {
            previous
                .scopes
                .insert(request.key, Arc::clone(&request.coverage));
        }
    }
    let same_certificate = match (&previous.certificate, &sources.certificate) {
        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
        (None, None) => true,
        _ => false,
    };
    let scope_changed = !same_certificate
        || previous.scene_present != scene.is_some()
        || scene.as_ref().is_some_and(|scene| scene.is_changed());
    if !scope_changed && previous.versions == Some((sources.candidate_version, materials.version)) {
        return;
    }
    #[cfg(test)]
    {
        previous.walks += 1;
    }
    let pending: HashSet<_> = main
        .get_resource::<DestinationRequests>()
        .into_iter()
        .flat_map(|requests| requests.iter())
        .map(|(_, request)| request.key)
        .collect();
    // Stale scopes are dropped only here, where they are read.
    previous.scopes.retain(|key, _| {
        let current = main
            .get_resource::<Generations>()
            .is_none_or(|generations| generations.current(key.world) == key.generation);
        current
            && (pending.contains(key)
                || main
                    .get_resource::<DestinationReports>()
                    .is_some_and(|reports| reports.get(key).is_some()))
    });
    let source_changed = previous
        .versions
        .is_none_or(|(source, _)| source != sources.candidate_version);
    let materials_changed = previous
        .versions
        .is_none_or(|(_, material)| material != materials.version);
    let old_worlds = previous.worlds.clone();
    if scope_changed || previous.versions.is_none() {
        previous.index = scene
            .as_ref()
            .map_or_else(destination_scope::BatchCells::default, |scene| {
                destination_scope::BatchCells::build(scene, sources.certificate.as_deref())
            });
        previous.worlds = sources
            .certificate
            .as_ref()
            .into_iter()
            .flat_map(|certificate| certificate.records())
            .map(|(_, record)| WorldId::exterior(record.worldspace))
            .chain(
                scene
                    .as_ref()
                    .into_iter()
                    .flat_map(|scene| scene.terrain_blocks.iter())
                    .map(|block| WorldId::exterior(block.worldspace_id)),
            )
            .collect();
        previous.certificate = sources.certificate.clone();
        previous.scene_present = scene.is_some();
    }
    previous.worlds.extend(
        sources
            .worlds
            .values()
            .flatten()
            .copied()
            .map(WorldId::exterior),
    );
    let empty_delta = CandidateWorlds::default();
    let changed_batches: HashSet<usize> = if materials_changed {
        materials
            .changed_batches
            .iter()
            .map(|&batch| batch as usize)
            .collect()
    } else {
        HashSet::new()
    };
    let mut effects = destination_scope::Effects::from_delta(
        if source_changed {
            &sources.candidate_worlds
        } else {
            &empty_delta
        },
        &changed_batches,
        &previous.index,
    );
    // A delta belongs to one version only. A missed version, absent material
    // dependency information, or replacement of the whole scope fails closed.
    effects.all |= scope_changed
        || previous.versions.is_none()
        || previous.versions.is_some_and(|(source, material)| {
            (source_changed && source.wrapping_add(1) != sources.candidate_version)
                || (materials_changed && material.wrapping_add(1) != materials.version)
        })
        || (materials_changed && materials.changed_batches.is_empty());
    let mut changed = effects.changed_worlds();
    if effects.all {
        changed.extend(&previous.worlds);
        changed.extend(old_worlds);
    }
    let mut revoke = HashSet::new();
    let mut remove = Vec::new();
    for world in changed {
        let mut observed = false;
        for (key, coverage) in previous.scopes.iter().filter(|(key, _)| key.world == world) {
            observed = true;
            if effects.affects(world, coverage) {
                if pending.contains(key) {
                    revoke.insert(world);
                } else {
                    remove.push(*key);
                }
            }
        }
        // Unknown/manual report scopes retain conservative world invalidation.
        if !observed {
            revoke.insert(world);
        }
    }
    if let Some(mut reports) = main.get_resource_mut::<DestinationReports>() {
        for key in &remove {
            reports.remove(key);
        }
    }
    for key in &remove {
        previous.scopes.remove(key);
    }
    debug!(
        source_version = sources.candidate_version,
        material_version = materials.version,
        global = effects.all,
        affected_cells = effects.cells.len(),
        affected_batches = sources.candidate_worlds.batches.len(),
        material_batches = materials.changed_batches.len(),
        ?revoke,
        stored_reports_removed = remove.len(),
        pending_requests = pending.len(),
        "cluster destination scoped invalidation"
    );
    if let (Some(frame), Some(mut invalidations), Some(mut generations)) =
        (frame, invalidations, main.get_resource_mut::<Generations>())
    {
        for world in revoke {
            generations.bump(world, InvalidationCause::Candidate);
            invalidations.revoke(world, *frame, InvalidationCause::Candidate);
        }
    }
    previous.versions = Some((sources.candidate_version, materials.version));
}

/// Stock coverage is current queued/drawable coverage for this actual pass.
/// Early selection alone never proves a late material or shadow pipeline.
#[cfg(test)]
fn terrain_pass_ready(
    inputs: &TerrainClipInputs,
    grid: IVec2,
    block: u32,
    shadow: bool,
    needs_depth: bool,
    cluster_drawable: bool,
) -> bool {
    let forward = (0..4u8).all(|quadrant| inputs.forward_covered.contains(&(grid, quadrant)));
    let depth = (0..4u8).all(|quadrant| inputs.depth_covered.contains(&(grid, quadrant)));
    let stock = if shadow {
        depth
    } else {
        forward && (!needs_depth || depth)
    };
    stock || (inputs.ready_blocks.contains(&block) && cluster_drawable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_clip_cache_rechecks_exact_target_offset_even_with_unchanged_grid_and_far_nonce() {
        let grid_origin = IVec2::new(-4, 8);
        let offset = super::super::render_origin_offset(grid_origin);
        let inputs = TerrainClipInputs {
            world: Some(12),
            origin: grid_origin,
            forward_covered: HashSet::new(),
            depth_covered: HashSet::new(),
            ready_blocks: [0].into_iter().collect(),
            target_offset: offset.to_array().map(f32::to_bits),
        };
        assert!(inputs.target_offset_matches(offset));
        assert!(!inputs.target_offset_matches(offset + Vec3::X));
        assert!(!inputs.target_offset_matches(Vec3::new(offset.x, -0.0, offset.z)));
        assert!(!inputs.target_offset_matches(Vec3::new(offset.x, f32::NAN, offset.z)));
        assert_eq!(inputs.origin, grid_origin);
    }

    #[test]
    fn terrain_selection_does_not_prove_a_missing_shadow_or_depth_draw() {
        let grid = IVec2::new(7, -9);
        let mut inputs = TerrainClipInputs {
            target_offset: [0; 3],
            world: Some(12),
            origin: IVec2::ZERO,
            forward_covered: (0..4).map(|q| (grid, q)).collect(),
            depth_covered: HashSet::new(),
            ready_blocks: [3].into_iter().collect(),
        };
        assert!(terrain_pass_ready(&inputs, grid, 3, false, false, false));
        assert!(!terrain_pass_ready(&inputs, grid, 3, false, true, false));
        assert!(!terrain_pass_ready(&inputs, grid, 3, true, false, false));
        assert!(terrain_pass_ready(&inputs, grid, 3, true, false, true));
        assert!(!terrain_pass_ready(&inputs, grid, 4, true, false, true));
        inputs.depth_covered = (0..4).map(|q| (grid, q)).collect();
        assert!(terrain_pass_ready(&inputs, grid, 4, true, false, false));
        inputs.depth_covered.remove(&(grid, 2));
        inputs.depth_covered.insert((grid + IVec2::X, 2));
        assert!(!terrain_pass_ready(&inputs, grid, 4, true, false, false));
    }
}

#[cfg(test)]
mod terrain_candidate_regressions {
    use super::*;
    use crate::streaming::TerrainPrimitiveIdentity;
    use bevy::ecs::schedule::Schedule;
    use shared::lod::LodTier;

    const WORLD: u32 = 12;
    const GRID: IVec2 = IVec2::new(7, -9);
    fn main(id: u32) -> MainEntity {
        Entity::from_raw_u32(id).unwrap().into()
    }
    fn near_map() -> HashMap<MainEntity, TerrainPrimitiveIdentity> {
        (0..4u8)
            .map(|quadrant| {
                (
                    main(100 + u32::from(quadrant)),
                    TerrainPrimitiveIdentity {
                        worldspace_id: WORLD,
                        grid: GRID,
                        quadrant,
                        tier: None,
                    },
                )
            })
            .collect()
    }

    struct Harness {
        render: World,
        observer: Schedule,
        key: DestinationKey,
    }
    impl Harness {
        fn ready_on_stock() -> Self {
            Self::ready_on_stock_with_map(near_map())
        }
        fn ready_on_stock_with_map(terrain: HashMap<MainEntity, TerrainPrimitiveIdentity>) -> Self {
            let inputs = TerrainClipInputs {
                target_offset: [0; 3],
                world: Some(WORLD),
                origin: IVec2::ZERO,
                forward_covered: (0..4).map(|q| (GRID, q)).collect(),
                depth_covered: (0..4).map(|q| (GRID, q)).collect(),
                ready_blocks: HashSet::new(),
            };
            assert!(
                terrain_pass_ready(&inputs, GRID, 3, false, true, false),
                "Ready is stock-only, with no ready cluster block"
            );
            let mut render = World::new();
            let sources = Sources {
                worlds: HashMap::from([(main(1), Some(WORLD))]),
                terrain_visible: terrain.keys().copied().collect(),
                terrain: terrain.into(),
                ..Default::default()
            };
            render.insert_resource(sources);
            render.init_resource::<ClusterBatchMaterials>();
            render.insert_resource(RenderFrame(1));
            render.init_resource::<RenderInvalidations>();
            let mut main_world = MainWorld::default();
            main_world.init_resource::<Generations>();
            main_world.init_resource::<DestinationReports>();
            render.insert_resource(main_world);
            render.init_resource::<CandidateObserver>();
            let mut observer = Schedule::default();
            observer.add_systems(observe_candidates);
            observer.run(&mut render);
            let generation = render
                .resource::<MainWorld>()
                .resource::<Generations>()
                .current(WorldId::exterior(WORLD));
            let key = DestinationKey {
                view: ViewKey {
                    view: RetainedViewEntity::new(main(1), None, 0),
                    incarnation: 1,
                },
                world: WorldId::exterior(WORLD),
                generation,
            };
            render
                .resource_mut::<MainWorld>()
                .resource_mut::<DestinationReports>()
                .set(DestinationReport {
                    key,
                    evaluated_render_frame: RenderFrame(10),
                    state: DestinationState::Ready,
                });
            render.insert_resource(RenderFrame(11));
            Self {
                render,
                observer,
                key,
            }
        }
        fn ready(&self) -> bool {
            let main = self.render.resource::<MainWorld>();
            main.resource::<DestinationReports>()
                .is_ready(&self.key, main.resource::<Generations>())
        }
        fn apply_terrain(
            &mut self,
            current: HashMap<MainEntity, TerrainPrimitiveIdentity>,
        ) -> TerrainMapChange {
            let visible = self
                .render
                .resource::<Sources>()
                .terrain_visible
                .iter()
                .filter(|entity| current.contains_key(*entity))
                .copied()
                .collect();
            self.apply_inputs(current, visible)
        }
        fn apply_inputs(
            &mut self,
            current: HashMap<MainEntity, TerrainPrimitiveIdentity>,
            visible: HashSet<MainEntity>,
        ) -> TerrainMapChange {
            let change = {
                let mut sources = self.render.resource_mut::<Sources>();
                let change = terrain_map_change(&sources.terrain, &current);
                let (visible_changed, withdrawn) =
                    terrain_visibility_change(&sources.terrain_visible, &visible);
                if change != TerrainMapChange::Unchanged || visible_changed {
                    let delta = {
                        let old = sources.candidate_inputs();
                        let new = CandidateInputs {
                            terrain: &current,
                            terrain_visible: &visible,
                            ..sources.candidate_inputs()
                        };
                        candidate_worlds(&old, &new)
                    };
                    sources.record_change(change == TerrainMapChange::Invalidating || withdrawn);
                    sources.candidate_worlds = delta;
                    sources.terrain = current.into();
                    sources.terrain_visible = visible;
                }
                change
            };
            self.observer.run(&mut self.render);
            change
        }
    }

    #[test]
    fn far_arrival_updates_selection_without_invalidating_prior_stock_ready() {
        let mut h = Harness::ready_on_stock();
        assert!(h.ready());
        let (selection, candidate) = {
            let sources = h.render.resource::<Sources>();
            (sources.version, sources.candidate_version)
        };
        let mut current = near_map();
        current.insert(
            main(999),
            TerrainPrimitiveIdentity {
                worldspace_id: WORLD,
                grid: GRID + IVec2::X,
                quadrant: 0,
                tier: Some(LodTier::Tier8),
            },
        );
        assert_eq!(h.apply_terrain(current), TerrainMapChange::FarAdditionsOnly);
        let sources = h.render.resource::<Sources>();
        assert_eq!(sources.version, selection + 1);
        assert_eq!(sources.candidate_version, candidate);
        assert!(
            h.ready(),
            "old report remains valid without any newly evaluated report"
        );
        assert_eq!(
            h.render
                .resource::<MainWorld>()
                .resource::<DestinationReports>()
                .get(&h.key)
                .unwrap()
                .evaluated_render_frame,
            RenderFrame(10)
        );
    }

    #[test]
    fn terrain_withdrawal_reassignment_and_near_arrival_revoke_before_a_new_report() {
        for mutation in ["withdraw", "reassign", "near_addition", "far_withdraw"] {
            let mut h = Harness::ready_on_stock();
            let mut current = near_map();
            match mutation {
                "withdraw" => {
                    current.remove(&main(100));
                }
                "reassign" => {
                    current.get_mut(&main(100)).unwrap().grid += IVec2::Y;
                }
                "near_addition" => {
                    current.insert(
                        main(998),
                        TerrainPrimitiveIdentity {
                            worldspace_id: WORLD,
                            grid: GRID + IVec2::X,
                            quadrant: 0,
                            tier: None,
                        },
                    );
                }
                "far_withdraw" => {
                    let far = TerrainPrimitiveIdentity {
                        worldspace_id: WORLD,
                        grid: GRID + IVec2::X,
                        quadrant: 0,
                        tier: Some(LodTier::Tier16),
                    };
                    let mut arrival = current.clone();
                    arrival.insert(main(999), far);
                    assert_eq!(h.apply_terrain(arrival), TerrainMapChange::FarAdditionsOnly);
                }
                _ => unreachable!(),
            }
            assert!(h.ready());
            assert_eq!(
                h.apply_terrain(current),
                TerrainMapChange::Invalidating,
                "{mutation}"
            );
            assert!(!h.ready(), "old Ready must fail immediately for {mutation}");
            let main = h.render.resource::<MainWorld>();
            assert_eq!(
                main.resource::<DestinationReports>()
                    .get(&h.key)
                    .unwrap()
                    .state,
                DestinationState::Ready,
                "the stored Ready is deliberately not overwritten by a newer evaluation"
            );
        }
    }

    #[test]
    fn other_world_candidate_churn_preserves_ready_and_delta_is_consumed_once() {
        let mut h = Harness::ready_on_stock();
        let foreign_world = WORLD + 1;
        h.render
            .resource_mut::<Sources>()
            .worlds
            .insert(main(2), Some(foreign_world));
        h.render.resource_mut::<Sources>().record_change(true);
        h.render.resource_mut::<Sources>().candidate_worlds = CandidateWorlds {
            all: false,
            worlds: HashSet::from([foreign_world]),
            ..default()
        };
        h.observer.run(&mut h.render);
        assert!(
            h.ready(),
            "a source-world change cannot stale a different destination"
        );
        let generation = h
            .render
            .resource::<MainWorld>()
            .resource::<Generations>()
            .current(WorldId::exterior(foreign_world));
        h.observer.run(&mut h.render);
        assert_eq!(
            h.render
                .resource::<MainWorld>()
                .resource::<Generations>()
                .current(WorldId::exterior(foreign_world)),
            generation
        );
        assert!(h.ready());
        // A later global material change still revokes it without a new source version.
        h.render.resource_mut::<ClusterBatchMaterials>().version += 1;
        h.observer.run(&mut h.render);
        assert!(!h.ready());
    }

    fn cover(grid: IVec2) -> CellKey {
        CellKey::Exterior {
            worldspace_id: WORLD,
            grid_x: grid.x,
            grid_y: grid.y,
        }
    }

    /// A pending request for the harness's key, as the crossing code holds while it waits.
    fn request_coverage(h: &mut Harness, coverage: Vec<CellKey>) -> RequestId {
        let mut requests = DestinationRequests::default();
        let id = requests.request(DestinationRequest {
            key: h.key,
            passes: PassFamilies::NONE,
            coverage: Arc::from(coverage),
        });
        h.render
            .resource_mut::<MainWorld>()
            .insert_resource(requests);
        // The observer records a request's coverage the first time it sees it.
        h.observer.run(&mut h.render);
        id
    }

    fn near_withdrawal() -> HashMap<MainEntity, TerrainPrimitiveIdentity> {
        let mut current = near_map();
        current.remove(&main(100));
        current
    }

    fn generation(h: &Harness) -> Generation {
        h.render
            .resource::<MainWorld>()
            .resource::<Generations>()
            .current(h.key.world)
    }

    #[test]
    fn same_world_change_outside_a_pending_destinations_coverage_does_not_bump_it() {
        let mut h = Harness::ready_on_stock();
        request_coverage(&mut h, vec![cover(GRID + IVec2::X)]);
        let before = generation(&h);
        assert_eq!(
            h.apply_terrain(near_withdrawal()),
            TerrainMapChange::Invalidating
        );
        assert_eq!(
            generation(&h),
            before,
            "an unrelated cell must not restart it"
        );
        assert!(h.ready());
    }

    #[test]
    fn same_world_change_inside_a_pending_destinations_coverage_bumps_it() {
        let mut h = Harness::ready_on_stock();
        request_coverage(&mut h, vec![cover(GRID + IVec2::X), cover(GRID)]);
        let before = generation(&h);
        h.apply_terrain(near_withdrawal());
        assert_ne!(generation(&h), before);
        assert!(!h.ready());
    }

    #[test]
    fn settled_frames_walk_no_requests_or_scopes() {
        let mut h = Harness::ready_on_stock();
        let id = request_coverage(&mut h, vec![cover(GRID)]);
        let walks = |h: &Harness| h.render.resource::<CandidateObserver>().walks;
        let settled = walks(&h);
        for _ in 0..8 {
            // A waiting consumer rebases every frame; an unchanged generation is no change.
            let mut main = h.render.resource_mut::<MainWorld>();
            let generations = main.remove_resource::<Generations>().unwrap();
            main.resource_mut::<DestinationRequests>()
                .rebase(id, &generations);
            main.insert_resource(generations);
            h.observer.run(&mut h.render);
        }
        assert_eq!(
            walks(&h),
            settled,
            "a settled frame walked requests or scopes"
        );
        h.render
            .resource_mut::<MainWorld>()
            .resource_mut::<DestinationRequests>()
            .withdraw(id);
        h.observer.run(&mut h.render);
        assert_eq!(walks(&h), settled + 1, "a withdrawal is one change");
        h.apply_terrain(near_withdrawal());
        assert_eq!(walks(&h), settled + 2, "a candidate change is one change");
    }

    #[test]
    fn stored_report_keeps_its_scope_after_the_request_is_withdrawn() {
        let mut h = Harness::ready_on_stock();
        let id = request_coverage(&mut h, vec![cover(GRID + IVec2::X)]);
        h.render
            .resource_mut::<MainWorld>()
            .resource_mut::<DestinationRequests>()
            .withdraw(id);
        h.observer.run(&mut h.render);
        h.apply_terrain(near_withdrawal());
        assert!(h.ready(), "its coverage never included the changed cell");
        let mut moved = near_withdrawal();
        moved.get_mut(&main(101)).unwrap().grid = GRID + IVec2::X;
        h.apply_terrain(moved);
        assert!(
            !h.ready(),
            "a change that reaches its coverage revokes the stored report"
        );
    }

    #[test]
    fn candidate_and_material_paths_still_revoke_existing_ready_reports() {
        let mut h = Harness::ready_on_stock();
        h.render.resource_mut::<Sources>().record_change(true);
        h.observer.run(&mut h.render);
        assert!(!h.ready());
        let mut h = Harness::ready_on_stock();
        h.render.resource_mut::<ClusterBatchMaterials>().version += 1;
        h.observer.run(&mut h.render);
        assert!(!h.ready());
    }

    #[test]
    fn unchanged_terrain_does_not_advance_either_counter() {
        let mut h = Harness::ready_on_stock();
        assert_eq!(h.apply_terrain(near_map()), TerrainMapChange::Unchanged);
        let sources = h.render.resource::<Sources>();
        assert_eq!((sources.version, sources.candidate_version), (0, 0));
        assert!(h.ready());
    }
    fn far_stock_map() -> HashMap<MainEntity, TerrainPrimitiveIdentity> {
        near_map()
            .into_iter()
            .map(|(entity, mut identity)| {
                identity.tier = Some(LodTier::Tier16);
                (entity, identity)
            })
            .collect()
    }

    #[test]
    fn finer_far_arrival_hiding_the_only_old_stock_revokes_stored_ready() {
        let old = far_stock_map();
        let mut h = Harness::ready_on_stock_with_map(old.clone());
        assert!(h.ready());
        let mut current = old;
        let mut new_visible = HashSet::new();
        for quadrant in 0..4u8 {
            let entity = main(200 + u32::from(quadrant));
            current.insert(
                entity,
                TerrainPrimitiveIdentity {
                    worldspace_id: WORLD,
                    grid: GRID,
                    quadrant,
                    tier: Some(LodTier::Tier8),
                },
            );
            new_visible.insert(entity);
        }
        // Identity map is a strict FAR-only superset, but visibility switches
        // from drawable old stock to new stock whose required passes are absent.
        assert_eq!(
            terrain_map_change(&h.render.resource::<Sources>().terrain, &current),
            TerrainMapChange::FarAdditionsOnly
        );
        let not_prepared = TerrainClipInputs {
            target_offset: [0; 3],
            world: Some(WORLD),
            origin: IVec2::ZERO,
            forward_covered: HashSet::new(),
            depth_covered: HashSet::new(),
            ready_blocks: HashSet::new(),
        };
        assert!(!terrain_pass_ready(
            &not_prepared,
            GRID,
            3,
            false,
            true,
            false
        ));
        assert_eq!(
            h.apply_inputs(current, new_visible),
            TerrainMapChange::FarAdditionsOnly
        );
        assert!(
            !h.ready(),
            "old stock-only Ready must be rejected before the new Waiting is mirrored"
        );
        assert_eq!(
            h.render
                .resource::<MainWorld>()
                .resource::<DestinationReports>()
                .get(&h.key)
                .unwrap()
                .state,
            DestinationState::Ready,
            "the test intentionally preserves the older report rather than replacing it"
        );
    }

    #[test]
    fn unchanged_identities_with_raw_visible_member_lost_revoke_stored_ready() {
        let current = far_stock_map();
        let mut h = Harness::ready_on_stock_with_map(current.clone());
        let mut visible: HashSet<_> = current.keys().copied().collect();
        visible.remove(&main(100));
        assert_eq!(
            h.apply_inputs(current, visible),
            TerrainMapChange::Unchanged
        );
        assert!(!h.ready());
        let sources = h.render.resource::<Sources>();
        assert_eq!((sources.version, sources.candidate_version), (1, 1));
    }

    #[test]
    fn far_and_visible_additions_preserving_old_visible_sources_do_not_revoke() {
        let old = far_stock_map();
        let mut h = Harness::ready_on_stock_with_map(old.clone());
        let old_visible: HashSet<_> = old.keys().copied().collect();
        let mut current = old;
        current.insert(
            main(999),
            TerrainPrimitiveIdentity {
                worldspace_id: WORLD,
                grid: GRID + IVec2::X,
                quadrant: 0,
                tier: Some(LodTier::Tier8),
            },
        );
        assert_eq!(
            h.apply_inputs(current.clone(), old_visible.clone()),
            TerrainMapChange::FarAdditionsOnly
        );
        assert!(h.ready());
        let mut all_visible = old_visible;
        all_visible.insert(main(999));
        assert_eq!(
            h.apply_inputs(current, all_visible),
            TerrainMapChange::Unchanged
        );
        assert!(h.ready());
        let sources = h.render.resource::<Sources>();
        assert_eq!((sources.version, sources.candidate_version), (2, 0));
    }
}
