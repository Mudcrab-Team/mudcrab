//! Rebuild the movement projection of an existing schema-4 or newer compatible package.
//! Reads the package's own winning raw records; no source plugin is reparsed.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use converter::esm::{
    exporter::{create_tables, export_to_db},
    records::RawRecord,
    types::ArchivedRecordData,
};
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

fn main() -> Result<()> {
    color_eyre::install()?;
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or_else(|| {
        color_eyre::eyre::eyre!("usage: movement-profile-annotate <skyrim_world.db>")
    })?);
    annotate(&path)?;
    println!(
        "movement profile projected from winning records in {}",
        path.display()
    );
    Ok(())
}

/// Rebuilds the movement projection of the package in place, refusing a package that is not
/// already there: an empty database is not a movement profile.
fn annotate(path: &Path) -> Result<()> {
    let root = path
        .parent()
        .filter(|root| !root.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let _lock = shared::asset_lock::AssetLock::acquire_exclusive(root)
        .wrap_err("close asset readers before rebuilding the movement profile")?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .wrap_err_with(|| format!("opening {}", path.display()))?;
    let version: u32 = conn.query_row("SELECT version FROM schema_info", [], |row| row.get(0))?;
    ensure!(
        (4..=shared::WORLD_DATABASE_SCHEMA_VERSION).contains(&version),
        "database schema {version} is unsupported"
    );
    let records = movement_records(&conn)?;
    create_tables(&conn)?;
    export_to_db(&conn, &records)?;
    Ok(())
}

/// Reads every winning `RACE`, `MOVT` and `GMST` record the package retains, keyed by FormID.
///
/// The exporter applies its own filters inside those types: it projects every `RACE` (and so
/// needs all of them, not the player race alone), the `NPC_Default_MT` `MOVT`, and the movement
/// `GMST` editor IDs. Projecting this whole set therefore rebuilds exactly the rows the pipeline
/// writes, so running the tool twice, or after a modded pack, cannot lose another race's links.
fn movement_records(conn: &Connection) -> Result<HashMap<u32, RawRecord>> {
    let mut statement = conn.prepare(
        "SELECT form_id, record_type, data, load_order FROM records \
         WHERE record_type IN ('RACE', 'MOVT', 'GMST')",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, u32>(3)?,
        ))
    })?;
    let mut records = HashMap::new();
    for row in rows {
        let (form_id, record_type, blob, load_order) = row?;
        let decoded = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&blob)
            .wrap_err_with(|| format!("decoding record {form_id:08X} {record_type}"))?;
        let record_type: [u8; 4] = record_type.as_bytes().try_into().map_err(|_| {
            color_eyre::eyre::eyre!(
                "record {form_id:08X} has a {}-byte type tag",
                record_type.len()
            )
        })?;
        records.insert(
            form_id,
            RawRecord {
                form_id,
                record_type,
                flags: 0,
                subrecords: decoded
                    .subrecords
                    .into_iter()
                    .map(|sub| (sub.tag.to_vec(), sub.data))
                    .collect(),
                cell_form_id: None,
                worldspace_form_id: None,
                load_order,
            },
        );
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use shared::records::ArchivedSubRecord;

    fn record_blob(subrecords: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData {
            subrecords: subrecords
                .iter()
                .map(|(tag, data)| ArchivedSubRecord {
                    tag: *tag,
                    data: data.clone(),
                })
                .collect(),
        })
        .unwrap()
        .to_vec()
    }

    fn insert_record(
        connection: &Connection,
        form_id: u32,
        record_type: &str,
        subrecords: &[([u8; 4], Vec<u8>)],
    ) {
        connection
            .execute(
                "INSERT INTO records(form_id, record_type, load_order, data) VALUES (?1, ?2, 0, ?3)",
                params![form_id, record_type, record_blob(subrecords)],
            )
            .unwrap();
    }

    /// A package holding the movement records the pipeline writes (one `MOVT`, two `GMST`s)
    /// plus the given races, each linked to the same movement type.
    fn package_with_races(races: &[(u32, u32)]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let pack = directory.path().join("pack");
        std::fs::create_dir(&pack).unwrap();
        let path = pack.join("skyrim_world.db");
        let connection = Connection::open(&path).unwrap();
        create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO schema_info(version) VALUES (?1)",
                params![shared::WORLD_DATABASE_SCHEMA_VERSION],
            )
            .unwrap();
        for (form_id, walk_movt_id) in races {
            insert_record(
                &connection,
                *form_id,
                "RACE",
                &[
                    (*b"EDID", b"TestRace\0".to_vec()),
                    (*b"WKMV", walk_movt_id.to_le_bytes().to_vec()),
                ],
            );
        }
        insert_record(
            &connection,
            0x0003_580D,
            "MOVT",
            &[
                (*b"EDID", b"NPC_Default_MT\0".to_vec()),
                (
                    *b"SPED",
                    [100.0_f32; 10]
                        .iter()
                        .flat_map(|speed| speed.to_le_bytes())
                        .collect(),
                ),
            ],
        );
        for (form_id, editor_id) in [
            (0x0001_EC72_u32, &b"fMoveCharWalkBase\0"[..]),
            (0x000A_BEF6, &b"fJumpHeightMin\0"[..]),
        ] {
            insert_record(
                &connection,
                form_id,
                "GMST",
                &[
                    (*b"EDID", editor_id.to_vec()),
                    (*b"DATA", 100.0_f32.to_le_bytes().to_vec()),
                ],
            );
        }
        (directory, path)
    }

    #[test]
    fn projects_a_link_for_every_race_in_the_package() {
        // The player race and a modded race the old hand-picked selection did not name.
        let (_directory, path) =
            package_with_races(&[(0x0000_1234, 0x0003_580D), (0x0001_3746, 0x0003_580D)]);
        // A repeated run replaces the projection: the exact rows and counts below would
        // grow if a run kept stale or duplicate rows.
        annotate(&path).unwrap();
        annotate(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        let links: Vec<(u32, Option<u32>)> = connection
            .prepare("SELECT id, walk_movt_id FROM race_movement_links ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            links,
            vec![
                (0x0000_1234, Some(0x0003_580D)),
                (0x0001_3746, Some(0x0003_580D))
            ]
        );
        // The MOVT and GMST projections still come from the package's own records.
        let count = |table: &str| {
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, u32>(0)
                })
                .unwrap()
        };
        assert_eq!(count("movement_types"), 1);
        assert_eq!(count("movement_game_settings"), 2);
    }

    #[test]
    fn refuses_a_package_that_does_not_exist_without_creating_one() {
        let (_directory, path) = package_with_races(&[]);
        std::fs::remove_file(&path).unwrap();
        assert!(annotate(&path).is_err());
        assert!(!path.exists(), "the tool created {}", path.display());
    }
}
