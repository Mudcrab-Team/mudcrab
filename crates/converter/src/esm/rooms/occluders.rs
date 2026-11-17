//! Authored occlusion primitives. Runtime ownership and activation remain per-view policy.

use super::{OCCLUSION_MARKER_BASE, RawRecord, SubrecordView, f32_at, transform_of, u32_at};
use rusqlite::{Connection, Result, Transaction, params};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct Exported {
    pub count: u64,
    pub invalid: u64,
    pub cells: HashSet<u32>,
    pub incomplete_cells: HashSet<u32>,
}

pub(super) fn export(tx: &Transaction<'_>, records: &[(&u32, &RawRecord)]) -> Result<Exported> {
    let mut result = Exported::default();
    for &(&id, record) in records {
        let view = SubrecordView::new(&record.subrecords);
        if record.record_type != *b"REFR"
            || view.get_form_id(b"NAME") != Some(OCCLUSION_MARKER_BASE)
            || record.is_deleted()
        {
            continue;
        }
        let cell = record.cell_form_id.unwrap_or(0);
        result.cells.insert(cell);
        // Links survive invalid geometry: otherwise a valid linked target could appear
        // independent and become an unsafe occluder.
        for (occurrence, data) in view.find_all(b"XORD").enumerate() {
            if data.len() != 16 {
                result.incomplete_cells.insert(cell);
                continue;
            }
            for (slot, bytes) in data.as_chunks::<4>().0.iter().enumerate() {
                let linked = u32::from_le_bytes(*bytes);
                if linked != 0 {
                    tx.execute(
                        "INSERT INTO room_occluder_links(ref_id,link_index,linked_ref) VALUES(?1,?2,?3)",
                        params![id, (occurrence * 4 + slot) as i64, linked],
                    )?;
                }
            }
        }
        let Some(primitive) = view.find(b"XPRM").filter(|data| data.len() == 32) else {
            result.invalid += 1;
            continue;
        };
        let shape = u32_at(primitive, 28).unwrap();
        let half = [
            f32_at(primitive, 0).unwrap(),
            f32_at(primitive, 4).unwrap(),
            f32_at(primitive, 8).unwrap(),
        ];
        let Some((position, rotation)) = transform_of(&view) else {
            result.invalid += 1;
            continue;
        };
        let scale = match view.find(b"XSCL") {
            None => Some(1.0),
            Some(data) if data.len() == 4 => f32_at(data, 0),
            Some(_) => None,
        };
        let Some(scale) = scale.filter(|scale| scale.is_finite() && *scale > 0.0) else {
            result.invalid += 1;
            continue;
        };
        let valid_half = match shape {
            1 => half.iter().all(|&value| value > 0.0),
            3 => half[0] > 0.0 && half[1] >= 0.0 && half[2] > 0.0,
            _ => false,
        };
        if !valid_half
            || !half
                .iter()
                .chain(&position)
                .chain(&rotation)
                .all(|value| value.is_finite())
            || !view.find(b"DATA").is_some_and(|data| data.len() == 24)
        {
            result.invalid += 1;
            continue;
        }
        let has_links = view.find(b"XORD").is_some();
        let target = view
            .find(b"XMBR")
            .map(|data| u32_at(data, 0).filter(|_| data.len() == 4).unwrap_or(0));
        tx.execute(
            "INSERT INTO room_occluders(ref_id,cell_id,shape,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z,scale,header_flags,has_enable_parent,has_links,target_ref)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                id,
                cell,
                shape,
                position[0],
                position[1],
                position[2],
                rotation[0],
                rotation[1],
                rotation[2],
                half[0],
                half[1],
                half[2],
                scale,
                record.flags,
                view.find(b"XESP").is_some(),
                has_links,
                target
            ],
        )?;
        result.count += 1;
    }
    Ok(result)
}

/// Canonical decoding can discard malformed optional fields. Keep source presence and
/// reject repaired geometry before publishing a trusted full-export version.
pub(crate) fn restore_source_provenance<'a>(
    conn: &Connection,
    canonical: &HashMap<u32, RawRecord>,
    records: impl IntoIterator<Item = &'a crate::records::DecodedRecord>,
) -> Result<()> {
    let mut removed = 0;
    for record in records {
        if record.record_type != *b"REFR" || record.flags & 0x20 != 0 {
            continue;
        }
        let id = record.form_id;
        let Some(canonical) = canonical.get(&id) else {
            continue;
        };
        let view = SubrecordView::new(&canonical.subrecords);
        // Runtime projection can omit a record whose other fields are invalid. Its
        // resolved links still protect any independently valid target primitives.
        for (occurrence, data) in view.find_all(b"XORD").enumerate() {
            if data.len() != 16 {
                continue;
            }
            for (slot, bytes) in data.as_chunks::<4>().0.iter().enumerate() {
                let linked = u32::from_le_bytes(*bytes);
                if linked != 0 {
                    conn.execute("INSERT OR REPLACE INTO room_occluder_links(ref_id,link_index,linked_ref) VALUES(?1,?2,?3)", params![id,(occurrence*4+slot) as i64,linked])?;
                }
            }
        }
        let source = |tag| record.unique_source_subrecord(tag);
        let present = |tag| {
            source(tag).is_some()
                || view.find(tag).is_some()
                || record.rejected_fields.contains(tag)
        };
        if !record.payload_complete {
            conn.execute(
                "UPDATE room_graph_cells SET complete=0 WHERE cell_id=?1",
                [record.cell_form_id],
            )?;
            removed += conn.execute("DELETE FROM room_occluders WHERE ref_id=?1", [id])?;
            continue;
        }
        let has_links = present(b"XORD");
        conn.execute(
            "UPDATE room_occluders SET header_flags=?2,has_enable_parent=?3,has_links=?4,target_ref=CASE WHEN ?5 THEN COALESCE(target_ref,0) ELSE target_ref END WHERE ref_id=?1",
            params![id, record.flags, present(b"XESP"), has_links, present(b"XMBR")],
        )?;
        // A rejected link may have hidden an incoming edge to a different source.
        if has_links
            && (source(b"XORD").is_some_and(|data| data.len() != 16)
                || record.rejected_fields.contains(b"XORD")
                || view.find(b"XORD").is_none())
        {
            conn.execute(
                "UPDATE room_graph_cells SET complete=0 WHERE cell_id=?1",
                [record.cell_form_id],
            )?;
        }
        let shape = source(b"XPRM");
        let placement = source(b"DATA");
        let scale = source(b"XSCL");
        let valid = shape.is_some_and(|data| {
            if data.len() != 32 {
                return false;
            }
            let half = [0, 4, 8].map(|offset| f32_at(data, offset).unwrap());
            half.iter().all(|value| value.is_finite())
                && match u32_at(data, 28) {
                    Some(1) => half.iter().all(|&value| value > 0.0),
                    Some(3) => half[0] > 0.0 && half[1] >= 0.0 && half[2] > 0.0,
                    _ => false,
                }
        }) && placement.is_some_and(|data| {
            data.len() == 24
                && data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|bytes| f32::from_le_bytes(*bytes).is_finite())
        }) && (!present(b"XSCL")
            || scale.is_some_and(|data| {
                data.len() == 4
                    && f32_at(data, 0).is_some_and(|value| value.is_finite() && value > 0.0)
            }));
        if !valid {
            removed += conn.execute("DELETE FROM room_occluders WHERE ref_id=?1", [id])?;
        }
    }
    // The source pass may remove canonical defaults; retain report/table agreement.
    conn.execute("UPDATE room_graph_stats SET value=(SELECT COUNT(*) FROM room_occluders) WHERE name='occluders'", [])?;
    conn.execute(
        "UPDATE room_graph_stats SET value=value+?1 WHERE name='invalid_occluders_skipped'",
        [removed as u64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::{exporter::create_tables, rooms::export_room_graph};
    use rusqlite::Connection;
    use std::collections::HashMap;

    fn marker(id: u32, shape: u32) -> RawRecord {
        let mut primitive = [12.0f32, if shape == 3 { 0.0 } else { 23.0 }, 34.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        primitive.resize(28, 0);
        primitive.extend(shape.to_le_bytes());
        RawRecord {
            form_id: id,
            record_type: *b"REFR",
            flags: 0,
            subrecords: vec![
                (
                    b"NAME".to_vec(),
                    OCCLUSION_MARKER_BASE.to_le_bytes().to_vec(),
                ),
                (b"XPRM".to_vec(), primitive),
                (
                    b"DATA".to_vec(),
                    [100.0f32, 200.0, 300.0, 0.1, 0.2, 0.3]
                        .into_iter()
                        .flat_map(f32::to_le_bytes)
                        .collect(),
                ),
            ],
            cell_form_id: Some(7),
            worldspace_form_id: None,
            load_order: 0,
        }
    }

    fn exported(records: Vec<RawRecord>) -> (Connection, super::super::RoomGraphReport) {
        let mut conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let master = records.into_iter().map(|r| (r.form_id, r)).collect();
        let tx = conn.transaction().unwrap();
        let report = export_room_graph(&tx, &master, &HashSet::from([7])).unwrap();
        tx.commit().unwrap();
        (conn, report)
    }

    #[test]
    fn plane_and_box_geometry_uses_data_and_xprm_in_roomless_interiors() {
        let mut plane = marker(100, 3);
        plane.subrecords.push((
            b"XOCP".to_vec(),
            [999.0f32; 9]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ));
        plane
            .subrecords
            .push((b"XSCL".to_vec(), 2.0f32.to_le_bytes().to_vec()));
        let (conn, report) = exported(vec![plane, marker(101, 1)]);
        assert_eq!(report.occluders, 2);
        let geometry: (u32, f32, f32, f32, f32, f32) = conn
            .query_row(
                "SELECT shape,pos_x,pos_z,half_x,half_y,scale FROM room_occluders WHERE ref_id=100",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(geometry, (3, 100.0, 300.0, 12.0, 0.0, 2.0));
        assert!(
            conn.query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=7",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );
        assert!(
            !conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='occlusion_version')",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
        );
        assert_eq!(super::super::RoomGraphReport::read(&conn).unwrap(), report);
    }

    #[test]
    fn retains_state_and_link_facts_without_activating_unsupported_markers() {
        let mut plane = marker(100, 3);
        plane.flags = 0x8000_0800;
        plane.subrecords.extend([
            (b"XESP".to_vec(), Vec::new()),
            (
                b"XORD".to_vec(),
                [101u32, 0, 102, 0]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect(),
            ),
            (b"XMBR".to_vec(), 500u32.to_le_bytes().to_vec()),
        ]);
        let mut malformed_link = marker(101, 3);
        malformed_link
            .subrecords
            .push((b"XORD".to_vec(), vec![1; 12]));
        malformed_link
            .subrecords
            .push((b"XMBR".to_vec(), vec![0; 3]));
        let (conn, _) = exported(vec![plane, malformed_link]);
        let state: (u32, bool, bool, u32) = conn.query_row("SELECT header_flags,has_enable_parent,has_links,target_ref FROM room_occluders WHERE ref_id=100", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(state, (0x8000_0800, true, true, 500));
        let links = conn.prepare("SELECT link_index,linked_ref FROM room_occluder_links WHERE ref_id=100 ORDER BY link_index").unwrap().query_map([], |r| Ok((r.get::<_,u32>(0)?,r.get::<_,u32>(1)?))).unwrap().collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(links, vec![(0, 101), (2, 102)]);
        assert!(
            conn.query_row(
                "SELECT has_links FROM room_occluders WHERE ref_id=101",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );
        assert_eq!(
            conn.query_row(
                "SELECT target_ref FROM room_occluders WHERE ref_id=101",
                [],
                |r| r.get::<_, Option<u32>>(0)
            )
            .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn invalid_deleted_and_exterior_occluders_do_not_gain_geometry() {
        let mut bad_size = marker(100, 1);
        bad_size.subrecords[1].1[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        let mut bad_scale = marker(101, 3);
        bad_scale
            .subrecords
            .push((b"XSCL".to_vec(), (-1.0f32).to_le_bytes().to_vec()));
        let mut deleted = marker(102, 3);
        deleted.flags = 0x20;
        let mut exterior = marker(103, 3);
        exterior.cell_form_id = Some(8);
        let (conn, report) = exported(vec![bad_size, bad_scale, deleted, exterior, marker(104, 2)]);
        assert_eq!((report.occluders, report.invalid_occluders_skipped), (0, 3));
        assert_eq!(
            conn.query_row("SELECT count(*) FROM room_occluders", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn skipped_geometry_keeps_incoming_links_and_malformed_links_make_cell_incomplete() {
        let mut invalid = marker(100, 2);
        invalid.subrecords.push((
            b"XORD".to_vec(),
            [101u32, 0, 0, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ));
        let mut malformed = marker(102, 2);
        malformed.subrecords.push((b"XORD".to_vec(), vec![0; 12]));
        let (conn, report) = exported(vec![invalid, marker(101, 3), malformed]);
        assert_eq!(report.occluders, 1);
        assert_eq!(
            conn.query_row(
                "SELECT linked_ref FROM room_occluder_links WHERE ref_id=100",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            101
        );
        assert!(
            !conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=7",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
        );
    }

    #[test]
    fn a_full_reexport_removes_old_occluders_and_links() {
        let mut plane = marker(100, 3);
        plane.subrecords.push((
            b"XORD".to_vec(),
            [101u32, 0, 0, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ));
        let (mut conn, _) = exported(vec![plane]);
        let tx = conn.transaction().unwrap();
        export_room_graph(&tx, &HashMap::new(), &HashSet::from([7])).unwrap();
        tx.commit().unwrap();
        for table in ["room_occluders", "room_occluder_links"] {
            assert_eq!(
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, u32>(0))
                    .unwrap(),
                0
            );
        }
    }
}
