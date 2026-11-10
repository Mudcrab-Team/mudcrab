//! Immutable mapped grass masks shared by asynchronous placement workers.
use bevy::prelude::Resource;
use color_eyre::{Result, eyre::WrapErr};
use memmap2::Mmap;
use rkyv::rancor::Error;
use shared::grass::{
    ArchivedGrassCache, ArchivedGrassCell, ArchivedGrassType, GRASS_CACHE_VERSION,
};
use std::{collections::HashMap, fs::File, path::Path, sync::Arc};

#[derive(Resource, Clone)]
pub struct GrassCache(Arc<MappedGrass>);

struct MappedGrass {
    mmap: Mmap,
    cells: HashMap<u32, usize>,
    grids: HashMap<(u32, i32, i32), u32>,
    types: HashMap<u32, usize>,
}

impl GrassCache {
    /// Validate the archive once; terrain arrays remain mapped rather than copied.
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).wrap_err_with(|| format!("failed to open {}", path.display()))?;
        // SAFETY: converted cache files are immutable for the lifetime of a running engine.
        let mmap = unsafe { Mmap::map(&file) }.wrap_err("failed to map grass cache")?;
        let archive = rkyv::access::<ArchivedGrassCache, Error>(&mmap)
            .wrap_err_with(|| format!("invalid grass cache {}", path.display()))?;
        color_eyre::eyre::ensure!(
            archive.version == GRASS_CACHE_VERSION,
            "unsupported grass cache version {}; reconvert grass masks for version {}",
            archive.version,
            GRASS_CACHE_VERSION
        );
        shared::grass::validate_archived_cache(archive).map_err(|reason| {
            color_eyre::eyre::eyre!("invalid grass cache {}: {reason}", path.display())
        })?;
        let cells = archive
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| (cell.cell_id.into(), index))
            .collect();
        let grids = archive
            .cells
            .iter()
            .map(|cell| {
                (
                    (
                        cell.worldspace_id.into(),
                        cell.grid_x.into(),
                        cell.grid_y.into(),
                    ),
                    cell.cell_id.into(),
                )
            })
            .collect();
        let types = archive
            .types
            .iter()
            .enumerate()
            .map(|(index, species)| (species.form_id.into(), index))
            .collect();
        Ok(Self(Arc::new(MappedGrass {
            mmap,
            cells,
            grids,
            types,
        })))
    }

    /// Borrow the validated archive while retaining its shared backing mapping.
    fn archive(&self) -> &ArchivedGrassCache {
        // SAFETY: open validated these immutable bytes, and Arc keeps the mapping alive
        // until every worker holding a clone has completed or been cancelled.
        unsafe { rkyv::access_unchecked::<ArchivedGrassCache>(&self.0.mmap) }
    }

    /// Borrow a cell directly from the archive while this mapping is alive.
    pub fn cell(&self, id: u32) -> Option<&ArchivedGrassCell> {
        self.archive().cells.get(*self.0.cells.get(&id)?)
    }

    /// Resolve a directional prefetch without a database query.
    pub fn cell_at(&self, world: u32, x: i32, y: i32) -> Option<u32> {
        self.0.grids.get(&(world, x, y)).copied()
    }

    /// Borrow the winning converted GRAS definition.
    pub fn species(&self, id: u32) -> Option<&ArchivedGrassType> {
        self.archive().types.get(*self.0.types.get(&id)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::grass::{GRASS_EXCLUSION_SIDE, GrassCell, GrassQuadrant};

    /// Build a valid owned cache fixture with one terrain cell and species rule.
    fn source() -> shared::grass::GrassCache {
        let cell = |id, x, z| GrassCell {
            cell_id: id,
            worldspace_id: 3,
            grid_x: x,
            grid_y: -8,
            input_hash: [0; 32],
            obstruction_failures: vec![],
            expanded_sampling: 0,
            heights: vec![z; 33 * 33],
            water_height: None,
            quadrants: (0..4)
                .map(|quadrant| GrassQuadrant {
                    quadrant,
                    layers: vec![],
                    exclusion: vec![0; GRASS_EXCLUSION_SIDE * GRASS_EXCLUSION_SIDE / 8],
                })
                .collect(),
        };
        shared::grass::GrassCache {
            version: GRASS_CACHE_VERSION,
            types: vec![],
            cells: vec![cell(7, -9, 101.0), cell(42, 11, 205.0)],
        }
    }

    #[test]
    /// Verify archive reads are shared across worker lifetimes without terrain copy.
    fn archive_reads_are_shared_across_worker_lifetimes_without_terrain_copy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grass_cache.rkyv");
        std::fs::write(&path, rkyv::to_bytes::<Error>(&source()).unwrap()).unwrap();
        let cache = GrassCache::open(&path).unwrap();
        assert_eq!(cache.cell_at(3, -9, -8), Some(7));
        assert_eq!(cache.cell_at(4, -9, -8), None);
        let worker = cache.clone();
        let pointer = cache.cell(42).unwrap().heights.as_ptr() as usize;
        assert_eq!(pointer, worker.cell(42).unwrap().heights.as_ptr() as usize);
        drop(cache);
        std::thread::spawn(move || {
            assert_eq!(f32::from(worker.cell(42).unwrap().heights[0]), 205.0);
            assert_eq!(f32::from(worker.cell(7).unwrap().heights[0]), 101.0);
            assert!(worker.cell(100).is_none());
        })
        .join()
        .unwrap();
    }

    #[test]
    /// Accept marker-filtered masks and reject both pre-order and pre-marker producers.
    fn accepts_marker_filtered_masks_and_rejects_stale_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grass_cache.rkyv");
        let mut data = source();
        data.version = GRASS_CACHE_VERSION;
        std::fs::write(&path, rkyv::to_bytes::<Error>(&data).unwrap()).unwrap();
        {
            let cache = GrassCache::open(&path).unwrap();
            assert_eq!(cache.cell_at(3, -9, -8), Some(7));
            assert_eq!(f32::from(cache.cell(42).unwrap().heights[0]), 205.0);
        }
        for version in [5, 6, 7] {
            data.version = version;
            std::fs::write(&path, rkyv::to_bytes::<Error>(&data).unwrap()).unwrap();
            let error = GrassCache::open(&path).err().unwrap().to_string();
            assert!(
                error.contains(&format!(
                    "unsupported grass cache version {version}; reconvert grass masks for version {GRASS_CACHE_VERSION}"
                )),
                "{error}"
            );
        }
    }

    #[test]
    /// Verify rejects invalid versions truncation and semantically short grids.
    fn rejects_invalid_versions_truncation_and_semantically_short_grids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grass_cache.rkyv");
        std::fs::write(&path, [0; 7]).unwrap();
        assert!(GrassCache::open(&path).is_err());
        let mut data = source();
        data.version = 0;
        std::fs::write(&path, rkyv::to_bytes::<Error>(&data).unwrap()).unwrap();
        assert!(GrassCache::open(&path).is_err());
        data.version = GRASS_CACHE_VERSION;
        data.cells[0].heights.pop();
        std::fs::write(&path, rkyv::to_bytes::<Error>(&data).unwrap()).unwrap();
        assert!(GrassCache::open(&path).is_err());
    }
}
