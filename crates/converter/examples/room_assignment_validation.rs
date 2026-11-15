//! Rebuild room metadata and selected source meshes in a private asset copy for native captures.
use color_eyre::{Result, eyre::ensure};
use converter::{
    archive::ArchiveExtractor,
    asset_path::{AssetKind, canonical_asset_path},
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
    ensure!(
        args.len() == 3,
        "usage: room_assignment_validation <Skyrim Data> <private assets> <cell-id,cell-id,...>"
    );
    let data = Path::new(&args[0]);
    let assets = Path::new(&args[1]);
    ensure!(
        !assets.join("meshes").is_symlink() && !assets.join("skyrim_world.db").is_symlink(),
        "use an independent database and mesh directory, not links to the installed assets"
    );
    let cells: HashSet<u32> = args[2]
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let mut conn = Connection::open(assets.join("skyrim_world.db"))?;
    create_tables(&conn)?;
    ensure!(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='visibility_version' AND value=2)",
        [], |row| row.get::<_, bool>(0))?, "fixture input must already have source-verified portal endpoints");
    let old_endpoints: Vec<_> = conn
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
    let tx = conn.transaction()?;
    rooms::export_room_graph(&tx, &master, &cells)?;
    // Preserve the previous source-verified NULL distinction; canonical archives can clear links.
    for (id, pair, a, b) in old_endpoints {
        tx.execute("UPDATE room_portals SET unbounded_a=?3,unbounded_b=?4 WHERE ref_id=?1 AND pair_index=?2",params![id,pair,a,b])?;
    }
    tx.commit()?;
    drop(conn);
    let sources = assets.parent().unwrap().join("room-assignment-nifs");
    fs::create_dir_all(&sources)?;
    let plugins: Vec<String> = Connection::open(assets.join("skyrim_world.db"))?
        .prepare("SELECT name FROM plugins ORDER BY priority")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
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
                extracted += ArchiveExtractor::extract_paths(archive, &sources, &models)?.len();
            }
        }
    }
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
    converter::integration::finalize_world_database(assets)?;
    let conn = Connection::open(assets.join("skyrim_world.db"))?;
    let bounds: u64 = conn.query_row("SELECT count(*) FROM room_model_bounds", [], |r| r.get(0))?;
    let report = serde_json::json!({
        "scope": "private capture fixture; source-verified endpoint flags retained",
        "cells": cells,
        "requested_models": models.len(),
        "extracted_entries": extracted,
        "base_records_with_source_bounds": bounds,
        "results": results,
    });
    fs::write(
        assets
            .parent()
            .unwrap()
            .join("room-assignment-preparation.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "Prepared {} models, {bounds} base records with source bounds",
        models.len()
    );
    Ok(())
}
