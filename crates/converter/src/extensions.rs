//! The data set extensions this converter writes, and their manifest entries.
//!
//! The core of a converted data set is exactly what `main`'s converter writes. Everything else is an
//! *extension* (see [`shared::extensions`]): whole tables, indexes, files or glTF extension names
//! with an id and a version, listed in `conversion-manifest.json` with a content hash.
//!
//! An extension never changes a core table or a core glTF extension. Moving one upstream means a
//! core change plus a version bump, after which the extension is dropped from [`specs`].
//! Spec: `docs/specs/converters/extensions.md`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use color_eyre::eyre::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use shared::extensions::{
    DIRECTIONAL_SNOW, DOOR_LINKS, ExtensionEntry, ExtensionHasher, ExtensionOwns, LOD_INVENTORY,
    MATERIAL, SPACE_LIGHTING, core_build_identity,
};
use walkdir::WalkDir;

use crate::cache::ConversionManifest;

/// File name of the world database inside a converted data set.
const DATABASE: &str = "skyrim_world.db";

const GLB_MAGIC: &[u8; 4] = b"glTF";
const GLB_JSON_CHUNK: &[u8; 4] = b"JSON";

/// One extension this converter can write: its identity and everything it owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionSpec {
    pub id: &'static str,
    pub version: u32,
    pub owns: ExtensionOwns,
}

fn names(items: &[&str]) -> Vec<String> {
    let mut list: Vec<String> = items.iter().map(|item| (*item).to_owned()).collect();
    list.sort_unstable();
    list
}

/// Persistent core tables owned by the ordinary exporter and its provenance writers,
/// including the shadow tables SQLite creates for core RTrees.
const MAIN_CORE_TABLES: &[&str] = &[
    "actor_model_parts",
    "actor_models",
    "cell_environment",
    "cell_lighting",
    "cell_regions",
    "cells",
    "climate_weathers",
    "climates",
    "conversion_cache",
    "door_links",
    "emittance",
    "exterior_spatial",
    "exterior_spatial_node",
    "exterior_spatial_parent",
    "exterior_spatial_rowid",
    "formid_map",
    "grass_types",
    "image_spaces",
    "inhouse_localized_fields",
    "inhouse_plugin_masters",
    "inhouse_schema_addons",
    "inhouse_source_records",
    "inhouse_terrain_layers",
    "inhouse_terrain_source_order",
    "inhouse_unresolved_terrain_textures",
    "land",
    "landscape_texture_grasses",
    "landscape_textures",
    "lighting_templates",
    "lights",
    "lod",
    "lod_build",
    "lod_chunks",
    "lod_chunks_spatial",
    "lod_chunks_spatial_node",
    "lod_chunks_spatial_parent",
    "lod_chunks_spatial_rowid",
    "movement_game_settings",
    "movement_types",
    "npcs",
    "plugins",
    "race_movement_links",
    "record_diagnostics",
    "record_overrides",
    "records",
    "reference_activate_parents",
    "reference_details",
    "reference_linked_refs",
    "reference_rooms",
    "references",
    "region_weathers",
    "room_bounds",
    "room_graph_stats",
    "room_links",
    "room_portals",
    "schema_info",
    "script_properties",
    "script_property_values",
    "scripts",
    "statics",
    "texture_sets",
    "waters",
    "weather_colors",
    "weather_directional_ambient",
    "weathers",
    "worldspace_environment",
    "worldspaces",
];
/// Persistent indexes owned by the core exporter and in-house provenance writers.
const MAIN_CORE_INDEXES: &[&str] = &[
    "idx_cells_grid",
    "idx_npcs_editor_id",
    "idx_records_cell_id",
    "idx_records_type",
    "idx_reference_activate_parents_parent",
    "idx_reference_linked_refs_target",
    "idx_reference_rooms_room",
    "idx_references_cell",
    "idx_room_bounds_cell",
    "idx_room_portals_cell",
    "idx_statics_editor_id",
    "record_diagnostics_form",
    "record_diagnostics_source",
    "record_overrides_chain",
    "record_overrides_source",
];

/// Every extension the converter writes. Each `owns` list is sorted.
pub fn specs() -> Vec<ExtensionSpec> {
    vec![
        ExtensionSpec {
            id: LOD_INVENTORY,
            version: 1,
            owns: ExtensionOwns {
                tables: names(&[
                    "lod_block",
                    "lod_grid",
                    "lod_tree_instance",
                    "lod_tree_type",
                ]),
                indexes: names(&["idx_lod_tree_block"]),
                // The block and tree-billboard meshes converted from .btr/.bto/.lst: main converts
                // only .nif meshes, so everything below this folder is the extension's.
                files: names(&["meshes/terrain/"]),
                ..Default::default()
            },
        },
        ExtensionSpec {
            id: SPACE_LIGHTING,
            version: 1,
            owns: ExtensionOwns {
                tables: names(&["space_lighting"]),
                ..Default::default()
            },
        },
        ExtensionSpec {
            id: DIRECTIONAL_SNOW,
            version: 1,
            owns: ExtensionOwns {
                tables: names(&["matos", "static_materials"]),
                ..Default::default()
            },
        },
        ExtensionSpec {
            id: MATERIAL,
            version: 1,
            owns: ExtensionOwns {
                gltf_extensions: names(&["MUDCRAB_material", "OPEN_SKYRIM_material_animation"]),
                ..Default::default()
            },
        },
    ]
}

/// Builds the manifest entries of the finished output folder `output`.
///
/// An extension is listed when the converter wrote it, even with zero rows: every table it owns
/// exists in `skyrim_world.db`. An extension that owns only glTF extensions is listed when an
/// output mesh declares one of those names in `extensionsUsed`. If a table is missing the extension is not listed.
pub fn build_entries(output: &Path) -> Result<Vec<ExtensionEntry>> {
    let database = output.join(DATABASE);
    let connection = if database.is_file() {
        Some(
            Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .wrap_err_with(|| format!("opening {}", database.display()))?,
        )
    } else {
        None
    };
    // `built_from` is the core build identity. A database without a `lod_build` row (a conversion
    // with no distant LOD, or no database at all) has none: the empty string says so.
    let built_from = match &connection {
        Some(connection) => core_build_identity(connection)?.unwrap_or_default(),
        None => String::new(),
    };

    struct Pending {
        spec: ExtensionSpec,
        hasher: ExtensionHasher,
        stats: BTreeMap<String, u64>,
        /// Whether any mesh actually uses one of the extension's glTF extension names.
        gltf_used: bool,
    }
    let mut pending = Vec::new();
    for spec in specs() {
        let owns_database = !spec.owns.tables.is_empty();
        if owns_database {
            let Some(connection) = &connection else {
                continue;
            };
            let mut all_present = true;
            for table in &spec.owns.tables {
                all_present &= table_exists(connection, table)?;
            }
            if !all_present {
                continue;
            }
        }
        let mut hasher = ExtensionHasher::new(spec.id, spec.version);
        let mut stats = BTreeMap::new();
        if let Some(connection) = &connection {
            for table in &spec.owns.tables {
                hasher.add_table(connection, table)?;
                stats.insert(table.clone(), count_rows(connection, table)?);
            }
            for index in &spec.owns.indexes {
                hasher.add_index(connection, index)?;
            }
        }
        if spec.id == LOD_INVENTORY
            && let Some(connection) = &connection
        {
            for kind in ["terrain", "objects"] {
                let count: u64 = connection.query_row(
                    "SELECT count(*) FROM lod_block WHERE kind = ?1",
                    [kind],
                    |row| row.get(0),
                )?;
                stats.insert(format!("lod_block_{kind}"), count);
            }
        }
        for (relative, path) in owned_files(output, &spec.owns.files)? {
            let bytes = std::fs::read(&path)
                .wrap_err_with(|| format!("reading owned file {}", path.display()))?;
            hasher.add_file(&relative, &bytes);
        }
        pending.push(Pending {
            spec,
            hasher,
            stats,
            gltf_used: false,
        });
    }

    // Every mesh is read once (header and JSON chunk only) for all glTF-owning extensions.
    if pending
        .iter()
        .any(|entry| !entry.spec.owns.gltf_extensions.is_empty())
    {
        for mesh in mesh_files(output)? {
            let relative = mesh
                .strip_prefix(output)
                .expect("mesh paths start at the output folder")
                .to_string_lossy()
                .replace('\\', "/");
            let json = read_gltf_json(&mesh)
                .wrap_err_with(|| format!("reading the glTF header of {}", mesh.display()))?;
            let mut document: Option<serde_json::Value> = None;
            for entry in &mut pending {
                for name in &entry.spec.owns.gltf_extensions {
                    // A document that never spells the name cannot use it.
                    if !contains(&json, name.as_bytes()) {
                        continue;
                    }
                    if document.is_none() {
                        document = Some(serde_json::from_slice(&json).wrap_err_with(|| {
                            format!("{} has an invalid glTF JSON chunk", mesh.display())
                        })?);
                    }
                    if let Some(document) = &document {
                        let used = document["extensionsUsed"].as_array().is_some_and(|used| {
                            used.iter()
                                .any(|value| value.as_str() == Some(name.as_str()))
                        });
                        entry.gltf_used |= used;
                        entry.hasher.add_gltf(name, &relative, document);
                    }
                }
            }
        }
        // An extension that owns only glTF extension names is listed when some mesh uses one of
        // them: a manifest entry for data nothing contains would hide a manifest/content mismatch.
        pending.retain(|entry| {
            let only_gltf = !entry.spec.owns.gltf_extensions.is_empty()
                && entry.spec.owns.tables.is_empty()
                && entry.spec.owns.indexes.is_empty()
                && entry.spec.owns.files.is_empty();
            !only_gltf || entry.gltf_used
        });
    }

    Ok(pending
        .into_iter()
        .map(|entry| ExtensionEntry {
            id: entry.spec.id.to_owned(),
            version: entry.spec.version,
            owns: entry.spec.owns,
            sha256: entry.hasher.finish(),
            built_from: built_from.clone(),
            stats: entry.stats,
        })
        .collect())
}

/// Recomputes `manifest.extensions` from the finished output folder. Call it wherever the
/// database or meshes a manifest describes were last changed, right before the manifest is saved.
pub fn refresh_manifest(manifest: &mut ConversionManifest, output: &Path) -> Result<()> {
    manifest.extensions = build_entries(output)?;
    Ok(())
}

/// Recomputes `mudcrab-ext-hash-v1` of `entry` from what `output` holds now, reading only what the
/// entry's `owns` lists. Errors when an owned table, index or exact file is missing.
pub fn recompute_hash(output: &Path, entry: &ExtensionEntry) -> Result<String> {
    let mut hasher = ExtensionHasher::new(&entry.id, entry.version);
    if !entry.owns.tables.is_empty() || !entry.owns.indexes.is_empty() {
        let database = output.join(DATABASE);
        let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .wrap_err_with(|| format!("opening {}", database.display()))?;
        for table in &entry.owns.tables {
            hasher.add_table(&connection, table)?;
        }
        for index in &entry.owns.indexes {
            hasher.add_index(&connection, index)?;
        }
    }
    for (relative, path) in owned_files(output, &entry.owns.files)? {
        let bytes =
            fs::read(&path).wrap_err_with(|| format!("reading owned file {}", path.display()))?;
        hasher.add_file(&relative, &bytes);
    }
    if !entry.owns.gltf_extensions.is_empty() {
        for mesh in mesh_files(output)? {
            let relative = mesh
                .strip_prefix(output)
                .expect("mesh paths start at the output folder")
                .to_string_lossy()
                .replace('\\', "/");
            let json = read_gltf_json(&mesh)
                .wrap_err_with(|| format!("reading the glTF header of {}", mesh.display()))?;
            let mut document: Option<serde_json::Value> = None;
            for name in &entry.owns.gltf_extensions {
                if !contains(&json, name.as_bytes()) {
                    continue;
                }
                if document.is_none() {
                    document = Some(serde_json::from_slice(&json).wrap_err_with(|| {
                        format!("{} has an invalid glTF JSON chunk", mesh.display())
                    })?);
                }
                if let Some(document) = &document {
                    hasher.add_gltf(name, &relative, document);
                }
            }
        }
    }
    Ok(hasher.finish())
}

/// Like [`refresh_manifest`], and also lists independently versioned payloads already written
/// by another finalizer.
///
/// The caller holds the exclusive asset lock and the entries come from finalizers that ran under it;
/// this function mints no provenance. It sets `manifest.extensions` to the converter's own entries
/// followed by `finalized`, or leaves the manifest untouched and returns an error when:
/// * a finalized entry's `built_from` is empty or not the data set's current core build identity,
/// * an id is listed twice (including a clash with this converter's own extensions),
/// * two extensions own the same table, index, file entry or glTF extension name,
/// * a finalized entry's `sha256` is not the hash of the content now in `output`.
///
/// With an empty `finalized` it behaves like [`refresh_manifest`].
pub fn refresh_manifest_with_finalized(
    manifest: &mut ConversionManifest,
    output: &Path,
    finalized: &[ExtensionEntry],
) -> Result<()> {
    let mut entries = build_entries(output)?;
    if !finalized.is_empty() {
        let database = output.join(DATABASE);
        let current = if database.is_file() {
            let connection =
                Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .wrap_err_with(|| format!("opening {}", database.display()))?;
            core_build_identity(&connection)?.unwrap_or_default()
        } else {
            String::new()
        };
        if current.is_empty() {
            bail!(
                "{} has no core build identity (lod_build), so finalized extensions cannot be listed",
                output.display()
            );
        }
        let mut ids: std::collections::BTreeSet<&str> =
            entries.iter().map(|entry| entry.id.as_str()).collect();
        for entry in finalized {
            if entry.id == DOOR_LINKS
                || entry.owns.tables.iter().any(|table| {
                    MAIN_CORE_TABLES
                        .iter()
                        .any(|core| core.eq_ignore_ascii_case(table))
                })
                || entry.owns.indexes.iter().any(|index| {
                    MAIN_CORE_INDEXES
                        .iter()
                        .any(|core| core.eq_ignore_ascii_case(index))
                })
            {
                bail!(
                    "extension {} claims promoted or core-owned output",
                    entry.id
                );
            }
            if entry.built_from != current {
                bail!(
                    "extension {} was built from {:?}, but the data set's core is {current}",
                    entry.id,
                    entry.built_from
                );
            }
            if !ids.insert(entry.id.as_str()) {
                bail!("extension {} is listed twice", entry.id);
            }
            let actual = recompute_hash(output, entry)
                .wrap_err_with(|| format!("hashing extension {}", entry.id))?;
            if actual != entry.sha256 {
                bail!(
                    "extension {} lists sha256 {}, but its content hashes to {actual}",
                    entry.id,
                    entry.sha256
                );
            }
        }
        entries.extend(finalized.iter().cloned());
        let mut problems = shared::extensions::ownership_problems(&entries);
        problems.extend(file_prefix_overlaps(&entries));
        if !problems.is_empty() {
            bail!("extension ownership conflicts: {}", problems.join("; "));
        }
    }
    manifest.extensions = entries;
    Ok(())
}

/// Files two extensions own through different spellings: a folder entry (ending in `/`) covers
/// every path below it, so it overlaps a file or folder of another extension that starts with it.
/// Exact equal strings are already reported by `ownership_problems`.
fn file_prefix_overlaps(entries: &[ExtensionEntry]) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, first) in entries.iter().enumerate() {
        for second in &entries[index + 1..] {
            for a in &first.owns.files {
                for b in &second.owns.files {
                    let nested = (a.ends_with('/') && b.starts_with(a.as_str()))
                        || (b.ends_with('/') && a.starts_with(b.as_str()));
                    if nested && a != b {
                        problems.push(format!(
                            "file {a} of {} overlaps file {b} of {}",
                            first.id, second.id
                        ));
                    }
                }
            }
        }
    }
    problems
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn count_rows(connection: &Connection, table: &str) -> Result<u64> {
    Ok(connection.query_row(
        &format!("SELECT count(*) FROM \"{}\"", table.replace('"', "\"\"")),
        [],
        |row| row.get(0),
    )?)
}

/// The files an extension owns: exact paths, and every file below a directory entry (an entry
/// ending in `/`; a directory that does not exist covers nothing). Paths are relative to `root`
/// with `/` separators. A missing exact file is an error.
fn owned_files(root: &Path, entries: &[String]) -> Result<Vec<(String, std::path::PathBuf)>> {
    let mut files = BTreeMap::new();
    for entry in entries {
        if entry.ends_with('/') {
            let folder = root.join(entry.trim_end_matches('/'));
            if !folder.is_dir() {
                continue;
            }
            for item in WalkDir::new(&folder) {
                let item = item?;
                if item.file_type().is_file() {
                    let relative = item
                        .path()
                        .strip_prefix(root)
                        .expect("walked paths start at the output folder")
                        .to_string_lossy()
                        .replace('\\', "/");
                    files.insert(relative, item.into_path());
                }
            }
        } else {
            let path = root.join(entry);
            if !path.is_file() {
                color_eyre::eyre::bail!("owned file {entry} is missing");
            }
            files.insert(entry.clone(), path);
        }
    }
    Ok(files.into_iter().collect())
}

/// Every `.glb` and `.gltf` under `root`, in any directory.
fn mesh_files(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut meshes = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let is_mesh = entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("glb") || extension.eq_ignore_ascii_case("gltf")
            });
        if is_mesh {
            meshes.push(entry.into_path());
        }
    }
    Ok(meshes)
}

/// The JSON document of a mesh: for a `.glb` only the header and the first (JSON) chunk are read.
fn read_gltf_json(path: &Path) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gltf"))
    {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    let mut header = [0u8; 20];
    file.read_exact(&mut header)?;
    if &header[0..4] != GLB_MAGIC {
        bail!("not a GLB");
    }
    if &header[16..20] != GLB_JSON_CHUNK {
        bail!("no JSON chunk first");
    }
    let length = u32::from_le_bytes(header[12..16].try_into().expect("four bytes")) as usize;
    let mut json = vec![0u8; length];
    file.read_exact(&mut json)?;
    Ok(json)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn specs_are_sorted_and_never_overlap() {
        let mut seen: BTreeSet<(&str, String)> = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for spec in specs() {
            assert!(ids.insert(spec.id), "{} is listed twice", spec.id);
            for (kind, list) in [
                ("table", &spec.owns.tables),
                ("index", &spec.owns.indexes),
                ("file", &spec.owns.files),
                ("gltf", &spec.owns.gltf_extensions),
            ] {
                assert!(list.is_sorted(), "{} {kind} list is not sorted", spec.id);
                for item in list {
                    assert!(
                        seen.insert((kind, item.clone())),
                        "{kind} {item} is owned twice"
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_owned_is_a_main_core_table() {
        for spec in specs() {
            for table in &spec.owns.tables {
                assert!(
                    !MAIN_CORE_TABLES.contains(&table.as_str()),
                    "{} owns the core table {table}",
                    spec.id
                );
            }
            for index in &spec.owns.indexes {
                assert!(
                    !MAIN_CORE_INDEXES.contains(&index.as_str()),
                    "{} owns the core index {index}",
                    spec.id
                );
            }
        }
    }

    /// The core list above is `main`'s: every table this converter writes is either on it or owned
    /// by an extension, so a new table cannot be added to the database without a decision.
    #[test]
    fn every_converter_table_is_core_or_owned() {
        let connection = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        let owned: BTreeSet<String> = specs()
            .into_iter()
            .flat_map(|spec| spec.owns.tables)
            .collect();
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap();
        let tables: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for table in tables {
            assert!(
                MAIN_CORE_TABLES.contains(&table.as_str()) || owned.contains(&table),
                "table {table} is neither main's core nor owned by an extension"
            );
        }
    }

    #[test]
    fn every_converter_index_is_core_or_owned() {
        let connection = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        let owned: BTreeSet<String> = specs()
            .into_iter()
            .flat_map(|spec| spec.owns.indexes)
            .collect();
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND sql IS NOT NULL")
            .unwrap();
        let indexes: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for index in indexes {
            assert!(
                MAIN_CORE_INDEXES.contains(&index.as_str()) || owned.contains(&index),
                "index {index} is neither core nor owned by an extension"
            );
        }
    }

    #[test]
    fn finalized_entries_cannot_claim_current_core_tables_or_indexes() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let template = custom_entry(directory.path(), "example.extra", "extra_table", "extra");
        // Exercise every persistent core name, including in-house provenance output which
        // create_tables alone does not produce. The guard must run before content hashing.
        for (kind, names) in [("table", MAIN_CORE_TABLES), ("index", MAIN_CORE_INDEXES)] {
            for name in names {
                for spelling in [(*name).to_owned(), name.to_ascii_uppercase()] {
                    let mut entry = template.clone();
                    entry.owns = ExtensionOwns::default();
                    if kind == "table" {
                        entry.owns.tables.push(spelling);
                    } else {
                        // Index-only ownership must not bypass the core table guard.
                        entry.owns.indexes.push(spelling);
                    }
                    assert!(
                        refused(directory.path(), &[entry]).contains("core-owned"),
                        "{kind} {name}"
                    );
                }
            }
        }
    }

    #[test]
    fn finalized_entries_cannot_own_generated_core_rtree_shadows() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let connection = Connection::open(directory.path().join(DATABASE)).unwrap();
        let shadows: Vec<String> = connection.prepare(
            "SELECT name FROM pragma_table_list WHERE schema='main' AND type='shadow' ORDER BY name"
        ).unwrap().query_map([], |row| row.get(0)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(
            shadows,
            [
                "exterior_spatial_node",
                "exterior_spatial_parent",
                "exterior_spatial_rowid",
                "lod_chunks_spatial_node",
                "lod_chunks_spatial_parent",
                "lod_chunks_spatial_rowid",
            ]
        );
        drop(connection);
        for name in shadows {
            for spelling in [name.clone(), name.to_ascii_uppercase()] {
                let mut entry = ExtensionEntry {
                    id: "example.extra".into(),
                    version: 1,
                    owns: ExtensionOwns {
                        tables: vec![spelling],
                        ..Default::default()
                    },
                    sha256: String::new(),
                    built_from: "abc".into(),
                    stats: BTreeMap::new(),
                };
                // A real, correctly hashed claim must fail ownership, not an earlier hash check.
                entry.sha256 = recompute_hash(directory.path(), &entry).unwrap();
                assert!(
                    refused(directory.path(), &[entry]).contains("core-owned"),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn finalized_sql_aliases_conflict_with_converter_and_foreign_owners() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        for (table, index) in [("SPACE_LIGHTING", ""), ("", "IDX_LOD_TREE_BLOCK")] {
            let mut entry = ExtensionEntry {
                id: "example.extra".into(),
                version: 1,
                owns: ExtensionOwns::default(),
                sha256: String::new(),
                built_from: "abc".into(),
                stats: BTreeMap::new(),
            };
            if !table.is_empty() {
                entry.owns.tables.push(table.into());
            }
            if !index.is_empty() {
                entry.owns.indexes.push(index.into());
            }
            entry.sha256 = recompute_hash(directory.path(), &entry).unwrap();
            assert!(refused(directory.path(), &[entry]).contains("ownership conflicts"));
        }
        let one = custom_entry(directory.path(), "example.a", "extra_table", "extra");
        let connection = Connection::open(directory.path().join(DATABASE)).unwrap();
        connection
            .execute_batch("CREATE INDEX idx_extra ON extra_table(a);")
            .unwrap();
        drop(connection);
        for owns in [
            ExtensionOwns {
                tables: vec!["EXTRA_TABLE".into()],
                ..Default::default()
            },
            ExtensionOwns {
                indexes: vec!["IDX_EXTRA".into()],
                ..Default::default()
            },
        ] {
            let mut a = one.clone();
            a.owns.files.clear();
            a.owns.indexes = vec!["idx_extra".into()];
            a.sha256 = recompute_hash(directory.path(), &a).unwrap();
            let mut b = a.clone();
            b.id = "example.b".into();
            b.owns = owns;
            b.sha256 = recompute_hash(directory.path(), &b).unwrap();
            assert!(
                refused(directory.path(), &[a.clone(), b.clone()]).contains("ownership conflicts")
            );
            assert!(refused(directory.path(), &[b, a]).contains("ownership conflicts"));
        }
    }

    #[test]
    fn malformed_identity_cannot_publish_or_replace_manifest_entries() {
        for sql in [
            "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, renamed_identity TEXT);",
            "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity);
             INSERT INTO lod_build VALUES(1, 42);",
        ] {
            let directory = output_with_tables(&[]);
            with_identity(directory.path(), "abc");
            let entry = custom_entry(directory.path(), "example.extra", "extra_table", "extra");
            let connection = Connection::open(directory.path().join(DATABASE)).unwrap();
            connection.execute_batch("DROP TABLE lod_build;").unwrap();
            connection.execute_batch(sql).unwrap();
            drop(connection);
            let error = build_entries(directory.path()).unwrap_err();
            assert!(format!("{error:#}").contains("reading core build identity"));
            let mut manifest = ConversionManifest {
                extensions: vec![entry.clone()],
                ..Default::default()
            };
            let before = serde_json::to_value(&manifest).unwrap();
            let error = refresh_manifest(&mut manifest, directory.path()).unwrap_err();
            assert!(format!("{error:#}").contains("reading core build identity"));
            assert_eq!(serde_json::to_value(&manifest).unwrap(), before);
            assert!(refused(directory.path(), &[entry]).contains("reading core build identity"));
        }
    }

    #[test]
    fn statics_is_main_s_table() {
        let connection = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        let columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('statics')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "editor_id",
                "model_path",
                "flags",
                "full_name",
                "bounds_min_x",
                "bounds_min_y",
                "bounds_min_z",
                "bounds_max_x",
                "bounds_max_y",
                "bounds_max_z",
                "bounds_valid"
            ]
        );
    }

    fn glb(json: &str) -> Vec<u8> {
        let mut json = json.as_bytes().to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bytes = b"glTF".to_vec();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&((20 + json.len()) as u32).to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"JSON");
        bytes.extend_from_slice(&json);
        bytes
    }

    fn output_with_tables(tables: &[&str]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join(DATABASE)).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        // Capability hashing reads existing extension schemas; its fixture must not
        // depend on optional production exporters being installed in this build.
        connection
            .execute_batch(
                r#"
         CREATE TABLE IF NOT EXISTS lod_grid (
             worldspace_id INTEGER PRIMARY KEY, origin_x INTEGER NOT NULL, origin_y INTEGER NOT NULL,
             levels TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS lod_block (
             worldspace_id INTEGER NOT NULL, kind TEXT NOT NULL, level INTEGER NOT NULL,
             block_x INTEGER NOT NULL, block_y INTEGER NOT NULL, mesh_path TEXT NOT NULL,
             bounds_min_x REAL, bounds_min_y REAL, bounds_min_z REAL,
             bounds_max_x REAL, bounds_max_y REAL, bounds_max_z REAL,
             PRIMARY KEY (worldspace_id, kind, level, block_x, block_y)
         );
         CREATE TABLE IF NOT EXISTS lod_tree_type (
             worldspace_id INTEGER NOT NULL, tree_index INTEGER NOT NULL, mesh_path TEXT NOT NULL,
             size_x REAL NOT NULL, size_y REAL NOT NULL,
             u0 REAL NOT NULL, v0 REAL NOT NULL, u1 REAL NOT NULL, v1 REAL NOT NULL,
             PRIMARY KEY (worldspace_id, tree_index)
         );
         CREATE TABLE IF NOT EXISTS lod_tree_instance (
             worldspace_id INTEGER NOT NULL, block_x INTEGER NOT NULL, block_y INTEGER NOT NULL,
             tree_index INTEGER NOT NULL, pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
             rotation REAL NOT NULL DEFAULT 0, scale REAL NOT NULL DEFAULT 1
         );
         CREATE INDEX IF NOT EXISTS idx_lod_tree_block ON lod_tree_instance(worldspace_id, block_x, block_y);
         CREATE TABLE IF NOT EXISTS static_materials (
             id INTEGER PRIMARY KEY,            -- the statics row
             material_object INTEGER NOT NULL,  -- DNAM bytes 4..8: the MATO
             material_max_angle REAL NOT NULL   -- DNAM bytes 0..4: degrees of coverage, in the snow shader's terms
         );
         CREATE TABLE IF NOT EXISTS matos (
             id INTEGER PRIMARY KEY, editor_id TEXT,
             falloff_scale REAL NOT NULL, falloff_bias REAL NOT NULL,
             noise_uv_scale REAL NOT NULL, material_uv_scale REAL NOT NULL,
             dir_proj_x REAL NOT NULL, dir_proj_y REAL NOT NULL, dir_proj_z REAL NOT NULL,
             normal_dampener REAL NOT NULL,
             single_pass_color INTEGER NOT NULL, single_pass INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS space_lighting (
             space_id INTEGER PRIMARY KEY,   -- a CELL or WRLD FormID: one row per space
             is_interior INTEGER NOT NULL,   -- 1 for a CELL, 0 for a WRLD
             template_id INTEGER,            -- the cell's LTMP, remapped; NULL for a worldspace
             ambient INTEGER, directional INTEGER, fog INTEGER,
             fog_near REAL, fog_far REAL, fog_power REAL, fog_clip REAL,
             direction_rot_xy INTEGER, direction_rot_z INTEGER, direction_fade REAL,
             sky_upper INTEGER, sky_fog INTEGER, sky_lower INTEGER,
             sun INTEGER, sun_illuminance REAL,
             climate_id INTEGER, weather_id INTEGER,
             has_sky INTEGER NOT NULL,
             -- An interior's XCLL beyond the near fog: the colour the fog reaches at its far
             -- distance, the most it covers, and the distances over which the cell's point
             -- lights fade out. NULL for a worldspace.
             fog_far_color INTEGER, fog_max REAL,
             light_fade_begin REAL, light_fade_end REAL
         );
                "#,
            )
            .unwrap();
        for table in tables {
            connection
                .execute(&format!("DROP TABLE IF EXISTS \"{table}\""), [])
                .unwrap();
        }
        directory
    }

    #[test]
    fn lists_what_was_written_even_with_zero_rows() {
        let directory = output_with_tables(&[]);
        let entries = build_entries(directory.path()).unwrap();
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        // No mesh in the folder: the glTF-only extension is not listed.
        assert_eq!(ids, [LOD_INVENTORY, SPACE_LIGHTING, DIRECTIONAL_SNOW]);
        let lod = &entries[0];
        assert_eq!(lod.stats["lod_grid"], 0);
        assert_eq!(lod.stats["lod_block_terrain"], 0);
        assert_eq!(
            lod.built_from, "",
            "no lod_build row means an empty identity"
        );
        assert!(shared::extensions::ownership_problems(&entries).is_empty());
    }

    #[test]
    fn promoted_door_links_are_core_and_old_manifest_entries_are_retired() {
        let directory = output_with_tables(&[]);
        let mut manifest = ConversionManifest::default();
        let mut legacy = build_entries(directory.path()).unwrap()[0].clone();
        legacy.id = DOOR_LINKS.to_owned();
        legacy.owns = ExtensionOwns {
            tables: names(&["door_links"]),
            ..Default::default()
        };
        manifest.extensions.push(legacy);
        refresh_manifest(&mut manifest, directory.path()).unwrap();
        assert!(
            !manifest
                .extensions
                .iter()
                .any(|entry| entry.id == DOOR_LINKS)
        );
        assert!(
            !manifest
                .extensions
                .iter()
                .flat_map(|entry| &entry.owns.tables)
                .any(|table| table == "door_links" || table == "emittance")
        );
    }

    #[test]
    fn finalized_extensions_cannot_claim_core_tables_or_retired_door_identity() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let mut entry = custom_entry(directory.path(), "mudcrab.extra", "extra_table", "extra");
        for table in ["door_links", "Door_Links", "emittance", "EMITTANCE"] {
            entry.owns = ExtensionOwns {
                tables: names(&[table]),
                ..Default::default()
            };
            entry.sha256 = recompute_hash(directory.path(), &entry).unwrap();
            assert!(refused(directory.path(), &[entry.clone()]).contains("core-owned"));
        }
        entry.id = DOOR_LINKS.to_owned();
        entry.owns = ExtensionOwns::default();
        entry.sha256 = recompute_hash(directory.path(), &entry).unwrap();
        assert!(refused(directory.path(), &[entry]).contains("core-owned"));
    }

    #[test]
    fn a_missing_table_leaves_the_extension_out() {
        let directory = output_with_tables(&["static_materials", "door_links"]);
        let entries = build_entries(directory.path()).unwrap();
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, [LOD_INVENTORY, SPACE_LIGHTING]);
    }

    #[test]
    fn no_database_lists_no_table_extension() {
        let directory = tempfile::tempdir().unwrap();
        assert!(build_entries(directory.path()).unwrap().is_empty());
    }

    #[test]
    fn rows_change_the_hash_and_stats_count_them() {
        let directory = output_with_tables(&[]);
        let before = build_entries(directory.path()).unwrap();
        let connection = Connection::open(directory.path().join(DATABASE)).unwrap();
        connection
            .execute(
                "INSERT INTO static_materials(id, material_object, material_max_angle) VALUES (1, 2, 3.5)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_build(id, build_identity) VALUES (1, 'abc')",
                [],
            )
            .unwrap();
        drop(connection);
        let after = build_entries(directory.path()).unwrap();
        let snow = |entries: &[ExtensionEntry]| {
            entries
                .iter()
                .find(|entry| entry.id == DIRECTIONAL_SNOW)
                .cloned()
                .unwrap()
        };
        assert_ne!(snow(&before).sha256, snow(&after).sha256);
        assert_eq!(snow(&after).stats["static_materials"], 1);
        assert_eq!(snow(&after).stats["matos"], 0);
        assert_eq!(snow(&after).built_from, "abc");
        assert_eq!(snow(&before).built_from, "");
    }

    #[test]
    fn meshes_feed_the_material_hash() {
        let directory = output_with_tables(&[]);
        fs::create_dir_all(directory.path().join("meshes")).unwrap();
        fs::write(
            directory.path().join("meshes/plain.glb"),
            glb(r#"{"asset":{"version":"2.0"}}"#),
        )
        .unwrap();
        // No mesh uses the extension: it is not listed (a listed entry would describe nothing).
        let listed = |entries: &[ExtensionEntry]| entries.iter().any(|entry| entry.id == MATERIAL);
        assert!(!listed(&build_entries(directory.path()).unwrap()));
        fs::write(
            directory.path().join("meshes/rock.glb"),
            glb(
                r#"{"asset":{"version":"2.0"},"extensionsUsed":["MUDCRAB_material"],
                "materials":[{"extensions":{"MUDCRAB_material":{"uvScale":[1,2.5]}}}]}"#,
            ),
        )
        .unwrap();
        let used = build_entries(directory.path())
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == MATERIAL)
            .expect("a mesh that uses MUDCRAB_material lists the extension");
        assert_ne!(used.sha256, ExtensionHasher::new(MATERIAL, 1).finish());
    }

    /// A finalized extension owning a table and a folder, as a cluster finalizer would write it.
    fn custom_entry(output: &Path, id: &str, table: &str, folder: &str) -> ExtensionEntry {
        let connection = Connection::open(output.join(DATABASE)).unwrap();
        connection
            .execute(
                &format!("CREATE TABLE IF NOT EXISTS {table}(a INTEGER)"),
                [],
            )
            .unwrap();
        connection
            .execute(&format!("INSERT INTO {table} VALUES (7)"), [])
            .unwrap();
        drop(connection);
        fs::create_dir_all(output.join(folder)).unwrap();
        fs::write(output.join(folder).join("a.bin"), b"payload").unwrap();
        let mut entry = ExtensionEntry {
            id: id.to_owned(),
            version: 1,
            owns: ExtensionOwns {
                tables: vec![table.to_owned()],
                files: vec![format!("{folder}/")],
                ..Default::default()
            },
            sha256: String::new(),
            built_from: "abc".to_owned(),
            stats: BTreeMap::new(),
        };
        entry.sha256 = recompute_hash(output, &entry).unwrap();
        entry
    }

    fn with_identity(output: &Path, identity: &str) {
        let connection = Connection::open(output.join(DATABASE)).unwrap();
        connection
            .execute(
                "INSERT OR REPLACE INTO lod_build(id, build_identity) VALUES (1, ?1)",
                [identity],
            )
            .unwrap();
    }

    #[test]
    fn finalized_entries_join_the_converter_s_own() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let base = custom_entry(
            directory.path(),
            "mudcrab.clusters",
            "cluster_x",
            "clusters",
        );
        let terrain = custom_entry(
            directory.path(),
            "mudcrab.cluster_terrain",
            "cluster_t",
            "clusters_terrain",
        );
        let mut manifest = ConversionManifest::default();
        refresh_manifest_with_finalized(
            &mut manifest,
            directory.path(),
            &[base.clone(), terrain.clone()],
        )
        .unwrap();
        let ids: Vec<&str> = manifest.extensions.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                LOD_INVENTORY,
                SPACE_LIGHTING,
                DIRECTIONAL_SNOW,
                "mudcrab.clusters",
                "mudcrab.cluster_terrain"
            ]
        );
        assert_eq!(
            manifest.extensions.iter().find(|entry| entry.id == base.id),
            Some(&base)
        );
        assert_eq!(
            manifest
                .extensions
                .iter()
                .find(|entry| entry.id == terrain.id),
            Some(&terrain)
        );
        // No finalized entries: the same list as refresh_manifest.
        let mut plain = ConversionManifest::default();
        refresh_manifest(&mut plain, directory.path()).unwrap();
        let mut empty = ConversionManifest::default();
        refresh_manifest_with_finalized(&mut empty, directory.path(), &[]).unwrap();
        assert_eq!(plain.extensions, empty.extensions);
    }

    /// Every refusal leaves the in-memory manifest as it was.
    fn refused(output: &Path, finalized: &[ExtensionEntry]) -> String {
        let mut manifest = ConversionManifest {
            extensions: vec![ExtensionEntry {
                id: "kept".to_owned(),
                version: 1,
                owns: ExtensionOwns::default(),
                sha256: "0".repeat(64),
                built_from: String::new(),
                stats: BTreeMap::new(),
            }],
            ..Default::default()
        };
        let before = manifest.clone();
        let error = refresh_manifest_with_finalized(&mut manifest, output, finalized)
            .expect_err("the call must be refused");
        assert_eq!(
            manifest, before,
            "a refused call must not change the manifest"
        );
        format!("{error:#}")
    }

    #[test]
    fn a_mismatched_core_identity_is_refused() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let entry = custom_entry(
            directory.path(),
            "mudcrab.clusters",
            "cluster_x",
            "clusters",
        );
        with_identity(directory.path(), "def");
        assert!(refused(directory.path(), std::slice::from_ref(&entry)).contains("was built from"));
        // A data set without any identity cannot take a finalized entry either.
        let bare = output_with_tables(&[]);
        let mut orphan = entry;
        orphan.built_from = String::new();
        assert!(refused(bare.path(), &[orphan]).contains("no core build identity"));
    }

    #[test]
    fn duplicate_ids_and_conflicting_ownership_are_refused() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let one = custom_entry(
            directory.path(),
            "mudcrab.clusters",
            "cluster_x",
            "clusters",
        );
        assert!(refused(directory.path(), &[one.clone(), one.clone()]).contains("listed twice"));
        let mut clash = custom_entry(directory.path(), "mudcrab.other", "cluster_y", "other");
        clash.id = SPACE_LIGHTING.to_owned();
        assert!(refused(directory.path(), &[clash]).contains("listed twice"));
        // Same table as the converter's own space_lighting, under a new id.
        let mut taken = one.clone();
        taken.id = "mudcrab.taken".to_owned();
        taken.owns = ExtensionOwns {
            tables: vec!["space_lighting".to_owned()],
            ..Default::default()
        };
        taken.sha256 = recompute_hash(directory.path(), &taken).unwrap();
        assert!(refused(directory.path(), &[taken]).contains("ownership conflicts"));
        let mut shared_folder =
            custom_entry(directory.path(), "mudcrab.b", "cluster_b", "clusters");
        shared_folder.sha256 = recompute_hash(directory.path(), &shared_folder).unwrap();
        assert!(refused(directory.path(), &[one, shared_folder]).contains("ownership conflicts"));
    }

    #[test]
    fn changed_owned_bytes_are_refused() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let entry = custom_entry(
            directory.path(),
            "mudcrab.clusters",
            "cluster_x",
            "clusters",
        );
        fs::write(directory.path().join("clusters/a.bin"), b"changed").unwrap();
        assert!(
            refused(directory.path(), std::slice::from_ref(&entry)).contains("content hashes to")
        );
        fs::write(directory.path().join("clusters/a.bin"), b"payload").unwrap();
        let connection = Connection::open(directory.path().join(DATABASE)).unwrap();
        connection
            .execute("INSERT INTO cluster_x VALUES (8)", [])
            .unwrap();
        drop(connection);
        assert!(refused(directory.path(), &[entry]).contains("content hashes to"));
    }

    #[test]
    fn nested_file_ownership_is_refused() {
        let directory = output_with_tables(&[]);
        with_identity(directory.path(), "abc");
        let base = custom_entry(
            directory.path(),
            "mudcrab.clusters",
            "cluster_x",
            "clusters",
        );
        // Another extension owning a folder or an exact file below the base's folder.
        let mut below = custom_entry(
            directory.path(),
            "mudcrab.cluster_terrain",
            "cluster_t",
            "clusters/terrain",
        );
        // The nested files exist now, so the base hash covers them.
        let mut base = base;
        base.sha256 = recompute_hash(directory.path(), &base).unwrap();
        assert!(
            refused(directory.path(), &[base.clone(), below.clone()]).contains("overlaps"),
            "a folder below another extension's folder"
        );
        below.owns.files = vec!["clusters/a.bin".to_owned()];
        below.sha256 = recompute_hash(directory.path(), &below).unwrap();
        assert!(
            refused(directory.path(), &[base.clone(), below.clone()]).contains("overlaps"),
            "an exact file below another extension's folder"
        );
        // Order does not matter, and the converter's own mesh folder counts too.
        assert!(refused(directory.path(), &[below, base.clone()]).contains("overlaps"));
        let mut meshes = custom_entry(
            directory.path(),
            "mudcrab.m",
            "cluster_m",
            "meshes/terrain/x",
        );
        meshes.owns.files = vec!["meshes/terrain/x/".to_owned()];
        meshes.sha256 = recompute_hash(directory.path(), &meshes).unwrap();
        assert!(refused(directory.path(), &[meshes]).contains("overlaps"));
        // Disjoint folders with a shared name prefix are fine.
        let sibling = custom_entry(directory.path(), "mudcrab.s", "cluster_s", "clusters_extra");
        let mut manifest = ConversionManifest::default();
        refresh_manifest_with_finalized(&mut manifest, directory.path(), &[base, sibling]).unwrap();
    }

    #[test]
    fn a_missing_exact_owned_file_is_an_error() {
        let directory = output_with_tables(&[]);
        let error = owned_files(directory.path(), &["clusters/nothing.bin".to_owned()])
            .expect_err("an exact file must exist");
        assert!(format!("{error}").contains("is missing"));
        // A missing folder entry covers nothing.
        assert!(
            owned_files(directory.path(), &["clusters/".to_owned()])
                .unwrap()
                .is_empty()
        );
    }
}
