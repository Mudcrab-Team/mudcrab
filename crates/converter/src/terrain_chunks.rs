//! Keys merged terrain cell meshes (#185 derived data) and writes them into terrain chunk
//! files (#215).
//!
//! One file holds every cell mesh of one LOD-chunk group of one worldspace, at the path
//! [`shared::terrain_mesh::terrain_chunk_path`] gives. Writing is the second half of the terrain
//! merge: `esm::terrain_mesh::build_cell_mesh` builds one cell's mesh, [`cell_mesh_key`] stamps
//! it with its derived-data key, and [`write_terrain_chunks`] groups, checks and writes them.
//! Nothing here decides which cells a conversion builds or finds the records to key them with;
//! pipeline wiring does that later.

use crate::esm::records::RawRecord;
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use rkyv::rancor::Error;
use sha2::{Digest, Sha256};
use shared::terrain_mesh::{
    DEFAULT_LAND_TEXTURE_FORM_ID, TERRAIN_CHUNK_SIDE_CELLS, TERRAIN_MESH_FORMAT_VERSION,
    TerrainCellMesh, TerrainChunkFile, terrain_chunk_for_cell, terrain_chunk_path,
};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// The #185 derived-data key of one exterior cell's merged terrain mesh.
///
/// SHA-256 over, in this order, with every field length-prefixed as a little-endian `u64` so no
/// two different inputs share a byte string:
///
/// 1. [`TERRAIN_MESH_FORMAT_VERSION`] as a little-endian `u32`: a mesh layout change rebuilds
///    every cell.
/// 2. The winning LAND record: its 4 record-type bytes, its FormID (load-order numbering), its
///    header flags, the cell and worldspace it sits in (a presence byte, then the FormID), and
///    each subrecord (its 4 tag bytes, then its data), each length-prefixed; the subrecords are
///    hashed in the order the merged record stores them.
/// 3. `land_masters`, the master list of the plugin that declared that LAND record: one
///    length-prefixed entry per name, with the count first. A rename-or-remove changes the
///    FormIDs the record's own references mean, so it must change the key.
/// 4. Each LTEX or TXST record the cell uses, sorted by FormID (ties broken by record type,
///    flags, subrecords and master list, so the caller's order never matters), each with its master
///    list, serialized exactly as 2 and 3 are; the count comes first, and a cell that uses no
///    texture records hashes nothing here.
/// 5. The LTEX FormIDs the LAND names that the land texture table has no entry for
///    (`skipped_ltex`), sorted, with the count first: the mesh leaves those layers out, so the
///    key must change when a texture that failed to convert in one run converts in the next.
///
/// The load-order slot is not hashed (it is not part of what the mesh is built from; the master
/// list stands for the plugin's context). The caller passes every LTEX the LAND names, present
/// or not in the load order, and the TXST each uses: a layer whose LTEX is missing is skipped
/// by `build_cell_mesh`, so the key must see that it is missing. Deleted records never reach
/// here (the merger removes them); removing a whole cell's derived file is the pipeline's job.
/// The caller passes `land_masters` — after merging, that is
/// `LoadOrder::metadata[land.load_order as usize].masters`, the plugin the winning record came
/// from (`RawRecord::load_order` is its priority slot).
pub fn cell_mesh_key(
    land: &RawRecord,
    land_masters: &[String],
    textures: &[(&RawRecord, &[String])],
    skipped_ltex: &[u32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TERRAIN_MESH_FORMAT_VERSION.to_le_bytes());
    put_record(&mut hasher, land, land_masters);
    let mut textures: Vec<&(&RawRecord, &[String])> = textures.iter().collect();
    textures.sort_by(|left, right| {
        left.0
            .form_id
            .cmp(&right.0.form_id)
            .then_with(|| left.0.record_type.cmp(&right.0.record_type))
            .then_with(|| left.0.flags.cmp(&right.0.flags))
            .then_with(|| left.0.subrecords.cmp(&right.0.subrecords))
            .then_with(|| left.1.cmp(right.1))
    });
    put_count(&mut hasher, textures.len());
    for (record, masters) in textures {
        put_record(&mut hasher, record, masters);
    }
    // The LTEX records the LAND names that have no entry in the land texture table: the mesh
    // skips those layers, so a texture that fails to convert in one run and converts in the next
    // changes the mesh without changing any record. Sorted, so the order never matters.
    let mut skipped: Vec<u32> = skipped_ltex.to_vec();
    skipped.sort_unstable();
    skipped.dedup();
    put_count(&mut hasher, skipped.len());
    for form_id in skipped {
        hasher.update(form_id.to_le_bytes());
    }
    hasher.finalize().into()
}

/// Writes `bytes` with a length prefix, so `"ab"` + `"c"` and `"a"` + `"bc"` cannot collide.
fn put_field(hasher: &mut Sha256, bytes: &[u8]) {
    put_count(hasher, bytes.len());
    hasher.update(bytes);
}

/// Writes how many items follow.
fn put_count(hasher: &mut Sha256, count: usize) {
    hasher.update((count as u64).to_le_bytes());
}

/// Hashes one record the way [`cell_mesh_key`] documents: type, FormID, header flags, the
/// cell and worldspace it sits in, subrecords, masters. A mod that changes only a header flag
/// or moves a record still changes the key.
fn put_record(hasher: &mut Sha256, record: &RawRecord, masters: &[String]) {
    put_field(hasher, &record.record_type);
    hasher.update(record.form_id.to_le_bytes());
    hasher.update(record.flags.to_le_bytes());
    for placement in [record.cell_form_id, record.worldspace_form_id] {
        match placement {
            Some(form_id) => {
                hasher.update([1]);
                hasher.update(form_id.to_le_bytes());
            }
            None => hasher.update([0]),
        }
    }
    put_count(hasher, record.subrecords.len());
    for (tag, data) in &record.subrecords {
        put_field(hasher, tag);
        put_field(hasher, data);
    }
    put_count(hasher, masters.len());
    for master in masters {
        put_field(hasher, master.as_bytes());
    }
}

/// One cell mesh with the placement the chunk file needs.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedCellMesh {
    /// The WRLD FormID (load-order numbering) the cell belongs to.
    pub worldspace_id: u32,
    /// The cell's grid position in its worldspace; negative west or south of the origin.
    pub grid_x: i32,
    pub grid_y: i32,
    pub mesh: TerrainCellMesh,
}

/// One worldspace's share of a [`ChunkWriteReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldspaceChunkWrite {
    pub worldspace_id: u32,
    /// Chunk files written for this worldspace.
    pub files: usize,
    /// Cell meshes written across those files.
    pub cells: usize,
}

/// What [`write_terrain_chunks`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkWriteReport {
    /// One entry per worldspace, by worldspace id.
    pub worldspaces: Vec<WorldspaceChunkWrite>,
    /// Cell meshes written, across all files.
    pub cells: usize,
    /// Bytes written, across all files.
    pub bytes: u64,
}

/// Writes `cells` as terrain chunk files under `output_root`, one file per
/// [`terrain_chunk_path`] group, and reports what went where.
///
/// Each file holds [`TerrainChunkFile`] with [`TERRAIN_MESH_FORMAT_VERSION`], its group's
/// worldspace id and its cells sorted by `cell_id`, so the same cells always produce the same
/// bytes. Every mesh is checked with [`TerrainCellMesh::validate`] against the textures its own
/// slots name (a slot the game-wide table lacks is the engine's check at load, not this one),
/// and a `cell_id` written twice for one worldspace is an error naming it. Each file is written
/// to `<path>.tmp` and renamed over its target, so a reader never sees a half-written chunk.
pub fn write_terrain_chunks(
    output_root: &Path,
    cells: Vec<PlacedCellMesh>,
) -> Result<ChunkWriteReport> {
    let mut by_chunk: BTreeMap<(u32, i32, i32), Vec<TerrainCellMesh>> = BTreeMap::new();
    let mut seen: HashMap<(u32, u32), (i32, i32)> = HashMap::new();
    for cell in cells {
        let mesh = cell.mesh;
        if let Some(first) = seen.insert(
            (cell.worldspace_id, mesh.cell_id),
            (cell.grid_x, cell.grid_y),
        ) {
            return Err(eyre!(
                "cell {:08X} of worldspace {:08X} is written twice (grid {},{} and grid {},{}); one cell has one merged mesh",
                mesh.cell_id,
                cell.worldspace_id,
                first.0,
                first.1,
                cell.grid_x,
                cell.grid_y
            ));
        }
        // Every slot texture is known by construction; the map only lets the shared check run,
        // as `build_cell_mesh` does. The game-wide land-texture table is checked at load.
        let ids: HashMap<u32, u16> = mesh
            .quadrant_slots
            .iter()
            .map(|&ltex| (ltex, 0))
            .chain(std::iter::once((DEFAULT_LAND_TEXTURE_FORM_ID, 0)))
            .collect();
        mesh.validate(&ids)
            .map_err(|error| eyre!("terrain mesh of cell {:08X}: {error}", mesh.cell_id))?;
        let (chunk_x, chunk_y) = terrain_chunk_for_cell(cell.grid_x, cell.grid_y);
        by_chunk
            .entry((cell.worldspace_id, chunk_x, chunk_y))
            .or_default()
            .push(mesh);
    }

    let mut report = ChunkWriteReport {
        worldspaces: Vec::new(),
        cells: 0,
        bytes: 0,
    };
    for ((worldspace_id, chunk_x, chunk_y), mut chunk_cells) in by_chunk {
        chunk_cells.sort_unstable_by_key(|mesh| mesh.cell_id);
        let cell_count = chunk_cells.len();
        // The path is derived from a cell of the chunk's south-west corner, the one coordinate
        // `terrain_chunk_path` maps back to this chunk.
        let relative = terrain_chunk_path(
            worldspace_id,
            chunk_x * TERRAIN_CHUNK_SIDE_CELLS,
            chunk_y * TERRAIN_CHUNK_SIDE_CELLS,
        );
        let path = output_root.join(&relative);
        let bytes = rkyv::to_bytes::<Error>(&TerrainChunkFile {
            format_version: TERRAIN_MESH_FORMAT_VERSION,
            worldspace_id,
            cells: chunk_cells,
        })
        .wrap_err_with(|| format!("failed to serialize terrain chunk {relative}"))?;
        write_atomically(&path, &bytes)
            .wrap_err_with(|| format!("failed to write terrain chunk {relative}"))?;
        match report
            .worldspaces
            .last_mut()
            .filter(|entry| entry.worldspace_id == worldspace_id)
        {
            Some(entry) => {
                entry.files += 1;
                entry.cells += cell_count;
            }
            None => report.worldspaces.push(WorldspaceChunkWrite {
                worldspace_id,
                files: 1,
                cells: cell_count,
            }),
        }
        report.cells += cell_count;
        report.bytes += bytes.len() as u64;
    }
    Ok(report)
}

/// Reads one terrain chunk file, validating the archive as it reads (rkyv bytecheck) and
/// refusing a [`TERRAIN_MESH_FORMAT_VERSION`] it does not know.
pub fn read_terrain_chunk(path: &Path) -> Result<TerrainChunkFile> {
    let file = fs::read(path)
        .wrap_err_with(|| format!("failed to read terrain chunk {}", path.display()))?;
    // rkyv needs an aligned buffer; a plain Vec<u8> is only checked in debug builds.
    let mut bytes = rkyv::util::AlignedVec::<16>::with_capacity(file.len());
    bytes.extend_from_slice(&file);
    let archived = rkyv::access::<shared::terrain_mesh::ArchivedTerrainChunkFile, Error>(&bytes)
        .wrap_err_with(|| format!("invalid terrain chunk {}", path.display()))?;
    let chunk = rkyv::deserialize::<TerrainChunkFile, Error>(archived)
        .wrap_err_with(|| format!("invalid terrain chunk {}", path.display()))?;
    if chunk.format_version != TERRAIN_MESH_FORMAT_VERSION {
        return Err(eyre!(
            "terrain chunk {} has format version {}, expected {}",
            path.display(),
            chunk.format_version,
            TERRAIN_MESH_FORMAT_VERSION
        ));
    }
    Ok(chunk)
}

/// Writes `bytes` to `<path>.tmp` and renames it over `path`, so a reader only ever sees the old
/// file or the new one; a failed write leaves no temporary file behind.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    let mut staged = path.as_os_str().to_owned();
    staged.push(".tmp");
    let staged = PathBuf::from(staged);
    let write = (|| -> Result<()> {
        let mut file = fs::File::create(&staged)
            .wrap_err_with(|| format!("failed to create {}", staged.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    if let Err(error) = fs::rename(&staged, path) {
        let _ = fs::remove_file(&staged);
        return Err(error).wrap_err_with(|| {
            format!(
                "failed to move {} over {}",
                staged.display(),
                path.display()
            )
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::terrain_mesh::build_cell_mesh;
    use shared::{LAND_SIDE, terrain_mesh::QUADRANT_VERTICES};
    use std::collections::HashSet;

    const TAMRIEL: u32 = 0x0000_003C;
    const SOLSTHEIM: u32 = 0x0001_4000;

    /// A record whose only content is one subrecord.
    fn record(record_type: [u8; 4], form_id: u32, tag: &[u8], data: &[u8]) -> RawRecord {
        RawRecord {
            form_id,
            record_type,
            flags: 0,
            subrecords: vec![(tag.to_vec(), data.to_vec())],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        }
    }

    fn ltex(form_id: u32) -> RawRecord {
        record(
            *b"LTEX",
            form_id,
            b"ICON",
            b"textures\\landscape\\fieldgrass01.dds",
        )
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    /// A VHGT whose height is `16 * (y + 1) + 8 * x` at grid (x, y), and a VNML to match.
    fn vhgt_and_vnml() -> (Vec<u8>, Vec<u8>) {
        let side = usize::from(LAND_SIDE);
        let mut vhgt = 0.0f32.to_le_bytes().to_vec();
        for _row in 0..side {
            vhgt.push(2); // row origin step: +16 per row
            vhgt.extend(std::iter::repeat_n(1u8, side - 1)); // +8 per column
        }
        vhgt.extend([0; 3]);
        let mut vnml = Vec::new();
        for y in 0..side {
            for x in 0..side {
                vnml.extend([(x + 1) as u8, (y + 1) as u8, 100]);
            }
        }
        (vhgt, vnml)
    }

    /// A synthetic LAND record for `cell`, with one BTXT per quadrant naming `0x100 + quadrant`.
    fn land(cell: u32) -> RawRecord {
        let (vhgt, vnml) = vhgt_and_vnml();
        let mut subrecords = vec![(b"VHGT".to_vec(), vhgt), (b"VNML".to_vec(), vnml)];
        for quadrant in 0u8..4 {
            let mut data = (0x100 + u32::from(quadrant)).to_le_bytes().to_vec();
            data.extend([quadrant, 0]);
            data.extend(0u16.to_le_bytes());
            subrecords.push((b"BTXT".to_vec(), data));
        }
        RawRecord {
            form_id: cell,
            record_type: *b"LAND",
            flags: 0,
            subrecords,
            cell_form_id: Some(cell),
            worldspace_form_id: None,
            load_order: 0,
        }
    }

    /// The mesh [`land`] builds, keyed by [`cell_mesh_key`].
    fn placed(worldspace_id: u32, grid_x: i32, grid_y: i32, cell_id: u32) -> PlacedCellMesh {
        let record = land(cell_id);
        let known: HashSet<u32> = (0x100..0x104).collect();
        let key = cell_mesh_key(&record, &names(&["Skyrim.esm"]), &[], &[]);
        let (mesh, _) = build_cell_mesh(cell_id, &record, &known, key).unwrap();
        PlacedCellMesh {
            worldspace_id,
            grid_x,
            grid_y,
            mesh,
        }
    }

    #[test]
    fn the_same_inputs_give_the_same_key() {
        let land = land(0x1234);
        let ltex = ltex(0x100);
        let masters = names(&["Skyrim.esm"]);
        let textures = [(&ltex, masters.as_slice())];
        assert_eq!(
            cell_mesh_key(&land, &masters, &textures, &[]),
            cell_mesh_key(&land, &masters, &textures, &[])
        );
        let encoded = cell_mesh_key(&land, &masters, &textures, &[]);
        assert_eq!(encoded.len(), 32, "the key is the raw digest");
    }

    #[test]
    fn every_hashed_input_changes_the_key_but_the_texture_order_does_not() {
        let land = land(0x1234);
        let masters = names(&["Skyrim.esm"]);
        let ltex_a = ltex(0x100);
        let ltex_b = ltex(0x101);
        let txst = record(*b"TXST", 0x200, b"TX00", b"textures\\landscape\\x.dds");
        let textures = [(&ltex_b, masters.as_slice()), (&ltex_a, masters.as_slice())];
        let reference = cell_mesh_key(&land, &masters, &textures, &[]);
        let reordered = [(&ltex_a, masters.as_slice()), (&ltex_b, masters.as_slice())];
        assert_eq!(
            reference,
            cell_mesh_key(&land, &masters, &reordered, &[]),
            "the textures are sorted internally, so the caller's order must not matter"
        );

        let mut edited_land = land.clone();
        edited_land.subrecords[0].1[0] ^= 0x01;
        assert_ne!(
            reference,
            cell_mesh_key(&edited_land, &masters, &textures, &[])
        );

        assert_ne!(
            reference,
            cell_mesh_key(&land, &names(&["Skyrim.esm", "Update.esm"]), &textures, &[]),
            "a master list change moves every FormID the record means"
        );

        let mut edited_ltex = ltex(0x101);
        edited_ltex.subrecords[0].1.push(b'.');
        let edited = [
            (&ltex_b, masters.as_slice()),
            (&edited_ltex, masters.as_slice()),
        ];
        assert_ne!(reference, cell_mesh_key(&land, &masters, &edited, &[]));

        let with_txst = [
            (&ltex_b, masters.as_slice()),
            (&ltex_a, masters.as_slice()),
            (&txst, masters.as_slice()),
        ];
        assert_ne!(
            reference,
            cell_mesh_key(&land, &masters, &with_txst, &[]),
            "a texture set change rebuilds the cell"
        );

        let mut edited_txst = txst.clone();
        edited_txst.subrecords[0].1[0] ^= 0x01;
        let with_edited_txst = [
            (&ltex_b, masters.as_slice()),
            (&ltex_a, masters.as_slice()),
            (&edited_txst, masters.as_slice()),
        ];
        assert_ne!(
            cell_mesh_key(&land, &masters, &with_txst, &[]),
            cell_mesh_key(&land, &masters, &with_edited_txst, &[]),
            "an edit to an existing texture set's payload rebuilds the cell"
        );

        let mut flagged_land = land.clone();
        flagged_land.flags ^= 0x0004_0000;
        assert_ne!(
            reference,
            cell_mesh_key(&flagged_land, &masters, &textures, &[]),
            "a header flag change on the LAND record changes the key"
        );

        let mut flagged_ltex = ltex(0x101);
        flagged_ltex.flags ^= 0x0000_0800;
        let with_flagged_ltex = [
            (&ltex_a, masters.as_slice()),
            (&flagged_ltex, masters.as_slice()),
        ];
        assert_ne!(
            reference,
            cell_mesh_key(&land, &masters, &with_flagged_ltex, &[]),
            "a header flag change on a texture record changes the key"
        );

        let mut moved_land = land.clone();
        moved_land.cell_form_id = Some(moved_land.cell_form_id.unwrap_or(0) ^ 0x10);
        assert_ne!(
            reference,
            cell_mesh_key(&moved_land, &masters, &textures, &[]),
            "a LAND moved to another cell changes the key"
        );
    }

    #[test]
    fn fields_are_length_prefixed_so_boundaries_cannot_shift() {
        let land = land(0x1234);
        let split_one = cell_mesh_key(&land, &names(&["ab", "c"]), &[], &[]);
        let split_two = cell_mesh_key(&land, &names(&["a", "bc"]), &[], &[]);
        assert_ne!(split_one, split_two);

        let left = record(*b"LTEX", 0x100, b"ICON", b"abcdef");
        let right = record(*b"LTEX", 0x100, b"ICON", b"abc");
        let masters = names(&["def"]);
        let split = [(&right, masters.as_slice())];
        let whole = [(&left, masters.as_slice())];
        assert_ne!(
            cell_mesh_key(&land, &names(&["Skyrim.esm"]), &split, &[]),
            cell_mesh_key(&land, &names(&["Skyrim.esm"]), &whole, &[]),
            "a subrecord's data and the next field's bytes cannot shift into each other"
        );
    }

    #[test]
    fn chunks_land_at_their_paths_with_negative_grids_and_read_back_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cells = vec![
            placed(TAMRIEL, 5, -1, 0x1002),
            placed(TAMRIEL, 0, 3, 0x1001),
            placed(TAMRIEL, 0, 0, 0x1000),
            placed(TAMRIEL, -4, -5, 0x1003),
            placed(SOLSTHEIM, -1, 2, 0x2001),
        ];
        let report = write_terrain_chunks(root, cells).unwrap();
        assert_eq!(report.cells, 5);
        assert_eq!(
            report.worldspaces,
            vec![
                WorldspaceChunkWrite {
                    worldspace_id: TAMRIEL,
                    files: 3,
                    cells: 4,
                },
                WorldspaceChunkWrite {
                    worldspace_id: SOLSTHEIM,
                    files: 1,
                    cells: 1,
                },
            ]
        );
        let on_disk: u64 = [
            (TAMRIEL, 5, -1),
            (TAMRIEL, 0, 3),
            (TAMRIEL, -4, -5),
            (SOLSTHEIM, -1, 2),
        ]
        .into_iter()
        .map(|(worldspace, x, y)| {
            root.join(terrain_chunk_path(worldspace, x, y))
                .metadata()
                .unwrap()
                .len()
        })
        .sum();
        assert_eq!(report.bytes, on_disk, "every byte written is counted");

        let chunk = read_terrain_chunk(&root.join("terrain/0000003c/chunk_0_0.rkyv")).unwrap();
        assert_eq!(chunk.format_version, TERRAIN_MESH_FORMAT_VERSION);
        assert_eq!(chunk.worldspace_id, TAMRIEL);
        assert_eq!(
            chunk
                .cells
                .iter()
                .map(|mesh| mesh.cell_id)
                .collect::<Vec<_>>(),
            vec![0x1000, 0x1001],
            "cells are written sorted by cell_id, not in the order they were given"
        );
        assert_eq!(
            chunk.cells[0].heights.len(),
            shared::terrain_mesh::CELL_MESH_VERTICES
        );
        assert!(
            chunk.cells[0].key != [0; 32],
            "the key travels in the file for the load-time check"
        );

        let west = read_terrain_chunk(&root.join("terrain/0000003c/chunk_-1_-2.rkyv")).unwrap();
        assert_eq!(west.cells.len(), 1);
        assert_eq!(west.cells[0].cell_id, 0x1003);
        assert!(!root.join("terrain/0000003c/chunk_0_-1.rkyv").exists());
        assert!(!root.join("terrain/0000003c/chunk_-1_-1.rkyv").exists());
        let other = read_terrain_chunk(&root.join("terrain/00014000/chunk_-1_0.rkyv")).unwrap();
        assert_eq!(other.worldspace_id, SOLSTHEIM);
        assert_eq!(other.cells[0].cell_id, 0x2001);
        assert!(
            !root.join("terrain/0000003c/chunk_-1_0.rkyv").exists(),
            "a worldspace's cells never land in another worldspace's file"
        );
    }

    #[test]
    fn round_tripped_meshes_equal_the_ones_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut placed = placed(TAMRIEL, -1, 2, 0x1000);
        let key = cell_mesh_key(&land(0x1000), &names(&["Update.esm"]), &[], &[]);
        placed.mesh.key = key;
        let expected = placed.mesh.clone();
        write_terrain_chunks(root, vec![placed]).unwrap();
        let chunk = read_terrain_chunk(&root.join(terrain_chunk_path(TAMRIEL, -1, 2))).unwrap();
        assert_eq!(chunk.cells[0], expected);
        assert_eq!(chunk.cells[0].key, key);
    }

    #[test]
    fn a_cell_id_written_twice_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let cells = vec![placed(TAMRIEL, 0, 0, 0x1234), placed(TAMRIEL, 3, 3, 0x1234)];
        let error = write_terrain_chunks(dir.path(), cells).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("00001234"), "{message}");
        assert!(message.contains("0000003C"), "{message}");
        assert!(
            !dir.path().join("terrain").exists(),
            "nothing is written for a rejected set"
        );

        // The same id in another worldspace is a different cell.
        let cells = vec![
            placed(TAMRIEL, 0, 0, 0x1234),
            placed(SOLSTHEIM, 0, 0, 0x1234),
        ];
        assert_eq!(write_terrain_chunks(dir.path(), cells).unwrap().cells, 2);
    }

    #[test]
    fn a_corrupted_or_unknown_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_terrain_chunks(root, vec![placed(TAMRIEL, 0, 0, 0x1000)]).unwrap();
        let path = root.join(terrain_chunk_path(TAMRIEL, 0, 0));
        let good = fs::read(&path).unwrap();

        fs::write(&path, b"not an archive at all").unwrap();
        let error = read_terrain_chunk(&path).unwrap_err();
        assert!(
            format!("{error:#}").contains("chunk_0_0.rkyv"),
            "the error names the file"
        );

        fs::write(&path, &good[..good.len() / 2]).unwrap();
        assert!(read_terrain_chunk(&path).is_err(), "a truncated file fails");

        let wrong_version = rkyv::to_bytes::<Error>(&TerrainChunkFile {
            format_version: TERRAIN_MESH_FORMAT_VERSION + 1,
            worldspace_id: TAMRIEL,
            cells: Vec::new(),
        })
        .unwrap();
        fs::write(&path, &wrong_version).unwrap();
        let error = read_terrain_chunk(&path).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format version"), "{message}");
        assert!(
            message.contains(&(TERRAIN_MESH_FORMAT_VERSION + 1).to_string()),
            "{message}"
        );
    }

    #[test]
    fn a_broken_mesh_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut placed = placed(TAMRIEL, 0, 0, 0x1000);
        placed.mesh.heights[0] = f32::NAN;
        let error = write_terrain_chunks(dir.path(), vec![placed]).unwrap_err();
        assert!(format!("{error:#}").contains("00001000"), "{error:#}");
        assert!(!dir.path().join("terrain").exists());
    }

    #[test]
    fn a_quadrant_over_eight_slots_round_trips_at_stride_16() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = land(0x1000);
        for layer in 0u16..8 {
            let mut data = (0x110 + u32::from(layer)).to_le_bytes().to_vec();
            data.extend([0, 0]);
            data.extend(layer.to_le_bytes());
            record.subrecords.push((b"ATXT".to_vec(), data));
            let mut vtxt = 0u16.to_le_bytes().to_vec();
            vtxt.extend([0, 0]);
            vtxt.extend(1.0f32.to_le_bytes());
            record.subrecords.push((b"VTXT".to_vec(), vtxt));
        }
        let known: HashSet<u32> = (0x100..0x200).collect();
        let key = cell_mesh_key(&record, &names(&["Skyrim.esm"]), &[], &[]);
        let (mesh, report) = build_cell_mesh(0x1000, &record, &known, key).unwrap();
        assert_eq!(mesh.slot_stride, 16);
        assert!(report.needed_stride_16);
        let quadrants = mesh.quadrant_slot_counts;
        assert_eq!(quadrants[0], 9);
        write_terrain_chunks(
            dir.path(),
            vec![PlacedCellMesh {
                worldspace_id: TAMRIEL,
                grid_x: 0,
                grid_y: 0,
                mesh: mesh.clone(),
            }],
        )
        .unwrap();
        let chunk =
            read_terrain_chunk(&dir.path().join(terrain_chunk_path(TAMRIEL, 0, 0))).unwrap();
        assert_eq!(chunk.cells[0], mesh);
        assert_eq!(
            chunk.cells[0].weights.len(),
            16 * QUADRANT_VERTICES * 4,
            "stride-16 weights survive the round trip"
        );
    }

    #[test]
    fn the_textures_the_table_lacks_move_the_key() {
        let land = land(0x1000);
        let masters = names(&["Skyrim.esm"]);
        let none = cell_mesh_key(&land, &masters, &[], &[]);
        let one = cell_mesh_key(&land, &masters, &[], &[0x2000]);
        assert_ne!(none, one, "a layer left out of the mesh changes the key");
        assert_eq!(
            cell_mesh_key(&land, &masters, &[], &[0x3000, 0x2000]),
            cell_mesh_key(&land, &masters, &[], &[0x2000, 0x3000, 0x2000]),
            "the order and repeats of the skipped textures never matter"
        );
        assert_ne!(one, cell_mesh_key(&land, &masters, &[], &[0x3000]));
    }
}
