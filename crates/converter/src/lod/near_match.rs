//! Near-match search for retail LOD files that the game data does not hold under the expected name.
//!
//! A message that only says "missing" hides a naming mismatch. The retail files of one world can
//! use a different name than the converter expects (for example `<world>objectslod.dds` instead of
//! `<world>.objects.dds`; evidence in [the LOD compiler spec](../../../../docs/specs/converters/lod-compiler.md)).
//! Every refusal or skip for an absent retail file must therefore say which
//! similar files the game data does hold. This module is the one place that builds that text, so the
//! object LOD and terrain LOD paths report the same way.
//!
//! The index holds every path the caller listed from loose files and archives, in the canonical
//! form: lowercase, forward slashes. The LOD readers list the `meshes/terrain` and
//! `textures/terrain` folders, where retail LOD files live. Files elsewhere are not searched.

use std::collections::BTreeSet;

/// The most near matches one message names. The message states how many more exist.
const MAX_LISTED: usize = 5;

/// Canonical form of a path: lowercase, forward slashes.
fn canonical(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

/// The folder part of a canonical path, without the trailing slash.
fn folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// The file name part of a canonical path.
fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// The number of leading bytes two strings share.
fn common_prefix(left: &str, right: &str) -> usize {
    left.bytes()
        .zip(right.bytes())
        .take_while(|(left, right)| left == right)
        .count()
}

/// Every game-data path the LOD readers listed, and how many archives they listed.
#[derive(Debug, Default, Clone)]
pub struct FileIndex {
    paths: BTreeSet<String>,
    archives: usize,
}

impl FileIndex {
    /// Records one listed path. Case and slash direction do not matter.
    pub fn add(&mut self, path: &str) {
        self.paths.insert(canonical(path));
    }

    /// Records that one archive was listed, whether or not it holds a matching entry.
    pub fn note_archive(&mut self) {
        self.archives += 1;
    }

    /// The number of archives that were listed.
    pub fn archives(&self) -> usize {
        self.archives
    }

    /// Whether the index holds `path`.
    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(&canonical(path))
    }

    /// The indexed paths that are near to `missing`, closest first, and the total count.
    ///
    /// A path is near when at least one of these holds:
    /// - it is in the same folder as `missing`;
    /// - its file name equals the file name of `missing`, ignoring case, in any folder;
    /// - `world` is given and its file name or folder names contain the world name.
    ///
    /// Paths in `missing` itself are never near matches. The closest paths share the longest start
    /// of the file name with `missing`. An equal file name comes first.
    pub fn near(&self, missing: &[&str], world: Option<&str>) -> Vec<&str> {
        let missing: Vec<String> = missing.iter().map(|path| canonical(path)).collect();
        let world = world.map(canonical).filter(|world| !world.is_empty());
        let mut near: Vec<(bool, usize, &str)> = Vec::new();
        for path in &self.paths {
            if missing.iter().any(|wanted| wanted == path) {
                continue;
            }
            let name = file_name(path);
            let mut same_name = false;
            let mut related = false;
            let mut best = 0usize;
            for wanted in &missing {
                let wanted_name = file_name(wanted);
                same_name |= name == wanted_name;
                related |= name == wanted_name || folder(path) == folder(wanted);
                best = best.max(common_prefix(name, wanted_name));
            }
            if let Some(world) = &world {
                related |= name.contains(world.as_str())
                    || path.split('/').any(|part| part == world.as_str());
            }
            if related {
                near.push((same_name, best, path.as_str()));
            }
        }
        near.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then(right.1.cmp(&left.1))
                .then(left.2.cmp(right.2))
        });
        near.into_iter().map(|(_, _, path)| path).collect()
    }

    /// The text for a refusal or skip: which expected path is absent and what is near.
    ///
    /// `missing` lists every name the caller accepts for the file. For example:
    /// `no textures/a.dds; near matches: textures/a_lod.dds`. With nothing near:
    /// `no textures/a.dds; no near matches in 12 archives (loose files searched too)`.
    pub fn describe_missing(&self, missing: &[&str], world: Option<&str>) -> String {
        let wanted: Vec<String> = missing.iter().map(|path| canonical(path)).collect();
        let near = self.near(missing, world);
        let head = format!("no {}", wanted.join(" or "));
        if near.is_empty() {
            return format!(
                "{head}; no near matches in {} archives (loose files searched too)",
                self.archives
            );
        }
        let listed: Vec<&str> = near.iter().copied().take(MAX_LISTED).collect();
        let mut text = format!("{head}; near matches: {}", listed.join(", "));
        if near.len() > listed.len() {
            text.push_str(&format!(" (and {} more)", near.len() - listed.len()));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(paths: &[&str], archives: usize) -> FileIndex {
        let mut index = FileIndex::default();
        for path in paths {
            index.add(path);
        }
        for _ in 0..archives {
            index.note_archive();
        }
        index
    }

    #[test]
    fn a_differently_named_file_in_the_same_folder_is_a_near_match() {
        let index = index(&["Textures\\Terrain\\W\\Objects\\Wobjectslod.dds"], 3);
        let text = index.describe_missing(&["textures/terrain/w/objects/w.objects.dds"], Some("w"));
        assert_eq!(
            text,
            "no textures/terrain/w/objects/w.objects.dds; near matches: \
             textures/terrain/w/objects/wobjectslod.dds"
        );
    }

    #[test]
    fn a_file_with_the_same_name_in_another_folder_or_case_comes_first() {
        let index = index(
            &[
                "textures/terrain/other/objects/w.objects_n.dds",
                "textures/elsewhere/W.Objects.DDS",
            ],
            1,
        );
        let near = index.near(&["textures/terrain/w/objects/w.objects.dds"], None);
        assert_eq!(near[0], "textures/elsewhere/w.objects.dds");
    }

    #[test]
    fn a_file_of_the_same_world_in_another_folder_is_a_near_match() {
        let index = index(
            &[
                "meshes/terrain/w/w.4.0.0.btr",
                "meshes/terrain/z/z.4.0.0.btr",
            ],
            1,
        );
        let near = index.near(&["textures/terrain/w/w.4.0.0.dds"], Some("w"));
        assert_eq!(near, vec!["meshes/terrain/w/w.4.0.0.btr"]);
    }

    #[test]
    fn the_missing_path_itself_is_not_listed() {
        let index = index(&["textures/a/b.dds"], 1);
        assert!(index.near(&["Textures\\A\\B.dds"], None).is_empty());
    }

    #[test]
    fn nothing_near_says_how_many_archives_were_searched() {
        let index = index(&["meshes/unrelated/file.nif"], 12);
        let text = index.describe_missing(&["textures/terrain/w/a.dds"], Some("w"));
        assert_eq!(
            text,
            "no textures/terrain/w/a.dds; no near matches in 12 archives (loose files searched too)"
        );
    }

    #[test]
    fn a_long_list_is_cut_and_counted() {
        let paths: Vec<String> = (0..9)
            .map(|number| format!("textures/terrain/w/w.4.{number}.0.dds"))
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let index = index(&refs, 1);
        let text = index.describe_missing(&["textures/terrain/w/w.4.99.0.dds"], Some("w"));
        assert!(text.ends_with("(and 4 more)"), "{text}");
    }

    #[test]
    fn several_accepted_names_are_all_stated() {
        let index = index(&[], 2);
        let text = index.describe_missing(&["t/a.dds", "t/b.dds"], None);
        assert!(text.starts_with("no t/a.dds or t/b.dds; no near matches in 2 archives"));
    }
}
