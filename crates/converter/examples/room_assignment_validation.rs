//! Refresh selected source meshes and their derived bounds in a private native-capture fixture.
//!
//! The input pack must already contain source-verified portal endpoints and authored assignments.
//! Requires Unix hard-link validation and exclusive access to the private asset copy.
use color_eyre::{Result, eyre::ensure};
use converter::{
    archive::ArchiveExtractor,
    asset_path::{
        AssetKind, AssetSourceIndex, canonical_asset_path, is_authoring_resource,
        is_authoring_source_name,
    },
    mesh::{MeshConverter, SkeletonIndex, is_skeleton_path},
    pipeline::active_data_bsa_paths,
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
    let asset_lock = shared::asset_lock::AssetLock::acquire_exclusive(assets)?;
    let cells: BTreeSet<u32> = args[2]
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let conn = Connection::open_with_flags(
        assets.join("skyrim_world.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
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
    let sources = capture_scratch(&asset_lock)?;
    let (extracted, staged, textures) =
        prepare_model_sources(data, sources.path(), &models, &plugins)?;
    let results = convert_capture_models(sources.path(), assets, &models, &staged, &textures)?;
    let changed: Vec<_> = results
        .iter()
        .filter(|result| result.converted)
        .map(|result| {
            result
                .model
                .with_extension("glb")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    if !changed.is_empty() {
        converter::integration::refresh_converted_model_bounds(&asset_lock, &changed)?;
    }
    ensure!(
        changed.len() == models.len() && results.iter().all(|result| result.error.is_none()),
        "capture preparation incomplete: one or more requested source conversions failed: {}",
        serde_json::to_string(&results)?
    );
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

fn validate_private_assets(assets: &Path) -> Result<()> {
    ensure!(
        cfg!(unix),
        "private capture preparation requires Unix hard-link validation"
    );
    let absolute = std::path::absolute(assets)?;
    // A trailing slash makes symlink_metadata follow the last directory link on Unix.
    let mut path = PathBuf::new();
    for component in absolute.components() {
        path.push(component);
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

#[derive(serde::Serialize)]
struct CaptureResult {
    model: PathBuf,
    converted: bool,
    error: Option<String>,
}

fn convert_capture_models(
    sources: &Path,
    assets: &Path,
    models: &BTreeSet<PathBuf>,
    staged: &BTreeSet<PathBuf>,
    textures: &BTreeSet<String>,
) -> Result<Vec<CaptureResult>> {
    let paths = staged
        .iter()
        .map(|key| (key.clone(), sources.join(key)))
        .collect();
    let skeletons = SkeletonIndex::from_sources(&paths);
    let keys: Vec<_> = models
        .iter()
        .map(|model| model.with_extension("glb").to_string_lossy().into_owned())
        .collect();
    let root = assets.canonicalize()?;
    let installed =
        converter::integration::installed_glb_paths(&root, keys.iter().map(String::as_str))?;
    let outputs: Vec<_> = models
        .iter()
        .zip(keys)
        .map(|(model, key)| {
            let output = installed
                .get(&key)
                .cloned()
                .unwrap_or_else(|| root.join(key));
            (model, output)
        })
        .collect();
    let results: Vec<_> = outputs
        .par_iter()
        .map(|(model, output)| {
            let source = sources.join(model);
            let converted = if source.is_file() {
                MeshConverter::convert_nif_to_glb_from_index(model, &source, output, &skeletons)
                    .map_err(|error| format!("{error:#}"))
            } else {
                Err(format!("missing staged model {}", source.display()))
            };
            CaptureResult {
                model: (*model).clone(),
                converted: converted.is_ok(),
                error: converted
                    .and_then(|()| {
                        MeshConverter::prune_glb_texture_uris(&root, output, textures)
                            .map(|_| ())
                            .map_err(|error| format!("{error:#}"))
                    })
                    .err(),
            }
        })
        .collect();
    Ok(results)
}

fn capture_scratch(lock: &shared::asset_lock::AssetLock) -> Result<tempfile::TempDir> {
    const NAME: &str = ".room-assignment-nifs";
    const MARKER: &str = ".capture-owner";
    const SIGNATURE: &[u8] = b"Mudcrab private capture scratch v1\n";
    let parent = lock.asset_dir();
    let path = parent.join(NAME);
    if path.try_exists()? || path.is_symlink() {
        ensure!(
            path.symlink_metadata()?.is_dir(),
            "invalid capture scratch directory"
        );
        let owner = path.join(MARKER);
        if owner.try_exists()? {
            ensure!(
                owner.symlink_metadata()?.is_file() && fs::read(&owner)? == SIGNATURE,
                "unrecognized capture scratch ownership at {}",
                path.display()
            );
            ArchiveExtractor::reclaim_abandoned(&path)?;
            fs::remove_dir_all(&path)?;
        } else {
            // An interruption between creating the directory and marking it can leave an empty root.
            fs::remove_dir(&path)?;
        }
    }
    let sources = tempfile::Builder::new()
        .prefix(NAME)
        .rand_bytes(0)
        .tempdir_in(parent)?;
    fs::write(sources.path().join(MARKER), SIGNATURE)?;
    Ok(sources)
}

fn prepare_model_sources(
    data: &Path,
    sources: &Path,
    models: &BTreeSet<PathBuf>,
    plugins: &[String],
) -> Result<(usize, BTreeSet<PathBuf>, BTreeSet<String>)> {
    if models.is_empty() {
        return Ok((0, BTreeSet::new(), BTreeSet::new()));
    }
    let directories: BTreeSet<_> = models.iter().filter_map(|path| path.parent()).collect();
    // Full-cover ranking uses skeletons from all sources and joint names from
    // every winning mesh in a selected model's directory.
    let needed = |path: &Path| {
        path.extension().is_some_and(|ext| ext == "nif")
            && !is_authoring_resource(&path.to_string_lossy())
            && (is_skeleton_path(path)
                || path.parent().is_some_and(|dir| directories.contains(dir)))
    };
    let plugins: Vec<_> = plugins.iter().map(PathBuf::from).collect();
    let archives = active_data_bsa_paths(data, &plugins)?;
    let mut staged = BTreeSet::new();
    let is_runtime = |name: &str| !is_authoring_source_name(name);
    let mut textures: BTreeSet<_> = AssetSourceIndex::build_with_name_filter(
        &[data.to_path_buf()],
        AssetKind::Texture,
        |name| {
            is_runtime(name)
                && Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("dds"))
        },
    )?
    .assets()
    .map(|asset| asset.key.clone())
    .collect();
    let loose = AssetSourceIndex::build_with_name_filter(
        &[data.to_path_buf()],
        AssetKind::Mesh,
        is_runtime,
    )?;
    for source in loose.assets() {
        let path = Path::new(&source.key);
        if needed(path) {
            staged.insert(path.to_owned());
            if let Some(parent) = path.parent() {
                fs::create_dir_all(sources.join(parent))?;
            }
            fs::copy(&source.path, sources.join(path))?;
        }
    }
    // Higher-priority sources claim each path first. Selection and extraction
    // use one open archive, so replacing its filename cannot mix revisions.
    let mut extracted = 0;
    for archive in archives.iter().rev() {
        let files = ArchiveExtractor::extract_matching_with_name_filter(
            archive,
            sources,
            is_runtime,
            |path| {
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("dds"))
                {
                    textures.insert(path.to_string_lossy().into_owned());
                }
                needed(path) && !staged.contains(path)
            },
        )?;
        extracted += files.len();
        staged.extend(files.into_iter().map(|file| file.path));
    }
    Ok((extracted, staged, textures))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare_model_sources(
        data: &Path,
        parent: &Path,
        models: &BTreeSet<PathBuf>,
        plugins: &[String],
    ) -> Result<(tempfile::TempDir, usize, BTreeSet<PathBuf>)> {
        let sources = tempfile::tempdir_in(parent)?;
        let (count, staged, _) =
            super::prepare_model_sources(data, sources.path(), models, plugins)?;
        Ok((sources, count, staged))
    }

    #[test]
    fn capture_retry_recovers_its_own_scratch() {
        let root = tempfile::tempdir().unwrap();
        let lock = shared::asset_lock::AssetLock::acquire_exclusive(root.path()).unwrap();
        let previous = capture_scratch(&lock).unwrap().keep();
        fs::write(previous.join("old.nif"), b"previous run").unwrap();
        let retry = capture_scratch(&lock).unwrap();
        assert_eq!(retry.path(), previous);
        assert!(!retry.path().join("old.nif").exists());
        drop(retry);
        fs::create_dir(&previous).unwrap();
        fs::write(previous.join("unrelated"), b"preserve").unwrap();
        assert!(capture_scratch(&lock).is_err());
        assert!(previous.join("unrelated").exists());
    }

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
            "INSERT INTO room_graph_stats VALUES('assignment_version',1),('occlusion_version',1);
             INSERT INTO room_occluders VALUES(83,8,3,0,0,0,0,0,0,1,1,0,1,0,0,0,NULL);
             INSERT INTO room_occluder_links VALUES(99,0,83,1);
             INSERT INTO statics(id,flags,model_path,bounds_valid,bounds_min_x) VALUES(91,0,'unselected.glb',1,-17);
             INSERT INTO room_reference_rules VALUES(81,8,'unknown',NULL);
             INSERT INTO room_portals VALUES(82,0,8,0,0,0,0,0,0,1,1,NULL,NULL,1,0);",
        )
        .unwrap();
        let fresh = root.join("fresh");
        fs::create_dir(&fresh).unwrap();
        drop(conn);
        fs::copy(
            assets.join("skyrim_world.db"),
            fresh.join("skyrim_world.db"),
        )
        .unwrap();
        for (copy, selection) in [(&assets, "7"), (&assets, "7,8"), (&fresh, "7,8")] {
            args[1] = copy.display().to_string();
            args[2] = selection.into();
            run(&args).unwrap();
            let conn = Connection::open(copy.join("skyrim_world.db")).unwrap();
            let planes: u32 = conn.query_row(
                "SELECT count(*) FROM room_occluders o JOIN room_occluder_links l ON l.linked_ref=o.ref_id
                 WHERE o.cell_id=8 AND o.ref_id=83 AND l.ref_id=99", [], |row| row.get(0)
            ).unwrap();
            assert_eq!(planes, 1);
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
            assert_eq!(
                conn.query_row(
                    "SELECT bounds_min_x FROM statics WHERE id=91 AND bounds_valid=1",
                    [],
                    |r| r.get::<_, f64>(0)
                )
                .unwrap(),
                -17.0
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
            INSERT INTO room_graph_stats VALUES('visibility_version',2),('assignment_version',1);",
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
    fn partial_capture_refreshes_only_successful_outputs() {
        use dummy_content::nif::{StaticShape, static_shape};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let assets = root.join("assets");
        let data = root.join("Data");
        fs::create_dir_all(data.join("meshes")).unwrap();
        fs::create_dir_all(assets.join("meshes")).unwrap();
        let mesh = static_shape(&StaticShape {
            name: "Triangle",
            positions: &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            normals: &[[0., 0., 1.]; 3],
            uvs: &[[0., 0.]; 3],
            indices: &[[0, 1, 2]],
            diffuse: "textures/d.dds",
            normal_texture: "textures/n.dds",
        })
        .unwrap();
        fs::create_dir(data.join("textures")).unwrap();
        fs::write(data.join("textures/d.dds"), b"present source").unwrap();
        fs::write(data.join("meshes/good.nif"), mesh).unwrap();
        fs::write(data.join("meshes/bad.nif"), b"invalid fixture").unwrap();
        fs::write(
            assets.join("meshes/good.glb"),
            b"previous successful output",
        )
        .unwrap();
        fs::write(assets.join("meshes/bad.glb"), b"previous output").unwrap();
        let conn = Connection::open(assets.join("skyrim_world.db")).unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch("INSERT INTO cells(id,flags) VALUES(7,0);
            INSERT INTO room_graph_stats VALUES('visibility_version',2),('assignment_version',1);
            INSERT INTO statics(id,flags,model_path,bounds_valid,bounds_min_x) VALUES
                (91,0,'good.nif',1,-17),(92,0,'bad.nif',1,-17),(93,0,'missing.nif',1,-17);
            INSERT INTO [references](id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z)
                VALUES (11,7,91,0,0,0,0,0,0,0),(12,7,92,0,0,0,0,0,0,0),(13,7,93,0,0,0,0,0,0,0);").unwrap();
        let args = [
            data.display().to_string(),
            assets.display().to_string(),
            "7".into(),
        ];
        let error = run(&args).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requested source conversions failed"),
            "{error:#}"
        );
        let detail = error.to_string();
        let (_, results) = detail
            .split_once("requested source conversions failed: ")
            .unwrap();
        let results: Vec<serde_json::Value> = serde_json::from_str(results).unwrap();
        let missing = results
            .iter()
            .find(|result| result["model"] == "meshes/missing.nif")
            .unwrap();
        assert_eq!(missing["converted"], false);
        let diagnostic = missing["error"].as_str().unwrap();
        assert!(diagnostic.starts_with("missing staged model "));
        assert!(diagnostic.ends_with("/meshes/missing.nif"));
        let bound = |id| {
            conn.query_row(
                "SELECT bounds_min_x FROM statics WHERE id=?1 AND bounds_valid=1",
                [id],
                |r| r.get::<_, f64>(0),
            )
            .unwrap()
        };
        let glb = fs::read(assets.join("meshes/good.glb")).unwrap();
        let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let json: serde_json::Value = serde_json::from_slice(&glb[20..20 + json_len]).unwrap();
        let images = json["images"].as_array().unwrap();
        assert_eq!(images.len(), 1, "{images:?}");
        assert!(images[0]["uri"].as_str().unwrap().contains("/d."));
        assert_eq!(bound(91), 0.);
        assert_eq!(bound(92), -17.);
        assert_eq!(bound(93), -17.);
        assert_eq!(
            fs::read(assets.join("meshes/bad.glb")).unwrap(),
            b"previous output"
        );
        let receipt = fs::read(assets.join("room-assignment-preparation.json")).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&receipt).unwrap()["complete"],
            false
        );
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
        let fixture_alias = physical.join("fixture-alias");
        std::os::unix::fs::symlink(&assets, &fixture_alias).unwrap();
        let receipt = assets.join("room-assignment-preparation.json");
        fs::write(&receipt, b"previous receipt").unwrap();
        for path in [fixture_alias.clone(), fixture_alias.join("")] {
            let args = [
                physical.display().to_string(),
                path.display().to_string(),
                "7".into(),
            ];
            assert!(
                run(&args)
                    .unwrap_err()
                    .to_string()
                    .contains("follow a symlink")
            );
            assert_eq!(fs::read(&receipt).unwrap(), b"previous receipt");
        }
        assert!(validate_private_assets(&assets.join("")).is_ok());
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
            !convert_capture_models(&sources, &assets, &models, &models, &BTreeSet::new()).unwrap()
                [0]
            .converted,
            "a missing source must abort before finalization"
        );
        fs::create_dir(sources.join("meshes")).unwrap();
        fs::write(sources.join(model), b"invalid NIF").unwrap();
        assert!(
            !convert_capture_models(&sources, &assets, &models, &models, &BTreeSet::new()).unwrap()
                [0]
            .converted,
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
        let mut rig = legacy_skinned_shape(&LegacySkin {
            with_geometry: false,
            ..skin
        })
        .unwrap();
        let (blocks, _) = NifHeader::parse(&rig).unwrap();
        let root_node = rig.len() - blocks.len();
        // Name the skeleton root with the first entry in the fixture’s string table.
        rig[root_node..root_node + 4].copy_from_slice(&0u32.to_le_bytes());
        let (_, parsed) = project_wormhole_nif::nif_file::NifFile::parse(&rig).unwrap();
        assert_eq!(parsed.blocks[0].as_node().unwrap().children, [1, 2]);
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
        let (staged, count, _) = prepare_model_sources(
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
    fn archive_texture_inventory_decodes_each_name_once() {
        use dummy_content::{Entry, bsa};

        let data = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let archive = data.path().join("Skyrim - Textures.bsa");
        let models = BTreeSet::from([PathBuf::from("meshes/test.nif")]);
        let entries = [
            Entry::new("textures/a%2520b.dds", b"dds"),
            Entry::new("textures/a%20b.dds", b"dds"),
        ];
        for count in 1..=entries.len() {
            fs::write(
                &archive,
                bsa::v105(&entries[..count], bsa::Compression::None).unwrap(),
            )
            .unwrap();
            let (_, _, textures) = super::prepare_model_sources(
                data.path(),
                sources.path(),
                &models,
                &["Skyrim.esm".into()],
            )
            .unwrap();
            assert!(textures.contains("textures/a%20b.dds"));
            assert_eq!(textures.contains("textures/a b.dds"), count == 2);
        }
    }

    #[test]
    fn repeated_preparations_do_not_reuse_removed_archive_sources() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        let archive = archive(&data);
        let models = BTreeSet::from([PathBuf::from("meshes/test.nif")]);
        let plugins = vec!["Skyrim.esm".to_owned()];
        let (first, count, _) =
            prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
        assert_eq!(count, 1);
        assert!(first.path().join("meshes/test.nif").is_file());
        fs::remove_file(archive).unwrap();
        let (second, count, _) =
            prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
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
        let (sources, count, _) =
            prepare_model_sources(&data, root.path(), &models, &["Skyrim.esm".to_owned()]).unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            fs::read(sources.path().join("meshes/test.nif")).unwrap(),
            b"loose"
        );
    }

    #[test]
    fn raw_authoring_loose_meshes_do_not_collide_with_runtime_meshes() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        archive(&data);
        for (directory, bytes) in [
            ("Meshes", b"runtime".as_slice()),
            ("CalienteTools/Meshes", b"authoring".as_slice()),
            ("%43alienteTools/Meshes", b"encoded authoring".as_slice()),
        ] {
            fs::create_dir_all(data.join(directory)).unwrap();
            fs::write(data.join(directory).join("Test.NIF"), bytes).unwrap();
        }
        let model = PathBuf::from("meshes/test.nif");
        let models = BTreeSet::from([model.clone()]);
        let plugins = ["Skyrim.esm".into()];
        let (sources, count, _) =
            prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
        assert_eq!(count, 0);
        assert_eq!(fs::read(sources.path().join(model)).unwrap(), b"runtime");

        fs::create_dir_all(data.join("Runtime/Meshes")).unwrap();
        fs::write(data.join("Runtime/Meshes/Test.NIF"), b"collision").unwrap();
        let error = prepare_model_sources(&data, root.path(), &models, &plugins).unwrap_err();
        assert!(error.to_string().contains("normalized asset collision"));
    }

    #[test]
    fn raw_authoring_archive_mesh_does_not_override_an_earlier_runtime_mesh() {
        use dummy_content::{Entry, bsa};

        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        archive(&data);
        fs::write(
            data.join("Later.bsa"),
            bsa::v105(
                &[
                    Entry::new("CaLiEnTeToOlS/Meshes/Test.NIF", b"authoring"),
                    Entry::new("meshes/other.nif", b"runtime sibling"),
                ],
                bsa::Compression::Lz4,
            )
            .unwrap(),
        )
        .unwrap();
        let model = PathBuf::from("meshes/test.nif");
        let models = BTreeSet::from([model.clone()]);
        let plugins = ["Skyrim.esm".into(), "Later.esp".into()];
        let (sources, count, _) =
            prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
        assert_eq!(count, 2);
        assert_eq!(fs::read(sources.path().join(model)).unwrap(), b"archive");
        assert_eq!(
            fs::read(sources.path().join("meshes/other.nif")).unwrap(),
            b"runtime sibling"
        );
    }

    #[test]
    fn raw_authoring_archive_mesh_does_not_collide_with_a_later_runtime_mesh() {
        use dummy_content::{Entry, bsa};

        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        archive(&data);
        let model = PathBuf::from("meshes/test.nif");
        let models = BTreeSet::from([model.clone()]);
        let plugins = ["Skyrim.esm".into(), "Later.esp".into()];
        let write_later = |alias| {
            fs::write(
                data.join("Later.bsa"),
                bsa::v105(
                    &[
                        Entry::new("meshes/test.nif", b"later runtime"),
                        Entry::new(alias, b"alias"),
                    ],
                    bsa::Compression::None,
                )
                .unwrap(),
            )
            .unwrap();
        };
        for alias in [
            "CalienteTools/Meshes/Test.NIF",
            "%43alienteTools/Meshes/Test.NIF",
            "tools%5cCalienteTools%2fMeshes/Test.NIF",
        ] {
            write_later(alias);
            let (sources, count, _) =
                prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
            assert_eq!(count, 1);
            assert_eq!(
                fs::read(sources.path().join(&model)).unwrap(),
                b"later runtime"
            );
        }

        write_later("Runtime/Meshes/Test.NIF");
        let error = prepare_model_sources(&data, root.path(), &models, &plugins).unwrap_err();
        assert!(error.to_string().contains("normalized path collision"));
    }

    #[test]
    fn selected_skin_uses_an_unselected_winning_skeleton() {
        use dummy_content::{
            Entry,
            bsa::{Compression, v105},
            nif::{LegacySkin, SkinInstanceKind, legacy_skinned_shape},
        };
        use nom_derive::Parse;
        use project_wormhole_nif::nif_header::NifHeader;
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Data");
        let assets = root.path().join("assets");
        fs::create_dir(&data).unwrap();
        fs::create_dir(&assets).unwrap();
        let mut spec = LegacySkin {
            name: "Body",
            instance_kind: SkinInstanceKind::Plain,
            bone_transforms: [([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0); 2],
            palette: &[1, 0],
            weights: &[[0.75, 0.25, 0.0, 0.0]; 4],
            with_geometry: true,
            joint_override: None,
        };
        let mesh = legacy_skinned_shape(&spec).unwrap();
        let mut sibling = mesh.clone();
        let name = sibling
            .windows(5)
            .position(|name| name == b"Bone1")
            .unwrap();
        sibling[name..name + 5].copy_from_slice(b"Extra");
        spec.with_geometry = false;
        spec.name = "Extra";
        let mut winner = legacy_skinned_shape(&spec).unwrap();
        let (body, header) = NifHeader::parse(&winner).unwrap();
        let root_start = winner.len() - body.len();
        let first_bone = root_start + header.block_size_index[0] as usize;
        winner[root_start..root_start + 4].copy_from_slice(&0u32.to_le_bytes());
        winner[first_bone + 16..first_bone + 20].copy_from_slice(&17.0f32.to_le_bytes());
        // Both rigs cover Body. Only the later-sorting rig also names the sibling's Extra joint.
        let mut alternate = winner.clone();
        let name = alternate
            .windows(5)
            .position(|name| name == b"Extra")
            .unwrap();
        alternate[name..name + 5].copy_from_slice(b"Other");
        alternate[first_bone + 16..first_bone + 20].copy_from_slice(&7.0f32.to_le_bytes());
        let sibling_path = "meshes/actors/selected/sibling.nif";
        let alternate_path = "meshes/rigs/another/skeleton.nif";
        let model = "meshes/actors/selected/50%fern.nif";
        let skeleton = "meshes/rigs/other/skeleton.nif";
        fs::write(data.join("Later Extra.esp"), b"disabled plugin").unwrap();
        fs::create_dir(data.join("mod-assets")).unwrap();
        for (name, entries) in [
            (
                "Skyrim - Meshes.bsa",
                vec![
                    Entry::new("meshes/actors/selected/50%25fern.nif", &mesh),
                    Entry::new(skeleton, b"shadowed"),
                    Entry::new(sibling_path, &sibling),
                    Entry::new(alternate_path, &alternate),
                ],
            ),
            ("mod-assets/Later.bsa", vec![Entry::new(skeleton, &winner)]),
            (
                "Later Extra.bsa",
                vec![Entry::new(skeleton, b"disabled override")],
            ),
        ] {
            fs::write(data.join(name), v105(&entries, Compression::None).unwrap()).unwrap();
        }
        let models = BTreeSet::from([PathBuf::from(model)]);
        let plugins = ["Skyrim.esm".into(), "Later.esp".into()];
        let (sources, count, mut staged) =
            prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
        assert_eq!(count, 4);
        assert_eq!(fs::read(sources.path().join(skeleton)).unwrap(), winner);
        let output = assets.join("Meshes/Actors/Selected/50%Fern.GLB");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, b"old winner").unwrap();
        convert_capture_models(sources.path(), &assets, &models, &staged, &BTreeSet::new())
            .unwrap();
        let captured = fs::read(&output).unwrap();
        let bone_translation = |bytes: &[u8]| {
            gltf::Gltf::from_slice(bytes)
                .unwrap()
                .skins()
                .flat_map(|skin| skin.joints())
                .find(|node| node.name() == Some("Bone0"))
                .unwrap()
                .transform()
                .decomposed()
                .0
        };
        assert_eq!(bone_translation(&captured), [17.0, 0.0, 0.0]);
        MeshConverter::convert_nif_to_glb_with_skeleton(
            &sources.path().join(model),
            &output,
            Some(&sources.path().join(skeleton)),
        )
        .unwrap();
        MeshConverter::prune_glb_texture_uris(&assets, &output, &BTreeSet::new()).unwrap();
        assert_eq!(fs::read(&output).unwrap(), captured);
        MeshConverter::convert_nif_to_glb_with_skeleton(&sources.path().join(model), &output, None)
            .unwrap();
        assert_eq!(bone_translation(&fs::read(&output).unwrap()), [0.0; 3]);
        fs::remove_file(sources.path().join(sibling_path)).unwrap();
        staged.remove(Path::new(sibling_path));
        convert_capture_models(sources.path(), &assets, &models, &staged, &BTreeSet::new())
            .unwrap();
        assert_eq!(
            bone_translation(&fs::read(output).unwrap()),
            [7.0, 0.0, 0.0]
        );
    }
}
