//! Refresh selected source meshes and their derived bounds in a private native-capture fixture.
//!
//! The input pack must already contain source-verified portal endpoints and authored assignments.
//! Requires Unix hard-link validation and exclusive access to the private asset copy.
use color_eyre::{Result, eyre::ensure};
use converter::{
    archive::ArchiveExtractor,
    asset_path::{AssetKind, AssetSourceIndex, canonical_asset_path, is_authoring_resource},
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
    let _asset_lock = shared::asset_lock::AssetLock::acquire_exclusive(assets)?;
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

fn convert_capture_models(
    sources: &Path,
    assets: &Path,
    models: &BTreeSet<PathBuf>,
) -> Result<Vec<serde_json::Value>> {
    let index = AssetSourceIndex::build(&[sources.to_path_buf()], AssetKind::Mesh)?;
    let paths = index
        .assets()
        .map(|asset| (PathBuf::from(&asset.key), asset.path.clone()))
        .collect();
    let skeletons = SkeletonIndex::from_sources(&paths);
    let results: Vec<_> = models
        .par_iter()
        .map(|model| {
            let source = sources.join(model);
            let output = assets.join(model).with_extension("glb");
            let converted = source.is_file().then(|| {
                MeshConverter::convert_nif_to_glb_from_index(model, &source, &output, &skeletons)
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
    if models.is_empty() {
        return Ok((sources, 0));
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
    let loose = AssetSourceIndex::build(&[data.to_path_buf()], AssetKind::Mesh)?;
    for source in loose.assets() {
        let path = Path::new(&source.key);
        if needed(path) && !is_authoring_resource(&source.relative_source) {
            staged.insert(path.to_owned());
            if let Some(parent) = path.parent() {
                fs::create_dir_all(sources.path().join(parent))?;
            }
            fs::copy(&source.path, sources.path().join(path))?;
        }
    }
    // Higher-priority sources claim each path first. Selection and extraction
    // use one open archive, so replacing its filename cannot mix revisions.
    let mut extracted = 0;
    for archive in archives.iter().rev() {
        let files = ArchiveExtractor::extract_matching(archive, sources.path(), |path| {
            needed(path) && !staged.contains(path)
        })?;
        extracted += files.len();
        staged.extend(files.into_iter().map(|file| file.path));
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
            "INSERT INTO room_graph_stats VALUES('assignment_version',1),('occlusion_version',1);
             INSERT INTO room_occluders VALUES(83,8,3,0,0,0,0,0,0,1,1,0,1,0,0,0,NULL);
             INSERT INTO room_occluder_links VALUES(99,0,83,1);
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
        assert_eq!(count, 0);
        assert_eq!(
            fs::read(sources.path().join("meshes/test.nif")).unwrap(),
            b"loose"
        );
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
        let model = "meshes/actors/selected/body.nif";
        let skeleton = "meshes/rigs/other/skeleton.nif";
        fs::write(data.join("Later Extra.esp"), b"disabled plugin").unwrap();
        fs::create_dir(data.join("mod-assets")).unwrap();
        for (name, entries) in [
            (
                "Skyrim - Meshes.bsa",
                vec![
                    Entry::new(model, &mesh),
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
        let (sources, count) =
            prepare_model_sources(&data, root.path(), &models, &plugins).unwrap();
        assert_eq!(count, 4);
        assert_eq!(fs::read(sources.path().join(skeleton)).unwrap(), winner);
        convert_capture_models(sources.path(), &assets, &models).unwrap();
        let output = assets.join(model).with_extension("glb");
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
        assert_eq!(fs::read(&output).unwrap(), captured);
        MeshConverter::convert_nif_to_glb_with_skeleton(&sources.path().join(model), &output, None)
            .unwrap();
        assert_eq!(bone_translation(&fs::read(&output).unwrap()), [0.0; 3]);
        fs::remove_file(sources.path().join(sibling_path)).unwrap();
        convert_capture_models(sources.path(), &assets, &models).unwrap();
        assert_eq!(
            bone_translation(&fs::read(output).unwrap()),
            [7.0, 0.0, 0.0]
        );
    }
}
