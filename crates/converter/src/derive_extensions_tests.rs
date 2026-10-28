use super::*;
use crate::derive_database;
use crate::{cache::CacheEntry, cluster::probe::ProbeResult, esm::exporter::create_tables};
use shared::extensions::{CLUSTERS, DataExtensions, ExtensionHasher, ExtensionOwns};
use std::collections::BTreeMap;
use tempfile::TempDir;

const CORE: &str = "c1";
const FOREIGN: &str = "test.foreign";

struct Fixture {
    directory: TempDir,
    options: DeriveOptions,
    foreign: ExtensionEntry,
}

fn probe() -> Option<ProbeResult> {
    Some(ProbeResult {
        passed: true,
        missing: Vec::new(),
        backend: "synthetic-no-device".into(),
        adapter_name: "CPU test capability fact, no GPU requested".into(),
        json: serde_json::json!({"test": true}),
    })
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("OWNER.txt"),
        "owner: derive regression fixture\n",
    )
    .unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("OWNER.txt"),
        "owner: synthetic source fixture\n",
    )
    .unwrap();
    fs::create_dir(source.join("scripts")).unwrap();
    fs::write(source.join("scripts/probe.luau"), b"return 1\n").unwrap();
    fs::create_dir(source.join("foreign")).unwrap();
    fs::write(
        source.join("foreign/data.bin"),
        b"foreign-owned immutable bytes",
    )
    .unwrap();
    fs::write(
        source.join("record-warnings.json"),
        b"{\"record_data_complete\":false,\"warnings\":[\"invented record limitation\"]}\n",
    )
    .unwrap();
    let connection = Connection::open(source.join("skyrim_world.db")).unwrap();
    create_tables(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO lod_build(id,build_identity) VALUES(1,?1)",
            [CORE.repeat(32)],
        )
        .unwrap();
    connection.execute_batch("INSERT INTO worldspaces(id,editor_id,parent_world,flags,lod_origin_x,lod_origin_y) VALUES(700,'InventedWorld',NULL,0,13,-7);
            CREATE TABLE foreign_rows(id INTEGER PRIMARY KEY,value BLOB NOT NULL DEFAULT X'01' CHECK(length(value)>0));
            CREATE INDEX foreign_value ON foreign_rows(value);
            INSERT INTO foreign_rows VALUES(1,X'00FF');").unwrap();
    let owns = ExtensionOwns {
        tables: vec!["foreign_rows".into()],
        indexes: vec!["foreign_value".into()],
        files: vec!["foreign/data.bin".into()],
        gltf_extensions: Vec::new(),
    };
    let mut hasher = ExtensionHasher::new(FOREIGN, 1);
    hasher.add_table(&connection, "foreign_rows").unwrap();
    hasher.add_index(&connection, "foreign_value").unwrap();
    hasher.add_file(
        "foreign/data.bin",
        &fs::read(source.join("foreign/data.bin")).unwrap(),
    );
    let foreign = ExtensionEntry {
        id: FOREIGN.into(),
        version: 1,
        owns,
        sha256: hasher.finish(),
        built_from: CORE.repeat(32),
        stats: BTreeMap::from([("rows".into(), 1)]),
    };
    drop(connection);
    let mut manifest = ConversionManifest {
        schema_version: CONVERTER_SCHEMA_VERSION,
        complete: false,
        record_data_complete: Some(false),
        ..Default::default()
    };
    manifest.entries.insert(
        "scripts/probe.luau".into(),
        CacheEntry {
            animation_diagnostics: None,
            source_hash: "02".repeat(32),
            output: "scripts/probe.luau".into(),
            output_size: 9,
            output_hash: hash_file(&source.join("scripts/probe.luau")).unwrap(),
        },
    );
    manifest.extensions = crate::extensions::build_entries(&source).unwrap();
    manifest.extensions.push(foreign.clone());
    let mut raw = serde_json::to_value(manifest).unwrap();
    raw["forward_compatible_fixture"] = serde_json::json!({
        "warning_file": "record-warnings.json", "nested": [true, {"kept": 42}]
    });
    let foreign_raw = raw["extensions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["id"] == FOREIGN)
        .unwrap();
    foreign_raw["future_foreign_metadata"] = serde_json::json!({
        "retained": ["opaque future field", {"version": 7}], "flag": true
    });
    fs::write(
        source.join("conversion-manifest.json"),
        serde_json::to_vec_pretty(&raw).unwrap(),
    )
    .unwrap();
    Fixture {
        options: DeriveOptions {
            source,
            label: "default".into(),
            replace: false,
            keep_failed_staging: false,
            terrain: false,
            terrain_sources: None,
            jobs: 1,
            triangle_threshold: 12,
            include_trees: false,
        },
        directory,
        foreign,
    }
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, directory: &Path, output: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            assert!(!is_link(&metadata));
            if metadata.is_dir() {
                if directory == root && entry.file_name() == "derived" {
                    continue;
                }
                walk(root, &entry.path(), output);
            } else {
                assert!(metadata.is_file());
                output.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    hash_file(&entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

fn mutate_manifest(fixture: &Fixture, change: impl FnOnce(&mut Value)) {
    let path = fixture.options.source.join("conversion-manifest.json");
    let mut raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    change(&mut raw);
    fs::write(path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
}

fn output(options: &DeriveOptions) -> PathBuf {
    options.source.join("derived").join(format!(
        "clusters-{}",
        requested_settings(options).settings_id()
    ))
}

#[test]
fn cluster_only_base_rejects_size_refusal_before_creating_a_package() {
    let fixture = fixture();
    mutate_manifest(&fixture, |raw| {
        raw["geometry_mode"] = serde_json::json!("cluster");
        raw["cluster_package"] = serde_json::json!("derived/clusters-0123456789abcdef");
    });
    let error = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX))
        .err()
        .expect("cluster-only bases cannot lose small primitives");
    assert!(format!("{error:#}").contains("cluster geometry requires --triangle-threshold 1"));
    assert!(!fixture.options.source.join("derived").exists());
}

#[test]
fn real_derive_hashes_every_base_file_and_preserves_foreign_and_unknown_metadata() {
    let fixture = fixture();
    let source = &fixture.options.source;
    let before_files = inventory(source);
    let before_rows = derive_database::snapshot(
        &Connection::open(source.join("skyrim_world.db")).unwrap(),
        &BTreeSet::new(),
    )
    .unwrap();
    let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    assert!(report.incomplete_core);
    assert_eq!(
        report.output,
        fs::canonicalize(output(&fixture.options)).unwrap()
    );
    assert_eq!(inventory(source), before_files, "base file bytes changed");
    assert_eq!(
        derive_database::snapshot(
            &Connection::open(source.join("skyrim_world.db")).unwrap(),
            &BTreeSet::new()
        )
        .unwrap(),
        before_rows
    );
    let metadata = shared::cluster_overlay::checked_overlay(source, &report.output).unwrap();
    assert_eq!(metadata.base_core_identity, CORE.repeat(32));
    assert_eq!(metadata.label, "default");
    assert_eq!(metadata.build_settings.settings_id(), metadata.settings_id);
    assert_eq!(
        report.output.file_name().unwrap().to_string_lossy(),
        format!("clusters-{}", metadata.build_settings.settings_id())
    );
    assert_eq!(
        DataExtensions::load(source).unwrap().get(FOREIGN),
        Some(&fixture.foreign)
    );
    let cluster = metadata
        .extensions
        .iter()
        .find(|entry| entry.id == CLUSTERS)
        .unwrap();
    assert_eq!(cluster.built_from, CORE.repeat(32));
    assert_eq!(cluster.stats["derived_over_incomplete_core"], 1);
    assert!(report.output.join("cluster-manifest.json").is_file());
    for absent in [
        "skyrim_world.db",
        "conversion-manifest.json",
        "foreign",
        "scripts",
        "meshes",
        "textures",
        "vfs",
    ] {
        assert!(
            !report.output.join(absent).exists(),
            "overlay copied {absent}"
        );
    }
    let db = Connection::open(report.output.join("clusters.db")).unwrap();
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        tables.iter().all(|name| name.starts_with("cluster_")),
        "core table copied: {tables:?}"
    );
    let reader = shared::cluster_overlay::open_overlay_database(source, &report.output).unwrap();
    assert!(reader.execute("DELETE FROM cluster_build", []).is_err());
    assert!(reader.execute("DELETE FROM lod_build", []).is_err());
}

#[test]
fn bad_source_contract_or_existing_cluster_store_never_publishes_output() {
    for case in [
        "converter_schema",
        "database_schema",
        "missing_identity",
        "changed_asset_hash",
        "cluster_table",
        "cluster_folder",
        "database_sidecar",
        "manifest_owns_derived",
        "future_builtin",
        "wrong_builtin_owns",
        "missing_builtin",
    ] {
        let fixture = fixture();
        if matches!(
            case,
            "future_builtin" | "wrong_builtin_owns" | "missing_builtin"
        ) {
            add_material_capability(&fixture);
        }
        match case {
            "converter_schema" => mutate_manifest(&fixture, |raw| {
                raw["schema_version"] = serde_json::json!(25)
            }),
            "database_schema" => Connection::open(fixture.options.source.join("skyrim_world.db"))
                .unwrap()
                .execute("UPDATE schema_info SET version=8", [])
                .map(|_| ())
                .unwrap(),
            "missing_identity" => Connection::open(fixture.options.source.join("skyrim_world.db"))
                .unwrap()
                .execute("DELETE FROM lod_build", [])
                .map(|_| ())
                .unwrap(),
            "changed_asset_hash" => fs::write(
                fixture.options.source.join("scripts/probe.luau"),
                b"return 2\n",
            )
            .unwrap(),
            "cluster_table" => Connection::open(fixture.options.source.join("skyrim_world.db"))
                .unwrap()
                .execute_batch("CREATE TABLE cluster_unowned_fixture(value);")
                .unwrap(),
            "manifest_owns_derived" => mutate_manifest(&fixture, |raw| {
                raw["entries"]["scripts/probe.luau"]["output"] =
                    serde_json::json!("derived/forbidden.bin")
            }),
            "database_sidecar" => fs::write(
                fixture.options.source.join("skyrim_world.db-shm"),
                b"sentinel sidecar",
            )
            .unwrap(),
            "cluster_folder" => fs::create_dir(fixture.options.source.join("clusters")).unwrap(),
            "future_builtin" => mutate_manifest(&fixture, |raw| {
                let entry = raw["extensions"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|entry| entry["id"] != FOREIGN)
                    .expect("fixture has actual built-ins");
                entry["version"] = serde_json::json!(99);
            }),
            "wrong_builtin_owns" => mutate_manifest(&fixture, |raw| {
                let entry = raw["extensions"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|entry| entry["id"] != FOREIGN)
                    .expect("fixture has actual built-ins");
                entry["owns"]["gltf_extensions"] = serde_json::json!(["invented_wrong_ownership"]);
            }),
            "missing_builtin" => mutate_manifest(&fixture, |raw| {
                let entries = raw["extensions"].as_array_mut().unwrap();
                let index = entries
                    .iter()
                    .position(|entry| entry["id"] != FOREIGN)
                    .expect("fixture has actual built-ins");
                entries.remove(index);
            }),
            _ => unreachable!(),
        }
        let before = inventory(&fixture.options.source);
        let error = run_with_probe(&fixture.options, probe()).err().expect(case);
        let expected = match case {
            "manifest_owns_derived" => "must not own files under derived",
            "database_sidecar" => "SQLite sidecars",
            "converter_schema" => "requires converter schema",
            "database_schema" => "requires the current world database schema",
            "missing_identity" => "core identity is missing or empty",
            "changed_asset_hash" => "source integrity check failed",
            "cluster_table" | "cluster_folder" => "existing cluster store",
            "future_builtin" | "wrong_builtin_owns" => "unsupported migration",
            "missing_builtin" => "omits a built-in extension declaration",
            _ => unreachable!(),
        };
        assert!(
            format!("{error:#}").contains(expected),
            "case {case}: wrong refusal: {error:#}"
        );
        assert!(
            !output(&fixture.options).exists(),
            "published rejected source case {case}"
        );
        assert_eq!(
            inventory(&fixture.options.source),
            before,
            "changed source case {case}"
        );
    }
}

/// Install a real retained producer only in tests that exercise its declaration.
fn add_material_capability(fixture: &Fixture) {
    let source = &fixture.options.source;
    fs::create_dir(source.join("meshes")).unwrap();
    let material = br#"{"asset":{"version":"2.0"},"extensionsUsed":["OPEN_SKYRIM_material_animation"],"materials":[{"extensions":{"OPEN_SKYRIM_material_animation":{"channels":[]}}}]}"#;
    fs::write(source.join("meshes/animated.gltf"), material).unwrap();
    let entries = crate::extensions::build_entries(source).unwrap();
    mutate_manifest(fixture, |raw| {
        raw["entries"]["meshes/animated.gltf"] = serde_json::to_value(CacheEntry {
            animation_diagnostics: None,
            source_hash: "03".repeat(32),
            output: "meshes/animated.gltf".into(),
            output_size: material.len() as u64,
            output_hash: hash_file(&source.join("meshes/animated.gltf")).unwrap(),
        })
        .unwrap();
        let extensions = raw["extensions"].as_array_mut().unwrap();
        for entry in entries.into_iter().rev() {
            extensions.insert(0, serde_json::to_value(entry).unwrap());
        }
    });
}

#[test]
fn existing_overlay_needs_replace_and_successfully_replaces_only_its_own_variant() {
    let mut fixture = fixture();
    let first = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    let before = inventory(&first.output);
    let error = run_with_probe(&fixture.options, probe())
        .err()
        .expect("existing overlay must refuse");
    assert!(format!("{error:#}").contains("use --replace"));
    assert_eq!(inventory(&first.output), before);
    fixture.options.replace = true;
    fixture.options.label = "updated".into();
    let second = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    assert_eq!(first.output, second.output);
    assert_eq!(
        shared::cluster_overlay::checked_overlay(&fixture.options.source, &second.output)
            .unwrap()
            .label,
        "updated"
    );
    assert!(
        !fs::read_dir(second.output.parent().unwrap())
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".replaced-"))
    );
}

#[test]
fn admission_refuses_before_staging_and_propagates_reader_errors() {
    for fail_reader in [false, true] {
        let fixture = fixture();
        let before = inventory(&fixture.options.source);
        fs::create_dir(fixture.options.source.join("derived")).unwrap();
        let error = run_with_space(&fixture.options, probe(), move |_| {
            if fail_reader {
                bail!("injected unavailable space reader");
            }
            Ok(0)
        })
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains(if fail_reader {
            "injected unavailable"
        } else {
            "needed"
        }));
        assert!(!output(&fixture.options).exists());
        assert!(
            !fs::read_dir(fixture.options.source.join("derived"))
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".staging-"))
        );
        assert_eq!(inventory(&fixture.options.source), before);
    }
}

#[test]
fn failed_staging_cleans_only_own_directory_and_keep_flag_retains_it() {
    for keep in [false, true] {
        let mut fixture = fixture();
        fixture.options.keep_failed_staging = keep;
        let derived = fixture.options.source.join("derived");
        fs::create_dir(&derived).unwrap();
        let other = derived.join(".staging-other-run");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("sentinel"), b"not ours").unwrap();
        let destination = output(&fixture.options);
        // The admission callback runs after output preflight. A competing path
        // appearing here makes real final publication fail after finalization.
        let competing = destination.clone();
        let mut injected = false;
        let error = run_with_space(&fixture.options, probe(), move |_| {
            if !injected {
                fs::create_dir(&competing)?;
                fs::write(competing.join("sentinel"), b"competing output")?;
                injected = true;
            }
            Ok(u64::MAX)
        })
        .err()
        .expect("late publication must refuse");
        assert!(format!("{error:#}").contains("use --replace"));
        let stages: Vec<_> = fs::read_dir(&derived)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".staging-")
                    && *path != other
            })
            .collect();
        assert_eq!(stages.len(), usize::from(keep));
        // A kept staging of this command is dead bookkeeping; the foreign one and the competing
        // output are content.
        let mut content = vec![
            ".staging-other-run".to_owned(),
            destination
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        content.sort();
        assert_eq!(derived_content_to_discard(&derived).unwrap(), content);
        assert_eq!(
            fs::read(destination.join("sentinel")).unwrap(),
            b"competing output"
        );
        assert_eq!(fs::read(other.join("sentinel")).unwrap(), b"not ours");
        assert!(fixture.directory.path().exists());
    }
}

#[test]
fn settings_id_excludes_jobs_and_label_but_tracks_build_settings() {
    let mut fixture = fixture();
    let baseline = requested_settings(&fixture.options).settings_id();
    fixture.options.jobs = 17;
    fixture.options.label = "another label".into();
    assert_eq!(requested_settings(&fixture.options).settings_id(), baseline);
    fixture.options.triangle_threshold += 1;
    assert_ne!(requested_settings(&fixture.options).settings_id(), baseline);
    fixture.options.triangle_threshold -= 1;
    fixture.options.terrain = true;
    assert_ne!(requested_settings(&fixture.options).settings_id(), baseline);
    fixture.options.terrain = false;
    fixture.options.include_trees = true;
    assert_ne!(requested_settings(&fixture.options).settings_id(), baseline);
}

/// A real no-LAND source recipe, not an empty terrain certificate.
fn empty_terrain_recipe(fixture: &Fixture) {
    fs::create_dir(fixture.options.source.join("vfs")).unwrap();
    fs::write(
        fixture.options.source.join("cell_cache.rkyv"),
        rkyv::to_bytes::<rkyv::rancor::Error>(&shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: Vec::new(),
        })
        .unwrap(),
    )
    .unwrap();
    fs::write(
        fixture.options.source.join("lod-manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "build_identity": CORE.repeat(32),
            "converter_schema": CONVERTER_SCHEMA_VERSION,
            "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
            "chunks": 0,
            "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
            "terrain_sources": {},
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn no_land_resolves_to_the_object_only_address_before_locking_and_retries_there() {
    let mut fixture = fixture();
    empty_terrain_recipe(&fixture);
    let object_settings = requested_settings(&fixture.options);
    fixture.options.terrain = true;
    let requested_id = requested_settings(&fixture.options).settings_id();
    let before = inventory(&fixture.options.source);
    let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    let metadata =
        shared::cluster_overlay::checked_overlay(&fixture.options.source, &report.output).unwrap();
    assert_eq!(metadata.build_settings, object_settings);
    assert_eq!(metadata.settings_id, object_settings.settings_id());
    assert_eq!(
        report.output.file_name().unwrap().to_string_lossy(),
        format!("clusters-{}", object_settings.settings_id())
    );
    assert!(!metadata.build_settings.terrain);
    assert!(metadata.terrain_sources.is_none());
    assert!(
        !report
            .extensions
            .iter()
            .any(|entry| entry.id == shared::extensions::CLUSTER_TERRAIN)
    );
    assert!(
        !fixture
            .options
            .source
            .join("derived")
            .join(format!("clusters-{requested_id}"))
            .exists()
    );
    assert!(
        !shared::asset_lock::asset_lock_path(
            &report
                .output
                .with_file_name(format!("clusters-{requested_id}"))
        )
        .exists()
    );
    assert_eq!(inventory(&fixture.options.source), before);
    let output_before = inventory(&report.output);
    let error = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX))
        .err()
        .unwrap();
    assert!(format!("{error:#}").contains("use --replace"));
    assert_eq!(inventory(&report.output), output_before);
    fixture.options.replace = true;
    let replaced = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    assert_eq!(replaced.output, report.output);
    // The recorded retry uses actual settings and needs no terrain inputs.
    fixture.options.terrain = metadata.build_settings.terrain;
    let retried = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    assert_eq!(retried.output, report.output);
    shared::cluster_overlay::checked_overlay(&fixture.options.source, &retried.output).unwrap();
    assert_eq!(inventory(&fixture.options.source), before);
}

#[test]
fn empty_terrain_does_not_hide_invalid_requested_recipes_or_raw_inputs() {
    for case in [
        "malformed",
        "stale",
        "missing_root",
        "changed_raw",
        "invalid_cache",
        "stale_cache",
    ] {
        let mut fixture = fixture();
        empty_terrain_recipe(&fixture);
        fixture.options.terrain = true;
        let recipe = fixture.options.source.join("lod-manifest.json");
        match case {
            "malformed" => fs::write(&recipe, b"invalid terrain recipe").unwrap(),
            "stale" => {
                let mut raw: Value = serde_json::from_slice(&fs::read(&recipe).unwrap()).unwrap();
                raw["build_identity"] = serde_json::json!("ab".repeat(32));
                fs::write(&recipe, serde_json::to_vec(&raw).unwrap()).unwrap();
            }
            "missing_root" => fs::remove_dir(fixture.options.source.join("vfs")).unwrap(),
            "invalid_cache" => fs::write(
                fixture.options.source.join("cell_cache.rkyv"),
                b"invalid cache",
            )
            .unwrap(),
            "stale_cache" => fs::write(
                fixture.options.source.join("cell_cache.rkyv"),
                rkyv::to_bytes::<rkyv::rancor::Error>(&shared::CellCache {
                    version: shared::CELL_CACHE_VERSION + 1,
                    cells: Vec::new(),
                })
                .unwrap(),
            )
            .unwrap(),
            "changed_raw" => {
                let input = "textures/landscape/invented.dds";
                let expected = crate::cache::hash_bytes(b"original raw source");
                let mut raw: Value = serde_json::from_slice(&fs::read(&recipe).unwrap()).unwrap();
                raw["terrain_sources"][input] = serde_json::json!(expected);
                fs::write(&recipe, serde_json::to_vec(&raw).unwrap()).unwrap();
                mutate_manifest(&fixture, |manifest| {
                    manifest["entries"][input] = serde_json::json!({
                        "source_hash": expected,
                        "output": "scripts/probe.luau",
                        "output_size": 9,
                        "output_hash": hash_file(&fixture.options.source.join("scripts/probe.luau")).unwrap(),
                    });
                });
                let path = fixture.options.source.join("vfs").join(input);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"changed raw source").unwrap();
            }
            _ => unreachable!(),
        }
        let before = inventory(&fixture.options.source);
        let error = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX))
            .err()
            .unwrap();
        let expected = match case {
            "malformed" => "invalid LOD build manifest",
            "stale" => "build identities do not match",
            "missing_root" => "terrain derivation requires --terrain-sources",
            "changed_raw" => "terrain source bytes disagree",
            "invalid_cache" => "invalid cell cache",
            "stale_cache" => "cell cache version",
            _ => unreachable!(),
        };
        assert!(format!("{error:#}").contains(expected), "{case}: {error:#}");
        assert!(!fixture.options.source.join("derived").exists(), "{case}");
        assert_eq!(inventory(&fixture.options.source), before, "{case}");
    }
}

#[test]
fn terrain_resolution_uses_stage_world_and_exterior_land_selection_and_propagates_errors() {
    let fixture = fixture();
    let mut options = fixture.options;
    options.terrain = true;
    let database = Connection::open(options.source.join("skyrim_world.db")).unwrap();
    assert!(!resolved_settings(&options, &database).unwrap().terrain);
    // LAND without a complete grid or a world's complete LOD origin is outside the stage domain.
    database
        .execute_batch(
            "INSERT INTO worldspaces(id,editor_id,flags) VALUES(701,'NoOrigin',0);
         INSERT INTO cells(id,worldspace_id,grid_x,grid_y,flags) VALUES
             (1,700,-7,NULL,0),(2,701,-7,11,0),(3,700,-7,11,0);
         INSERT INTO land(cell_id,heightmap) VALUES(1,X'00'),(2,X'00');",
        )
        .unwrap();
    assert!(!resolved_settings(&options, &database).unwrap().terrain);
    database
        .execute("INSERT INTO land(cell_id,heightmap) VALUES(3,X'00')", [])
        .unwrap();
    let resolved = resolved_settings(&options, &database).unwrap();
    assert!(resolved.terrain);
    assert_eq!(
        resolved.settings_id(),
        requested_settings(&options).settings_id()
    );
    // Inspect all selected worlds, including a malformed later world after valid LAND.
    database
        .execute_batch(
            "INSERT INTO worldspaces(id,editor_id,flags,lod_origin_x,lod_origin_y)
             VALUES(702,'MalformedLaterWorld',0,0,0);
         INSERT INTO cells(id,worldspace_id,grid_x,grid_y,flags)
             VALUES(4,702,'invalid coordinate',11,0);
         INSERT INTO land(cell_id,heightmap) VALUES(4,X'00');",
        )
        .unwrap();
    assert!(resolved_settings(&options, &database).is_err());
}

// Direct source verification uses invented bytes; this does not test DDS decoding.
mod terrain_input_verification_tests {
    use super::*;
    use crate::cache::hash_bytes;
    use crate::cluster::terrain_sources::TerrainSourceContract;

    const INPUT_A: &str = "textures/landscape/invented-copy-a.dds";
    const INPUT_B: &str = "textures/landscape/invented-copy-b.dds";
    const BYTES_A: &[u8] = b"invented raw terrain input a";
    const BYTES_B: &[u8] = b"invented raw terrain input b";

    struct TerrainInputFixture {
        _directory: TempDir,
        raw: PathBuf,
        staging: PathBuf,
        contract: TerrainSourceContract,
    }

    fn terrain_input_fixture() -> TerrainInputFixture {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("OWNER.txt"),
            "owner: terrain input helper test\n",
        )
        .unwrap();
        let raw = directory.path().join("raw");
        let converted = directory.path().join("converted");
        let staging = directory.path().join("staging");
        for path in [&raw, &converted, &staging] {
            fs::create_dir(path).unwrap();
            fs::write(
                path.join("OWNER.txt"),
                "owner: terrain input helper fixture root\n",
            )
            .unwrap();
        }
        for (relative, bytes) in [(INPUT_A, BYTES_A), (INPUT_B, BYTES_B)] {
            let path = raw.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        // Neither unrelated files nor source ownership metadata belongs to the
        // selective temporary VFS; its OWNER must be the helper's own marker.
        fs::write(raw.join("source-only.bin"), b"unrelated raw source bytes").unwrap();
        fs::write(
            staging.join("retained-core.bin"),
            b"retained staging core bytes",
        )
        .unwrap();
        let db = Connection::open(converted.join("skyrim_world.db")).unwrap();
        create_tables(&db).unwrap();
        db.execute(
            "INSERT INTO lod_build(id,build_identity) VALUES(1,?1)",
            ["ab".repeat(32)],
        )
        .unwrap();
        drop(db);
        let sources = BTreeMap::from([
            (INPUT_A, hash_bytes(BYTES_A)),
            (INPUT_B, hash_bytes(BYTES_B)),
        ]);
        let entries: BTreeMap<_, _> = sources.iter().map(|(path, hash)| {
            (*path, serde_json::json!({"source_hash": format!("{hash}:texture-encoding:ColorSrgb")}))
        }).collect();
        fs::write(
            converted.join("conversion-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": CONVERTER_SCHEMA_VERSION,
                "entries": entries,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            converted.join("lod-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "build_identity": "ab".repeat(32),
                "converter_schema": CONVERTER_SCHEMA_VERSION,
                "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
                "chunks": 0,
                "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
                "terrain_sources": sources,
            }))
            .unwrap(),
        )
        .unwrap();
        let contract = TerrainSourceContract::open(&converted).unwrap();
        TerrainInputFixture {
            _directory: directory,
            raw,
            staging,
            contract,
        }
    }

    fn terrain_input_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(root: &Path, directory: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(root, &path, result);
                } else {
                    result.insert(
                        path.strip_prefix(root).unwrap().to_owned(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut result = BTreeMap::new();
        collect(root, root, &mut result);
        result
    }

    #[test]
    fn terrain_input_identity_survives_relocation_and_rejects_changed_bytes() {
        let f = terrain_input_fixture();
        let relocated = f.raw.with_file_name("relocated-raw");
        fs::rename(&f.raw, &relocated).unwrap();
        verify_terrain_input_root(&relocated, &f.contract).unwrap();
        fs::write(relocated.join(INPUT_A), b"changed raw source bytes").unwrap();
        assert!(verify_terrain_input_root(&relocated, &f.contract).is_err());
    }

    #[test]
    fn terrain_inputs_are_verified_in_place_without_any_staged_copy() {
        let f = terrain_input_fixture();
        let source_before = terrain_input_tree(&f.raw);
        let staging_before = terrain_input_tree(&f.staging);
        verify_terrain_input_root(&f.raw, &f.contract).unwrap();
        assert_eq!(terrain_input_tree(&f.raw), source_before);
        assert_eq!(terrain_input_tree(&f.staging), staging_before);
        fs::write(f.raw.join(INPUT_A), b"changed raw source bytes").unwrap();
        assert!(verify_terrain_input_root(&f.raw, &f.contract).is_err());
    }
}

#[test]
fn write_floor_failure_cleans_own_staging_or_keeps_it_on_request() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for keep in [false, true] {
        let mut fixture = fixture();
        fixture.options.keep_failed_staging = keep;
        let before = inventory(&fixture.options.source);
        let derived = fixture.options.source.join("derived");
        fs::create_dir(&derived).unwrap();
        let foreign = derived.join(".staging-another-process");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("sentinel"), b"keep").unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let sampled = reads.clone();
        let error = run_with_space(&fixture.options, probe(), move |_| {
            Ok(if sampled.fetch_add(1, Ordering::SeqCst) < 2 {
                u64::MAX
            } else {
                10 * 1024 * 1024 * 1024
            })
        })
        .err()
        .expect("falling free space must stop an admitted derivation");
        let message = format!("{error:#}");
        assert!(
            message.contains("needed") && message.contains("free"),
            "{message}"
        );
        assert!(reads.load(Ordering::SeqCst) >= 3);
        assert!(!output(&fixture.options).exists());
        let own_stages: Vec<_> = fs::read_dir(&derived)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir() && *path != foreign)
            .collect();
        assert_eq!(own_stages.len(), usize::from(keep));
        assert_eq!(fs::read(foreign.join("sentinel")).unwrap(), b"keep");
        assert_eq!(inventory(&fixture.options.source), before);
    }
}

#[test]
fn advisory_uses_measured_payload_and_table_ratios() {
    let fixture = fixture();
    let base = &fixture.options.source;
    let derived = base.join("derived");
    fs::create_dir(&derived).unwrap();
    fs::create_dir(base.join("meshes")).unwrap();
    fs::write(base.join("meshes/measured.glb"), [0; 123]).unwrap();
    fs::write(base.join("cell_cache.rkyv"), [0; 73]).unwrap();
    // Lead's same-core 2026-10-20 measurements, refined by apparent-byte dust inventory:
    // meshes 2,570,110,144; object payloads 2,908,039,936; terrain 165,344,400;
    // cell cache 488,986,104; DB 2,173,202,432 -> 2,736,652,288 bytes.
    let ratio = |bytes: u64, output: u64, input: u64| (bytes * output).div_ceil(input);
    let tables = ratio(
        fs::metadata(base.join("skyrim_world.db")).unwrap().len(),
        563_449_856,
        2_173_202_432,
    );
    let objects = ratio(123, 2_908_039_936, 2_570_110_144);
    let terrain = ratio(73, 165_344_400, 488_986_104);
    let object_needed = 10 * 1024 * 1024 * 1024 + 1024 * 1024 + objects + tables;
    for (with_terrain, needed) in [(false, object_needed), (true, object_needed + terrain)] {
        assert!(
            derive_storage::admit(
                base,
                &derived,
                &output(&fixture.options),
                with_terrain,
                |_| Ok(needed),
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            derive_storage::admit(
                base,
                &derived,
                &output(&fixture.options),
                with_terrain,
                |_| { Ok(needed - 1) }
            )
            .unwrap()
            .iter()
            .any(|notice| notice.contains("advisory"))
        );
    }
}

#[test]
fn advisory_exact_budget_boundary() {
    let fixture = fixture();
    let base = &fixture.options.source;
    let derived = base.join("derived");
    fs::create_dir(&derived).unwrap();
    fs::create_dir(base.join("meshes")).unwrap();
    fs::write(base.join("meshes/invented.glb"), [0; 11]).unwrap();
    // Object-only derivation must not budget terrain geometry or atlas scratch.
    fs::write(base.join("cell_cache.rkyv"), [0; 73]).unwrap();
    let needed = 10 * 1024 * 1024 * 1024
        + 1024 * 1024
        + (11_u64 * 2_908_039_936).div_ceil(2_570_110_144)
        + (fs::metadata(base.join("skyrim_world.db")).unwrap().len() * 563_449_856)
            .div_ceil(2_173_202_432);
    assert!(
        derive_storage::admit(base, &derived, &output(&fixture.options), false, |_| {
            Ok(needed)
        })
        .unwrap()
        .is_empty()
    );
    assert!(
        derive_storage::admit(base, &derived, &output(&fixture.options), false, |_| Ok(
            needed - 1
        ))
        .unwrap()
        .iter()
        .any(|notice| notice.contains("advisory"))
    );
}

#[test]
fn previous_same_base_overlay_uses_actual_bytes_without_multiplier() {
    let fixture = fixture();
    let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    let base = &fixture.options.source;
    fs::create_dir(base.join("meshes")).unwrap();
    let mesh = fs::File::create(base.join("meshes/logical-size.glb")).unwrap();
    mesh.set_len(64 * 1024 * 1024).unwrap();
    drop(mesh);
    // The complete previous overlay is the hint; no multiplier or additional scratch budget.
    let actual: u64 = inventory(&report.output)
        .keys()
        .map(|path| fs::metadata(report.output.join(path)).unwrap().len())
        .sum();
    let needed = 10 * 1024 * 1024 * 1024 + actual;
    assert!(
        derive_storage::admit(base, &base.join("derived"), &report.output, false, |_| {
            Ok(needed)
        })
        .unwrap()
        .is_empty()
    );
    assert!(
        derive_storage::admit(base, &base.join("derived"), &report.output, false, |_| Ok(
            needed - 1
        ))
        .unwrap()
        .iter()
        .any(|notice| notice.contains("advisory"))
    );
}

#[test]
fn replace_can_rebuild_overlay_after_base_manifest_changed() {
    let mut fixture = fixture();
    let first = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    mutate_manifest(&fixture, |manifest| {
        manifest["new_unknown_field"] = serde_json::json!(true)
    });
    assert!(
        shared::cluster_overlay::checked_overlay(&fixture.options.source, &first.output).is_err()
    );
    fixture.options.replace = true;
    let second = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    shared::cluster_overlay::checked_overlay(&fixture.options.source, &second.output).unwrap();
}

#[test]
fn previous_same_base_overlay_can_raise_advisory_and_foreign_hint_cannot() {
    let fixture = fixture();
    let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    // A bounded logical-size fixture raises the prior-overlay admission hint.
    let hint = fs::File::create(report.output.join("large-prior-payload")).unwrap();
    hint.set_len(64 * 1024 * 1024).unwrap();
    drop(hint);
    let base = &fixture.options.source;
    let seed_only = 10 * 1024 * 1024 * 1024
        + 1024 * 1024
        + (fs::metadata(base.join("skyrim_world.db")).unwrap().len() * 563_449_856)
            .div_ceil(2_173_202_432);
    assert!(
        derive_storage::admit(base, &base.join("derived"), &report.output, false, |_| Ok(
            seed_only
        ))
        .unwrap()
        .iter()
        .any(|notice| notice.contains("advisory"))
    );
    let path = report.output.join("overlay.json");
    let mut metadata: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    metadata["base_fingerprint"] = serde_json::json!("different-base");
    fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(
        derive_storage::admit(base, &base.join("derived"), &report.output, false, |_| {
            Ok(seed_only)
        })
        .unwrap()
        .is_empty()
    );
}

#[test]
fn unrelated_mod_table_with_a_cluster_prefix_is_not_a_cluster_store() {
    let fixture = fixture();
    let db = Connection::open(fixture.options.source.join("skyrim_world.db")).unwrap();
    db.execute_batch("CREATE TABLE clustered_notes(value TEXT); INSERT INTO clustered_notes VALUES('foreign extension data');").unwrap();
    drop(db);
    preflight(&fixture.options.source).unwrap();
}

fn failures(fixture: &Fixture) {
    mutate_manifest(fixture, |raw| {
        raw["failures"] = serde_json::json!({
            "textures/a.dds": "invented conversion failure",
            "textures/b.dds": "another invented failure"
        })
    });
}

#[test]
fn recorded_asset_failures_are_admitted_with_a_loud_count_and_recorded() {
    let fixture = fixture();
    failures(&fixture);
    preflight(&fixture.options.source).unwrap();
    let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    assert!(report.incomplete_core);
    assert!(
        report
            .notices
            .iter()
            .any(|notice| notice.contains("2 failed asset conversion(s)")),
        "no loud failure notice: {:?}",
        report.notices
    );
    let metadata =
        shared::cluster_overlay::checked_overlay(&fixture.options.source, &report.output).unwrap();
    assert_eq!(metadata.base_failure_count, 2);
    // A base without failures records none.
    let clean = fixture_overlay_failure_count();
    assert_eq!(clean, 0);
}

fn fixture_overlay_failure_count() -> usize {
    let fixture = fixture();
    let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    shared::cluster_overlay::checked_overlay(&fixture.options.source, &report.output)
        .unwrap()
        .base_failure_count
}

#[test]
fn recorded_failures_do_not_excuse_other_integrity_problems_or_a_complete_flag() {
    let fixture = fixture();
    failures(&fixture);
    fs::write(
        fixture.options.source.join("scripts/probe.luau"),
        b"return 2\n",
    )
    .unwrap();
    let error = preflight(&fixture.options.source).err().unwrap();
    assert!(format!("{error:#}").contains("source integrity check failed"));
    let fixture = self::fixture();
    failures(&fixture);
    mutate_manifest(&fixture, |raw| raw["complete"] = serde_json::json!(true));
    let error = preflight(&fixture.options.source).err().unwrap();
    assert!(format!("{error:#}").contains("completeness flags disagree"));
    assert!(!output(&fixture.options).exists());
}

#[test]
fn labels_are_unique_within_a_base_and_replace_works_across_settings_ids() {
    let mut fixture = fixture();
    let first = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    let derived = fixture.options.source.join("derived");
    // After a derivation the folder holds the overlay and this command's bookkeeping only.
    assert_eq!(
        derived_content_to_discard(&derived).unwrap(),
        vec![
            first
                .output
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ]
    );
    assert!(
        !fs::read_dir(&derived).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")),
        "a finished derivation left its staging or staging lock behind"
    );
    let before = inventory(&first.output);
    fixture.options.triangle_threshold = 16;
    let other_destination = output(&fixture.options);
    assert_ne!(other_destination, first.output);
    // Same label, other settings id: refused until --replace, and nothing changes.
    let error = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX))
        .err()
        .expect("a label is unique within a base");
    let text = format!("{error:#}");
    assert!(
        text.contains("already used") && text.contains("--replace"),
        "{text}"
    );
    assert!(!other_destination.exists());
    assert_eq!(inventory(&first.output), before);
    // Another label coexists.
    fixture.options.label = "other".into();
    let other = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    assert!(first.output.is_dir() && other.output.is_dir());
    // A derivation that is claiming the label blocks a concurrent one.
    fixture.options.label = "default".into();
    fixture.options.replace = true;
    let digest = crate::cache::hash_bytes(b"default");
    let claim =
        AssetLock::acquire_exclusive(derived.join(format!(".label-{}", &digest[..16]))).unwrap();
    let error = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX))
        .err()
        .expect("a concurrent claim blocks");
    assert!(format!("{error:#}").contains("claiming overlay label"));
    drop(claim);
    // With --replace the other settings id's overlay is replaced and the old one retired.
    fixture.options.triangle_threshold = 20;
    let third = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    assert!(third.output.is_dir());
    assert!(
        !first.output.exists(),
        "the overlay that held the label stays"
    );
    assert!(
        other.output.is_dir(),
        "an overlay with another label was touched"
    );
    assert_eq!(
        shared::cluster_overlay::checked_overlay(&fixture.options.source, &third.output)
            .unwrap()
            .label,
        "default"
    );
    assert!(
        !fs::read_dir(&derived).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".replaced-")),
        "a retired overlay was left behind"
    );
}

#[test]
fn a_failed_cross_id_replace_keeps_the_overlay_that_held_the_label() {
    let mut fixture = fixture();
    let first = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    let before = inventory(&first.output);
    fixture.options.triangle_threshold = 16;
    fixture.options.replace = true;
    let destination = output(&fixture.options);
    // A late failure on the new overlay's destination (a file where the overlay belongs). The
    // storage tests pin the put-back after a failure at the exact publication step.
    let competing = destination.clone();
    let _error = run_with_space(&fixture.options, probe(), move |_| {
        fs::write(&competing, b"not an overlay")?;
        Ok(u64::MAX)
    })
    .err()
    .expect("the derivation must fail");
    assert_eq!(inventory(&first.output), before);
    assert_eq!(fs::read(&destination).unwrap(), b"not an overlay");
    let derived = fixture.options.source.join("derived");
    assert!(
        !fs::read_dir(&derived).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".replaced-")),
        "the set-aside overlay was not put back"
    );
    shared::cluster_overlay::checked_overlay(&fixture.options.source, &first.output).unwrap();
}

#[test]
fn malformed_previous_store_rebuilds_with_an_actionable_notice() {
    for damage in ["non-file", "database", "schema", "reject-row", "mesh-row"] {
        let mut fixture = fixture();
        let first = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
        let database = first.output.join("clusters.db");
        if damage == "non-file" {
            fs::remove_file(&database).unwrap();
            fs::create_dir(&database).unwrap();
        } else if damage == "database" {
            fs::write(&database, b"not a database").unwrap();
        } else {
            let db = Connection::open(&database).unwrap();
            match damage {
                "schema" => db.execute_batch("ALTER TABLE cluster_meshes RENAME COLUMN payload_path TO broken;").unwrap(),
                "reject-row" => db.execute_batch("INSERT INTO cluster_rejects(mesh_hash,primitive_index,reason,detail) VALUES('invented','not-an-integer','unsupported','invented');").unwrap(),
                "mesh-row" => db.execute_batch("INSERT INTO cluster_meshes(mesh_hash,primitive_index,payload_path,content_hash,source_triangles,cluster_count,vertex_count,payload_bytes,material_key,material_index,alpha_mode,alpha_cutoff,double_sided) VALUES('invented','not-an-integer','clusters/a.mcl','invented',1,1,3,1,'invented',NULL,'opaque',NULL,0);").unwrap(),
                _ => unreachable!(),
            }
        }
        let base_before = inventory(&fixture.options.source);
        fixture.options.replace = true;
        let rebuilt = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
        assert!(
            rebuilt.stage.notices.iter().any(|notice| {
                notice.contains("previous cluster store")
                    && notice.contains(&first.output.display().to_string())
                    && notice.contains("rebuild")
            }),
            "{damage}: {:?}",
            rebuilt.stage.notices
        );
        shared::cluster_overlay::checked_overlay(&fixture.options.source, &rebuilt.output).unwrap();
        assert_eq!(inventory(&fixture.options.source), base_before);
    }
}

#[test]
fn unrelated_malformed_sibling_is_preserved_and_reported_without_blocking_derivation() {
    for damage in ["missing-metadata", "invalid-json", "invalid-schema"] {
        let fixture = fixture();
        let derived = fixture.options.source.join("derived");
        fs::create_dir(&derived).unwrap();
        let sibling = derived.join("clusters-unrelated");
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("sentinel"), b"unrelated data").unwrap();
        if damage != "missing-metadata" {
            fs::write(
                sibling.join("overlay.json"),
                match damage {
                    "invalid-json" => b"{broken".as_slice(),
                    _ => b"{}".as_slice(),
                },
            )
            .unwrap();
        }
        let before = inventory(&sibling);
        let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
        assert_eq!(inventory(&sibling), before);
        assert!(
            report
                .notices
                .iter()
                .any(|notice| notice.contains("skipped unrelated overlay")
                    && notice.contains(&sibling.display().to_string())),
            "{:?}",
            report.notices
        );
        shared::cluster_overlay::checked_overlay(&fixture.options.source, &report.output).unwrap();
    }
}

#[test]
fn invalid_target_overlay_remains_an_error_when_replacement_is_requested() {
    let mut fixture = fixture();
    let derived = fixture.options.source.join("derived");
    fs::create_dir(&derived).unwrap();
    let target = output(&fixture.options);
    fs::create_dir(&target).unwrap();
    fs::write(target.join("overlay.json"), b"{broken").unwrap();
    fs::write(target.join("sentinel"), b"keep").unwrap();
    fixture.options.replace = true;
    let before = inventory(&target);
    assert!(run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).is_err());
    assert_eq!(inventory(&target), before);
}

#[test]
fn overlay_owner_note_has_no_source_host_path() {
    let fixture = fixture();
    let report = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    let owner = fs::read_to_string(report.output.join("OWNER.txt")).unwrap();
    let metadata = shared::cluster_overlay::read_metadata(&report.output).unwrap();
    assert_eq!(metadata.owner, owner);
    assert!(owner.starts_with(STAGING_OWNER_PREFIX));
    assert!(!owner.contains(&fixture.options.source.display().to_string()));
    assert!(!owner.contains("base:"));
}

#[test]
fn measured_estimate_warning_does_not_override_the_authoritative_write_guard() {
    let fixture = fixture();
    let derived = fixture.options.source.join("derived");
    fs::create_dir(&derived).unwrap();
    fs::create_dir(fixture.options.source.join("meshes")).unwrap();
    let logical = fs::File::create(fixture.options.source.join("meshes/invented.bin")).unwrap();
    logical.set_len(1024 * 1024).unwrap();
    drop(logical);
    let report = run_with_space(&fixture.options, probe(), |_| {
        Ok(crate::derive_space::RETAINED_FREE_BYTES + 1024 * 1024)
    })
    .unwrap();
    assert!(
        report
            .notices
            .iter()
            .any(|notice| notice.contains("advisory") && notice.contains("write-time guard")),
        "{:?}",
        report.notices
    );
    shared::cluster_overlay::checked_overlay(&fixture.options.source, &report.output).unwrap();
}

#[test]
fn busy_valid_sibling_cannot_be_skipped_when_claiming_its_label() {
    let mut fixture = fixture();
    let first = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX)).unwrap();
    let before = inventory(&first.output);
    let _writer = AssetLock::acquire_exclusive(&first.output).unwrap();
    fixture.options.triangle_threshold += 1;
    fixture.options.replace = true;
    let error = run_with_space(&fixture.options, probe(), |_| Ok(u64::MAX))
        .err()
        .unwrap();
    assert!(format!("{error:#}").contains("lock"), "{error:#}");
    assert!(!output(&fixture.options).exists());
    assert_eq!(inventory(&first.output), before);
}
