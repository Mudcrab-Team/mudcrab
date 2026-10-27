//! Validated dataset capability state and the ordinary database's door access policy.
//!
//! Undeclared complete core door tables remain available. A declared legacy door extension
//! needs a supported version and must match any recorded core identity; stale or unsupported declarations
//! disable its join. A usable declaration with missing or incomplete columns is an error.
//!
//! This module installs capability and notice resources. Declaration checks do
//! not recompute payload hashes. See `docs/specs/converters/extensions.md`.

use std::ops::RangeInclusive;
use std::path::Path;

use bevy::prelude::*;
use color_eyre::eyre::{Result, WrapErr};
use shared::extensions::{DOOR_LINKS, DataExtensions, ExtensionStatus};

use crate::world::database::{DoorLinksAccess, WorldDatabaseFeatures, has_door_links};

/// An engine feature that needs a data set extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Feature {
    /// Ordinary load-door destinations in [`crate::world::database`].
    DoorLinks,
}

impl Feature {
    /// Every feature, in the order notices are listed.
    pub const ALL: [Self; 1] = [Self::DoorLinks];

    /// The extension id the feature reads.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::DoorLinks => DOOR_LINKS,
        }
    }

    /// What the settings menu and the log call the feature.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DoorLinks => "load-door links",
        }
    }

    /// The versions of the extension this engine reads.
    pub const fn versions(self) -> RangeInclusive<u32> {
        1..=1
    }
}

/// One capability that is unavailable in the loaded dataset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionNotice {
    /// The extension id (`mudcrab.door_links`).
    pub extension: &'static str,
    /// The feature's name, for the menu entry ([`Feature::label`]).
    pub feature: &'static str,
    /// Why it is off and what to do, the [`ExtensionStatus`] text.
    pub reason: String,
}

/// Capability notices in [`Feature::ALL`] order, available to later settings consumers.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionNotices(Vec<ExtensionNotice>);

impl ExtensionNotices {
    pub fn iter(&self) -> impl Iterator<Item = &ExtensionNotice> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The notice for a feature, if it is off.
    pub fn for_feature(&self, feature: Feature) -> Option<&ExtensionNotice> {
        self.0
            .iter()
            .find(|notice| notice.extension == feature.extension())
    }
}

/// The extensions of the loaded data set, read once when it is opened.
#[derive(Resource, Clone, Debug, Default)]
pub struct DataSetExtensions {
    data: DataExtensions,
}

impl DataSetExtensions {
    pub fn new(data: DataExtensions) -> Self {
        Self { data }
    }

    /// The status of the extension a feature needs, for the versions this engine reads.
    pub fn status(&self, feature: Feature) -> ExtensionStatus {
        self.data.status(feature.extension(), feature.versions())
    }

    pub fn data(&self) -> &DataExtensions {
        &self.data
    }
}

/// Reads the extensions list of `assets_dir`'s `conversion-manifest.json` (and the core build
/// identity from its database). A dataset without a manifest lists no extensions; ordinary core
/// doors still use schema discovery. A manifest that cannot be read is an error.
pub fn read_data_extensions(assets_dir: &Path) -> Result<DataExtensions> {
    if !assets_dir.join("conversion-manifest.json").is_file() {
        return Ok(DataExtensions::default());
    }
    DataExtensions::load(assets_dir)
        .wrap_err_with(|| format!("cannot read the extensions of {}", assets_dir.display()))
}

/// The text kept in a notice and logged: the status, plus what to do when the status alone does not
/// say it.
fn reason(status: &ExtensionStatus) -> String {
    match status {
        ExtensionStatus::UnsupportedVersion { .. } => {
            format!("{status}; update the engine, or convert the data set again with this build")
        }
        _ => status.to_string(),
    }
}

/// The public capability state used by the database and mesh readers.
/// Private extension catalogs are installed by their introducing adapters.
pub struct RuntimeFeatures {
    pub extensions: DataSetExtensions,
    pub notices: ExtensionNotices,
    door_links: DoorLinksAccess,
    has_doors: bool,
}

impl RuntimeFeatures {
    pub fn world_database(&self) -> WorldDatabaseFeatures {
        WorldDatabaseFeatures {
            door_links: self.door_links,
        }
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::DoorLinks => self.has_doors && self.door_links != DoorLinksAccess::Disabled,
        }
    }

    pub fn install(self, app: &mut App) {
        app.insert_resource(self.extensions)
            .insert_resource(self.notices);
    }
}

/// Opens the public capability gates without importing private feature readers.
/// An invalid legacy declaration cannot bypass the gate through core discovery.
pub fn open_features(assets_dir: &Path, data: DataExtensions) -> Result<RuntimeFeatures> {
    let extensions = DataSetExtensions::new(data);
    let database = assets_dir.join("skyrim_world.db");
    let has_doors = if database.is_file() {
        let connection = rusqlite::Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        has_door_links(&connection)?
    } else {
        false
    };
    let declared = extensions.data().get(DOOR_LINKS).is_some();
    let door_links = if !declared {
        DoorLinksAccess::Core
    } else if extensions.status(Feature::DoorLinks).is_present() {
        color_eyre::eyre::ensure!(
            has_doors,
            "extension {} is listed but its door_links table lacks columns",
            DOOR_LINKS
        );
        DoorLinksAccess::LegacyExtension
    } else {
        DoorLinksAccess::Disabled
    };
    let mut notices = Vec::new();
    for feature in Feature::ALL {
        if feature == Feature::DoorLinks && !declared && has_doors {
            continue;
        }
        let status = extensions.status(feature);
        if !status.is_present() {
            let reason = reason(&status);
            warn!(
                feature = feature.label(),
                extension = feature.extension(),
                "{} is off: extension {}: {reason}",
                feature.label(),
                feature.extension()
            );
            notices.push(ExtensionNotice {
                extension: feature.extension(),
                feature: feature.label(),
                reason,
            });
        }
    }
    Ok(RuntimeFeatures {
        extensions,
        notices: ExtensionNotices(notices),
        door_links,
        has_doors,
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use rusqlite::Connection;
    use shared::extensions::{ExtensionEntry, ExtensionOwns};
    use std::collections::BTreeMap;

    /// Synthetic core door schema and unrelated payloads for capability status tests.
    pub const TABLES: &str = "
        CREATE TABLE cells(id INTEGER PRIMARY KEY, worldspace_id INTEGER);
        CREATE TABLE \"references\"(id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
            pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL);
        CREATE TABLE door_links(ref_id INTEGER PRIMARY KEY, destination_ref_id INTEGER NOT NULL,
            pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
            rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
            destination_cell_id INTEGER, destination_worldspace_id INTEGER);
        INSERT INTO cells VALUES(5, 60);
        INSERT INTO cells VALUES(100, NULL);
        INSERT INTO \"references\" VALUES(1, 5, 10, 20, 30);
        INSERT INTO \"references\" VALUES(2, 100, 1, 2, 3);
        INSERT INTO door_links VALUES(1, 2, 1, 2, 3, 0, 0, 1.5, 100, NULL);
        INSERT INTO door_links VALUES(2, 1, 10, 20, 30, 0, 0, 0.5, 5, 60);
    ";

    /// The manifest of a `main` data set: no `extensions` key.
    pub const MAIN_MANIFEST: &str = r#"{"schema_version": 26, "complete": true}"#;

    /// A manifest text listing `entries` as `(id, version, built_from)`.
    pub fn manifest(entries: &[(&str, u32, &str)]) -> String {
        let extensions: Vec<ExtensionEntry> = entries
            .iter()
            .map(|(id, version, built_from)| ExtensionEntry {
                id: (*id).to_owned(),
                version: *version,
                owns: ExtensionOwns::default(),
                sha256: String::new(),
                built_from: (*built_from).to_owned(),
                stats: BTreeMap::new(),
            })
            .collect();
        serde_json::json!({"schema_version": 26, "complete": true, "extensions": extensions})
            .to_string()
    }

    /// A data set folder: the manifest text and a database holding [`TABLES`] (without the ones in
    /// `without`), whose `lod_build` identity is `core` when it is given.
    pub fn data_set(
        manifest_text: &str,
        core: Option<&str>,
        without: &[&str],
    ) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("conversion-manifest.json"),
            manifest_text,
        )
        .unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection.execute_batch(TABLES).unwrap();
        for table in without {
            connection
                .execute_batch(&format!("DROP TABLE \"{table}\";"))
                .unwrap();
        }
        if let Some(core) = core {
            connection
                .execute_batch(&format!(
                    "CREATE TABLE lod_build(id INTEGER PRIMARY KEY CHECK (id=1), build_identity TEXT NOT NULL);
                     INSERT INTO lod_build VALUES(1, '{core}');"
                ))
                .unwrap();
        }
        directory
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{MAIN_MANIFEST, data_set, manifest};
    use super::*;
    fn open(directory: &tempfile::TempDir) -> Result<RuntimeFeatures> {
        open_features(directory.path(), read_data_extensions(directory.path())?)
    }

    fn only(feature: Feature, version: u32, built_from: &str) -> String {
        manifest(&[(feature.extension(), version, built_from)])
    }

    #[test]
    fn core_door_links_need_no_legacy_extension_declaration() {
        let directory = data_set(MAIN_MANIFEST, None, &[]);
        let features = open(&directory).unwrap();
        assert!(features.enabled(Feature::DoorLinks));
        assert!(features.notices.for_feature(Feature::DoorLinks).is_none());
    }

    #[test]
    fn converter_core_schema_opens_without_a_door_extension() {
        let directory = tempfile::tempdir().unwrap();
        let connection =
            rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        drop(connection);
        std::fs::write(directory.path().join("conversion-manifest.json"),
            serde_json::json!({"schema_version": shared::LOD_CONVERTER_SCHEMA_VERSION, "complete": true}).to_string()).unwrap();
        let features = open(&directory).unwrap();
        assert!(features.enabled(Feature::DoorLinks));
        assert_eq!(features.world_database().door_links, DoorLinksAccess::Core);
        assert!(features.extensions.data().get(DOOR_LINKS).is_none());
        crate::world::database::WorldDatabase::open_with(
            &directory.path().join("skyrim_world.db"),
            features.world_database(),
        )
        .unwrap();
    }

    #[test]
    fn a_missing_manifest_lists_no_extensions() {
        let directory = tempfile::tempdir().unwrap();
        let data = read_data_extensions(directory.path()).unwrap();
        assert_eq!(data.entries().count(), 0);
    }

    #[test]
    fn a_manifest_that_cannot_be_read_is_an_error() {
        let directory = data_set("{ not json", None, &[]);
        assert!(read_data_extensions(directory.path()).is_err());
    }

    #[test]
    fn a_stale_extension_is_off_with_the_reason() {
        for feature in Feature::ALL {
            let directory = data_set(&only(feature, 1, "older"), Some("core"), &[]);
            let features = open(&directory).unwrap();
            let notice = features.notices.for_feature(feature).unwrap();
            assert!(
                notice
                    .reason
                    .contains("built from another conversion (older)")
                    && notice.reason.contains("convert the data set again"),
                "{}",
                notice.reason
            );
            assert!(!features.enabled(feature));
            if feature == Feature::DoorLinks {
                assert_eq!(
                    features.world_database().door_links,
                    DoorLinksAccess::Disabled
                );
            }
        }
    }

    #[test]
    fn legacy_doors_allow_absent_identity_but_reject_malformed_identity() {
        for sql in [
            "",
            "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity TEXT);",
        ] {
            let directory = data_set(&only(Feature::DoorLinks, 1, "older"), None, &[]);
            let connection =
                rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
            connection.execute_batch(sql).unwrap();
            drop(connection);
            let features = open(&directory).unwrap();
            assert!(features.enabled(Feature::DoorLinks));
            assert_eq!(
                features.world_database().door_links,
                DoorLinksAccess::LegacyExtension
            );
        }
        for sql in [
            "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, renamed_identity TEXT);",
            "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity);
             INSERT INTO lod_build VALUES(1, 42);",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let connection =
                rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
            converter::esm::exporter::create_tables(&connection).unwrap();
            // Keep the current-schema no-LOD path: complete doors, zero chunks, no LOD manifest.
            connection.execute_batch("DROP TABLE lod_build;").unwrap();
            connection.execute_batch(sql).unwrap();
            drop(connection);
            std::fs::write(
                directory.path().join("conversion-manifest.json"),
                only(Feature::DoorLinks, 1, "older"),
            )
            .unwrap();
            let error = open(&directory)
                .err()
                .expect("malformed identity must fail loading");
            let context = format!("{error:#}");
            assert!(context.contains("cannot read the extensions"), "{context}");
            assert!(context.contains("reading core build identity"), "{context}");
        }
    }

    #[test]
    fn an_unsupported_version_is_off_with_the_reason() {
        for feature in Feature::ALL {
            let directory = data_set(&only(feature, 2, "core"), Some("core"), &[]);
            let features = open(&directory).unwrap();
            let notice = features.notices.for_feature(feature).unwrap();
            assert!(
                notice
                    .reason
                    .contains("version 2, but this build reads versions 1 to 1")
                    && notice.reason.contains("convert the data set again"),
                "{}",
                notice.reason
            );
            assert!(!features.enabled(feature));
            if feature == Feature::DoorLinks {
                assert_eq!(
                    features.world_database().door_links,
                    DoorLinksAccess::Disabled
                );
            }
        }
    }

    #[test]
    fn extensions_the_engine_does_not_read_are_ignored() {
        let directory = data_set(
            &manifest(&[("example.payload", 1, "core"), ("someone.else", 9, "x")]),
            Some("core"),
            &[],
        );
        let features = open(&directory).unwrap();
        assert_eq!(features.notices.len(), 0);
        assert!(features.enabled(Feature::DoorLinks));
    }
    #[test]
    fn a_declared_door_extension_requires_its_complete_table() {
        let directory = data_set(
            &only(Feature::DoorLinks, 1, "core"),
            Some("core"),
            &["door_links"],
        );
        let error = open(&directory)
            .err()
            .expect("missing declared table must fail");
        assert!(format!("{error:#}").contains("door_links"));
    }

    #[test]
    fn declared_legacy_doors_reject_incomplete_columns() {
        let directory = data_set(&only(Feature::DoorLinks, 1, "core"), Some("core"), &[]);
        let connection =
            rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch("ALTER TABLE door_links DROP COLUMN rot_z;")
            .unwrap();
        let error = open(&directory)
            .err()
            .expect("incomplete declared table must fail");
        assert!(format!("{error:#}").contains("door_links"));
    }

    #[test]
    fn install_retains_validated_capabilities_and_notices() {
        let directory = data_set(&only(Feature::DoorLinks, 2, "core"), Some("core"), &[]);
        let features = open(&directory).unwrap();
        let mut app = App::new();
        features.install(&mut app);
        assert!(matches!(
            app.world()
                .resource::<DataSetExtensions>()
                .status(Feature::DoorLinks),
            ExtensionStatus::UnsupportedVersion { .. }
        ));
        assert!(
            app.world()
                .resource::<ExtensionNotices>()
                .for_feature(Feature::DoorLinks)
                .is_some()
        );
    }

    #[test]
    fn legacy_door_open_accepts_sqlite_table_case_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("skyrim_world.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE door_links RENAME TO renamed_door_links;
             ALTER TABLE renamed_door_links RENAME TO DOOR_LINKS;",
            )
            .unwrap();
        drop(connection);
        std::fs::write(
            directory.path().join("conversion-manifest.json"),
            only(Feature::DoorLinks, 1, "core"),
        )
        .unwrap();
        let features = open(&directory).unwrap();
        assert_eq!(
            features.world_database().door_links,
            DoorLinksAccess::LegacyExtension
        );
        crate::world::database::WorldDatabase::open_with(&path, features.world_database()).unwrap();
    }

    #[test]
    fn database_open_rejects_missing_or_incomplete_declared_legacy_doors() {
        for drop_column in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("skyrim_world.db");
            let connection = rusqlite::Connection::open(&path).unwrap();
            converter::esm::exporter::create_tables(&connection).unwrap();
            connection
                .execute_batch(if drop_column {
                    "ALTER TABLE door_links DROP COLUMN rot_z;"
                } else {
                    "DROP TABLE door_links;"
                })
                .unwrap();
            drop(connection);
            let error = crate::world::database::WorldDatabase::open_with(
                &path,
                WorldDatabaseFeatures {
                    door_links: DoorLinksAccess::LegacyExtension,
                },
            )
            .err()
            .expect("declared malformed legacy schema must fail at open");
            assert!(format!("{error:#}").contains("door_links"));
            // Undeclared legacy core packages retain the existing optional-table behavior.
            crate::world::database::WorldDatabase::open(&path).unwrap();
        }
    }
}
