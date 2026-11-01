//! Exact world ownership of changed replacement inputs. Storage blocks may belong
//! to a parent world; invalidation follows each reference's placing world.
use super::*;

#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct CandidateWorlds {
    pub(super) all: bool,
    pub(super) worlds: HashSet<u32>,
    pub(super) cells: HashSet<(u32, IVec2)>,
    pub(super) batches: HashSet<usize>,
    pub(super) global_worlds: HashSet<u32>,
}

pub(super) struct CandidateInputs<'a> {
    pub(super) certificate: Option<&'a Arc<crate::cluster::reader::CertifiedPackage>>,
    pub(super) present: &'a BTreeSet<u32>,
    pub(super) retired_refs: &'a HashSet<u32>,
    pub(super) retired_present: &'a HashSet<u32>,
    pub(super) retired_materials: &'a HashMap<MainEntity, Option<bevy::asset::UntypedAssetId>>,
    pub(super) retired_eligibility: &'a HashMap<MainEntity, RetiredEligibility>,
    pub(super) bound: &'a HashMap<RecordKey, MainEntity>,
    pub(super) stock_primitives: &'a HashMap<(u32, u32), MainEntity>,
    pub(super) templates: &'a [bool],
    pub(super) normal_maps: &'a [bool],
    pub(super) worlds: &'a HashMap<MainEntity, Option<u32>>,
    pub(super) light_dependencies: &'a HashMap<MainEntity, Option<HashSet<MainEntity>>>,
    pub(super) terrain: &'a HashMap<MainEntity, crate::streaming::TerrainPrimitiveIdentity>,
    pub(super) terrain_visible: &'a HashSet<MainEntity>,
}

impl Sources {
    pub(super) fn candidate_inputs(&self) -> CandidateInputs<'_> {
        CandidateInputs {
            certificate: self.certificate.as_ref(),
            present: &self.present,
            retired_refs: &self.retired_refs,
            retired_present: &self.retired_present,
            retired_materials: &self.retired_materials,
            retired_eligibility: &self.retired_eligibility,
            bound: &self.bound,
            stock_primitives: &self.stock_primitives,
            templates: &self.templates,
            normal_maps: &self.normal_maps,
            worlds: &self.worlds,
            light_dependencies: &self.light_dependencies,
            terrain: &self.terrain,
            terrain_visible: &self.terrain_visible,
        }
    }
}

fn changed_keys<'a, K: Eq + std::hash::Hash, V: PartialEq>(
    old: &'a HashMap<K, V>,
    new: &'a HashMap<K, V>,
) -> impl Iterator<Item = &'a K> {
    old.iter()
        .filter(|(key, value)| new.get(key) != Some(*value))
        .map(|(key, _)| key)
        .chain(new.keys().filter(|key| !old.contains_key(key)))
}

impl CandidateWorlds {
    fn cell(&mut self, world: u32, grid: IVec2) {
        self.worlds.insert(world);
        self.cells.insert((world, grid));
    }

    fn global(&mut self, world: u32) {
        self.worlds.insert(world);
        self.global_worlds.insert(world);
    }

    fn reference(&mut self, input: &CandidateInputs<'_>, reference: u32) {
        if let Some(reference) = input.certificate.and_then(|c| c.reference(reference)) {
            self.cell(
                reference.placed_worldspace_id,
                IVec2::new(reference.cell.0, reference.cell.1),
            );
        } else {
            self.all = true;
        }
    }

    fn entities(&mut self, input: &CandidateInputs<'_>, entities: &HashSet<MainEntity>) {
        if entities.is_empty() {
            return;
        }
        let mut found = HashSet::new();
        for (key, owner) in input.bound {
            if entities.contains(owner) {
                found.insert(*owner);
                if let Some(record) = input.certificate.and_then(|c| c.record(key)) {
                    self.reference(input, record.reference);
                } else {
                    self.all = true;
                }
            }
        }
        for ((reference, _), owner) in input.stock_primitives {
            if entities.contains(owner) {
                found.insert(*owner);
                self.reference(input, *reference);
            }
        }
        self.all |= !entities.is_subset(&found);
    }

    /// Names the exterior worlds an entity belonged to before and after a change. A move into an
    /// interior (no exterior world on one side) still revokes the exterior it left; an entity
    /// that never had an exterior world to name cannot be scoped at all.
    fn entity_worlds<'a>(
        &mut self,
        inputs: impl IntoIterator<Item = &'a CandidateInputs<'a>>,
        entity: &MainEntity,
    ) {
        let (mut known, mut unknown) = (false, false);
        for input in inputs {
            match input.worlds.get(entity) {
                Some(Some(world)) => {
                    known = true;
                    self.global(*world);
                }
                Some(None) | None => unknown = true,
            }
        }
        self.all |= unknown && !known;
    }
}

/// Slots whose readiness differs, in either direction and across a change of length.
fn changed_slots(old: &[bool], new: &[bool]) -> impl Iterator<Item = usize> {
    (0..old.len().max(new.len())).filter(move |&index| old.get(index) != new.get(index))
}

pub(super) fn candidate_worlds(
    old: &CandidateInputs<'_>,
    new: &CandidateInputs<'_>,
) -> CandidateWorlds {
    let same_certificate = match (old.certificate, new.certificate) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    let mut delta = CandidateWorlds {
        all: !same_certificate,
        ..default()
    };
    if delta.all {
        return delta;
    }
    // Shared batches are resolved to the cells of their members when the destination is judged.
    delta
        .batches
        .extend(changed_slots(old.templates, new.templates));
    delta
        .batches
        .extend(changed_slots(old.normal_maps, new.normal_maps));
    for reference in old.present.symmetric_difference(new.present) {
        delta.reference(old, *reference);
        delta.reference(new, *reference);
    }
    for reference in old
        .retired_refs
        .symmetric_difference(new.retired_refs)
        .chain(
            old.retired_present
                .symmetric_difference(new.retired_present),
        )
    {
        delta.reference(old, *reference);
        delta.reference(new, *reference);
    }
    for key in changed_keys(old.bound, new.bound) {
        if let Some(record) = old.certificate.and_then(|c| c.record(key)) {
            delta.reference(old, record.reference);
        } else {
            delta.all = true;
        }
    }
    for (reference, _) in changed_keys(old.stock_primitives, new.stock_primitives) {
        delta.reference(old, *reference);
        delta.reference(new, *reference);
    }
    let changed_entities: HashSet<_> = changed_keys(old.retired_materials, new.retired_materials)
        .chain(changed_keys(
            old.retired_eligibility,
            new.retired_eligibility,
        ))
        .copied()
        .collect();
    for input in [old, new] {
        let owned: HashSet<_> = changed_entities
            .iter()
            .copied()
            .filter(|entity| {
                input.retired_materials.contains_key(entity)
                    || input.retired_eligibility.contains_key(entity)
            })
            .collect();
        delta.entities(input, &owned);
    }
    // A camera or light changing world changes what every destination in that world is lit and
    // viewed by, so it stays world-wide within the worlds it names.
    for entity in changed_keys(old.worlds, new.worlds) {
        delta.entity_worlds([old, new], entity);
    }
    for entity in changed_keys(old.light_dependencies, new.light_dependencies) {
        let owners: Vec<_> = [old, new]
            .into_iter()
            .filter(|input| input.light_dependencies.contains_key(entity))
            .collect();
        delta.entity_worlds(owners, entity);
    }
    if terrain_map_change(old.terrain, new.terrain) == TerrainMapChange::Invalidating {
        for entity in changed_keys(old.terrain, new.terrain) {
            debug!(?entity, previous = ?old.terrain.get(entity), current = ?new.terrain.get(entity),
                "cluster terrain candidate identity changed");
            for input in [old, new] {
                if let Some(identity) = input.terrain.get(entity) {
                    delta.cell(identity.worldspace_id, identity.grid);
                }
            }
        }
    }
    for entity in old.terrain_visible.difference(new.terrain_visible) {
        debug!(?entity, previous = ?old.terrain.get(entity), current = ?new.terrain.get(entity),
            "cluster terrain candidate visibility withdrawn");
        if let Some(identity) = old.terrain.get(entity) {
            delta.cell(identity.worldspace_id, identity.grid);
        } else {
            delta.all = true;
        }
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::reader::producer_fixture_tests::published_for_loader;

    #[test]
    fn object_changes_follow_placed_world_and_preserve_other_world_ready_reports() {
        use crate::swap_bridge::destination::*;
        let directory = published_for_loader();
        let certificate = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let (key, child) = certificate
            .records()
            .find(|(key, record)| key.worldspace != record.worldspace)
            .unwrap();
        let parent_world = WorldId::exterior(key.worldspace);
        let child_world = WorldId::exterior(child.worldspace);
        let mut old = Sources {
            certificate: Some(certificate.clone()),
            ..default()
        };
        let entity = MainEntity::from(Entity::from_bits(123));
        old.present.insert(child.reference);
        old.bound.insert(*key, entity);
        let new = Sources {
            certificate: Some(certificate.clone()),
            ..default()
        };
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert!(!delta.all);
        assert_eq!(delta.worlds, HashSet::from([child.worldspace]));
        let snapshot = certificate.reference(child.reference).unwrap();
        assert_eq!(
            delta.cells,
            HashSet::from([(
                snapshot.placed_worldspace_id,
                IVec2::new(snapshot.cell.0, snapshot.cell.1),
            )])
        );
        assert!(delta.global_worlds.is_empty());
        assert!(delta.batches.is_empty());
        let mut generations = Generations::default();
        let ready_key = DestinationKey {
            view: ViewKey {
                view: RetainedViewEntity::new(MainEntity::from(Entity::from_bits(456)), None, 0),
                incarnation: 0,
            },
            world: parent_world,
            generation: generations.current(parent_world),
        };
        let mut reports = DestinationReports::default();
        reports.set(DestinationReport {
            key: ready_key,
            evaluated_render_frame: RenderFrame(5),
            state: DestinationState::Ready,
        });
        for world in delta.worlds {
            generations.bump(WorldId::exterior(world), InvalidationCause::Candidate);
        }
        assert!(reports.is_ready(&ready_key, &generations));
        assert_ne!(generations.current(child_world), Generation::default());
        // Equal-count entity replacement still changes the child's binding.
        let mut replacement = Sources {
            certificate: Some(certificate.clone()),
            ..default()
        };
        replacement.present = old.present.clone();
        replacement
            .bound
            .insert(*key, MainEntity::from(Entity::from_bits(124)));
        let delta = candidate_worlds(&old.candidate_inputs(), &replacement.candidate_inputs());
        assert_eq!(delta.worlds, HashSet::from([child.worldspace]));
        let snapshot = certificate.reference(child.reference).unwrap();
        assert_eq!(
            delta.cells,
            HashSet::from([(
                snapshot.placed_worldspace_id,
                IVec2::new(snapshot.cell.0, snapshot.cell.1),
            )])
        );
        assert!(delta.global_worlds.is_empty());
        assert!(delta.batches.is_empty());
        assert!(!delta.all);
    }

    #[test]
    fn terrain_and_light_withdrawals_revoke_only_their_old_and_new_worlds() {
        let entity = MainEntity::from(Entity::from_bits(123));
        let identity = crate::streaming::TerrainPrimitiveIdentity {
            worldspace_id: 100,
            grid: IVec2::ZERO,
            quadrant: 0,
            tier: None,
        };
        let mut old = Sources::default();
        Arc::make_mut(&mut old.terrain).insert(entity, identity);
        old.terrain_visible.insert(entity);
        let mut new = Sources::default();
        Arc::make_mut(&mut new.terrain).insert(
            entity,
            crate::streaming::TerrainPrimitiveIdentity {
                worldspace_id: 200,
                ..identity
            },
        );
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(delta.worlds, HashSet::from([100, 200]));
        assert!(!delta.all);
        assert_eq!(
            delta.cells,
            HashSet::from([(100, IVec2::ZERO), (200, IVec2::ZERO)])
        );
        assert!(delta.global_worlds.is_empty());
        old.worlds.insert(entity, Some(100));
        new.worlds.insert(entity, Some(200));
        old.light_dependencies.insert(entity, Some(HashSet::new()));
        new.light_dependencies.insert(entity, None);
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(delta.worlds, HashSet::from([100, 200]));
        assert!(!delta.all);
        assert_eq!(delta.global_worlds, HashSet::from([100, 200]));
    }

    #[test]
    fn unknown_reference_and_entity_ownership_fail_closed() {
        let old = Sources::default();
        let mut new = Sources::default();
        new.present.insert(999);
        assert!(candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs()).all);
        new.present.clear();
        new.retired_materials
            .insert(MainEntity::from(Entity::from_bits(123)), None);
        assert!(candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs()).all);
    }

    #[test]
    fn readiness_arrays_track_changed_slots_in_both_directions_and_lengths() {
        for normal_maps in [false, true] {
            for (before, after, expected) in [
                (
                    vec![false, true, true],
                    vec![true, true, false],
                    HashSet::from([0, 2]),
                ),
                (vec![true], vec![true, false, true], HashSet::from([1, 2])),
                (vec![true, false, true], vec![true], HashSet::from([1, 2])),
            ] {
                let mut old = Sources::default();
                let mut new = Sources::default();
                if normal_maps {
                    old.normal_maps = before;
                    new.normal_maps = after;
                } else {
                    old.templates = before;
                    new.templates = after;
                }
                let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
                assert_eq!(delta.batches, expected, "normal_maps={normal_maps}");
                assert!(!delta.all);
                assert!(delta.cells.is_empty());
                assert!(delta.worlds.is_empty());
                assert!(delta.global_worlds.is_empty());
            }
        }
    }

    #[test]
    fn unchanged_shared_batches_produce_no_delta() {
        let old = Sources {
            templates: vec![true, false, true],
            normal_maps: vec![false, true, false],
            ..default()
        };
        let new = Sources {
            templates: old.templates.clone(),
            normal_maps: old.normal_maps.clone(),
            ..default()
        };
        assert_eq!(
            candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs()),
            CandidateWorlds::default()
        );
    }

    #[test]
    fn same_world_terrain_changes_preserve_exact_old_and_new_cells() {
        let entity = MainEntity::from(Entity::from_bits(123));
        let identity = crate::streaming::TerrainPrimitiveIdentity {
            worldspace_id: 100,
            grid: IVec2::new(-3, 7),
            quadrant: 0,
            tier: None,
        };
        let mut old = Sources::default();
        Arc::make_mut(&mut old.terrain).insert(entity, identity);
        let mut new = Sources::default();
        let next_grid = IVec2::new(8, -2);
        Arc::make_mut(&mut new.terrain).insert(
            entity,
            crate::streaming::TerrainPrimitiveIdentity {
                grid: next_grid,
                ..identity
            },
        );
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(
            delta.cells,
            HashSet::from([(100, identity.grid), (100, next_grid)])
        );
        assert_eq!(delta.worlds, HashSet::from([100]));
        assert!(delta.global_worlds.is_empty());
        assert!(!delta.all);
    }

    #[test]
    fn terrain_visibility_withdrawal_scopes_unchanged_identity() {
        let entity = MainEntity::from(Entity::from_bits(123));
        let identity = crate::streaming::TerrainPrimitiveIdentity {
            worldspace_id: 100,
            grid: IVec2::new(5, -9),
            quadrant: 0,
            tier: None,
        };
        let mut old = Sources::default();
        Arc::make_mut(&mut old.terrain).insert(entity, identity);
        old.terrain_visible.insert(entity);
        let new = Sources {
            terrain: old.terrain.clone(),
            ..default()
        };
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(delta.cells, HashSet::from([(100, identity.grid)]));
        assert_eq!(delta.worlds, HashSet::from([100]));
        assert!(delta.global_worlds.is_empty());
        assert!(!delta.all);
    }

    #[test]
    fn bound_and_retired_entity_changes_use_child_reference_cell() {
        let directory = published_for_loader();
        let certificate = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let (key, record) = certificate
            .records()
            .find(|(key, record)| key.worldspace != record.worldspace)
            .unwrap();
        let snapshot = certificate.reference(record.reference).unwrap();
        let expected = HashSet::from([(
            snapshot.placed_worldspace_id,
            IVec2::new(snapshot.cell.0, snapshot.cell.1),
        )]);
        let entity = MainEntity::from(Entity::from_bits(123));
        let mut old = Sources {
            certificate: Some(certificate.clone()),
            ..default()
        };
        old.bound.insert(*key, entity);
        let mut new = Sources {
            certificate: Some(certificate.clone()),
            ..default()
        };
        // No presence delta can mask an incorrectly scoped binding delta.
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(delta.cells, expected);
        assert!(!delta.all);
        new.bound = old.bound.clone();
        new.retired_materials.insert(entity, None);
        for (before, after) in [(&old, &new), (&new, &old)] {
            let delta = candidate_worlds(&before.candidate_inputs(), &after.candidate_inputs());
            assert_eq!(delta.cells, expected);
            assert_eq!(delta.worlds, HashSet::from([snapshot.placed_worldspace_id]));
            assert!(delta.global_worlds.is_empty());
            assert!(!delta.all);
        }
        old.bound.clear();
        new.bound.clear();
        old.stock_primitives.insert((record.reference, 0), entity);
        new.stock_primitives = old.stock_primitives.clone();
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(delta.cells, expected);
        assert!(!delta.all);
    }

    #[test]
    fn world_and_light_changes_remain_global_within_known_worlds() {
        let entity = MainEntity::from(Entity::from_bits(123));
        let mut old = Sources::default();
        old.worlds.insert(entity, Some(100));
        let mut new = Sources::default();
        new.worlds.insert(entity, Some(200));
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(delta.global_worlds, HashSet::from([100, 200]));
        assert_eq!(delta.worlds, delta.global_worlds);
        assert!(delta.cells.is_empty());
        assert!(!delta.all);
        new.worlds = old.worlds.clone();
        old.light_dependencies.insert(entity, Some(HashSet::new()));
        new.light_dependencies.insert(entity, None);
        let delta = candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs());
        assert_eq!(delta.global_worlds, HashSet::from([100]));
        assert!(delta.cells.is_empty());
        assert!(!delta.all);
    }

    #[test]
    fn unknown_world_and_light_ownership_fail_closed() {
        let old = Sources::default();
        let entity = MainEntity::from(Entity::from_bits(123));
        let mut new = Sources::default();
        new.worlds.insert(entity, None);
        assert!(candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs()).all);
        new.worlds.clear();
        new.light_dependencies.insert(entity, None);
        assert!(candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs()).all);
    }

    #[test]
    fn certificate_replacement_invalidates_all_scopes() {
        let directory = published_for_loader();
        let old = Sources {
            certificate: Some(Arc::new(
                crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
            )),
            ..default()
        };
        let new = Sources {
            certificate: Some(Arc::new(
                crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
            )),
            ..default()
        };
        assert!(candidate_worlds(&old.candidate_inputs(), &new.candidate_inputs()).all);
    }
}
