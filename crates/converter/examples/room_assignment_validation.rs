//! Rebuild room metadata and selected source meshes in a private asset copy for native captures.
//!
//! Each invocation replaces all room metadata with the selected cells. Use a fresh private copy
//! for independent cell sets, or include every desired cell when reusing a copy.
//! Requires Unix hard-link validation and exclusive access to the private asset copy.
use color_eyre::{Result, eyre::ensure};
use converter::{
    archive::ArchiveExtractor,
    asset_path::{AssetKind, AssetSourceIndex, canonical_asset_path},
    esm::{exporter::create_tables, records::RawRecord, rooms},
    mesh::MeshConverter,
};
use rayon::prelude::*;
use rusqlite::{Connection, params};
use shared::records::ArchivedRecordData;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    run(&args)
}

fn run(args: &[String]) -> Result<()> {
    ensure!(
        args.len() == 3,
        "usage: room_assignment_validation <Skyrim Data> <private assets> <cell-id,cell-id,...>"
    );
    let data = Path::new(&args[0]);
    let assets = Path::new(&args[1]);
    validate_private_assets(assets)?;
    let _asset_lock = shared::asset_lock::AssetLock::acquire_exclusive(assets)?;
    let cells: HashSet<u32> = args[2]
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let mut conn = Connection::open(assets.join("skyrim_world.db"))?;
    for cell in &cells {
        ensure!(
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM cells WHERE id=?1 AND worldspace_id IS NULL
                 AND grid_x IS NULL AND grid_y IS NULL)",
                [cell],
                |row| row.get::<_, bool>(0)
            )?,
            "requested cell {cell} is absent or exterior"
        );
    }
    // From the first mutation onward, an interrupted run has an incomplete receipt.
    publish_receipt(assets, &serde_json::json!({"complete": false}))?;
    create_tables(&conn)?;
    ensure!(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='visibility_version' AND value=2)",
        [], |row| row.get::<_, bool>(0))?, "fixture input must already have source-verified portal endpoints");
    let mut master = HashMap::new();
    let mut models = BTreeSet::new();
    for &cell in &cells {
        let mut query=conn.prepare("SELECT r.form_id,r.record_type,r.load_order,r.data,p.header_flags FROM records r JOIN \"references\" p ON p.id=r.form_id WHERE p.cell_id=?1")?;
        let mut rows = query.query([cell])?;
        while let Some(row) = rows.next()? {
            let blob: Vec<u8> = row.get(3)?;
            let fields = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&blob)?;
            let kind: String = row.get(1)?;
            let record = RawRecord {
                form_id: row.get(0)?,
                record_type: kind.as_bytes().try_into()?,
                flags: row.get(4)?,
                subrecords: fields
                    .subrecords
                    .into_iter()
                    .map(|s| (s.tag.to_vec(), s.data))
                    .collect(),
                cell_form_id: Some(cell),
                worldspace_form_id: None,
                load_order: row.get(2)?,
            };
            master.insert(record.form_id, record);
        }
        let paths=conn.prepare("SELECT DISTINCT s.model_path FROM statics s JOIN \"references\" r ON r.base_form_id=s.id WHERE r.cell_id=?1 AND s.model_path IS NOT NULL AND s.model_path<>''")?
            .query_map([cell],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for path in paths {
            let model = PathBuf::from(canonical_asset_path(&path, AssetKind::Mesh, "nif")?);
            let output = assets.join(&model).with_extension("glb");
            let mut ancestor = Some(output.as_path());
            while let Some(path) = ancestor {
                ensure!(
                    !path.is_symlink(),
                    "fixture output follows a symlink: {}",
                    path.display()
                );
                if path == assets {
                    break;
                }
                ancestor = path.parent();
            }
            models.insert(model);
        }
    }
    rebuild_room_graph(&mut conn, &master, &cells)?;
    drop(conn);
    let plugins: Vec<String> = Connection::open(assets.join("skyrim_world.db"))?
        .prepare("SELECT name FROM plugins ORDER BY priority")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let (sources, extracted) =
        prepare_model_sources(data, assets.parent().unwrap(), &models, &plugins)?;
    let results = convert_capture_models(sources.path(), assets, &models)?;
    converter::integration::finalize_world_database(assets)?;
    let conn = Connection::open(assets.join("skyrim_world.db"))?;
    let bounds: u64 = conn.query_row("SELECT count(*) FROM room_model_bounds", [], |r| r.get(0))?;
    let report = serde_json::json!({
        "complete": true,
        "scope": "private capture fixture; source-verified endpoint flags retained",
        "cells": cells,
        "requested_models": models.len(),
        "extracted_entries": extracted,
        "base_records_with_source_bounds": bounds,
        "results": results,
    });
    publish_receipt(assets, &report)?;
    println!(
        "Prepared {} models, {bounds} base records with source bounds",
        models.len()
    );
    Ok(())
}

const RECEIPT: &str = "room-assignment-preparation.json";

fn publish_receipt(assets: &Path, report: &serde_json::Value) -> Result<()> {
    let file = tempfile::NamedTempFile::new_in(assets)?;
    serde_json::to_writer_pretty(file.as_file(), report)?;
    file.persist(assets.join(RECEIPT))?;
    Ok(())
}

fn rebuild_room_graph(
    conn: &mut Connection,
    master: &HashMap<u32, RawRecord>,
    cells: &HashSet<u32>,
) -> Result<()> {
    let tx = conn.transaction()?;
    let old_endpoints: Vec<_> = tx
        .prepare("SELECT ref_id,pair_index,unbounded_a,unbounded_b FROM room_portals")?
        .query_map([], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let incomplete_cells: Vec<u32> = tx
        .prepare("SELECT cell_id FROM room_graph_cells WHERE complete=0")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let unknown_assignments: Vec<u32> = tx
        .prepare("SELECT reference_id FROM room_reference_rules WHERE assignment='unknown'")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let verified_occlusion = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='occlusion_version' AND value=1)",
        [],
        |r| r.get::<_, bool>(0),
    )?;
    if verified_occlusion {
        tx.execute(
            "CREATE TEMP TABLE capture_cells(id INTEGER PRIMARY KEY)",
            [],
        )?;
        for cell in cells {
            tx.execute("INSERT INTO capture_cells VALUES (?1)", [cell])?;
        }
        // The engine checks both ends of every XORD. Even a source outside the
        // selection (or missing from the runtime projection) can veto a selected plane.
        tx.execute_batch(
            "CREATE TEMP TABLE capture_occluders AS
                 SELECT * FROM room_occluders WHERE cell_id IN (SELECT id FROM capture_cells);
             CREATE TEMP TABLE capture_links AS
                 SELECT * FROM room_occluder_links WHERE ref_id IN (SELECT ref_id FROM capture_occluders)
                    OR linked_ref IN (SELECT ref_id FROM capture_occluders)
                    OR ref_id IN (SELECT id FROM \"references\" WHERE cell_id IN (SELECT id FROM capture_cells));
             CREATE TEMP TABLE capture_rules AS
                 SELECT * FROM room_reference_rules WHERE reference_id IN (SELECT ref_id FROM capture_occluders);",
        )?;
    }
    rooms::export_room_graph(&tx, master, cells)?;
    if verified_occlusion {
        tx.execute_batch(
            "UPDATE room_graph_stats SET value=value+(SELECT count(*) FROM room_occluders
                 WHERE ref_id NOT IN (SELECT ref_id FROM capture_occluders)) WHERE name='invalid_occluders_skipped';
             DELETE FROM room_occluders;
             INSERT INTO room_occluders SELECT * FROM capture_occluders;
             DELETE FROM room_occluder_links;
             INSERT INTO room_occluder_links SELECT * FROM capture_links;
             DELETE FROM room_reference_rules WHERE reference_id IN (SELECT ref_id FROM capture_occluders);
             INSERT INTO room_reference_rules SELECT * FROM capture_rules;
             UPDATE room_graph_stats SET value=(SELECT count(*) FROM room_occluders) WHERE name='occluders';
             INSERT INTO room_graph_stats VALUES ('occlusion_version',1);
             DROP TABLE capture_rules;
             DROP TABLE capture_links;
             DROP TABLE capture_occluders;
             DROP TABLE capture_cells;",
        )?;
    }
    // Preserve the previous source-verified NULL distinction; canonical archives can clear links.
    for (id, pair, a, b) in old_endpoints {
        tx.execute("UPDATE room_portals SET unbounded_a=?3,unbounded_b=?4 WHERE ref_id=?1 AND pair_index=?2",params![id,pair,a,b])?;
    }
    // Rejected source fields are absent from canonical records. Rebuilding cannot resolve
    // their uncertainty; updates affect only rows retained in the selected capture cells.
    for cell in incomplete_cells {
        tx.execute(
            "UPDATE room_graph_cells SET complete=0 WHERE cell_id=?1",
            [cell],
        )?;
    }
    for id in unknown_assignments {
        tx.execute(
            "UPDATE room_reference_rules SET assignment='unknown',target_ref=NULL WHERE reference_id=?1",
            [id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn validate_private_assets(assets: &Path) -> Result<()> {
    ensure!(
        cfg!(unix),
        "private capture preparation requires Unix hard-link validation"
    );
    let absolute = std::path::absolute(assets)?;
    for path in absolute.ancestors() {
        ensure!(
            !path.is_symlink(),
            "fixture assets follow a symlink: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            fs::metadata(assets.join("skyrim_world.db"))?.nlink() == 1,
            "use an independent database, not a hard link to the installed assets"
        );
    }
    ensure!(
        !assets.join("meshes").is_symlink() && !assets.join("skyrim_world.db").is_symlink(),
        "use an independent database and mesh directory, not links to the installed assets"
    );
    Ok(())
}

fn convert_capture_models(
    sources: &Path,
    assets: &Path,
    models: &BTreeSet<PathBuf>,
) -> Result<Vec<serde_json::Value>> {
    let results: Vec<_> = models
        .par_iter()
        .map(|model| {
            let source = sources.join(model);
            let output = assets.join(model).with_extension("glb");
            let converted = source.is_file().then(|| {
                MeshConverter::convert_nif_to_glb(&source, &output)
                    .map_err(|error| format!("{error:#}"))
            });
            serde_json::json!({
                "model": model,
                "converted": matches!(converted, Some(Ok(()))),
                "error": converted.and_then(|result| result.err())
            })
        })
        .collect();
    ensure!(
        results.iter().all(|result| result["converted"] == true),
        "capture preparation incomplete: one or more requested source conversions failed: {}",
        serde_json::to_string(&results)?
    );
    Ok(results)
}

fn prepare_model_sources(
    data: &Path,
    parent: &Path,
    models: &BTreeSet<PathBuf>,
    plugins: &[String],
) -> Result<(tempfile::TempDir, usize)> {
    let sources = tempfile::Builder::new()
        .prefix("room-assignment-nifs-")
        .tempdir_in(parent)?;
    let mut archives: Vec<_> = fs::read_dir(data)?
        .filter_map(|p| p.ok().map(|p| p.path()))
        .filter(|p| p.extension().is_some_and(|s| s.eq_ignore_ascii_case("bsa")))
        .collect();
    archives.sort();
    let mut extracted = 0;
    let skeleton_inputs = MeshConverter::skeleton_search_inputs(models);
    let selected = |path: &Path| models.contains(path) || skeleton_inputs(path);
    for plugin in plugins {
        let stem = Path::new(&plugin)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        for archive in &archives {
            let name = archive
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_lowercase();
            if name == stem || name.starts_with(&(stem.clone() + " - ")) {
                extracted +=
                    ArchiveExtractor::extract_matching(archive, sources.path(), selected)?.len();
            }
        }
    }
    let loose = AssetSourceIndex::build(&[data.to_path_buf()], AssetKind::Mesh)?;
    for source in loose
        .entries()
        .filter(|entry| selected(Path::new(&entry.key)))
    {
        let destination = sources.path().join(&source.key);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if destination.exists() {
            fs::remove_file(&destination)?;
        }
        fs::copy(&source.path, destination)?;
    }
    Ok((sources, extracted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_fixture() -> (Connection, HashMap<u32, RawRecord>, HashSet<u32>) {
        let mut conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let cells = HashSet::from([7, 8]);
        let mut master = HashMap::new();
        for (id, cell, base) in [
            (70u32, 7, rooms::ROOM_MARKER_BASE),
            (80, 8, rooms::ROOM_MARKER_BASE),
            (71, 7, 500),
            (72, 7, 500),
        ] {
            let mut fields = vec![
                (b"NAME".to_vec(), base.to_le_bytes().to_vec()),
                (b"DATA".to_vec(), vec![0; 24]),
            ];
            if base == rooms::ROOM_MARKER_BASE {
                let mut shape = vec![0; 32];
                for offset in [0, 4, 8] {
                    shape[offset..offset + 4].copy_from_slice(&100f32.to_le_bytes());
                }
                shape[28..32].copy_from_slice(&1u32.to_le_bytes());
                fields.push((b"XPRM".to_vec(), shape));
                fields.push((b"XRMR".to_vec(), vec![0, 0, 1, 0]));
            }
            master.insert(
                id,
                RawRecord {
                    form_id: id,
                    record_type: *b"REFR",
                    flags: 0,
                    subrecords: fields,
                    cell_form_id: Some(cell),
                    worldspace_form_id: None,
                    load_order: 0,
                },
            );
        }
        rebuild_room_graph(&mut conn, &master, &cells).unwrap();
        (conn, master, cells)
    }

    #[test]
    fn capture_rebuild_preserves_source_incomplete_cells() {
        let (mut conn, master, cells) = canonical_fixture();
        assert!(master[&70].subrecords.iter().all(|(tag, _)| tag != b"XLRM"));
        // Source restoration retained a rejected join that the canonical archive omitted.
        conn.execute("UPDATE room_graph_cells SET complete=0 WHERE cell_id=7", [])
            .unwrap();
        rebuild_room_graph(&mut conn, &master, &cells).unwrap();
        let states = conn
            .prepare("SELECT cell_id,complete FROM room_graph_cells ORDER BY cell_id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(states, vec![(7, false), (8, true)]);
    }

    #[test]
    fn capture_rebuild_preserves_source_unknown_assignments() {
        let (mut conn, master, cells) = canonical_fixture();
        assert!(master[&71].subrecords.iter().all(|(tag, _)| tag != b"XMBR"));
        // A malformed source override has already been restored as unknown in this pack.
        conn.execute("UPDATE room_reference_rules SET assignment='unknown', target_ref=NULL WHERE reference_id=71", []).unwrap();
        rebuild_room_graph(&mut conn, &master, &cells).unwrap();
        // Supplying model bounds and rebuilding overlap membership must not erase that rule.
        conn.execute("INSERT INTO room_model_bounds VALUES (500,0,0,0,10)", [])
            .unwrap();
        rooms::add_overlap_rows(&mut conn).unwrap();
        let rules = conn.prepare("SELECT reference_id,assignment,target_ref FROM room_reference_rules ORDER BY reference_id").unwrap()
            .query_map([], |row| Ok((row.get::<_,u32>(0)?, row.get::<_,String>(1)?, row.get::<_,Option<u32>>(2)?))).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(
            rules,
            vec![(71, "unknown".into(), None), (72, "auto".into(), None)]
        );
    }

    #[test]
    fn verified_occlusion_replaces_canonical_geometry_links_and_target_rules() {
        let (mut conn, mut master, cells) = canonical_fixture();
        for (id, cell) in [(73, 7), (74, 7), (75, 7), (76, 7), (83, 8)] {
            let mut marker = master[&70].clone();
            marker.form_id = id;
            marker.cell_form_id = Some(cell);
            marker.subrecords[0].1 = rooms::OCCLUSION_MARKER_BASE.to_le_bytes().to_vec();
            marker
                .subrecords
                .iter_mut()
                .find(|(tag, _)| tag == b"XPRM")
                .unwrap()
                .1[28..32]
                .copy_from_slice(&3u32.to_le_bytes());
            marker
                .subrecords
                .push((b"XMBR".to_vec(), 70u32.to_le_bytes().to_vec()));
            master.insert(id, marker);
        }
        rebuild_room_graph(&mut conn, &master, &cells).unwrap();
        conn.execute_batch(
            "INSERT INTO room_graph_stats VALUES ('occlusion_version',1);
             DELETE FROM room_occluders WHERE ref_id=74;
             UPDATE room_occluders SET has_enable_parent=1 WHERE ref_id=73;
             INSERT INTO room_reference_rules VALUES (73,7,'target',70),(75,7,'none',NULL),(83,8,'target',80);
             INSERT INTO room_occluder_links VALUES (73,2,99,1),(999,0,73,0),(83,1,80,1);",
        )
        .unwrap();
        rebuild_room_graph(&mut conn, &master, &HashSet::from([7])).unwrap();
        let occluders: Vec<(u32, bool)> = conn
            .prepare("SELECT ref_id,has_enable_parent FROM room_occluders ORDER BY ref_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(occluders, [(73, true), (75, false), (76, false)]);
        let links: Vec<(u32, u32, u32, bool)> = conn
            .prepare("SELECT * FROM room_occluder_links ORDER BY ref_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(links, [(73, 2, 99, true), (999, 0, 73, false)]);
        let rules = conn
            .prepare(
                "SELECT reference_id,assignment,target_ref FROM room_reference_rules
               WHERE reference_id IN (73,74,75,76,83) ORDER BY reference_id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<u32>>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rules,
            [(73, "target".into(), Some(70)), (75, "none".into(), None)]
        );
        for (stat, expected) in [
            ("occlusion_version", 1),
            ("occluders", 3),
            ("invalid_occluders_skipped", 1),
        ] {
            assert_eq!(
                conn.query_row(
                    "SELECT value FROM room_graph_stats WHERE name=?1",
                    [stat],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn invalid_requests_preserve_the_existing_graph_and_empty_interiors_can_pass() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let assets = root.join("assets");
        let data = root.join("Data");
        fs::create_dir(&assets).unwrap();
        fs::create_dir(&data).unwrap();
        let db = assets.join("skyrim_world.db");
        let conn = Connection::open(&db).unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO cells(id,flags,worldspace_id) VALUES(7,1,NULL),(8,0,1);
            INSERT INTO room_graph_cells VALUES(9,0);
            INSERT INTO room_graph_stats VALUES('visibility_version',2);",
        )
        .unwrap();
        drop(conn);
        let prepare = |cells: &str| {
            run(&[
                data.display().to_string(),
                assets.display().to_string(),
                cells.to_owned(),
            ])
        };
        fs::write(assets.join(RECEIPT), b"previous receipt").unwrap();
        for cells in ["7,999", "7,8"] {
            assert!(
                prepare(cells)
                    .unwrap_err()
                    .to_string()
                    .contains("absent or exterior")
            );
            assert_eq!(fs::read(assets.join(RECEIPT)).unwrap(), b"previous receipt");
            assert_eq!(
                Connection::open(&db)
                    .unwrap()
                    .query_row("SELECT cell_id FROM room_graph_cells", [], |r| r
                        .get::<_, u32>(0))
                    .unwrap(),
                9
            );
        }
        prepare("7").unwrap();
        let receipt: serde_json::Value =
            serde_json::from_reader(fs::File::open(assets.join(RECEIPT)).unwrap()).unwrap();
        assert_eq!(receipt["complete"], true);
        assert_eq!(receipt["requested_models"], 0);
    }

    #[cfg(unix)]
    #[test]
    fn sibling_receipts_are_independent_even_if_originally_hard_linked() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        publish_receipt(&a, &serde_json::json!({"complete":true})).unwrap();
        fs::hard_link(a.join(RECEIPT), b.join(RECEIPT)).unwrap();
        publish_receipt(&b, &serde_json::json!({"complete":false})).unwrap();
        for (copy, complete) in [(a, true), (b, false)] {
            let receipt: serde_json::Value =
                serde_json::from_reader(fs::File::open(copy.join(RECEIPT)).unwrap()).unwrap();
            assert_eq!(receipt["complete"], complete);
        }
    }

    #[cfg(unix)]
    #[test]
    fn capture_preparation_respects_readers_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let assets = root.join("assets");
        fs::create_dir(&assets).unwrap();
        let database = assets.join("skyrim_world.db");
        fs::write(&database, b"independent database").unwrap();
        let report = assets.join(RECEIPT);
        fs::write(&report, b"previous receipt").unwrap();
        let reader = shared::asset_lock::AssetLock::acquire_shared(&assets).unwrap();
        let args = vec![
            root.join("Data").display().to_string(),
            assets.display().to_string(),
            "7".into(),
        ];
        let error = run(&args).unwrap_err();
        assert!(
            error
                .downcast_ref::<shared::asset_lock::AssetLockError>()
                .is_some_and(|e| e.is_held()),
            "{error:#}"
        );
        assert_eq!(fs::read(&database).unwrap(), b"independent database");
        assert_eq!(fs::read(&report).unwrap(), b"previous receipt");
        drop(reader);
        assert!(run(&args).is_err());
        assert!(
            shared::asset_lock::AssetLock::acquire_exclusive(&assets).is_ok(),
            "failed preparation must release its lock"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn capture_preparation_refuses_unverified_database_independence() {
        let root = tempfile::tempdir().unwrap();
        let assets = root.path().join("assets");
        fs::create_dir(&assets).unwrap();
        let database = assets.join("skyrim_world.db");
        fs::write(&database, b"independent database").unwrap();
        let args = vec![
            root.path().join("Data").display().to_string(),
            assets.display().to_string(),
            "7".into(),
        ];
        let error = run(&args).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires Unix hard-link validation")
        );
        assert_eq!(fs::read(&database).unwrap(), b"independent database");
        assert!(!assets.join(RECEIPT).exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_database_must_not_share_its_inode() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("installed.db");
        fs::write(&original, b"installed database").unwrap();
        let assets = root.path().join("assets");
        fs::create_dir(&assets).unwrap();
        fs::hard_link(&original, assets.join("skyrim_world.db")).unwrap();
        assert!(validate_private_assets(&assets).is_err());
        assert_eq!(fs::read(&original).unwrap(), b"installed database");
    }

    #[cfg(unix)]
    #[test]
    fn private_assets_reject_a_symlinked_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let parent = physical.join("independent");
        let assets = parent.join("assets");
        fs::create_dir_all(assets.join("meshes")).unwrap();
        fs::write(assets.join("skyrim_world.db"), b"independent database").unwrap();
        assert!(validate_private_assets(&assets).is_ok());
        let alias = physical.join("alias");
        std::os::unix::fs::symlink(&parent, &alias).unwrap();
        assert!(
            validate_private_assets(&alias.join("assets")).is_err(),
            "an indirect link must be rejected before the receipt or database is opened"
        );
        assert_eq!(
            fs::read(assets.join("skyrim_world.db")).unwrap(),
            b"independent database"
        );
    }

    #[test]
    fn failed_capture_conversions_cannot_reuse_old_glbs() {
        let root = tempfile::tempdir().unwrap();
        let sources = root.path().join("sources");
        fs::create_dir(&sources).unwrap();
        let assets = root.path().join("assets");
        fs::create_dir_all(assets.join("meshes")).unwrap();
        let model = PathBuf::from("meshes/test.nif");
        let output = assets.join(&model).with_extension("glb");
        fs::write(&output, b"previous capture").unwrap();
        let models = BTreeSet::from([model.clone()]);
        assert!(
            convert_capture_models(&sources, &assets, &models).is_err(),
            "a missing source must abort before finalization"
        );
        fs::create_dir(sources.join("meshes")).unwrap();
        fs::write(sources.join(model), b"invalid NIF").unwrap();
        assert!(
            convert_capture_models(&sources, &assets, &models).is_err(),
            "a failed conversion must abort before finalization"
        );
    }

    fn archive(data: &Path) -> PathBuf {
        fs::create_dir_all(data).unwrap();
        let path = data.join("Skyrim - Meshes.bsa");
        fs::write(
            &path,
            dummy_content::bsa::v105(
                &[dummy_content::Entry::new("meshes/test.nif", b"archive")],
                dummy_content::bsa::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn staged_skinned_mesh_matches_the_normal_tree_with_an_unplaced_skeleton() {
        use dummy_content::nif::{LegacySkin, SkinInstanceKind, legacy_skinned_shape};
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        fs::create_dir(&data).unwrap();
        let model = Path::new("meshes/actors/moth/body.nif");
        let skeleton = Path::new("meshes/actors/other/skeleton.nif");
        let skin = LegacySkin {
            name: "CaptureSkin",
            instance_kind: SkinInstanceKind::Plain,
            bone_transforms: [([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0); 2],
            palette: &[0, 1],
            weights: &[[1.0, 0.0, 0.0, 0.0]; 4],
            with_geometry: true,
            joint_override: None,
        };
        let mesh = legacy_skinned_shape(&skin).unwrap();
        use nom_derive::Parse;
        use project_wormhole_nif::nif_header::NifHeader;
        let mut rig = mesh.clone();
        let (blocks, _) = NifHeader::parse(&rig).unwrap();
        let root_node = rig.len() - blocks.len();
        // Give the skeleton root a name and just its two bone children.
        rig[root_node..root_node + 4].copy_from_slice(&0u32.to_le_bytes());
        rig[root_node + 72..root_node + 76].copy_from_slice(&2u32.to_le_bytes());
        let archive = data.join("Skyrim - Meshes.bsa");
        fs::write(
            &archive,
            dummy_content::bsa::v105(
                &[
                    dummy_content::Entry::new(model.to_str().unwrap(), &mesh),
                    dummy_content::Entry::new(skeleton.to_str().unwrap(), &rig),
                ],
                dummy_content::bsa::Compression::None,
            )
            .unwrap(),
        )
        .unwrap();
        let normal = root.path().join("normal");
        ArchiveExtractor::extract(&archive, &normal).unwrap();
        let (staged, count) = prepare_model_sources(
            &data,
            root.path(),
            &BTreeSet::from([model.to_owned()]),
            &["Skyrim.esm".to_owned()],
        )
        .unwrap();
        assert_eq!(count, 2);
        let convert = |tree: &Path| {
            let nif = tree.join(model);
            assert_eq!(MeshConverter::dependency_paths(&nif), [tree.join(skeleton)]);
            let glb = tree.join("output.glb");
            MeshConverter::convert_nif_to_glb(&nif, &glb).unwrap();
            fs::read(glb).unwrap()
        };
        assert_eq!(convert(&normal), convert(staged.path()));
    }

    #[test]
    fn repeated_preparations_do_not_reuse_removed_archive_sources() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        let archive = archive(&data);
        let models = BTreeSet::from([PathBuf::from("meshes/test.nif")]);
        let plugins = vec!["Skyrim.esm".to_owned()];
        let (first, count) = prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
        assert_eq!(count, 1);
        assert!(first.path().join("meshes/test.nif").is_file());
        fs::remove_file(archive).unwrap();
        let (second, count) = prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
        assert_eq!(count, 0);
        assert_ne!(first.path(), second.path());
        assert!(!second.path().join("meshes/test.nif").exists());
    }

    #[test]
    fn selected_loose_meshes_override_archive_bytes_case_insensitively() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        archive(&data);
        fs::create_dir(data.join("Meshes")).unwrap();
        fs::write(data.join("Meshes/Test.NIF"), b"loose").unwrap();
        let models = BTreeSet::from([PathBuf::from("meshes/test.nif")]);
        let (sources, count) =
            prepare_model_sources(&data, root.path(), &models, &["Skyrim.esm".to_owned()]).unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            fs::read(sources.path().join("meshes/test.nif")).unwrap(),
            b"loose"
        );
    }
}
