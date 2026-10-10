//! Project winning door teleport records into the optional runtime door table.

use super::types::ArchivedRecordData;
use rusqlite::{Connection, Result, params};

/// Rebuild door links from this database's canonical winning records, atomically.
/// Meshes, terrain, provenance and producer identities are unchanged.
pub fn rebuild_door_links(conn: &Connection) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let count = project_door_links(&tx)?;
    tx.commit()?;
    Ok(count)
}

pub(super) fn project_door_links(conn: &Connection) -> Result<usize> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS door_links (
            ref_id INTEGER PRIMARY KEY, destination_ref_id INTEGER NOT NULL,
            pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
            rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
            destination_cell_id INTEGER NOT NULL, destination_worldspace_id INTEGER,
            destination_name TEXT
         );
         DELETE FROM door_links;",
    )?;
    let has_name: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('door_links') WHERE name='destination_name')",
        [], |row| row.get(0),
    )?;
    if !has_name {
        conn.execute_batch("ALTER TABLE door_links ADD COLUMN destination_name TEXT;")?;
    }
    let mut sources = conn.prepare(
        "SELECT r.id, raw.data FROM \"references\" r
         JOIN records base ON base.form_id=r.base_form_id AND base.record_type='DOOR'
         JOIN records raw ON raw.form_id=r.id AND raw.record_type='REFR'
         ORDER BY r.id",
    )?;
    let mut destination = conn.prepare(
        "SELECT c.id,c.worldspace_id,c.data,c.interior_name,w.editor_id FROM \"references\" r
         JOIN records base ON base.form_id=r.base_form_id AND base.record_type='DOOR'
         JOIN cells c ON c.id=r.cell_id
         LEFT JOIN worldspaces w ON w.id=c.worldspace_id
         WHERE r.id=?1 AND c.id != 0
           AND (c.worldspace_id IS NULL OR (c.grid_x IS NOT NULL AND c.grid_y IS NOT NULL))",
    )?;
    let mut insert = conn.prepare(
        "INSERT INTO door_links(ref_id,destination_ref_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,
             destination_cell_id,destination_worldspace_id,destination_name)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
    )?;
    let mut rows = sources.query([])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let ref_id: u32 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        let record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&blob).map_err(
            |error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Blob,
                    Box::new(error),
                )
            },
        )?;
        let Some(xtel) = record.subrecords.iter().find(|sub| &sub.tag == b"XTEL") else {
            continue;
        };
        // Both frontends preserve the SE flags word after position and rotation.
        // Never substitute a door's own position for a malformed arrival frame.
        if xtel.data.len() != 32 {
            continue;
        }
        let target = u32::from_le_bytes(xtel.data[..4].try_into().unwrap());
        let mut arrival = [0.0f32; 6];
        for (value, bytes) in arrival.iter_mut().zip(xtel.data[4..28].as_chunks::<4>().0) {
            *value = f32::from_le_bytes(*bytes);
        }
        if target == 0 || arrival.iter().any(|value| !value.is_finite()) {
            continue;
        }
        let mut targets = destination.query([target])?;
        let Some(target_row) = targets.next()? else {
            continue;
        };
        let cell: u32 = target_row.get(0)?;
        let world: Option<u32> = target_row.get(1)?;
        let cell_data: Option<Vec<u8>> = target_row.get(2)?;
        let editor_name: Option<String> = target_row.get(3)?;
        let world_name: Option<String> = target_row.get(4)?;
        let name = if world.is_some() {
            world_name
        } else {
            cell_data
                .and_then(|data| {
                    let record =
                        rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&data).ok()?;
                    let full = record.subrecords.iter().find(|sub| &sub.tag == b"FULL")?;
                    let text = std::str::from_utf8(&full.data).ok()?.trim_end_matches('\0');
                    (!text.is_empty() && !text.chars().any(char::is_control))
                        .then(|| text.to_owned())
                })
                .or(editor_name)
        };
        insert.execute(params![
            ref_id, target, arrival[0], arrival[1], arrival[2], arrival[3], arrival[4], arrival[5],
            cell, world, name
        ])?;
        count += 1;
    }
    Ok(count)
}
