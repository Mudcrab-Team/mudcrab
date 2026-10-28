//! Validated dataset capability state and the ordinary database's door access policy.
//!
//! Undeclared complete core door tables remain available. A declared legacy door extension
//! needs a supported version and must match any recorded core identity; stale or unsupported declarations
//! disable its join. A usable declaration with missing or incomplete columns is an error.
//!
//! Declaration checks do not recompute payload hashes. The database validates door columns
//! when it opens. See `docs/specs/converters/extensions.md`.

use std::ops::RangeInclusive;
use std::path::Path;

use bevy::prelude::*;
use color_eyre::eyre::{Result, WrapErr};
use shared::extensions::{
    DOOR_LINKS, DataExtensions, ExtensionStatus, MATERIAL, core_build_identity,
};

use crate::world::database::{DoorLinksAccess, WorldDatabaseFeatures};

/// An engine feature that needs a data set extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Feature {
    /// Ordinary load-door destinations in [`crate::world::database`].
    DoorLinks,
    /// Authored material animation capability consumed by the glTF loader.
    Material,
}

impl Feature {
    /// Every feature, in the order notices are listed.
    pub const ALL: [Self; 2] = [Self::DoorLinks, Self::Material];

    /// The extension id the feature reads.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::DoorLinks => DOOR_LINKS,
            Self::Material => MATERIAL,
        }
    }

    /// The feature's name in notices and logs.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DoorLinks => "load-door links",
            Self::Material => "material animation",
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
    /// The feature's name ([`Feature::label`]).
    pub feature: &'static str,
    /// Why it is off and what to do.
    pub reason: String,
}

/// Capability notices in [`Feature::ALL`] order.
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

/// Reads `conversion-manifest.json`, consulting the database's core identity only for a supported
/// known declaration with a freshness anchor. A dataset without a manifest lists no extensions;
/// ordinary core doors still use schema discovery. A manifest that cannot be read is an error.
pub fn read_data_extensions(assets_dir: &Path) -> Result<DataExtensions> {
    let manifest = assets_dir.join("conversion-manifest.json");
    if !manifest.is_file() {
        return Ok(DataExtensions::default());
    }
    let text = std::fs::read_to_string(&manifest)
        .wrap_err_with(|| format!("reading {}", manifest.display()))?;
    let data = DataExtensions::from_manifest_json(&text, None)
        .wrap_err_with(|| format!("cannot read the extensions of {}", assets_dir.display()))?;
    let needs_identity = Feature::ALL.into_iter().any(|feature| {
        data.get(feature.extension()).is_some_and(|entry| {
            feature.versions().contains(&entry.version) && !entry.built_from.is_empty()
        })
    });
    if !needs_identity {
        return Ok(data);
    }
    let database = assets_dir.join("skyrim_world.db");
    let core_identity = if database.is_file() {
        let connection = rusqlite::Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .wrap_err_with(|| format!("opening {}", database.display()))?;
        core_build_identity(&connection)
            .wrap_err_with(|| format!("cannot read the extensions of {}", assets_dir.display()))?
    } else {
        None
    };
    DataExtensions::from_manifest_json(&text, core_identity)
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

/// Selects the door access policy and capability notices from the manifest.
/// An invalid legacy declaration cannot bypass the gate through core discovery.
pub fn open_features(
    data: DataExtensions,
) -> (DataSetExtensions, ExtensionNotices, WorldDatabaseFeatures) {
    let extensions = DataSetExtensions::new(data);
    let declared = extensions.data().is_declared(DOOR_LINKS);
    let door_links = if !declared {
        DoorLinksAccess::Core
    } else if extensions.status(Feature::DoorLinks).is_present() {
        DoorLinksAccess::LegacyExtension
    } else {
        DoorLinksAccess::Disabled
    };
    let mut notices = Vec::new();
    for feature in Feature::ALL {
        if !extensions.data().is_declared(feature.extension()) {
            continue;
        }
        let status = extensions.status(feature);
        if !status.is_present() {
            let reason = match extensions
                .data()
                .issues()
                .find(|issue| issue.id.as_deref() == Some(feature.extension()))
            {
                Some(issue) => format!(
                    "invalid declaration: {}; convert the data set again",
                    issue.reason
                ),
                None => reason(&status),
            };
            // Capability selection precedes LogPlugin installation.
            eprintln!(
                "warning: {} is off: extension {}: {reason}",
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
    (
        extensions,
        ExtensionNotices(notices),
        WorldDatabaseFeatures { door_links },
    )
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use rusqlite::Connection;
    use shared::extensions::{ExtensionEntry, ExtensionOwns};
    use std::collections::BTreeMap;

    /// Synthetic core door rows for capability status tests using the converter schema.
    pub const TABLES: &str = "
        INSERT INTO cells(id, worldspace_id, flags) VALUES(5, 60, 0);
        INSERT INTO cells(id, worldspace_id, flags) VALUES(100, NULL, 0);
        INSERT INTO \"references\"(id, cell_id, base_form_id, is_exterior,
            pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
            VALUES(1, 5, 1, 1, 10, 20, 30, 0, 0, 0);
        INSERT INTO \"references\"(id, cell_id, base_form_id, is_exterior,
            pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
            VALUES(2, 100, 2, 0, 1, 2, 3, 0, 0, 0);
        INSERT INTO door_links(ref_id, destination_ref_id, pos_x, pos_y, pos_z,
            rot_x, rot_y, rot_z, destination_cell_id, destination_worldspace_id)
            VALUES(1, 2, 1, 2, 3, 0, 0, 1.5, 100, NULL);
        INSERT INTO door_links(ref_id, destination_ref_id, pos_x, pos_y, pos_z,
            rot_x, rot_y, rot_z, destination_cell_id, destination_worldspace_id)
            VALUES(2, 1, 10, 20, 30, 0, 0, 0.5, 5, 60);
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

    /// A data set folder: the manifest text and current converter schema with [`TABLES`] rows,
    /// excluding tables in `without`. Its `lod_build` identity is `core` when given.
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
        converter::esm::exporter::create_tables(&connection).unwrap();
        // Keep absent-identity cases distinct from an empty or malformed identity table.
        connection.execute_batch("DROP TABLE lod_build;").unwrap();
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

    #[test]
    fn capability_rejection_is_visible_before_logging() {
        const CHILD: &str = "MUDCRAB_TEST_CAPABILITY_STDERR_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let directory = data_set(&only(Feature::DoorLinks, 2, "older"), Some("core"), &[]);
            let (_, notices, _) = open(&directory).unwrap();
            assert_eq!(notices.len(), 1);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "extensions::tests::capability_rejection_is_visible_before_logging",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("load-door links is off"),
            "pre-logging reason absent from stderr: {stderr}"
        );
        assert!(
            stderr.contains("mudcrab.door_links"),
            "extension id absent: {stderr}"
        );
        assert!(
            stderr.contains("update the engine"),
            "remediation absent: {stderr}"
        );
    }
    fn open(
        directory: &tempfile::TempDir,
    ) -> Result<(DataSetExtensions, ExtensionNotices, WorldDatabaseFeatures)> {
        Ok(open_features(read_data_extensions(directory.path())?))
    }

    fn only(feature: Feature, version: u32, built_from: &str) -> String {
        manifest(&[(feature.extension(), version, built_from)])
    }

    #[test]
    fn core_door_links_need_no_legacy_extension_declaration() {
        let directory = data_set(MAIN_MANIFEST, None, &[]);
        let (_, notices, database_features) = open(&directory).unwrap();
        crate::world::database::WorldDatabase::open_with(
            &directory.path().join("skyrim_world.db"),
            database_features,
        )
        .unwrap();
        assert_eq!(database_features.door_links, DoorLinksAccess::Core);
        assert!(notices.for_feature(Feature::DoorLinks).is_none());
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
        let (extensions, notices, database_features) = open(&directory).unwrap();
        assert_eq!(database_features.door_links, DoorLinksAccess::Core);
        assert!(extensions.data().get(DOOR_LINKS).is_none());
        assert!(notices.is_empty());
        crate::world::database::WorldDatabase::open_with(
            &directory.path().join("skyrim_world.db"),
            database_features,
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
    fn irrelevant_registries_do_not_read_an_odd_core_identity() {
        let cases = [
            (MAIN_MANIFEST.to_owned(), DoorLinksAccess::Core, 0),
            (r#"{"extensions":[]}"#.to_owned(), DoorLinksAccess::Core, 0),
            (
                r#"{"extensions":null}"#.to_owned(),
                DoorLinksAccess::Core,
                0,
            ),
            (r#"{"extensions":{}}"#.to_owned(), DoorLinksAccess::Core, 0),
            (
                manifest(&[("example.payload", 1, "older")]),
                DoorLinksAccess::Core,
                0,
            ),
            (
                r#"{"extensions":[{"id":"example.payload","version":"future"}]}"#.to_owned(),
                DoorLinksAccess::Core,
                0,
            ),
            (
                r#"{"extensions":[null,42,{"version":1}]}"#.to_owned(),
                DoorLinksAccess::Core,
                0,
            ),
            (
                only(Feature::DoorLinks, 2, "older"),
                DoorLinksAccess::Disabled,
                1,
            ),
            (
                only(Feature::DoorLinks, 1, ""),
                DoorLinksAccess::LegacyExtension,
                0,
            ),
        ];
        for (text, access, notice_count) in cases {
            for sql in [
                "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, renamed_identity TEXT);",
                "CREATE TABLE lod_build(id INTEGER PRIMARY KEY, build_identity);
                 INSERT INTO lod_build VALUES(1, 42);",
            ] {
                let directory = data_set(&text, None, &[]);
                let connection =
                    rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
                connection.execute_batch(sql).unwrap();
                drop(connection);
                let (_, notices, database_features) = open(&directory).unwrap();
                assert_eq!(database_features.door_links, access, "{text}; {sql}");
                assert_eq!(notices.len(), notice_count, "{text}");
                assert!(notices.for_feature(Feature::Material).is_none(), "{text}");
            }
        }
    }

    #[test]
    fn a_malformed_legacy_door_declaration_disables_core_discovery_with_a_notice() {
        let text = r#"{"extensions":[{"id":"mudcrab.door_links","version":"broken"}]}"#;
        let directory = data_set(text, None, &[]);
        let connection =
            rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch("CREATE TABLE lod_build(renamed_identity TEXT);")
            .unwrap();
        drop(connection);
        let (extensions, notices, database_features) = open(&directory).unwrap();
        assert!(extensions.data().is_declared(DOOR_LINKS));
        assert_eq!(
            extensions.status(Feature::DoorLinks),
            ExtensionStatus::Missing
        );
        assert_eq!(database_features.door_links, DoorLinksAccess::Disabled);
        let issue = extensions
            .data()
            .issues()
            .find(|issue| issue.id.as_deref() == Some(DOOR_LINKS))
            .unwrap();
        let notice = notices.for_feature(Feature::DoorLinks).unwrap();
        assert!(notice.reason.contains(&issue.reason), "{}", notice.reason);
        assert_eq!(notices.len(), 1);
        assert!(notices.for_feature(Feature::Material).is_none());
    }

    #[test]
    fn undeclared_features_do_not_open_the_database() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("skyrim_world.db"), b"not sqlite").unwrap();
        let (_, notices, database_features) = open(&directory).unwrap();
        assert_eq!(database_features.door_links, DoorLinksAccess::Core);
        assert!(notices.is_empty());
    }

    #[test]
    fn a_stale_extension_is_off_with_the_reason() {
        for feature in Feature::ALL {
            let directory = data_set(&only(feature, 1, "older"), Some("core"), &[]);
            let (extensions, notices, database_features) = open(&directory).unwrap();
            let notice = notices.for_feature(feature).unwrap();
            assert!(
                notice
                    .reason
                    .contains("built from another conversion (older)")
                    && notice.reason.contains("convert the data set again"),
                "{}",
                notice.reason
            );
            assert!(!extensions.status(feature).is_present());
            if feature == Feature::DoorLinks {
                assert_eq!(database_features.door_links, DoorLinksAccess::Disabled);
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
            let (_, notices, database_features) = open(&directory).unwrap();
            crate::world::database::WorldDatabase::open_with(
                &directory.path().join("skyrim_world.db"),
                database_features,
            )
            .unwrap();
            assert!(notices.is_empty());
            assert_eq!(
                database_features.door_links,
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
            let error = open(&directory).expect_err("malformed identity must fail loading");
            let context = format!("{error:#}");
            assert!(context.contains("cannot read the extensions"), "{context}");
            assert!(context.contains("reading core build identity"), "{context}");
        }
    }

    #[test]
    fn an_unsupported_version_is_off_with_the_reason() {
        for feature in Feature::ALL {
            let directory = data_set(&only(feature, 2, "core"), Some("core"), &[]);
            let (extensions, notices, database_features) = open(&directory).unwrap();
            let notice = notices.for_feature(feature).unwrap();
            assert!(
                notice
                    .reason
                    .contains("version 2, but this build reads versions 1 to 1")
                    && notice.reason.contains("convert the data set again"),
                "{}",
                notice.reason
            );
            assert!(!extensions.status(feature).is_present());
            if feature == Feature::DoorLinks {
                assert_eq!(database_features.door_links, DoorLinksAccess::Disabled);
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
        let (_, notices, database_features) = open(&directory).unwrap();
        crate::world::database::WorldDatabase::open_with(
            &directory.path().join("skyrim_world.db"),
            database_features,
        )
        .unwrap();
        assert!(notices.is_empty());
        assert_eq!(database_features.door_links, DoorLinksAccess::Core);
    }

    #[test]
    fn unsupported_capability_retains_its_status_and_notice() {
        let directory = data_set(&only(Feature::DoorLinks, 2, "core"), Some("core"), &[]);
        let (extensions, notices, _) = open(&directory).unwrap();
        assert!(matches!(
            extensions.status(Feature::DoorLinks),
            ExtensionStatus::UnsupportedVersion { .. }
        ));
        assert!(notices.for_feature(Feature::DoorLinks).is_some());
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
        let (_, notices, database_features) = open(&directory).unwrap();
        assert!(notices.is_empty());
        assert_eq!(
            database_features.door_links,
            DoorLinksAccess::LegacyExtension
        );
        crate::world::database::WorldDatabase::open_with(&path, database_features).unwrap();
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
            std::fs::write(
                directory.path().join("conversion-manifest.json"),
                only(Feature::DoorLinks, 1, "core"),
            )
            .unwrap();
            let (_, notices, database_features) = open(&directory).unwrap();
            assert!(notices.is_empty());
            assert_eq!(
                database_features.door_links,
                DoorLinksAccess::LegacyExtension
            );
            let error = crate::world::database::WorldDatabase::open_with(&path, database_features)
                .err()
                .expect("declared malformed legacy schema must fail at open");
            assert!(format!("{error:#}").contains("door_links"));
            // Undeclared legacy core packages retain the existing optional-table behavior.
            std::fs::write(
                directory.path().join("conversion-manifest.json"),
                MAIN_MANIFEST,
            )
            .unwrap();
            let (_, notices, database_features) = open(&directory).unwrap();
            assert!(notices.is_empty());
            assert_eq!(database_features.door_links, DoorLinksAccess::Core);
            crate::world::database::WorldDatabase::open_with(&path, database_features).unwrap();
        }
    }

    #[test]
    fn a_declared_door_extension_requires_its_complete_table() {
        let directory = data_set(
            &only(Feature::DoorLinks, 1, "core"),
            Some("core"),
            &["door_links"],
        );
        let (_, _, database_features) = open(&directory).unwrap();
        let error = crate::world::database::WorldDatabase::open_with(
            &directory.path().join("skyrim_world.db"),
            database_features,
        )
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
        let (_, _, database_features) = open(&directory).unwrap();
        let error = crate::world::database::WorldDatabase::open_with(
            &directory.path().join("skyrim_world.db"),
            database_features,
        )
        .err()
        .expect("incomplete declared table must fail");
        assert!(format!("{error:#}").contains("door_links"));
    }

    #[test]
    fn install_retains_validated_capabilities_and_notices() {
        let directory = data_set(&only(Feature::DoorLinks, 2, "core"), Some("core"), &[]);
        let (extensions, notices, _) = open(&directory).unwrap();
        let mut app = App::new();
        app.insert_resource(extensions).insert_resource(notices);
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
    fn material_capability_requires_a_present_supported_current_declaration() {
        for (declaration, expected) in [
            (MAIN_MANIFEST.to_owned(), false),
            (only(Feature::Material, 1, "core"), true),
            (only(Feature::Material, 1, "older"), false),
            (only(Feature::Material, 2, "core"), false),
        ] {
            let directory = data_set(&declaration, Some("core"), &[]);
            let (extensions, notices, database_features) = open(&directory).unwrap();
            assert_eq!(extensions.status(Feature::Material).is_present(), expected);
            assert_eq!(
                notices.for_feature(Feature::Material).is_none(),
                expected || declaration == MAIN_MANIFEST
            );
            assert_eq!(database_features.door_links, DoorLinksAccess::Core);
            crate::world::database::WorldDatabase::open_with(
                &directory.path().join("skyrim_world.db"),
                database_features,
            )
            .unwrap();
        }
    }

    #[test]
    fn malformed_material_is_off_with_its_parser_issue_without_reading_identity() {
        let text = r#"{"extensions":[{"id":"mudcrab.material","version":"broken"}]}"#;
        let directory = data_set(text, None, &[]);
        let connection =
            rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch("CREATE TABLE lod_build(renamed_identity TEXT);")
            .unwrap();
        drop(connection);
        let (extensions, notices, database_features) = open(&directory).unwrap();
        assert!(extensions.data().is_declared(MATERIAL));
        assert_eq!(
            extensions.status(Feature::Material),
            ExtensionStatus::Missing
        );
        assert_eq!(database_features.door_links, DoorLinksAccess::Core);
        let issue = extensions
            .data()
            .issues()
            .find(|issue| issue.id.as_deref() == Some(MATERIAL))
            .unwrap();
        let notice = notices.for_feature(Feature::Material).unwrap();
        assert!(notice.reason.contains(&issue.reason), "{}", notice.reason);
        assert!(notice.reason.contains("convert the data set again"));
        assert_eq!(notices.len(), 1);
    }

    #[test]
    fn material_identity_is_read_only_for_a_supported_anchored_declaration() {
        for (version, built_from, reads_identity) in
            [(2, "older", false), (1, "", false), (1, "core", true)]
        {
            let directory = data_set(&only(Feature::Material, version, built_from), None, &[]);
            let connection =
                rusqlite::Connection::open(directory.path().join("skyrim_world.db")).unwrap();
            connection
                .execute_batch("CREATE TABLE lod_build(renamed_identity TEXT);")
                .unwrap();
            drop(connection);
            let result = open(&directory);
            if reads_identity {
                let context = format!(
                    "{:#}",
                    result.expect_err("supported material needs valid identity")
                );
                assert!(context.contains("cannot read the extensions"), "{context}");
                assert!(context.contains("reading core build identity"), "{context}");
            } else {
                let (extensions, notices, database_features) = result.unwrap();
                assert_eq!(
                    extensions.status(Feature::Material).is_present(),
                    version == 1
                );
                assert_eq!(
                    notices.for_feature(Feature::Material).is_some(),
                    version != 1
                );
                assert_eq!(database_features.door_links, DoorLinksAccess::Core);
            }
        }
    }

    #[test]
    fn malformed_foreign_entries_do_not_poison_supported_material() {
        let mut document: serde_json::Value =
            serde_json::from_str(&only(Feature::Material, 1, "core")).unwrap();
        document["extensions"].as_array_mut().unwrap().extend([
            serde_json::json!({"id": "example.payload", "version": "future"}),
            serde_json::Value::Null,
            serde_json::json!(42),
        ]);
        let directory = data_set(&document.to_string(), Some("core"), &[]);
        let (extensions, notices, database_features) = open(&directory).unwrap();
        assert!(extensions.status(Feature::Material).is_present());
        assert_eq!(extensions.data().issues().count(), 3);
        assert!(notices.is_empty());
        assert_eq!(database_features.door_links, DoorLinksAccess::Core);
    }
}
