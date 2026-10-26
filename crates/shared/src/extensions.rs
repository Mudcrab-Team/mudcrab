//! Data set extensions: derived output that `main`'s converter does not write.
//!
//! The core of a converted data set (every table, column, file and manifest value `main`'s converter
//! writes) stays byte for byte `main`'s. Everything else is an *extension*: a named, versioned set
//! of whole tables, indexes, files or glTF extension names, listed in `conversion-manifest.json`.
//! For example, a legacy door-link declaration (door links are now core output):
//!
//! ```json
//! "extensions": [{ "id": "mudcrab.door_links", "version": 1,
//!                  "owns": { "tables": ["door_links"], "indexes": [], "files": [], "gltf_extensions": [] },
//!                  "sha256": "...", "built_from": "...", "stats": { "links": 2 } }]
//! ```
//!
//! A runtime reads the core it supports plus the extensions it knows ([`DataExtensions`]); a
//! feature whose extension is missing, stale or too new is not built and says why. The content
//! hash (`mudcrab-ext-hash-v1`, [`ExtensionHasher`]) matches the reference implementation in
//! `crates/converter/tests/support/extension_hash_reference.py`.
//! Contract: `docs/specs/converters/extensions.md`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::RangeInclusive;
use std::path::Path;

use color_eyre::eyre::{Context, Result, bail, ensure};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Name of the content hash algorithm, the first line of every hashed stream.
pub const HASH_ALGORITHM: &str = "mudcrab-ext-hash-v1";

/// Distant-object LOD inventory: grids, blocks, tree types and instances.
pub const LOD_INVENTORY: &str = "mudcrab.lod_inventory";
/// Load-door teleport links (`XTEL`).
pub const DOOR_LINKS: &str = "mudcrab.door_links";
/// Per-space lighting (`XCLL`, `LGTM`).
pub const SPACE_LIGHTING: &str = "mudcrab.space_lighting";
/// Directional snow: material objects (`MATO`) and each static's material (`STAT DNAM`).
pub const DIRECTIONAL_SNOW: &str = "mudcrab.directional_snow";
/// Material data beyond `OPEN_SKYRIM_material`: UV transform, effect falloff, blend factors, animation.
pub const MATERIAL: &str = "mudcrab.material";

/// What one extension wrote. Every list is sorted; a `files` entry ending in `/` covers every file
/// below that directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionOwns {
    #[serde(default)]
    pub tables: Vec<String>,
    #[serde(default)]
    pub indexes: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub gltf_extensions: Vec<String>,
}

/// One entry of the manifest's `extensions` list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionEntry {
    pub id: String,
    pub version: u32,
    #[serde(default)]
    pub owns: ExtensionOwns,
    pub sha256: String,
    /// The core identity the extension was derived from (`lod_build.build_identity`).
    pub built_from: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stats: BTreeMap<String, u64>,
}

/// The `extensions` key of a conversion manifest; every other key is ignored.
#[derive(Debug, Default, Deserialize)]
struct ManifestExtensions {
    #[serde(default)]
    extensions: Vec<ExtensionEntry>,
}

/// Whether a data set carries an extension a feature needs, at a version it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionStatus {
    Present {
        version: u32,
    },
    Missing,
    /// Derived from another core build than the data set's current one.
    Stale {
        built_from: String,
    },
    /// A format this runtime does not read.
    UnsupportedVersion {
        version: u32,
        supported: RangeInclusive<u32>,
    },
}

impl ExtensionStatus {
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present { .. })
    }
}

impl fmt::Display for ExtensionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Present { version } => write!(f, "present (version {version})"),
            Self::Missing => write!(
                f,
                "not in this converted data set; convert it again with this converter"
            ),
            Self::Stale { built_from } => write!(
                f,
                "built from another conversion ({built_from}); convert the data set again"
            ),
            Self::UnsupportedVersion { version, supported } => write!(
                f,
                "version {version}, but this build reads versions {} to {}",
                supported.start(),
                supported.end()
            ),
        }
    }
}

/// The extensions of one loaded data set.
#[derive(Clone, Debug, Default)]
pub struct DataExtensions {
    entries: BTreeMap<String, ExtensionEntry>,
    core_identity: Option<String>,
}

impl DataExtensions {
    /// Reads the `extensions` list from a conversion manifest's text. `core_identity` is the data
    /// set's current core build identity, when it has one; without it no extension counts as stale.
    pub fn from_manifest_json(text: &str, core_identity: Option<String>) -> Result<Self> {
        let manifest: ManifestExtensions =
            serde_json::from_str(text).context("reading the manifest's extensions list")?;
        let mut entries = BTreeMap::new();
        for entry in manifest.extensions {
            if let Some(previous) = entries.insert(entry.id.clone(), entry) {
                bail!("extension {} is listed twice", previous.id);
            }
        }
        Ok(Self {
            entries,
            core_identity,
        })
    }

    /// Reads `conversion-manifest.json` in `root` and the core identity from `skyrim_world.db`'s
    /// `lod_build` table, when there is one.
    pub fn load(root: &Path) -> Result<Self> {
        let manifest_path = root.join("conversion-manifest.json");
        let text = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let database = root.join("skyrim_world.db");
        let core_identity = if database.is_file() {
            core_build_identity(&Connection::open_with_flags(
                &database,
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?)?
        } else {
            None
        };
        Self::from_manifest_json(&text, core_identity)
    }

    pub fn entries(&self) -> impl Iterator<Item = &ExtensionEntry> {
        self.entries.values()
    }

    pub fn get(&self, id: &str) -> Option<&ExtensionEntry> {
        self.entries.get(id)
    }

    /// The status of extension `id` for a reader of `supported` versions.
    pub fn status(&self, id: &str, supported: RangeInclusive<u32>) -> ExtensionStatus {
        let Some(entry) = self.entries.get(id) else {
            return ExtensionStatus::Missing;
        };
        if !supported.contains(&entry.version) {
            return ExtensionStatus::UnsupportedVersion {
                version: entry.version,
                supported,
            };
        }
        if let Some(core) = &self.core_identity
            && *core != entry.built_from
        {
            return ExtensionStatus::Stale {
                built_from: entry.built_from.clone(),
            };
        }
        ExtensionStatus::Present {
            version: entry.version,
        }
    }
}

/// The schemas of `connection` that can hold dataset objects: `main` and every attached database,
/// never `temp`. A converter stage connection is a cluster database with the base attached as
/// `base`; the engine's is the base with the overlay attached as `clusters`.
pub fn dataset_schemas(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare("PRAGMA database_list")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.into_iter().filter(|name| name != "temp").collect())
}

/// The one schema holding the table or index `name` (`kind` is `table` or `index`, names compare
/// ASCII case-insensitively like SQLite's own). `None` when no schema has it.
///
/// Two schemas holding the same object, or a `temp` object shadowing it, is an error: an unqualified
/// query would silently pick one of them, and a certified overlay table must never be answered by a
/// base table (or the other way round). For a table, a *view* of that name in any schema (including
/// `temp`) is refused the same way: a view would answer unqualified queries while the extension
/// hasher reads the real table.
pub fn locate_object(connection: &Connection, kind: &str, name: &str) -> Result<Option<String>> {
    let kinds: &[&str] = if kind == "table" {
        &["table", "view"]
    } else {
        &[kind]
    };
    let mut found: Option<String> = None;
    for schema in dataset_schemas(connection)? {
        let mut statement = connection.prepare(&format!(
            "SELECT type FROM {}.sqlite_master WHERE name = ?1 COLLATE NOCASE",
            quote_identifier(&schema)
        ))?;
        let types = statement
            .query_map([name], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut present = false;
        for found_type in types {
            if !kinds.contains(&found_type.as_str()) {
                continue;
            }
            ensure!(
                found_type == kind,
                "{name} is a {found_type} in the {schema} database; it would shadow the {kind}"
            );
            present = true;
        }
        if present {
            if let Some(first) = &found {
                bail!("{kind} {name} exists in both the {first} and {schema} databases");
            }
            found = Some(schema);
        }
    }
    let mut statement =
        connection.prepare("SELECT type FROM sqlite_temp_master WHERE name = ?1 COLLATE NOCASE")?;
    let temp = statement
        .query_map([name], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !temp
            .iter()
            .any(|found_type| kinds.contains(&found_type.as_str())),
        "{kind} {name} is shadowed by a temporary object"
    );
    Ok(found)
}

/// The core build identity a data set's extensions are derived from, if the database has one. The
/// `lod_build` table is found in whichever one schema of `connection` has it.
/// An absent table or no row means no identity; schema, value-type and read errors are propagated.
pub fn core_build_identity(connection: &Connection) -> Result<Option<String>> {
    let Some(schema) = locate_object(connection, "table", "lod_build")? else {
        return Ok(None);
    };
    let identity = connection
        .query_row(
            &format!(
                "SELECT build_identity FROM {}.lod_build LIMIT 1",
                quote_identifier(&schema)
            ),
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| format!("reading core build identity from {schema}.lod_build"))?;
    Ok(identity)
}

/// Problems with ownership across a list: two extensions owning the same item, or an id twice.
/// SQL table/index names compare with SQLite ASCII case equivalence. File and glTF names
/// retain their exact spelling, as do the names in the content hash stream.
pub fn ownership_problems<'a>(
    entries: impl IntoIterator<Item = &'a ExtensionEntry>,
) -> Vec<String> {
    let mut seen: BTreeMap<(&'static str, Cow<'a, str>), &'a str> = BTreeMap::new();
    let mut problems = Vec::new();
    for entry in entries {
        let kinds: [(&'static str, &'a Vec<String>); 4] = [
            ("table", &entry.owns.tables),
            ("index", &entry.owns.indexes),
            ("file", &entry.owns.files),
            ("gltf_extension", &entry.owns.gltf_extensions),
        ];
        for (kind, names) in kinds {
            for name in names {
                let key = if matches!(kind, "table" | "index") {
                    Cow::Owned(name.to_ascii_lowercase())
                } else {
                    Cow::Borrowed(name.as_str())
                };
                let owner = *seen.entry((kind, key)).or_insert(entry.id.as_str());
                if owner != entry.id {
                    problems.push(format!(
                        "{kind} {name} is owned by {owner} and {}",
                        entry.id
                    ));
                }
            }
        }
    }
    problems
}

/// Builds an extension's `mudcrab-ext-hash-v1` content hash. Parts may be added in any order; the
/// stream is put in the defined order by [`ExtensionHasher::finish`].
#[derive(Debug)]
pub struct ExtensionHasher {
    id: String,
    version: u32,
    tables: BTreeMap<String, TableSection>,
    indexes: BTreeMap<String, String>,
    files: BTreeMap<String, String>,
    gltf: BTreeMap<String, BTreeMap<String, String>>,
}

/// Rows share one allocation instead of repeatedly growing a string for every SQL row.
/// Ranges retain canonical bytewise ordering without moving the encoded bytes themselves.
#[derive(Debug)]
struct TableSection {
    header: String,
    rows: String,
    order: Vec<std::ops::Range<usize>>,
    columns: String,
}

impl ExtensionHasher {
    pub fn new(id: &str, version: u32) -> Self {
        Self {
            id: id.to_owned(),
            version,
            tables: BTreeMap::new(),
            indexes: BTreeMap::new(),
            files: BTreeMap::new(),
            gltf: BTreeMap::new(),
        }
    }

    /// Adds every row and the column declarations of `table`.
    pub fn add_table(&mut self, connection: &Connection, table: &str) -> Result<()> {
        let schema = locate_object(connection, "table", table)?
            .ok_or_else(|| color_eyre::eyre::eyre!("table {table} is missing"))?;
        let quoted = format!("{}.{}", quote_identifier(&schema), quote_identifier(table));
        let mut columns = Vec::new();
        let mut info = connection.prepare(&format!(
            "PRAGMA {}.table_info({})",
            quote_identifier(&schema),
            quote_identifier(table)
        ))?;
        let mut rows = info.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            let declared: String = row.get(2)?;
            columns.push(format!("{name}:{declared}"));
        }
        ensure!(!columns.is_empty(), "table {table} is missing");
        let mut encoded = String::new();
        let mut order = Vec::new();
        let mut select = connection.prepare(&format!("SELECT * FROM {quoted}"))?;
        let count = select.column_count();
        let mut rows = select.query([])?;
        while let Some(row) = rows.next()? {
            let start = encoded.len();
            for index in 0..count {
                if index > 0 {
                    encoded.push(',');
                }
                encode_sql(row.get_ref(index)?, &mut encoded);
            }
            order.push(start..encoded.len());
        }
        order.sort_unstable_by(|left, right| encoded[left.clone()].cmp(&encoded[right.clone()]));
        self.tables.insert(
            table.to_owned(),
            TableSection {
                header: format!("table {table}"),
                rows: encoded,
                order,
                columns: format!("columns {}", columns.join(",")),
            },
        );
        Ok(())
    }

    /// Adds an index's stored `CREATE INDEX` statement.
    pub fn add_index(&mut self, connection: &Connection, index: &str) -> Result<()> {
        let Some(schema) = locate_object(connection, "index", index)? else {
            bail!("index {index} is missing");
        };
        let sql: Option<String> = connection
            .query_row(
                &format!(
                    "SELECT sql FROM {}.sqlite_master WHERE type = 'index' AND name = ?1 COLLATE NOCASE",
                    quote_identifier(&schema)
                ),
                [index],
                |row| row.get(0),
            )
            .ok();
        let Some(sql) = sql else {
            bail!("index {index} is missing");
        };
        self.indexes
            .insert(index.to_owned(), format!("index {index} {sql}"));
        Ok(())
    }

    /// Adds one owned file by its path relative to the data set root (`/` separators).
    pub fn add_file(&mut self, relative_path: &str, bytes: &[u8]) {
        self.add_file_digest(relative_path, &hex(&Sha256::digest(bytes)));
    }

    /// Adds one owned file whose SHA-256 (64 lowercase hex digits) the caller already computed.
    pub fn add_file_digest(&mut self, relative_path: &str, sha256_hex: &str) {
        self.files.insert(
            relative_path.to_owned(),
            format!("file {relative_path} {sha256_hex}"),
        );
    }

    /// Adds the values of glTF extension `name` in one mesh document, if the mesh uses it.
    pub fn add_gltf(&mut self, name: &str, relative_path: &str, document: &serde_json::Value) {
        let used = document["extensionsUsed"]
            .as_array()
            .is_some_and(|used| used.iter().any(|value| value.as_str() == Some(name)));
        if !used {
            return;
        }
        let mut parts = Vec::new();
        if let Some(value) = document["extensions"].get(name) {
            parts.push(format!("/extensions/{name}={}", encode_json(value)));
        }
        if let Some(materials) = document["materials"].as_array() {
            for (index, material) in materials.iter().enumerate() {
                if let Some(value) = material["extensions"].get(name) {
                    parts.push(format!(
                        "/materials/{index}/extensions/{name}={}",
                        encode_json(value)
                    ));
                }
            }
        }
        if parts.is_empty() {
            return;
        }
        let digest = hex(&Sha256::digest(parts.join(";").as_bytes()));
        self.gltf.entry(name.to_owned()).or_default().insert(
            relative_path.to_owned(),
            format!("gltf {name} {relative_path} {digest}"),
        );
    }

    /// The lowercase hex SHA-256 of the stream.
    pub fn finish(self) -> String {
        let mut digest = Sha256::new();
        let mut line = |text: &str| {
            digest.update(text.as_bytes());
            digest.update(b"\n");
        };
        line(HASH_ALGORITHM);
        line(&self.id);
        line(&self.version.to_string());
        for section in self.tables.values() {
            line(&section.header);
            for range in &section.order {
                line(&section.rows[range.clone()]);
            }
            line(&section.columns);
        }
        for text in self.indexes.values() {
            line(text);
        }
        for text in self.files.values() {
            line(text);
        }
        for meshes in self.gltf.values() {
            for text in meshes.values() {
                line(text);
            }
        }
        hex(&digest.finalize())
    }
}

/// Hashes the database parts of an entry (tables and indexes) plus the given files and meshes.
pub fn hash_database_extension(
    connection: &Connection,
    id: &str,
    version: u32,
    owns: &ExtensionOwns,
) -> Result<String> {
    ensure!(
        owns.files.is_empty() && owns.gltf_extensions.is_empty(),
        "extension {id} owns files or glTF extensions; hash them with ExtensionHasher"
    );
    let mut hasher = ExtensionHasher::new(id, version);
    for table in &owns.tables {
        hasher.add_table(connection, table)?;
    }
    for index in &owns.indexes {
        hasher.add_index(connection, index)?;
    }
    Ok(hasher.finish())
}

/// `name` as a quoted SQLite identifier.
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    push_hex(bytes, &mut text);
    text
}

/// Append the exact ext-hash-v1 lowercase byte encoding without formatting or
/// allocating a temporary string for every SQL text/blob value.
fn push_hex(bytes: &[u8], text: &mut String) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    text.reserve(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
}

fn push_real(value: f64, out: &mut String) {
    use fmt::Write;
    let _ = write!(out, "f{:016x}", value.to_bits());
}

fn encode_sql(value: ValueRef<'_>, out: &mut String) {
    use fmt::Write;
    match value {
        ValueRef::Null => out.push('n'),
        ValueRef::Integer(value) => {
            let _ = write!(out, "i{value}");
        }
        ValueRef::Real(value) => push_real(value, out),
        ValueRef::Text(bytes) => {
            out.push('t');
            push_hex(bytes, out);
        }
        ValueRef::Blob(bytes) => {
            out.push('b');
            push_hex(bytes, out);
        }
    }
}

/// Canonical JSON: keys sorted bytewise, numbers as binary64 bits, strings as UTF-8 hex.
pub fn encode_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    encode_json_into(value, &mut out);
    out
}

fn encode_json_into(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => push_real(number.as_f64().unwrap_or(f64::NAN), out),
        Value::String(text) => {
            out.push('t');
            out.push_str(&hex(text.as_bytes()));
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                encode_json_into(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('t');
                out.push_str(&hex(key.as_bytes()));
                out.push(':');
                encode_json_into(&map[key], out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_sql_hex_matches_the_original_byte_encoding() {
        let bytes: Vec<u8> = (0..=255).cycle().take(65537).collect();
        for bytes in [&[][..], &bytes[..1], &bytes[..256], bytes.as_slice()] {
            let original: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex(bytes), original);
            for (value, prefix) in [(ValueRef::Text(bytes), "t"), (ValueRef::Blob(bytes), "b")] {
                let mut encoded = String::from("existing,");
                encode_sql(value, &mut encoded);
                assert_eq!(encoded, format!("existing,{prefix}{original}"));
            }
        }
    }

    #[test]
    fn table_arena_matches_the_original_rows_for_all_sql_types_and_duplicates() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE values_to_hash (n, i, f, t, b);
             INSERT INTO values_to_hash VALUES (NULL, -9223372036854775808, -0.125, '✓', X'00FF');
             INSERT INTO values_to_hash VALUES (NULL, 42, 1e100, '', X'');
             INSERT INTO values_to_hash SELECT * FROM values_to_hash;",
            )
            .unwrap();
        let mut hasher = ExtensionHasher::new("fixture", 1);
        hasher.add_table(&connection, "values_to_hash").unwrap();
        let section = &hasher.tables["values_to_hash"];
        let mut statement = connection.prepare("SELECT * FROM values_to_hash").unwrap();
        let mut rows = statement.query([]).unwrap();
        let mut original = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let mut line = String::new();
            for index in 0..5 {
                if index != 0 {
                    line.push(',');
                }
                encode_sql(row.get_ref(index).unwrap(), &mut line);
            }
            original.push(line);
        }
        original.sort_unstable();
        let arena: Vec<_> = section
            .order
            .iter()
            .map(|range| &section.rows[range.clone()])
            .collect();
        assert_eq!(arena, original);
        let mut expected = Sha256::new();
        for line in [HASH_ALGORITHM, "fixture", "1", &section.header]
            .into_iter()
            .chain(original.iter().map(String::as_str))
            .chain([section.columns.as_str()])
        {
            expected.update(line.as_bytes());
            expected.update(b"\n");
        }
        assert_eq!(hasher.finish(), hex(&expected.finalize()));
    }

    // These values also pin the neutral Python oracle in the converter integration tests.
    const PINNED_DOOR_LINKS: &str =
        "5c9f99438d7658c66aafebaa45045f6d553f0d52521309be28b1ef108db20e87";
    const PINNED_MATERIAL: &str =
        "714a8bfd6413db8bf63f22174dfb3f5c173a8126f93625d1c63a00b7df144567";

    fn sample_database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "
        CREATE TABLE statics (id INTEGER PRIMARY KEY, editor_id TEXT, model_path TEXT);
        INSERT INTO statics VALUES (16, 'RockA', 'meshes/rock.glb');
        CREATE TABLE door_links (ref_id INTEGER PRIMARY KEY, destination_ref_id INTEGER,
                                 pos_x REAL, pos_y REAL, pos_z REAL, note TEXT, data BLOB);
        INSERT INTO door_links VALUES (2, 1, 0.5, -1.25, 1e-7, 'Whiterun ✓', x'00ff');
        INSERT INTO door_links VALUES (1, 2, 3.0, 4.0, NULL, NULL, NULL);
        CREATE INDEX idx_door_links_destination ON door_links(destination_ref_id);
        ",
            )
            .unwrap();
        connection
    }

    fn door_links_owns() -> ExtensionOwns {
        ExtensionOwns {
            tables: vec!["door_links".into()],
            indexes: vec!["idx_door_links_destination".into()],
            ..Default::default()
        }
    }

    #[test]
    fn database_hash_matches_the_python_reference() {
        let connection = sample_database();
        let hash = hash_database_extension(&connection, DOOR_LINKS, 1, &door_links_owns()).unwrap();
        assert_eq!(hash, PINNED_DOOR_LINKS);
    }

    #[test]
    fn file_digest_equals_file_bytes() {
        let bytes = b"cluster bytes";
        let mut by_bytes = ExtensionHasher::new(DOOR_LINKS, 1);
        by_bytes.add_file("clusters/a.bin", bytes);
        let mut by_digest = ExtensionHasher::new(DOOR_LINKS, 1);
        by_digest.add_file_digest("clusters/a.bin", &hex(&Sha256::digest(bytes)));
        assert_eq!(by_bytes.finish(), by_digest.finish());
    }

    #[test]
    fn gltf_hash_matches_the_python_reference() {
        let rock = serde_json::json!({
            "asset": {"version": "2.0"},
            "extensionsUsed": ["MUDCRAB_material", "OPEN_SKYRIM_material"],
            "materials": [
                {"extensions": {"OPEN_SKYRIM_material": {"shaderFlags1": 1},
                                "MUDCRAB_material": {"uvScale": [1, 2.5], "blendSource": "SRC_ALPHA"}}},
                {"extensions": {"OPEN_SKYRIM_material": {"shaderFlags1": 2}}}
            ]
        });
        let plain = serde_json::json!({"asset": {"version": "2.0"}});
        let mut hasher = ExtensionHasher::new(MATERIAL, 1);
        hasher.add_gltf("MUDCRAB_material", "meshes/rock.glb", &rock);
        hasher.add_gltf("MUDCRAB_material", "meshes/plain.glb", &plain);
        assert_eq!(hasher.finish(), PINNED_MATERIAL);
    }

    #[test]
    fn a_row_change_changes_the_hash() {
        let connection = sample_database();
        connection
            .execute(
                "UPDATE door_links SET pos_x = 0.5000001 WHERE ref_id = 2",
                [],
            )
            .unwrap();
        let hash = hash_database_extension(&connection, DOOR_LINKS, 1, &door_links_owns()).unwrap();
        assert_ne!(hash, PINNED_DOOR_LINKS);
    }

    #[test]
    fn a_missing_table_is_an_error() {
        let connection = Connection::open_in_memory().unwrap();
        let error =
            hash_database_extension(&connection, DOOR_LINKS, 1, &door_links_owns()).unwrap_err();
        assert!(error.to_string().contains("door_links is missing"));
    }

    fn entry(id: &str, version: u32, built_from: &str) -> ExtensionEntry {
        ExtensionEntry {
            id: id.into(),
            version,
            owns: ExtensionOwns::default(),
            sha256: String::new(),
            built_from: built_from.into(),
            stats: BTreeMap::new(),
        }
    }

    #[test]
    fn status_reports_each_case() {
        let manifest = serde_json::json!({
            "schema_version": 26,
            "complete": true,
            "extensions": [entry(DOOR_LINKS, 1, "core"), entry(MATERIAL, 3, "core"),
                           entry(SPACE_LIGHTING, 1, "older")]
        });
        let data =
            DataExtensions::from_manifest_json(&manifest.to_string(), Some("core".into())).unwrap();
        assert_eq!(
            data.status(DOOR_LINKS, 1..=1),
            ExtensionStatus::Present { version: 1 }
        );
        assert_eq!(data.status(LOD_INVENTORY, 1..=1), ExtensionStatus::Missing);
        assert_eq!(
            data.status(MATERIAL, 1..=2),
            ExtensionStatus::UnsupportedVersion {
                version: 3,
                supported: 1..=2
            }
        );
        assert_eq!(
            data.status(SPACE_LIGHTING, 1..=1),
            ExtensionStatus::Stale {
                built_from: "older".into()
            }
        );
    }

    #[test]
    fn a_main_manifest_has_no_extensions() {
        let data =
            DataExtensions::from_manifest_json(r#"{"schema_version": 26, "complete": true}"#, None)
                .unwrap();
        assert_eq!(data.entries().count(), 0);
        assert_eq!(data.status(DOOR_LINKS, 1..=1), ExtensionStatus::Missing);
    }

    #[test]
    fn sql_ownership_uses_ascii_case_but_preserves_other_name_rules() {
        for (first, second, conflicts) in [
            ("space_lighting", "SPACE_LIGHTING", true),
            ("idx_lod_tree_block", "IDX_LOD_TREE_BLOCK", true),
            ("Mixed_Ä", "mIXED_Ä", true),
            ("Ä", "ä", false),
            ("Σ", "σ", false),
            ("ß", "ss", false),
        ] {
            let mut a = entry("example.a", 1, "core");
            let mut b = entry("example.b", 1, "core");
            a.owns.tables = vec![first.into()];
            b.owns.tables = vec![second.into()];
            assert_eq!(!ownership_problems([&a, &b]).is_empty(), conflicts);
            a.owns.tables.clear();
            b.owns.tables.clear();
            a.owns.indexes = vec![first.into()];
            b.owns.indexes = vec![second.into()];
            assert_eq!(!ownership_problems([&b, &a]).is_empty(), conflicts);
        }
        let mut a = entry("example.a", 1, "core");
        let mut b = entry("example.b", 1, "core");
        a.owns.files = vec!["meshes/Asset.glb".into()];
        b.owns.files = vec!["meshes/asset.glb".into()];
        a.owns.gltf_extensions = vec!["EXAMPLE_payload".into()];
        b.owns.gltf_extensions = vec!["example_payload".into()];
        assert!(ownership_problems([&a, &b]).is_empty());
        b.owns.files = a.owns.files.clone();
        b.owns.gltf_extensions = a.owns.gltf_extensions.clone();
        assert_eq!(ownership_problems([&a, &b]).len(), 2);
    }

    #[test]
    fn core_identity_preserves_absent_and_empty_row_compatibility() {
        let connection = Connection::open_in_memory().unwrap();
        assert_eq!(core_build_identity(&connection).unwrap(), None);
        connection
            .execute_batch(
                "ATTACH ':memory:' AS base;
            CREATE TABLE base.lod_build(id INTEGER PRIMARY KEY, build_identity TEXT);",
            )
            .unwrap();
        assert_eq!(core_build_identity(&connection).unwrap(), None);
        connection
            .execute_batch("INSERT INTO base.lod_build VALUES(1, 'core');")
            .unwrap();
        assert_eq!(
            core_build_identity(&connection).unwrap(),
            Some("core".into())
        );
        connection
            .execute_batch("UPDATE base.lod_build SET build_identity = '';")
            .unwrap();
        assert_eq!(
            core_build_identity(&connection).unwrap(),
            Some(String::new())
        );
    }

    #[test]
    fn core_identity_rejects_malformed_columns_and_values() {
        for schema in ["main", "base"] {
            for sql in [
                "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, renamed_identity TEXT);",
                "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity);
                 INSERT INTO lod_build VALUES(1, 42);",
                "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity);
                 INSERT INTO lod_build VALUES(1, X'636f7265');",
                "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity);
                 INSERT INTO lod_build VALUES(1, NULL);",
            ] {
                let connection = Connection::open_in_memory().unwrap();
                if schema == "base" {
                    connection
                        .execute_batch("ATTACH ':memory:' AS base;")
                        .unwrap();
                }
                connection
                    .execute_batch(&sql.replace("lod_build", &format!("{schema}.lod_build")))
                    .unwrap();
                let error = core_build_identity(&connection).unwrap_err();
                let context = format!("{error:#}");
                assert!(context.contains("reading core build identity"), "{context}");
                assert!(context.contains(schema), "{context}");
                assert!(error.downcast_ref::<rusqlite::Error>().is_some());
            }
        }
    }

    #[test]
    fn core_identity_rejects_ambiguous_or_shadowed_objects() {
        for sql in [
            "CREATE TABLE lod_build(build_identity TEXT);
             ATTACH ':memory:' AS base; CREATE TABLE base.lod_build(build_identity TEXT);",
            "CREATE TABLE lod_build(build_identity TEXT);
             CREATE TEMP TABLE lod_build(build_identity TEXT);",
            "CREATE VIEW lod_build AS SELECT 'core' AS build_identity;",
        ] {
            let connection = Connection::open_in_memory().unwrap();
            connection.execute_batch(sql).unwrap();
            assert!(core_build_identity(&connection).is_err());
        }
    }

    #[test]
    fn duplicate_ids_and_shared_items_are_refused() {
        let manifest = serde_json::json!({"extensions": [entry(DOOR_LINKS, 1, "c"), entry(DOOR_LINKS, 1, "c")]});
        assert!(DataExtensions::from_manifest_json(&manifest.to_string(), None).is_err());
        let mut a = entry(DOOR_LINKS, 1, "c");
        a.owns.tables = vec!["door_links".into()];
        let mut b = entry(MATERIAL, 1, "c");
        b.owns.tables = vec!["door_links".into()];
        assert_eq!(
            ownership_problems([&a, &b]),
            vec!["table door_links is owned by mudcrab.door_links and mudcrab.material".to_owned()]
        );
    }
}
