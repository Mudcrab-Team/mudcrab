//! The production authority: what the dataset's own winning inputs say, recomputed when a
//! package is finalized, apart from anything the cluster stage wrote.
//!
//! [`CoreAuthority`] reads the core database (references, base records, statics, worldspaces and
//! the build identity) and the published models' GLB headers, and answers
//! [`AuthoritativeInputs`] through the same recipe the stage uses ([`resolve`]). It reads
//! all examined source rows in one bulk query during fallible construction. SQL, storage and row
//! decoding errors refuse construction; later reference lookups perform no database reads.
//! Model and snapshot preparation retain bounded parallel workers, without parallel source-row
//! rereads. The caller must hold its dataset source lock from before construction through
//! certification and for the lifetime of any retained authority or certified package.

use super::{
    classify::{
        ScopedReference, Sources, read_sources, scoped_reference_ids, scoped_references_by_ids,
    },
    resolve::{ModelCatalog, snapshot},
    scope::{Scope, ScopeRequest},
};
use crate::{
    cluster_mapping::{AuthoritativeInputs, ReferenceSnapshot},
    extensions::core_build_identity,
};
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::Connection;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// Connection-local page reuse for dedicated read-only certification connections. mmap shares OS
/// pages between workers; the 64 MiB page cache is an upper budget per connection, not a trust
/// cache. SQLite still executes every query, checks its own file state and may clamp mmap_size.
pub fn configure_read_only_cache(connection: &Connection) -> rusqlite::Result<()> {
    configure_schema_cache(connection, None)
}

/// [`configure_read_only_cache`] for the attached database `schema` (a cluster overlay attached to
/// the base): cache and mmap sizes are per database, so each attached file is tuned on its own.
pub fn configure_attached_read_only_cache(
    connection: &Connection,
    schema: &str,
) -> rusqlite::Result<()> {
    configure_schema_cache(connection, Some(rusqlite::DatabaseName::Attached(schema)))
}

fn configure_schema_cache(
    connection: &Connection,
    schema: Option<rusqlite::DatabaseName<'_>>,
) -> rusqlite::Result<()> {
    connection.pragma_update(schema, "cache_size", -(64 * 1024))?;
    let _: i64 = connection.pragma_update_and_check(
        schema,
        "mmap_size",
        2_i64 * 1024 * 1024 * 1024,
        |row| row.get(0),
    )?;
    Ok(())
}

pub struct CoreAuthority {
    core_identity: Option<String>,
    builder_identity: String,
    scope: Scope,
    catalog: ModelCatalog,
    examined: BTreeSet<u32>,
    references: BTreeMap<u32, Option<ScopedReference>>,
    preloaded_snapshots: Option<BTreeMap<u32, Option<ReferenceSnapshot>>>,
    origins: HashMap<u32, (i32, i32)>,
}

impl CoreAuthority {
    /// Opens the authority over `connection` (the dataset's world database), the published files
    /// under `assets_root` (`published` are their (path, content hash) pairs), for the scope that
    /// `request` and this database declare (the snow source is read from the database itself).
    /// `builder_identity` is the hex identity of the converter that is publishing. All examined
    /// source rows are read before returning; database query failures are returned as errors.
    pub fn open(
        connection: Connection,
        assets_root: &Path,
        published: impl IntoIterator<Item = (String, String)>,
        request: &ScopeRequest,
        builder_identity: String,
    ) -> Result<Self> {
        let sources = read_sources(&connection, &request.child_worldspaces_sorted())?;
        let scope = Scope::from_request(request, sources.snow_source);
        Self::finish(
            connection,
            assets_root,
            published,
            scope,
            sources,
            builder_identity,
        )
    }

    /// Opens the authority for a scope a package declares (`classifier_scope`, read with
    /// [`Scope::from_declared_json`]). The declared snow source must be the one this database has:
    /// the reader's authority is this database, not the declaration. As with [`Self::open`], all
    /// examined source rows must be read successfully before returning an authority.
    pub fn open_declared(
        connection: Connection,
        assets_root: &Path,
        published: impl IntoIterator<Item = (String, String)>,
        scope: Scope,
        builder_identity: String,
    ) -> Result<Self> {
        let sources = read_sources(&connection, &scope.child_worldspaces)?;
        color_eyre::eyre::ensure!(
            sources.snow_source == scope.snow_source,
            "the declared snow source {} is not this database's {}",
            scope.snow_source.as_str(),
            sources.snow_source.as_str()
        );
        Self::finish(
            connection,
            assets_root,
            published,
            scope,
            sources,
            builder_identity,
        )
    }

    fn finish(
        connection: Connection,
        assets_root: &Path,
        published: impl IntoIterator<Item = (String, String)>,
        scope: Scope,
        sources: Sources,
        builder_identity: String,
    ) -> Result<Self> {
        let core_identity = core_build_identity(&connection)?;
        let examined: BTreeSet<_> =
            scoped_reference_ids(&connection, &sources, scope.include_trees)?
                .into_iter()
                .filter(|(_, cell, world)| scope.contains(*world, *cell))
                .map(|(id, _, _)| id)
                .collect();
        // Read every examined source row while errors can still reach the caller. The map
        // retains missing rows as unavailable; certification must still refuse them.
        let references =
            scoped_references_by_ids(&connection, &sources, &examined, scope.include_trees)
                .wrap_err("reading authoritative cluster reference rows")?;
        let origins = sources
            .worldspaces
            .iter()
            .map(|world| {
                (
                    world.worldspace_id,
                    (world.origin.grid_x, world.origin.grid_y),
                )
            })
            .collect();
        let catalog = ModelCatalog::new(assets_root, published)
            .map_err(|problem| color_eyre::eyre::eyre!("{problem}"))?;
        Ok(Self {
            core_identity,
            builder_identity,
            scope,
            catalog,
            examined,
            references,
            preloaded_snapshots: None,
            origins,
        })
    }

    /// Borrows the fully preloaded authority without retaining the SQLite connection. The
    /// returned view can be shared with certification workers; no source read is skipped.
    pub fn preloaded_authority(&self) -> Result<PreloadedAuthority<'_>> {
        let snapshots = self.preloaded_snapshots.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "authority snapshots must be preloaded before parallel validation"
            )
        })?;
        Ok(PreloadedAuthority {
            core_identity: &self.core_identity,
            builder_identity: &self.builder_identity,
            scope: &self.scope,
            catalog: &self.catalog,
            examined: &self.examined,
            snapshots,
            origins: &self.origins,
        })
    }

    /// An examined row retained at construction, when successfully decoded. The reader uses
    /// it only after mapping validation has accepted the authority's snapshots.
    pub fn preloaded_reference(&self, reference_id: u32) -> Option<&ScopedReference> {
        self.references.get(&reference_id)?.as_ref()
    }

    /// Computes the original snapshot recipe exactly once for every constructed reference. Each
    /// unique candidate model is resolved first, retaining the catalog's full byte, texture,
    /// alias and material checks. Dynamic work assignment avoids concentrating expensive models
    /// on one worker. Only the catalog and decoded rows cross threads, never the connection.
    pub fn preload_snapshots(&mut self) -> Result<()> {
        self.preload_snapshots_with_headroom(0)
    }

    /// Retains the normal two-CPU headroom plus a caller's concurrently busy threads. The
    /// caller must join that work before consuming snapshots or reporting a certification.
    pub fn preload_snapshots_with_headroom(
        &mut self,
        additional_busy_threads: usize,
    ) -> Result<()> {
        if self.preloaded_snapshots.is_some() {
            return Ok(());
        }
        let references = &self.references;
        let catalog = &self.catalog;
        let model_paths: Vec<_> = references
            .values()
            .flatten()
            .filter_map(|reference| reference.candidate.as_ref())
            .map(|candidate| candidate.glb_path.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        parallel_collect_with_headroom(
            &model_paths,
            "cluster-model-authority",
            additional_busy_threads,
            |path| {
                catalog.info(path);
            },
        )?;
        let rows: Vec<_> = references
            .iter()
            .map(|(&id, row)| (id, row.as_ref()))
            .collect();
        let snapshots = parallel_collect_with_headroom(
            &rows,
            "cluster-reference-authority",
            additional_busy_threads,
            |&(id, row)| (id, row.map(|reference| snapshot(reference, catalog))),
        )?;
        self.preloaded_snapshots = Some(snapshots.into_iter().collect());
        Ok(())
    }
}

/// Immutable, connection-free view of source facts freshly read by [`CoreAuthority`].
pub struct PreloadedAuthority<'a> {
    core_identity: &'a Option<String>,
    builder_identity: &'a str,
    scope: &'a Scope,
    catalog: &'a ModelCatalog,
    examined: &'a BTreeSet<u32>,
    snapshots: &'a BTreeMap<u32, Option<ReferenceSnapshot>>,
    origins: &'a HashMap<u32, (i32, i32)>,
}

impl AuthoritativeInputs for PreloadedAuthority<'_> {
    fn core_build_identity(&self) -> Option<String> {
        self.core_identity.clone()
    }

    fn builder_identity(&self) -> String {
        self.builder_identity.to_owned()
    }

    fn reference(&self, reference_id: u32) -> Option<ReferenceSnapshot> {
        self.snapshots.get(&reference_id).cloned().flatten()
    }

    fn reference_view(&self, reference_id: u32) -> Option<&ReferenceSnapshot> {
        self.snapshots.get(&reference_id).and_then(Option::as_ref)
    }

    fn classifier_scope(&self) -> String {
        self.scope.json()
    }

    fn lod_origin(&self, worldspace_id: u32) -> Option<(i32, i32)> {
        self.origins.get(&worldspace_id).copied()
    }

    fn model_primitives(&self, model_hash: &str) -> Option<BTreeSet<u32>> {
        self.catalog.facts_by_hash(model_hash)
    }

    fn model_materials(
        &self,
        model_hash: &str,
    ) -> Option<BTreeMap<u32, crate::cluster_mapping::MaterialSource>> {
        self.catalog.materials_by_hash(model_hash)
    }

    fn examined_references(&self) -> BTreeSet<u32> {
        self.examined.clone()
    }
}

/// Runs bounded scoped workers without involving Bevy's compute pool. Every worker is joined
/// before returning; spawn failures and panics refuse the preload rather than skipping work.
fn parallel_collect_with_headroom<T: Sync, R: Send>(
    inputs: &[T],
    name: &str,
    additional_busy_threads: usize,
    operation: impl Fn(&T) -> R + Sync,
) -> Result<Vec<R>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let workers = thread::available_parallelism()?
        .get()
        .saturating_sub(2)
        .saturating_sub(additional_busy_threads)
        .max(1)
        .min(inputs.len());
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for worker in 0..workers {
            let next = &next;
            let operation = &operation;
            handles.push(
                thread::Builder::new()
                    .name(format!("{name}-{worker}"))
                    .spawn_scoped(scope, move || {
                        let mut output = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(input) = inputs.get(index) else {
                                break;
                            };
                            output.push(operation(input));
                        }
                        output
                    })?,
            );
        }
        let mut output = Vec::with_capacity(inputs.len());
        let mut panicked = false;
        for handle in handles {
            match handle.join() {
                Ok(values) => output.extend(values),
                Err(_) => panicked = true,
            }
        }
        color_eyre::eyre::ensure!(!panicked, "{name} certification worker panicked");
        Ok(output)
    })
}

impl AuthoritativeInputs for CoreAuthority {
    fn core_build_identity(&self) -> Option<String> {
        self.core_identity.clone()
    }

    fn builder_identity(&self) -> String {
        self.builder_identity.clone()
    }

    fn reference(&self, reference_id: u32) -> Option<ReferenceSnapshot> {
        if !self.examined.contains(&reference_id) {
            return None;
        }
        if let Some(snapshots) = &self.preloaded_snapshots {
            return snapshots.get(&reference_id).cloned().flatten();
        }
        self.preloaded_reference(reference_id)
            .map(|scoped| snapshot(scoped, &self.catalog))
    }

    fn reference_view(&self, reference_id: u32) -> Option<&ReferenceSnapshot> {
        self.preloaded_snapshots
            .as_ref()?
            .get(&reference_id)?
            .as_ref()
    }

    fn classifier_scope(&self) -> String {
        self.scope.json()
    }

    fn lod_origin(&self, worldspace_id: u32) -> Option<(i32, i32)> {
        self.origins.get(&worldspace_id).copied()
    }

    fn model_primitives(&self, model_hash: &str) -> Option<BTreeSet<u32>> {
        self.catalog.facts_by_hash(model_hash)
    }

    fn model_materials(
        &self,
        model_hash: &str,
    ) -> Option<std::collections::BTreeMap<u32, crate::cluster_mapping::MaterialSource>> {
        self.catalog.materials_by_hash(model_hash)
    }

    fn examined_references(&self) -> BTreeSet<u32> {
        self.examined.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster_recipe::{
        classify::{SnowSource, scoped_reference_by_id},
        scope::CellArea,
    };
    use rusqlite::OpenFlags;

    /// A world database with one worldspace that has a LOD origin and one static reference, plus
    /// the snow tables `snow` asks for.
    fn database(snow: SnowSource) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE worldspaces (id INTEGER PRIMARY KEY, lod_origin_x INTEGER,
                     lod_origin_y INTEGER, parent_world INTEGER);
                 INSERT INTO worldspaces VALUES (60, -32, -32, NULL);
                 CREATE TABLE records (form_id INTEGER, record_type TEXT);
                 INSERT INTO records VALUES (17, 'STAT');
                 CREATE TABLE statics (id INTEGER PRIMARY KEY);
                 CREATE TABLE \"references\" (id INTEGER PRIMARY KEY, base_form_id INTEGER,
                     worldspace_id INTEGER, is_exterior INTEGER, pos_x REAL, pos_y REAL);
                 INSERT INTO \"references\" VALUES (256, 17, 60, 1, 100.0, 100.0);
                 CREATE TABLE lod_build (id INTEGER PRIMARY KEY, build_identity TEXT);
                 INSERT INTO lod_build VALUES (1, 'core-1');",
            )
            .unwrap();
        match snow {
            SnowSource::StaticMaterials => connection
                .execute_batch(
                    "CREATE TABLE static_materials (id INTEGER PRIMARY KEY,
                         material_object INTEGER, material_max_angle REAL);",
                )
                .unwrap(),
            SnowSource::StaticsColumn => connection
                .execute_batch("ALTER TABLE statics ADD COLUMN material_object INTEGER;")
                .unwrap(),
            SnowSource::None => {}
        }
        connection
    }

    fn declared(snow: SnowSource) -> Scope {
        Scope::from_declared_json(&Scope::from_request(&ScopeRequest::default(), snow).json())
            .unwrap()
    }

    fn open_declared(connection: Connection, scope: Scope) -> Result<CoreAuthority> {
        CoreAuthority::open_declared(connection, Path::new("."), [], scope, "builder".into())
    }

    /// Complete reference columns, with accepted/classifier-refused rows and rows
    /// outside each kind of scope. Unlike the open-only fixture, this exercises the row recipe.
    fn reference_database(snow: SnowSource) -> Connection {
        let connection = database(snow);
        connection
            .execute_batch(
                "ALTER TABLE records ADD COLUMN data BLOB;
                 UPDATE records SET data = X'';
                 INSERT INTO records VALUES (18, 'STAT', X'');
                 INSERT INTO records VALUES (19, 'ACTI', X'');
                 INSERT INTO records VALUES (20, 'TREE', X'');
                 ALTER TABLE statics ADD COLUMN model_path TEXT;
                 INSERT INTO statics(id, model_path) VALUES (17, 'architecture/test.nif');
                 INSERT INTO statics(id, model_path) VALUES (18, NULL);
                 INSERT INTO statics(id, model_path) VALUES (20, 'trees/test.nif');
                 CREATE TABLE scripts (form_id INTEGER);
                 ALTER TABLE \"references\" ADD COLUMN pos_z REAL DEFAULT 0;
                 ALTER TABLE \"references\" ADD COLUMN rot_x REAL DEFAULT 0;
                 ALTER TABLE \"references\" ADD COLUMN rot_y REAL DEFAULT 0;
                 ALTER TABLE \"references\" ADD COLUMN rot_z REAL DEFAULT 0;
                 ALTER TABLE \"references\" ADD COLUMN scale REAL DEFAULT 1;
                 ALTER TABLE \"references\" ADD COLUMN header_flags INTEGER DEFAULT 0;
                 ALTER TABLE \"references\" ADD COLUMN enable_parent_id INTEGER;
                 ALTER TABLE \"references\" ADD COLUMN cell_id INTEGER DEFAULT 7;
                 ALTER TABLE \"references\" ADD COLUMN data BLOB DEFAULT X'';
                 INSERT INTO worldspaces VALUES (61, NULL, NULL, 60);
                 INSERT INTO worldspaces VALUES (62, NULL, NULL, NULL);
                 INSERT INTO \"references\"(id, base_form_id, worldspace_id, is_exterior,
                     pos_x, pos_y, header_flags) VALUES (257, 17, 60, 1, 100, 100, 2048);
                 INSERT INTO \"references\"(id, base_form_id, worldspace_id, is_exterior,
                     pos_x, pos_y) VALUES
                     (258, 18, 60, 1, 100, 100),
                     (259, 19, 60, 1, 100, 100),
                     (260, 17, 61, 1, 100, 100),
                     (262, 17, 60, 1, 8192, 100),
                     (263, 17, 60, 0, 100, 100),
                     (264, 20, 60, 1, 100, 100),
                     (265, 17, 62, 1, 100, 100);",
            )
            .unwrap();
        connection
    }

    struct DiskSourceFixture(std::path::PathBuf);

    impl DiskSourceFixture {
        fn new(snow: SnowSource) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "mudcrab-parallel-source-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(
                root.join("OWNER.txt"),
                "owner: shared parallel source preload test\nclass: A, regenerable\n",
            )
            .unwrap();
            let fixture = Self(root);
            reference_database(snow)
                .execute("VACUUM INTO ?1", [fixture.database().to_str().unwrap()])
                .unwrap();
            fixture
        }

        fn database(&self) -> std::path::PathBuf {
            self.0.join("source.db")
        }

        fn authority(&self, request: &ScopeRequest) -> CoreAuthority {
            CoreAuthority::open(
                Connection::open_with_flags(self.database(), OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap(),
                &self.0,
                [],
                request,
                "builder".into(),
            )
            .unwrap()
        }
    }

    impl Drop for DiskSourceFixture {
        fn drop(&mut self) {
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
            assert!(
                self.0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mudcrab-parallel-source-")
            );
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn child_parent_reads_propagate_database_errors() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        for declared_scope in [false, true] {
            for denied in [false, true] {
                let connection = reference_database(SnowSource::None);
                if denied {
                    connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
                        AuthAction::Read {
                            table_name: "worldspaces",
                            column_name: "parent_world",
                        } => Authorization::Deny,
                        _ => Authorization::Allow,
                    }));
                } else {
                    connection
                        .execute(
                            "UPDATE worldspaces SET parent_world='malformed' WHERE id=61",
                            [],
                        )
                        .unwrap();
                }
                let request = ScopeRequest {
                    child_worldspaces: vec![61],
                    ..Default::default()
                };
                let result = if declared_scope {
                    open_declared(connection, Scope::from_request(&request, SnowSource::None))
                } else {
                    CoreAuthority::open(connection, Path::new("."), [], &request, "builder".into())
                };
                let error = result.err().expect(
                    "a failed parent read must not silently exclude requested child references",
                );
                assert!(
                    error.downcast_ref::<rusqlite::Error>().is_some(),
                    "{error:#}"
                );
                if denied {
                    assert!(format!("{error:#}").contains("prohibited"), "{error:#}");
                } else {
                    assert!(
                        matches!(
                            error.downcast_ref::<rusqlite::Error>(),
                            Some(rusqlite::Error::InvalidColumnType(..))
                        ),
                        "{error:#}"
                    );
                }
            }
        }
    }

    #[test]
    fn missing_null_and_unusable_child_parents_remain_explicitly_skipped() {
        for declared_scope in [false, true] {
            let connection = reference_database(SnowSource::None);
            connection
                .execute("INSERT INTO worldspaces VALUES (64,NULL,NULL,62)", [])
                .unwrap();
            let request = ScopeRequest {
                child_worldspaces: vec![61, 62, 63, 64],
                ..Default::default()
            };
            let sources = read_sources(&connection, &request.child_worldspaces).unwrap();
            assert!(sources.sources.contains(&(61, 60)));
            assert_eq!(sources.skipped_children, [62, 63, 64]);
            let authority = if declared_scope {
                open_declared(connection, Scope::from_request(&request, SnowSource::None))
            } else {
                CoreAuthority::open(connection, Path::new("."), [], &request, "builder".into())
            }
            .unwrap();
            assert!(authority.examined_references().contains(&260));
            assert!(authority.reference(260).is_some());
            assert!(!authority.examined_references().contains(&265));
            assert!(authority.reference(265).is_none());
        }
    }

    #[test]
    fn a_late_reference_decode_error_discards_earlier_successful_rows() {
        for declared_scope in [false, true] {
            let connection = reference_database(SnowSource::None);
            connection
                .execute(
                    "UPDATE \"references\" SET scale='malformed' WHERE id=258",
                    [],
                )
                .unwrap();
            let sources = read_sources(&connection, &[]).unwrap();
            assert!(
                scoped_reference_by_id(&connection, &sources, 256, false)
                    .unwrap()
                    .is_some()
            );
            let result = if declared_scope {
                open_declared(connection, declared(SnowSource::None))
            } else {
                CoreAuthority::open(
                    connection,
                    Path::new("."),
                    [],
                    &ScopeRequest::default(),
                    "builder".into(),
                )
            };
            let error = result
                .err()
                .expect("a later broken row must refuse the entire authority");
            assert!(
                matches!(
                    error.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::InvalidColumnType(..))
                ),
                "{error:#}"
            );
        }
    }

    #[test]
    fn constructors_propagate_reference_row_conversion_errors() {
        for declared_scope in [false, true] {
            let connection = reference_database(SnowSource::None);
            connection
                .execute(
                    "UPDATE \"references\" SET scale = 'malformed' WHERE id = 256",
                    [],
                )
                .unwrap();
            let result = if declared_scope {
                open_declared(connection, declared(SnowSource::None))
            } else {
                CoreAuthority::open(
                    connection,
                    Path::new("."),
                    [],
                    &ScopeRequest::default(),
                    "builder".into(),
                )
            };
            let error = result
                .err()
                .expect("row conversion errors must not become unknown references");
            assert!(
                matches!(
                    error.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::InvalidColumnType(..))
                ),
                "{error:#}"
            );
        }
    }

    #[test]
    fn constructors_propagate_reference_query_schema_errors() {
        for declared_scope in [false, true] {
            // Identity and examined-ID reads succeed; the full row query needs more schema.
            let connection = database(SnowSource::None);
            let result = if declared_scope {
                open_declared(connection, declared(SnowSource::None))
            } else {
                CoreAuthority::open(
                    connection,
                    Path::new("."),
                    [],
                    &ScopeRequest::default(),
                    "builder".into(),
                )
            };
            let error = result
                .err()
                .expect("a DB read failure must prevent authority construction");
            assert!(
                error.downcast_ref::<rusqlite::Error>().is_some(),
                "{error:#}"
            );
            assert!(format!("{error:#}").contains("no such"), "{error:#}");
        }
    }

    #[test]
    fn constructors_propagate_reference_query_authorization_errors() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        for declared_scope in [false, true] {
            let connection = reference_database(SnowSource::None);
            // The examined-ID query reads records.record_type, not records.data.
            connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Read {
                    table_name: "records",
                    column_name: "data",
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }));
            let result = if declared_scope {
                open_declared(connection, declared(SnowSource::None))
            } else {
                CoreAuthority::open(
                    connection,
                    Path::new("."),
                    [],
                    &ScopeRequest::default(),
                    "builder".into(),
                )
            };
            let error = result
                .err()
                .expect("denied source reads must not become unknown references");
            assert!(
                error.downcast_ref::<rusqlite::Error>().is_some(),
                "{error:#}"
            );
            assert!(format!("{error:#}").contains("prohibited"), "{error:#}");
        }
    }

    #[test]
    fn constructors_propagate_reference_query_step_errors() {
        for declared_scope in [false, true] {
            let connection = reference_database(SnowSource::None);
            connection
                .execute_batch(
                    "DROP TABLE scripts;
                 CREATE VIEW scripts AS SELECT json_extract('invalid JSON', '$') AS form_id;",
                )
                .unwrap();
            let result = if declared_scope {
                open_declared(connection, declared(SnowSource::None))
            } else {
                CoreAuthority::open(
                    connection,
                    Path::new("."),
                    [],
                    &ScopeRequest::default(),
                    "builder".into(),
                )
            };
            let error = result
                .err()
                .expect("row-step errors must prevent authority construction");
            assert!(
                error.downcast_ref::<rusqlite::Error>().is_some(),
                "{error:#}"
            );
            assert!(format!("{error:#}").contains("malformed JSON"), "{error:#}");
        }
    }

    #[test]
    fn constructors_propagate_reference_bound_storage_limit_errors() {
        for declared_scope in [false, true] {
            let connection = reference_database(SnowSource::None);
            // Each row fits the limit; the complete examined-ID JSON parameter does not.
            // Build the data before installing the connection-local storage limit.
            for id in 1000..1100 {
                connection.execute(
                    "INSERT INTO \"references\" (id, base_form_id, worldspace_id, is_exterior, pos_x, pos_y) VALUES (?1, 17, 60, 1, 100, 100)",
                    [id],
                ).unwrap();
            }
            let sources = read_sources(&connection, &[]).unwrap();
            assert!(
                scoped_reference_by_id(&connection, &sources, 256, false)
                    .unwrap()
                    .is_some()
            );
            connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 200);
            assert!(
                scoped_reference_by_id(&connection, &sources, 256, false)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                core_build_identity(&connection).unwrap().as_deref(),
                Some("core-1")
            );
            let limited_sources = read_sources(&connection, &[]).unwrap();
            let ids = scoped_reference_ids(&connection, &limited_sources, false).unwrap();
            assert!(
                ids.len() > 100,
                "the compact source reads must still succeed"
            );
            let result = if declared_scope {
                open_declared(connection, declared(SnowSource::None))
            } else {
                CoreAuthority::open(
                    connection,
                    Path::new("."),
                    [],
                    &ScopeRequest::default(),
                    "builder".into(),
                )
            };
            let error = result
                .err()
                .expect("a failed bulk parameter bind must refuse construction");
            assert!(
                matches!(error.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::TooBig),
                "{error:#}"
            );
        }
    }

    #[test]
    fn disk_construction_and_snapshot_workers_match_independent_per_row_reads() {
        for snow in [
            SnowSource::None,
            SnowSource::StaticMaterials,
            SnowSource::StaticsColumn,
        ] {
            let disk = DiskSourceFixture::new(snow);
            for request in [
                ScopeRequest::default(),
                ScopeRequest {
                    include_trees: true,
                    child_worldspaces: vec![61],
                    area: None,
                },
                ScopeRequest {
                    include_trees: false,
                    child_worldspaces: vec![61],
                    area: Some(CellArea {
                        worldspace_id: 60,
                        min: (0, 0),
                        max: (0, 0),
                    }),
                },
            ] {
                let connection =
                    Connection::open_with_flags(disk.database(), OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .unwrap();
                let sources =
                    read_sources(&connection, &request.child_worldspaces_sorted()).unwrap();
                let mut authority = disk.authority(&request);
                let examined = authority.examined_references();
                assert_eq!(
                    authority
                        .references
                        .keys()
                        .copied()
                        .collect::<BTreeSet<_>>(),
                    examined
                );
                let mut expected = BTreeMap::new();
                for id in 256..=266 {
                    let row = if examined.contains(&id) {
                        scoped_reference_by_id(&connection, &sources, id, request.include_trees)
                            .unwrap()
                    } else {
                        None
                    };
                    assert_eq!(
                        format!("{:?}", authority.preloaded_reference(id)),
                        format!("{row:?}"),
                        "constructed row {id}, snow={snow:?}, request={request:?}"
                    );
                    let snapshot = row.as_ref().map(|row| snapshot(row, &authority.catalog));
                    assert_eq!(
                        authority.reference(id),
                        snapshot,
                        "constructed snapshot {id}"
                    );
                    expected.insert(id, snapshot);
                }
                authority.preload_snapshots_with_headroom(1).unwrap();
                let view = authority.preloaded_authority().unwrap();
                assert_eq!(view.examined_references(), examined);
                for id in 256..=266 {
                    assert_eq!(
                        authority.reference(id),
                        expected[&id],
                        "cached snapshot {id}"
                    );
                    assert_eq!(view.reference(id), expected[&id], "shared snapshot {id}");
                    assert_eq!(
                        view.reference_view(id),
                        expected[&id].as_ref(),
                        "borrowed snapshot {id}"
                    );
                }
            }
        }
    }

    #[test]
    fn construction_reads_connection_private_sources_without_reopening() {
        for (private_state, refusal) in [
            (
                "BEGIN; UPDATE \"references\" SET header_flags = 2048 WHERE id = 256;",
                "initially_disabled",
            ),
            (
                "CREATE TEMP VIEW scripts AS SELECT 256 AS form_id;",
                "scripted",
            ),
            (
                "ATTACH DATABASE ':memory:' AS private_source_state;
              CREATE TABLE private_source_state.scripts (form_id INTEGER);
              INSERT INTO private_source_state.scripts VALUES (256);
              CREATE TEMP VIEW scripts AS SELECT form_id FROM private_source_state.scripts;",
                "scripted",
            ),
            (
                "DROP TABLE scripts; CREATE VIEW scripts AS SELECT 256 AS form_id;",
                "scripted",
            ),
        ] {
            let disk = DiskSourceFixture::new(SnowSource::None);
            let connection = Connection::open(disk.database()).unwrap();
            connection.execute_batch(private_state).unwrap();
            let mut authority = open_declared(connection, declared(SnowSource::None)).unwrap();
            assert_eq!(
                authority
                    .preloaded_reference(256)
                    .unwrap()
                    .refusal
                    .as_deref(),
                Some(refusal)
            );
            let expected = authority
                .reference(256)
                .expect("refused rows still have source snapshots");
            if refusal == "initially_disabled" {
                // Closing the consumed connection rolls back its uncommitted change. The
                // authority retains what that connection actually read, not a reopened view.
                let db = Connection::open(disk.database()).unwrap();
                let flags: u32 = db
                    .query_row(
                        "SELECT header_flags FROM \"references\" WHERE id = 256",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(flags, 0);
            }
            authority.preload_snapshots().unwrap();
            assert_eq!(authority.reference(256), Some(expected.clone()));
            assert_eq!(
                authority.preloaded_authority().unwrap().reference(256),
                Some(expected)
            );
        }
    }

    #[test]
    fn construction_respects_connection_local_reference_query_limits() {
        let connection = reference_database(SnowSource::None);
        // The compact identity/ID queries fit; the complete reference query does not.
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH, 512);
        let error = open_declared(connection, declared(SnowSource::None))
            .err()
            .expect("SQL limits must fail construction, not erase references");
        assert!(
            error.downcast_ref::<rusqlite::Error>().is_some(),
            "{error:#}"
        );
        assert!(
            format!("{error:#}").contains("reading authoritative cluster reference rows"),
            "{error:#}"
        );
    }

    #[test]
    fn bulk_and_per_row_authorities_have_identical_snapshots_and_refusals() {
        for snow in [
            SnowSource::StaticMaterials,
            SnowSource::StaticsColumn,
            SnowSource::None,
        ] {
            for request in [
                ScopeRequest::default(),
                ScopeRequest {
                    include_trees: true,
                    child_worldspaces: vec![61],
                    area: None,
                },
                ScopeRequest {
                    include_trees: false,
                    child_worldspaces: vec![61],
                    area: Some(CellArea {
                        worldspace_id: 60,
                        min: (0, 0),
                        max: (0, 0),
                    }),
                },
            ] {
                let connection = reference_database(snow);
                let sources =
                    read_sources(&connection, &request.child_worldspaces_sorted()).unwrap();
                let mut authority = CoreAuthority::open(
                    reference_database(snow),
                    Path::new("."),
                    [],
                    &request,
                    "builder".into(),
                )
                .unwrap();
                let ids = 256..=266; // Includes an absent id after all fixture rows.
                let bulk_rows = scoped_references_by_ids(
                    &connection,
                    &sources,
                    &ids.clone().collect(),
                    request.include_trees,
                )
                .unwrap();
                for id in ids.clone() {
                    let old =
                        scoped_reference_by_id(&connection, &sources, id, request.include_trees)
                            .unwrap();
                    assert_eq!(
                        format!("{:?}", bulk_rows[&id]),
                        format!("{old:?}"),
                        "raw row id={id}, snow={snow:?}, request={request:?}"
                    );
                }
                let before: BTreeMap<_, _> = ids
                    .clone()
                    .map(|id| (id, authority.reference(id)))
                    .collect();
                let examined = authority.examined_references();
                assert_eq!(
                    authority
                        .references
                        .keys()
                        .copied()
                        .collect::<BTreeSet<_>>(),
                    examined
                );
                for &id in &examined {
                    assert_eq!(
                        format!("{:?}", authority.preloaded_reference(id)),
                        format!("{:?}", bulk_rows[&id]),
                        "constructed row {id}"
                    );
                }
                assert!(authority.preloaded_reference(256).is_some());

                authority.preload_snapshots().unwrap();
                let view = authority.preloaded_authority().unwrap();
                fn requires_sync(_: &impl Sync) {}
                requires_sync(&view);
                assert_eq!(view.examined_references(), authority.examined_references());
                assert_eq!(view.classifier_scope(), authority.classifier_scope());
                assert_eq!(view.core_build_identity(), authority.core_build_identity());
                assert_eq!(view.builder_identity(), authority.builder_identity());
                assert_eq!(view.lod_origin(60), authority.lod_origin(60));
                for id in ids {
                    assert_eq!(view.reference(id), authority.reference(id));
                    assert_eq!(view.reference_view(id), authority.reference_view(id));
                    assert_eq!(
                        authority.reference(id),
                        before[&id],
                        "parallel id={id}, snow={snow:?}, request={request:?}"
                    );
                }
                assert_eq!(
                    authority
                        .preloaded_reference(257)
                        .unwrap()
                        .refusal
                        .as_deref(),
                    Some("initially_disabled")
                );
                assert_eq!(
                    authority
                        .preloaded_reference(258)
                        .unwrap()
                        .refusal
                        .as_deref(),
                    Some("no_model")
                );
                // ACTI belongs to resting-model scope; this fixture omits its model row.
                assert_eq!(
                    authority
                        .preloaded_reference(259)
                        .unwrap()
                        .refusal
                        .as_deref(),
                    Some("no_model")
                );
                assert!(authority.preloaded_reference(261).is_none());
                assert!(authority.preloaded_reference(263).is_none());
                assert!(authority.preloaded_reference(265).is_none());
                if request.child_worldspaces.contains(&61) {
                    let child = authority.preloaded_reference(260).unwrap();
                    assert_eq!(child.block_worldspace_id, 60);
                    assert_eq!(child.placed_worldspace_id, 61);
                } else {
                    assert!(authority.preloaded_reference(260).is_none());
                }
                assert_eq!(
                    authority.preloaded_reference(264).is_some(),
                    request.include_trees
                );
                assert_eq!(
                    authority.preloaded_reference(262).is_none(),
                    request.area.is_some()
                );
            }
        }
    }

    #[test]
    fn missing_and_out_of_scope_rows_remain_unavailable_in_all_authority_views() {
        let mut authority = open_declared(
            reference_database(SnowSource::None),
            declared(SnowSource::None),
        )
        .unwrap();
        assert!(!authority.examined_references().contains(&261));
        assert!(!authority.examined_references().contains(&266));
        for id in [261, 263, 265, 266] {
            assert!(authority.reference(id).is_none());
            assert!(authority.preloaded_reference(id).is_none());
        }
        authority.preload_snapshots().unwrap();
        let view = authority.preloaded_authority().unwrap();
        for id in [261, 263, 265, 266] {
            assert!(authority.reference(id).is_none());
            assert!(view.reference(id).is_none());
            assert!(view.reference_view(id).is_none());
        }
        // These are classifier refusals, not missing rows.
        assert_eq!(
            authority
                .preloaded_reference(257)
                .unwrap()
                .refusal
                .as_deref(),
            Some("initially_disabled")
        );
        assert_eq!(
            authority
                .preloaded_reference(258)
                .unwrap()
                .refusal
                .as_deref(),
            Some("no_model")
        );
    }

    #[test]
    fn constructed_rows_agree_before_and_after_snapshot_preparation() {
        let mut authority = open_declared(
            reference_database(SnowSource::None),
            declared(SnowSource::None),
        )
        .unwrap();
        // CoreAuthority owns no SQLite connection: no later query can fail or substitute None.
        let expected = authority
            .reference(256)
            .expect("known row must survive construction");

        authority.preload_snapshots().unwrap();
        assert_eq!(authority.reference(256), Some(expected.clone()));
        assert_eq!(
            authority.preloaded_authority().unwrap().reference(256),
            Some(expected)
        );
    }

    #[test]
    fn bulk_static_lod_metadata_matches_per_row_without_changing_snapshots() {
        let request = ScopeRequest {
            include_trees: true,
            ..Default::default()
        };
        let baseline = CoreAuthority::open(
            reference_database(SnowSource::None),
            Path::new("."),
            [],
            &request,
            "builder".into(),
        )
        .unwrap();
        for (schema, expected, static_is_tree) in [
            ("", None, false),
            ("ALTER TABLE statics ADD COLUMN flags INTEGER;", None, false),
            (
                "ALTER TABLE statics ADD COLUMN flags INTEGER DEFAULT 0;",
                Some(false),
                false,
            ),
            (
                "ALTER TABLE statics ADD COLUMN flags INTEGER DEFAULT 32768;",
                Some(true),
                false,
            ),
            (
                "ALTER TABLE statics ADD COLUMN flags INTEGER DEFAULT 64;",
                Some(false),
                true,
            ),
            (
                "ALTER TABLE statics ADD COLUMN flags INTEGER DEFAULT 32832;",
                Some(true),
                true,
            ),
        ] {
            let connection = reference_database(SnowSource::None);
            connection.execute_batch(schema).unwrap();
            let sources = read_sources(&connection, &[]).unwrap();
            let legacy = scoped_reference_by_id(&connection, &sources, 256, true)
                .unwrap()
                .unwrap();
            let mut authority =
                CoreAuthority::open(connection, Path::new("."), [], &request, "builder".into())
                    .unwrap();
            assert_eq!(legacy.base_has_distant_lod, expected);
            assert_eq!(legacy.is_tree, static_is_tree);
            assert_eq!(authority.reference(256), baseline.reference(256));

            let loaded = authority.preloaded_reference(256).unwrap();
            assert_eq!(loaded.base_has_distant_lod, expected);
            assert_eq!(loaded.is_tree, static_is_tree);
            assert_eq!(format!("{legacy:?}"), format!("{loaded:?}"));
            assert_eq!(authority.reference(256), baseline.reference(256));
            let tree = authority.preloaded_reference(264).unwrap();
            assert_eq!(tree.base_has_distant_lod, expected);
            assert!(tree.is_tree);
            assert_eq!(authority.reference(264), baseline.reference(264));
            authority.preload_snapshots().unwrap();
            for id in [256, 264] {
                assert_eq!(authority.reference(id), baseline.reference(id));
            }
        }
    }

    #[test]
    fn snapshots_require_preparation_then_reuse_constructed_rows_without_invalidating_the_cache() {
        let mut authority = open_declared(
            reference_database(SnowSource::None),
            declared(SnowSource::None),
        )
        .unwrap();
        let expected: BTreeMap<_, _> = (256..=266)
            .map(|id| (id, authority.reference(id)))
            .collect();
        assert!(expected[&256].is_some());
        assert!(expected[&266].is_none());
        assert!(authority.reference_view(256).is_none());
        assert!(authority.preloaded_authority().is_err());
        assert!(authority.preloaded_snapshots.is_none());
        authority.preload_snapshots().unwrap();
        // Once ready, even extreme headroom must reuse the existing cache and its answers.
        authority
            .preload_snapshots_with_headroom(usize::MAX)
            .unwrap();
        let view = authority.preloaded_authority().unwrap();
        for (id, expected) in expected {
            assert_eq!(authority.reference(id), expected, "owned {id}");
            assert_eq!(
                authority.reference_view(id),
                expected.as_ref(),
                "borrowed {id}"
            );
            assert_eq!(view.reference(id), expected, "shared owned {id}");
            assert_eq!(
                view.reference_view(id),
                expected.as_ref(),
                "shared borrowed {id}"
            );
        }
    }

    #[test]
    fn parallel_cold_catalog_preserves_model_texture_and_alias_refusals() {
        use crate::cluster_mapping::{
            Eligibility, REASON_MODEL_CONTENT_CHANGED, REASON_MODEL_NOT_LOADED,
            REASON_TEXTURE_CONTENT_CHANGED, REASON_TEXTURE_NOT_LOADED,
        };
        use sha2::{Digest, Sha256};
        use std::{fs, path::PathBuf};

        struct Files(PathBuf);
        impl Drop for Files {
            fn drop(&mut self) {
                assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
                assert!(
                    self.0
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("mudcrab-parallel-authority-")
                );
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mudcrab-parallel-authority-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let files = Files(root);
        fs::write(
            files.0.join("OWNER.txt"),
            b"owner: cert-speed parallel source authority test\npurpose: generated model/texture refusal fixtures\nclass: A, regenerable\n",
        )
        .unwrap();
        fs::create_dir_all(files.0.join("meshes/architecture")).unwrap();
        fs::create_dir(files.0.join("textures")).unwrap();
        let document = serde_json::json!({
            "asset": {"version":"2.0"}, "scene":0,
            "scenes":[{"nodes":[0]}], "nodes":[{"mesh":0}],
            "meshes":[{"primitives":[{"material":0}]}],
            "materials":[{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}],
            "textures":[{"source":0}],
            "images":[{"uri":"../../textures/a.opensky-srgb.opensky-wrap1.ktx2"}],
        });
        let mut json = serde_json::to_vec(&document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(20u32 + json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        let texture = b"invented bytes, certification hashes without decoding images";
        let model_path = files.0.join("meshes/architecture/test.glb");
        let source_path = files.0.join("textures/a.ktx2");
        let alias_path = files.0.join("textures/a.opensky-srgb.opensky-wrap1.ktx2");
        let hash = |bytes: &[u8]| crate::cluster::hex(&Sha256::digest(bytes));
        for (failure, reason) in [
            ("none", None),
            ("model_missing", Some(REASON_MODEL_NOT_LOADED)),
            ("model_changed", Some(REASON_MODEL_CONTENT_CHANGED)),
            ("model_invalid", Some(REASON_MODEL_NOT_LOADED)),
            ("alias_missing", Some(REASON_TEXTURE_NOT_LOADED)),
            ("alias_changed", Some(REASON_TEXTURE_CONTENT_CHANGED)),
            ("source_missing", Some(REASON_TEXTURE_NOT_LOADED)),
            ("source_changed", Some(REASON_TEXTURE_CONTENT_CHANGED)),
        ] {
            fs::write(&model_path, &glb).unwrap();
            fs::write(&source_path, texture).unwrap();
            fs::write(&alias_path, texture).unwrap();
            let mut model_hash = hash(&glb);
            match failure {
                "model_missing" => fs::remove_file(&model_path).unwrap(),
                "model_changed" => fs::write(&model_path, b"changed model").unwrap(),
                "model_invalid" => {
                    fs::write(&model_path, b"not a GLB").unwrap();
                    model_hash = hash(b"not a GLB");
                }
                "alias_missing" => fs::remove_file(&alias_path).unwrap(),
                "alias_changed" => fs::write(&alias_path, b"changed alias").unwrap(),
                "source_missing" => fs::remove_file(&source_path).unwrap(),
                "source_changed" => fs::write(&source_path, b"changed source").unwrap(),
                "none" => {}
                _ => unreachable!(),
            }
            let published = vec![
                ("meshes/architecture/test.glb".to_string(), model_hash),
                ("textures/a.ktx2".to_string(), hash(texture)),
            ];
            let open = || {
                let connection = reference_database(SnowSource::None);
                let data =
                    rkyv::to_bytes::<rkyv::rancor::Error>(&crate::records::ArchivedRecordData {
                        subrecords: vec![],
                    })
                    .unwrap();
                connection
                    .execute("UPDATE records SET data = ?1", [data.as_slice()])
                    .unwrap();
                connection
                    .execute("UPDATE \"references\" SET data = ?1", [data.as_slice()])
                    .unwrap();
                CoreAuthority::open(
                    connection,
                    &files.0,
                    published.clone(),
                    &ScopeRequest::default(),
                    "builder".into(),
                )
                .unwrap()
            };
            let legacy = open();
            let mut parallel = open();

            parallel.preload_snapshots().unwrap();
            for id in 256..=266 {
                assert_eq!(
                    parallel.reference(id),
                    legacy.reference(id),
                    "{failure}: {id}"
                );
            }
            let expected = reason.map_or(Eligibility::Eligible, |reason| {
                Eligibility::Unknown(reason.into())
            });
            assert_eq!(
                parallel.reference(256).unwrap().eligibility,
                expected,
                "{failure}"
            );
        }
    }

    #[test]
    fn a_declared_scope_matching_the_database_opens_and_answers_as_declared() {
        for snow in [
            SnowSource::StaticMaterials,
            SnowSource::StaticsColumn,
            SnowSource::None,
        ] {
            let scope = declared(snow);
            let authority = open_declared(reference_database(snow), scope.clone()).unwrap();
            assert_eq!(authority.classifier_scope(), scope.json());
            assert_eq!(authority.core_build_identity().as_deref(), Some("core-1"));
            assert_eq!(authority.builder_identity(), "builder");
            assert_eq!(authority.lod_origin(60), Some((-32, -32)));
            assert_eq!(
                authority.examined_references(),
                BTreeSet::from([256, 257, 258, 259, 262])
            );
        }
    }

    #[test]
    fn a_stale_declared_snow_source_is_refused() {
        let all = [
            SnowSource::StaticMaterials,
            SnowSource::StaticsColumn,
            SnowSource::None,
        ];
        for current in all {
            for stale in all.into_iter().filter(|stale| *stale != current) {
                let error = open_declared(database(current), declared(stale))
                    .err()
                    .unwrap_or_else(|| panic!("declared {stale:?} opened over {current:?}"));
                assert!(
                    format!("{error:#}").contains("declared snow source"),
                    "{error:#}"
                );
            }
        }
    }

    #[test]
    fn the_declared_and_requested_paths_agree_over_the_same_database() {
        let request = ScopeRequest {
            include_trees: false,
            child_worldspaces: vec![],
            area: None,
        };
        let requested = CoreAuthority::open(
            reference_database(SnowSource::StaticMaterials),
            Path::new("."),
            [],
            &request,
            "builder".into(),
        )
        .unwrap();
        let declared = open_declared(
            reference_database(SnowSource::StaticMaterials),
            declared(SnowSource::StaticMaterials),
        )
        .unwrap();
        assert_eq!(requested.classifier_scope(), declared.classifier_scope());
        assert_eq!(
            requested.examined_references(),
            declared.examined_references()
        );
    }
}
