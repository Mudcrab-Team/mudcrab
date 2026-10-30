//! Destination invalidation derived once from a scene and its retained certificate.
use super::*;

pub(super) type Cell = (u32, IVec2);

#[derive(Default)]
pub(super) struct BatchCells {
    pub(super) cells: HashMap<usize, HashSet<Cell>>,
    pub(super) unknown: HashSet<usize>,
}

impl BatchCells {
    pub(super) fn build(
        scene: &ClusterScene,
        certificate: Option<&crate::cluster::reader::CertifiedPackage>,
    ) -> Self {
        let mut index = Self::default();
        for instance in &scene.instances {
            let batch = instance.batch as usize;
            match instance.identity {
                ClusterInstanceIdentity::Object(key) => {
                    let reference = certificate
                        .and_then(|certificate| certificate.record(&key))
                        .and_then(|record| certificate?.reference(record.reference));
                    if let Some(reference) = reference {
                        index.cells.entry(batch).or_default().insert((
                            reference.placed_worldspace_id,
                            IVec2::new(reference.cell.0, reference.cell.1),
                        ));
                    } else {
                        index.unknown.insert(batch);
                    }
                }
                ClusterInstanceIdentity::Terrain { block, .. } => {
                    if let Some(block) = scene.terrain_blocks.get(block as usize)
                        && !block.cells.is_empty()
                    {
                        index
                            .cells
                            .entry(batch)
                            .or_default()
                            .extend(block.cells.iter().map(|cell| (block.worldspace_id, *cell)));
                    } else {
                        index.unknown.insert(batch);
                    }
                }
                ClusterInstanceIdentity::Fixture => {
                    index.unknown.insert(batch);
                }
            }
        }
        index
    }
}

#[derive(Default)]
pub(super) struct Effects {
    pub(super) all: bool,
    pub(super) worlds: HashSet<WorldId>,
    pub(super) cells: HashSet<Cell>,
}

impl Effects {
    pub(super) fn from_delta(
        delta: &CandidateWorlds,
        material_batches: &HashSet<usize>,
        index: &BatchCells,
    ) -> Self {
        let mut effects = Self {
            all: delta.all,
            worlds: delta
                .global_worlds
                .iter()
                .copied()
                .map(WorldId::exterior)
                .collect(),
            cells: delta.cells.clone(),
        };
        // Older manual callers provide only the union of changed worlds.
        if delta.cells.is_empty() && delta.batches.is_empty() && delta.global_worlds.is_empty() {
            effects
                .worlds
                .extend(delta.worlds.iter().copied().map(WorldId::exterior));
        }
        for batch in delta.batches.union(material_batches) {
            if index.unknown.contains(batch) {
                effects.all = true;
            }
            if let Some(cells) = index.cells.get(batch).filter(|cells| !cells.is_empty()) {
                effects.cells.extend(cells.iter().copied());
            } else {
                effects.all = true;
            }
        }
        effects
    }

    /// Whether a destination that depends on `coverage` in `world` must be revoked.
    ///
    /// A world-wide effect, or a change whose cells cannot be compared with the destination's
    /// coverage (it names no exterior cell of that world), fails closed. Otherwise only an
    /// intersection of a changed cell with the destination's own coverage revokes it.
    pub(super) fn affects(&self, world: WorldId, coverage: &[CellKey]) -> bool {
        if self.all || self.worlds.contains(&world) {
            return true;
        }
        if !self
            .cells
            .iter()
            .any(|(changed, _)| WorldId::exterior(*changed) == world)
        {
            return false;
        }
        let mut comparable = false;
        for key in coverage {
            if let CellKey::Exterior {
                worldspace_id,
                grid_x,
                grid_y,
            } = *key
                && WorldId::exterior(worldspace_id) == world
            {
                comparable = true;
                if self
                    .cells
                    .contains(&(worldspace_id, IVec2::new(grid_x, grid_y)))
                {
                    return true;
                }
            }
        }
        !comparable
    }

    pub(super) fn changed_worlds(&self) -> HashSet<WorldId> {
        self.worlds
            .iter()
            .copied()
            .chain(
                self.cells
                    .iter()
                    .map(|(world, _)| WorldId::exterior(*world)),
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{
        reader::producer_fixture_tests::published_for_loader, render::ClusterInstance,
        terrain::TerrainViewBlock,
    };

    fn coverage((worldspace_id, grid): Cell) -> CellKey {
        CellKey::Exterior {
            worldspace_id,
            grid_x: grid.x,
            grid_y: grid.y,
        }
    }

    fn instance(identity: ClusterInstanceIdentity, batch: u32) -> ClusterInstance {
        ClusterInstance {
            identity,
            mesh: 0,
            batch,
            world_from_local: Mat4::IDENTITY,
            source_worldspace: 0,
        }
    }

    fn child_scene() -> (tempfile::TempDir, ClusterScene, Cell, u32) {
        let directory = published_for_loader();
        let certificate = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let (key, record) = certificate
            .records()
            .find(|(key, record)| key.worldspace != record.worldspace)
            .unwrap();
        let reference = certificate.reference(record.reference).unwrap();
        let cell = (
            reference.placed_worldspace_id,
            IVec2::new(reference.cell.0, reference.cell.1),
        );
        let parent = key.worldspace;
        let scene = ClusterScene {
            ready_revision: default(),
            residency_revision: default(),
            instances: vec![instance(ClusterInstanceIdentity::Object(*key), 0)],
            certificate: Some(certificate.clone()),
            inventory: None,
            terrain_blocks: Vec::new(),
            meshes: Vec::new(),
            origin_offset: Vec3::ZERO,
            blocks: Vec::new(),
        };
        (directory, scene, cell, parent)
    }

    #[test]
    fn certified_child_reference_uses_placement_instead_of_storage_block() {
        let (_directory, scene, cell, parent) = child_scene();
        let index = BatchCells::build(&scene, scene.certificate.as_deref());
        assert_ne!(cell.0, parent);
        assert_eq!(index.cells[&0], HashSet::from([cell]));
        assert!(index.unknown.is_empty());
    }

    #[test]
    fn shared_batch_unions_object_and_every_explicit_terrain_cell() {
        let (_directory, mut scene, cell, _) = child_scene();
        let additional = [(cell.0, cell.1 + IVec2::X), (cell.0 + 1, IVec2::Y)];
        for (worldspace_id, grid) in additional {
            let block = scene.terrain_blocks.len() as u32;
            scene.terrain_blocks.push(TerrainViewBlock {
                worldspace_id,
                anchor: IVec2::ZERO,
                side_cells: 1,
                cells: vec![grid],
                batch: Some(0),
            });
            scene.instances.push(instance(
                ClusterInstanceIdentity::Terrain {
                    block,
                    worldspace: 0,
                    tier: 0,
                    anchor: [0, 0],
                },
                0,
            ));
        }
        let index = BatchCells::build(&scene, scene.certificate.as_deref());
        assert_eq!(
            index.cells[&0],
            HashSet::from([cell, additional[0], additional[1]])
        );
        assert!(index.unknown.is_empty());
    }

    #[test]
    fn unrelated_same_world_coverage_stays_valid() {
        let cell = (12, IVec2::new(7, -9));
        let delta = CandidateWorlds {
            cells: HashSet::from([cell]),
            worlds: HashSet::from([cell.0]),
            ..default()
        };
        let effects = Effects::from_delta(&delta, &HashSet::new(), &BatchCells::default());
        assert!(!effects.affects(
            WorldId::exterior(cell.0),
            &[coverage((cell.0, cell.1 + IVec2::X))],
        ));
    }

    #[test]
    fn landing_dependency_intersection_invalidates_destination() {
        let dependency = (12, IVec2::new(7, -9));
        let delta = CandidateWorlds {
            cells: HashSet::from([dependency]),
            ..default()
        };
        let effects = Effects::from_delta(&delta, &HashSet::new(), &BatchCells::default());
        assert!(effects.affects(
            WorldId::exterior(dependency.0),
            &[
                coverage((dependency.0, dependency.1 + IVec2::X)),
                coverage(dependency),
            ],
        ));
    }

    #[test]
    fn material_and_template_normal_batch_changes_both_resolve_to_cells() {
        let cells = [(12, IVec2::X), (12, IVec2::Y)];
        let index = BatchCells {
            cells: HashMap::from([
                (0, HashSet::from([cells[0]])),
                (1, HashSet::from([cells[1]])),
            ]),
            ..default()
        };
        let delta = CandidateWorlds {
            batches: HashSet::from([1]),
            ..default()
        };
        let effects = Effects::from_delta(&delta, &HashSet::from([0]), &index);
        assert!(!effects.all);
        assert_eq!(effects.cells, HashSet::from(cells));
        for cell in cells {
            assert!(effects.affects(WorldId::exterior(cell.0), &[coverage(cell)]));
        }
    }

    #[test]
    fn light_global_world_change_invalidates_all_coverage_only_in_that_world() {
        let delta = CandidateWorlds {
            global_worlds: HashSet::from([12]),
            ..default()
        };
        let effects = Effects::from_delta(&delta, &HashSet::new(), &BatchCells::default());
        assert!(effects.affects(WorldId::exterior(12), &[coverage((12, IVec2::ZERO))]));
        assert!(!effects.affects(WorldId::exterior(13), &[]));
        assert_eq!(
            effects.changed_worlds(),
            HashSet::from([WorldId::exterior(12)])
        );
    }

    #[test]
    fn unknown_shared_member_or_unindexed_batch_fails_closed() {
        let (_directory, mut scene, _, _) = child_scene();
        scene
            .instances
            .push(instance(ClusterInstanceIdentity::Fixture, 0));
        let index = BatchCells::build(&scene, scene.certificate.as_deref());
        assert!(index.unknown.contains(&0));
        for batch in [0, 99] {
            let effects =
                Effects::from_delta(&CandidateWorlds::default(), &HashSet::from([batch]), &index);
            assert!(effects.all);
            assert!(effects.affects(WorldId::interior(1), &[]));
        }
    }

    #[test]
    fn missing_certificate_and_invalid_terrain_identity_are_unknown() {
        let (_directory, mut scene, _, _) = child_scene();
        scene.instances.push(instance(
            ClusterInstanceIdentity::Terrain {
                block: 99,
                worldspace: 12,
                tier: 0,
                anchor: [0, 0],
            },
            1,
        ));
        let index = BatchCells::build(&scene, None);
        assert_eq!(index.unknown, HashSet::from([0, 1]));
    }

    #[test]
    fn affected_world_with_empty_or_foreign_coverage_fails_closed() {
        let cell = (12, IVec2::ZERO);
        let effects = Effects {
            cells: HashSet::from([cell]),
            ..default()
        };
        let world = WorldId::exterior(cell.0);
        assert!(effects.affects(world, &[]));
        assert!(effects.affects(world, &[coverage((13, IVec2::X))]));
        assert!(effects.affects(world, &[CellKey::Interior(1)]));
        assert!(!effects.affects(WorldId::exterior(13), &[]));
    }

    #[test]
    fn legacy_world_union_is_only_an_unscoped_fallback() {
        let delta = CandidateWorlds {
            worlds: HashSet::from([12]),
            ..default()
        };
        let effects = Effects::from_delta(&delta, &HashSet::new(), &BatchCells::default());
        assert!(effects.worlds.contains(&WorldId::exterior(12)));
        let scoped = CandidateWorlds {
            cells: HashSet::from([(13, IVec2::ZERO)]),
            ..delta
        };
        let effects = Effects::from_delta(&scoped, &HashSet::new(), &BatchCells::default());
        assert!(effects.worlds.is_empty());
        assert_eq!(
            effects.changed_worlds(),
            HashSet::from([WorldId::exterior(13)])
        );
    }
}
