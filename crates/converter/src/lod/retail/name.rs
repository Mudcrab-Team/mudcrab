//! File names of retail LOD files.
//!
//! `<world>.<level>.<x>.<y>.<ext>` for `.btr`, `.bto` and `.btt` (`tamriel.16.0.-32.bto`): the
//! block level in cells and the south-west cell of the block. `<world>.lst` has no grid.
//! Matching is case-insensitive and ignores the directory.

/// Which retail LOD file a name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RetailLodKind {
    /// Terrain block, `.btr`.
    Terrain,
    /// Object block, `.bto`.
    Object,
    /// Tree block, `.btt`.
    Tree,
    /// Tree type list, `.lst`.
    TreeList,
}

/// A parsed retail LOD file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetailLodName {
    pub kind: RetailLodKind,
    /// Worldspace name as it appears in the file name, lowercased.
    pub world: String,
    /// Block level in cells (4, 8, 16, 32 for terrain; 4, 8, 16 for objects; 4 for trees).
    /// `None` for a `.lst`.
    pub level: Option<u32>,
    /// South-west cell of the block. `None` for a `.lst`.
    pub cell: Option<(i32, i32)>,
}

/// Parses a retail LOD file name or path. Returns `None` for anything that does not match, so a
/// caller counts it as unrecognised instead of guessing.
pub fn parse_lod_file_name(name: &str) -> Option<RetailLodName> {
    let file = name.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    let (stem, ext) = file.rsplit_once('.')?;
    let kind = match ext {
        "btr" => RetailLodKind::Terrain,
        "bto" => RetailLodKind::Object,
        "btt" => RetailLodKind::Tree,
        "lst" => RetailLodKind::TreeList,
        _ => return None,
    };
    if kind == RetailLodKind::TreeList {
        if stem.is_empty() || stem.contains('.') {
            return None;
        }
        return Some(RetailLodName {
            kind,
            world: stem.to_owned(),
            level: None,
            cell: None,
        });
    }
    // The world name has no dot in any shipped file, so the last three fields are the grid.
    let mut parts = stem.rsplitn(4, '.');
    let y = parts.next()?.parse::<i32>().ok()?;
    let x = parts.next()?.parse::<i32>().ok()?;
    let level = parts.next()?.parse::<u32>().ok()?;
    let world = parts.next()?;
    if world.is_empty() || level == 0 {
        return None;
    }
    Some(RetailLodName {
        kind,
        world: world.to_owned(),
        level: Some(level),
        cell: Some((x, y)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_names_carry_world_level_and_south_west_cell() {
        let name =
            parse_lod_file_name(r"Meshes\Terrain\Tamriel\Objects\Tamriel.16.-32.0.BTO").unwrap();
        assert_eq!(name.kind, RetailLodKind::Object);
        assert_eq!(name.world, "tamriel");
        assert_eq!(name.level, Some(16));
        assert_eq!(name.cell, Some((-32, 0)));
        let tree = parse_lod_file_name("tamriel.4.52.-28.btt").unwrap();
        assert_eq!(
            (tree.kind, tree.level, tree.cell),
            (RetailLodKind::Tree, Some(4), Some((52, -28)))
        );
        let terrain = parse_lod_file_name("dlc2solstheimworld.8.-8.12.btr").unwrap();
        assert_eq!(terrain.world, "dlc2solstheimworld");
        assert_eq!(terrain.kind, RetailLodKind::Terrain);
    }

    #[test]
    fn a_tree_list_has_no_grid() {
        let list = parse_lod_file_name("meshes/terrain/tamriel/trees/tamriel.lst").unwrap();
        assert_eq!(list.kind, RetailLodKind::TreeList);
        assert_eq!((list.level, list.cell), (None, None));
    }

    #[test]
    fn other_names_are_not_guessed() {
        for name in [
            "tamriel.16.0.bto",
            "tamriel.x.0.0.bto",
            "tamriel.0.0.0.bto",
            "tamriel.16.0.0.nif",
            ".16.0.0.bto",
            "tamriel.1.lst",
            "readme.txt",
        ] {
            assert!(parse_lod_file_name(name).is_none(), "{name}");
        }
    }
}
