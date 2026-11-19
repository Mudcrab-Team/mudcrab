//! Refresh selected source meshes and their derived bounds in a private native-capture fixture.
//!
//! The input pack must already contain source-verified portal endpoints and authored assignments.
//! Requires Unix hard-link validation and exclusive access to the private asset copy.
use color_eyre::{Result, eyre::ensure};
use converter::{
    archive::ArchiveExtractor,
    asset_path::{AssetKind, AssetSourceIndex, canonical_asset_path},
    mesh::MeshConverter,
};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::BTreeSet,
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
    // Any failure below leaves a visible incomplete marker instead of a previous success receipt.
    write_receipt(assets, &serde_json::json!({"complete": false}))?;
    let cells: BTreeSet<u32> = args[2]
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let conn = Connection::open_with_flags(
        assets.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let verified = conn.query_row(
        "SELECT count(*)=2 FROM room_graph_stats WHERE (name='visibility_version' AND value=2)
         OR (name='assignment_version' AND value=1)",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    ensure!(
        verified,
        "capture preparation requires source-verified portal endpoints and authored room assignments; reconvert the pack"
    );
    let mut models = BTreeSet::new();
    for &cell in &cells {
        ensure!(
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM cells WHERE id=?1)",
                [cell],
                |row| row.get::<_, bool>(0)
            )?,
            "requested cell {cell:#010x} is absent from the fixture database"
        );
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
    let plugins: Vec<String> = conn
        .prepare("SELECT name FROM plugins ORDER BY priority")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(conn);
    let (sources, extracted) =
        prepare_model_sources(data, assets.parent().unwrap(), &models, &plugins)?;
    let results = convert_capture_models(sources.path(), assets, &models)?;
    converter::integration::finalize_world_database(assets)?;
    let conn = Connection::open(assets.join("skyrim_world.db"))?;
    let bounds: u64 = conn.query_row("SELECT count(*) FROM room_model_bounds", [], |r| r.get(0))?;
    let report = serde_json::json!({
        "complete": true,
        "scope": "private capture fixture; source-verified portal endpoints and authored assignments retained",
        "cells": cells,
        "requested_models": models.len(),
        "extracted_entries": extracted,
        "base_records_with_source_bounds": bounds,
        "results": results,
    });
    write_receipt(assets, &report)?;
    println!(
        "Prepared {} models, {bounds} base records with source bounds",
        models.len()
    );
    Ok(())
}

fn write_receipt(assets: &Path, report: &serde_json::Value) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(assets)?;
    serde_json::to_writer_pretty(file.as_file_mut(), report)?;
    file.persist(assets.join("room-assignment-preparation.json"))?;
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
                    ArchiveExtractor::extract_paths(archive, sources.path(), models)?.len();
            }
        }
    }
    let loose = AssetSourceIndex::build(&[data.to_path_buf()], AssetKind::Mesh)?;
    for model in models {
        if let Some(source) = loose.get(&model.to_string_lossy()) {
            let destination = sources.path().join(model);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            if destination.exists() {
                fs::remove_file(&destination)?;
            }
            fs::copy(&source.path, destination)?;
        }
    }
    Ok((sources, extracted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use converter::esm::exporter::create_tables;

    #[cfg(unix)]
    #[test]
    fn repeated_cell_selections_require_and_preserve_source_assignments() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let assets = root.join("assets");
        let data = root.join("Data");
        fs::create_dir(&assets).unwrap();
        fs::create_dir(&data).unwrap();
        let conn = Connection::open(assets.join("skyrim_world.db")).unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO cells(id,flags) VALUES(7,0),(8,0);
             INSERT INTO room_graph_stats VALUES('visibility_version',2);
             INSERT INTO room_graph_cells VALUES(7,1),(8,0);",
        )
        .unwrap();
        let mut args = [
            data.display().to_string(),
            assets.display().to_string(),
            "7".into(),
        ];
        assert!(
            run(&args)
                .unwrap_err()
                .to_string()
                .contains("authored room assignments")
        );
        conn.execute_batch(
            "INSERT INTO room_graph_stats VALUES('assignment_version',1);
             INSERT INTO room_reference_rules VALUES(81,8,'unknown',NULL);
             INSERT INTO room_portals VALUES(82,0,8,0,0,0,0,0,0,1,1,NULL,NULL,1,0);",
        )
        .unwrap();
        for selection in ["7", "7,8"] {
            args[2] = selection.into();
            run(&args).unwrap();
            let state: (bool, String, bool, bool) = conn
                .query_row(
                    "SELECT g.complete,r.assignment,p.unbounded_a,p.unbounded_b
                 FROM room_graph_cells g JOIN room_reference_rules r USING(cell_id)
                 JOIN room_portals p USING(cell_id)
                 WHERE g.cell_id=8 AND r.reference_id=81 AND p.ref_id=82",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(state, (false, "unknown".to_owned(), true, false));
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
        let report = assets.join("room-assignment-preparation.json");
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

    #[cfg(unix)]
    #[test]
    fn missing_cell_preserves_graph_and_sibling_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let successful = serde_json::json!({"complete": true});
        write_receipt(&first, &successful).unwrap();
        let db = second.join("skyrim_world.db");
        let conn = Connection::open(&db).unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO cells(id,flags) VALUES(7,0);
          INSERT INTO room_graph_stats(name,value) VALUES('visibility_version',2),('assignment_version',1);
          INSERT INTO room_graph_cells(cell_id,complete) VALUES(7,1);",
        )
        .unwrap();
        let args = [
            root.display().to_string(),
            second.display().to_string(),
            "7,8".into(),
        ];
        assert!(run(&args).unwrap_err().to_string().contains("0x00000008"));
        assert!(
            conn.query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=7",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );
        for (copy, expected) in [
            (first, successful),
            (second, serde_json::json!({"complete":false})),
        ] {
            let saved = fs::read(copy.join("room-assignment-preparation.json")).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&saved).unwrap(),
                expected
            );
        }
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
        assert!(!assets.join("room-assignment-preparation.json").exists());
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
