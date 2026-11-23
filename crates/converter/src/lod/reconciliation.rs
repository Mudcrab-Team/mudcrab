//! Structured LOD skip counts for the read-only retail-versus-pack reconciliation (#931).
//!
//! The converter already knows every block it did not import and why. The pipeline, the terrain
//! derive and the object derive write those counts into `lod-manifest.json` under
//! `reconciliation`, so `converter reconcile` can explain a retail block that the pack does not
//! hold without reading the conversion report. A pack without the block gets no invented
//! explanation: `converter reconcile` reports the count as unknown.
//!
//! Counts are per world file name (the lowercase editor id used in LOD file names), per block
//! level and per reason class. A terrain class also records how many skipped blocks had each of
//! the three source files, because one block can be skipped for a missing colour map while its
//! mesh and normal map exist. The file names follow
//! [the LOD compiler spec](../../../../docs/specs/converters/lod-compiler.md).

use super::retail_terrain_source::{BlockId, RetailTerrainFiles};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Format tag of the `reconciliation` block in `lod-manifest.json`.
pub(crate) const FORMAT: &str = "lod-reconciliation/2";
/// The reason class the importer uses for a level with no runtime tier. `converter reconcile`
/// derives this class from the retail level, so it works on packs that predate this block.
pub(crate) const NO_RUNTIME_TIER: &str = "no_runtime_tier";
/// `cells` mask bit: the `.btr` mesh existed when the block was skipped.
pub(crate) const MESH_BIT: u8 = 1;
/// `cells` mask bit: the colour map existed when the block was skipped.
pub(crate) const COLOUR_BIT: u8 = 1 << 1;
/// `cells` mask bit: the normal map existed when the block was skipped.
pub(crate) const NORMAL_BIT: u8 = 1 << 2;

/// What one skipped terrain block had, by retail file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SkipPresence {
    /// Blocks skipped with this class.
    pub blocks: u64,
    /// Of those, blocks whose `.btr` mesh existed.
    pub mesh: u64,
    /// Of those, blocks whose colour map existed.
    pub colour: u64,
    /// Of those, blocks whose normal map existed.
    pub normal: u64,
    /// South-west cell and file mask of each skipped block: `[x, y, mask]`, with
    /// [`MESH_BIT`], [`COLOUR_BIT`] and [`NORMAL_BIT`]. The mask is what the converter saw, so a
    /// file that appeared after the conversion is not explained by this record.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub cells: BTreeSet<(i32, i32, u8)>,
}

/// One object block class at one level: how many blocks, and which cells.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ObjectSkip {
    pub blocks: u64,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub cells: BTreeSet<(i32, i32)>,
}

impl SkipPresence {
    /// The presence of one block from the retail listing.
    pub(crate) fn of(files: &RetailTerrainFiles, id: &BlockId) -> Self {
        let Some(block) = files.blocks.get(id) else {
            return Self::default();
        };
        Self {
            blocks: 1,
            mesh: if block.mesh.is_empty() { 0 } else { 1 },
            colour: if block.colour.is_empty() { 0 } else { 1 },
            normal: if block.normal.is_empty() { 0 } else { 1 },
            cells: BTreeSet::new(),
        }
    }

    fn add(&mut self, other: Self) {
        self.blocks += other.blocks;
        self.mesh += other.mesh;
        self.colour += other.colour;
        self.normal += other.normal;
        self.cells.extend(other.cells);
    }

    /// Whether the per-cell masks agree with the aggregate counts. A hand-edited or truncated
    /// record is incomplete and must not explain anything.
    pub(crate) fn is_consistent(&self) -> bool {
        let cells = u64::try_from(self.cells.len()).unwrap_or(u64::MAX);
        if cells != self.blocks {
            return false;
        }
        let count = |bit: u8| {
            self.cells
                .iter()
                .filter(|(_, _, mask)| mask & bit != 0)
                .count()
        };
        u64::try_from(count(MESH_BIT)).unwrap_or(u64::MAX) == self.mesh
            && u64::try_from(count(COLOUR_BIT)).unwrap_or(u64::MAX) == self.colour
            && u64::try_from(count(NORMAL_BIT)).unwrap_or(u64::MAX) == self.normal
    }
}

/// Terrain skips of one level, by reason class.
pub(crate) type TerrainSkips = BTreeMap<String, BTreeMap<u32, BTreeMap<String, SkipPresence>>>;
/// Object skips of one level, by reason class.
pub(crate) type ObjectSkips = BTreeMap<String, BTreeMap<u32, BTreeMap<String, ObjectSkip>>>;

/// The `reconciliation` block of `lod-manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LodReconciliation {
    pub format: String,
    /// Whether the producer could not list the retail terrain files. The check then reports
    /// terrain counts as unknown instead of turning an empty listing into a pass.
    #[serde(default)]
    pub terrain_listing_failed: bool,
    /// World file names whose terrain skips this producer recorded. A world absent from the set
    /// was not inspected, so a missing block there is unknown, not zero.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub terrain_recorded_worlds: BTreeSet<String>,
    /// World file names whose object skips this producer recorded. A terrain-only derive
    /// preserves the prior set instead of claiming object skips it never inspected.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub objects_recorded_worlds: BTreeSet<String>,
    /// Terrain blocks the importer skipped, by world file name, level and class.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub terrain_skips: TerrainSkips,
    /// Object blocks the compiler skipped, by world file name, level and class.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub object_skips: ObjectSkips,
    /// Worlds with retail object blocks whose object LOD was skipped at the world level, with the
    /// reason class. The only class the object stage produces is `missing_atlas`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub object_world_skips: BTreeMap<String, String>,
}

impl Default for LodReconciliation {
    fn default() -> Self {
        Self::new()
    }
}

impl LodReconciliation {
    /// An empty block with the current format tag.
    pub(crate) fn new() -> Self {
        Self {
            format: FORMAT.to_owned(),
            terrain_listing_failed: false,
            terrain_recorded_worlds: BTreeSet::new(),
            objects_recorded_worlds: BTreeSet::new(),
            terrain_skips: BTreeMap::new(),
            object_skips: BTreeMap::new(),
            object_world_skips: BTreeMap::new(),
        }
    }

    /// Adds one skipped terrain block. `class` must be a class the importer reported.
    pub(crate) fn record_terrain_skip(
        &mut self,
        world: &str,
        level: u32,
        class: &str,
        cell: (i32, i32),
        mut presence: SkipPresence,
    ) {
        let mask = if presence.mesh > 0 { MESH_BIT } else { 0 }
            | if presence.colour > 0 { COLOUR_BIT } else { 0 }
            | if presence.normal > 0 { NORMAL_BIT } else { 0 };
        presence.cells.insert((cell.0, cell.1, mask));
        self.terrain_skips
            .entry(world.to_owned())
            .or_default()
            .entry(level)
            .or_default()
            .entry(class.to_owned())
            .or_default()
            .add(presence);
    }

    /// Records that this producer inspected the terrain LOD of `world`.
    pub(crate) fn record_terrain_world(&mut self, world: &str) {
        self.terrain_recorded_worlds.insert(world.to_owned());
    }

    /// Records that this producer inspected the object LOD of `world`.
    pub(crate) fn record_object_world(&mut self, world: &str) {
        self.objects_recorded_worlds.insert(world.to_owned());
    }

    /// Drops every terrain record and recorded world. A derive that re-reads the retail listing
    /// must not keep a stale explanation for a world it did not inspect.
    pub(crate) fn clear_terrain(&mut self) {
        self.terrain_skips.clear();
        self.terrain_recorded_worlds.clear();
        self.terrain_listing_failed = false;
    }

    /// Adds one skipped object block. `class` must be a class the compiler reported.
    pub(crate) fn record_object_skip(
        &mut self,
        world: &str,
        level: u32,
        class: &str,
        cell: (i32, i32),
    ) {
        let entry = self
            .object_skips
            .entry(world.to_owned())
            .or_default()
            .entry(level)
            .or_default()
            .entry(class.to_owned())
            .or_default();
        entry.blocks += 1;
        entry.cells.insert(cell);
    }

    /// Records a world whose object LOD was skipped at the world level.
    pub(crate) fn record_object_world_skip(&mut self, world: &str, class: &str) {
        self.object_world_skips
            .insert(world.to_owned(), class.to_owned());
    }

    /// Removes the object entries of one world, before a derive replaces them.
    pub(crate) fn remove_object_world(&mut self, world: &str) {
        self.object_skips.remove(world);
        self.object_world_skips.remove(world);
        self.objects_recorded_worlds.remove(world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_accumulate_per_world_level_and_class() {
        let mut counts = LodReconciliation::new();
        counts.record_terrain_skip(
            "w",
            32,
            NO_RUNTIME_TIER,
            (0, 0),
            SkipPresence {
                blocks: 1,
                mesh: 1,
                colour: 1,
                normal: 1,
                ..Default::default()
            },
        );
        counts.record_terrain_skip(
            "w",
            32,
            NO_RUNTIME_TIER,
            (4, 0),
            SkipPresence {
                blocks: 1,
                mesh: 1,
                colour: 0,
                normal: 0,
                ..Default::default()
            },
        );
        let skip = &counts.terrain_skips["w"][&32][NO_RUNTIME_TIER];
        assert_eq!(skip.blocks, 2);
        assert_eq!(skip.colour, 1);
        assert_eq!(skip.cells.len(), 2);
        assert!(skip.is_consistent());
        assert!(
            skip.cells
                .contains(&(0, 0, MESH_BIT | COLOUR_BIT | NORMAL_BIT))
        );
        assert!(skip.cells.contains(&(4, 0, MESH_BIT)));
    }

    #[test]
    fn a_truncated_cell_mask_is_inconsistent() {
        let mut skip = SkipPresence {
            blocks: 2,
            mesh: 2,
            colour: 0,
            normal: 0,
            ..Default::default()
        };
        skip.cells.insert((0, 0, MESH_BIT));
        assert!(!skip.is_consistent());
    }

    #[test]
    fn object_blocks_and_world_skips_are_separate() {
        let mut counts = LodReconciliation::new();
        counts.record_object_skip("w", 16, "block cannot be read", (0, 0));
        counts.record_object_world_skip("v", "missing_atlas");
        assert_eq!(
            counts.object_skips["w"][&16]["block cannot be read"].blocks,
            1
        );
        assert_eq!(counts.object_world_skips["v"], "missing_atlas");
        assert_eq!(counts.format, FORMAT);
    }

    #[test]
    fn a_removed_world_leaves_no_stale_counts_or_recording() {
        let mut counts = LodReconciliation::new();
        counts.record_terrain_skip(
            "w",
            4,
            "missing_colour_map",
            (0, 0),
            SkipPresence::default(),
        );
        counts.record_object_skip("w", 16, "block cannot be read", (0, 0));
        counts.record_terrain_world("w");
        counts.record_object_world("w");
        counts.remove_object_world("w");
        assert!(!counts.terrain_skips.is_empty());
        assert!(counts.object_skips.is_empty());
        assert!(!counts.terrain_recorded_worlds.is_empty());
        assert!(counts.objects_recorded_worlds.is_empty());
    }

    #[test]
    fn clearing_terrain_drops_every_world_record() {
        let mut counts = LodReconciliation::new();
        counts.record_terrain_world("w");
        counts.record_terrain_skip("w", 4, "missing_mesh", (0, 0), SkipPresence::default());
        counts.record_object_world("w");
        counts.clear_terrain();
        assert!(counts.terrain_skips.is_empty());
        assert!(counts.terrain_recorded_worlds.is_empty());
        assert_eq!(counts.objects_recorded_worlds.len(), 1);
    }
}
