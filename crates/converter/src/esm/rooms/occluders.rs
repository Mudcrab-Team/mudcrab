//! Authored occlusion primitives. Runtime ownership and activation remain per-view policy.

use super::{OCCLUSION_MARKER_BASE, RawRecord, SubrecordView, u32_at};
use crate::esm::{
    load_order::LoadOrder,
    projection_omissions::{OmissionCategory, ProjectionOmissions},
};
use crate::records::{DecodedRecord, ReadResult, source_identity};
use rusqlite::{Result, Transaction, params};
use std::collections::{HashMap, HashSet};

mod subset;

pub use subset::{NativeOccluderChange, NativeOccluderShape, apply_native_occluders};
pub(in crate::esm) use subset::{update_authored_subset, update_subset};

#[derive(Default)]
pub(super) struct Exported {
    pub count: u64,
    pub invalid: u64,
    pub cells: HashSet<u32>,
    pub incomplete_cells: HashSet<u32>,
    pub unverified_link_targets: bool,
}

fn primitive(data: &[u8]) -> Option<(u32, [f32; 3])> {
    let ([x, y, z, _, _, _, _, shape], []) = data.as_chunks::<4>() else {
        return None;
    };
    let half = [*x, *y, *z].map(f32::from_le_bytes);
    let shape = u32::from_le_bytes(*shape);
    let valid = half.iter().all(|value| value.is_finite())
        && match shape {
            1 => half.iter().all(|&value| value > 0.0),
            3 => half[0] > 0.0 && half[1] >= 0.0 && half[2] > 0.0,
            _ => false,
        };
    valid.then_some((shape, half))
}

fn placement(data: &[u8]) -> Option<([f32; 3], [f32; 3])> {
    let (words, []) = data.as_chunks::<4>() else {
        return None;
    };
    // silent-ok: geometry reports invalid DATA unless there are six transform words.
    let words: &[[u8; 4]; 6] = words.try_into().ok()?;
    let values = words.map(f32::from_le_bytes);
    let [x, y, z, rx, ry, rz] = values;
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(([x, y, z], [rx, ry, rz]))
}

fn link_targets(data: &[u8]) -> Option<[u32; 4]> {
    let (words, []) = data.as_chunks::<4>() else {
        return None;
    };
    // silent-ok: A malformed XORD leaves link targets unverified.
    let words: &[[u8; 4]; 4] = words.try_into().ok()?;
    Some(words.map(u32::from_le_bytes))
}

fn is_marker(record: &RawRecord, view: &SubrecordView<'_>) -> bool {
    record.record_type == *b"REFR"
        && !record.is_deleted()
        && view
            .find_all(b"NAME")
            .any(|name| name == OCCLUSION_MARKER_BASE.to_le_bytes())
}

struct Geometry {
    shape: u32,
    half: [f32; 3],
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
}

fn geometry<'a>(
    view: &SubrecordView<'_>,
    complete: bool,
    has_scale: bool,
    field: impl Fn(&[u8; 4]) -> Option<&'a [u8]>,
) -> std::result::Result<Geometry, &'static str> {
    if !complete {
        return Err("incomplete PlaneMarker payload");
    }
    let mut names = view.find_all(b"NAME");
    if names.next() != Some(OCCLUSION_MARKER_BASE.to_le_bytes().as_slice())
        || names.next().is_some()
        || !field(b"NAME").is_some_and(|data| data.len() == 4)
    {
        return Err("ambiguous or malformed PlaneMarker NAME");
    }
    let (shape, half) = field(b"XPRM")
        .and_then(primitive)
        .ok_or("missing, malformed, or unsupported PlaneMarker XPRM")?;
    let (position, rotation) = field(b"DATA")
        .and_then(placement)
        .ok_or("missing or invalid PlaneMarker DATA placement")?;
    let scale = if has_scale {
        field(b"XSCL")
            // silent-ok: A width mismatch returns the XSCL error below.
            .and_then(|data| <[u8; 4]>::try_from(data).ok())
            .map(f32::from_le_bytes)
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or("PlaneMarker XSCL must be one finite positive float")?
    } else {
        1.0
    };
    Ok(Geometry {
        shape,
        half,
        position,
        rotation,
        scale,
    })
}

pub(super) fn export(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    interior_cells: &HashSet<u32>,
    sources: Option<(&ReadResult, &LoadOrder)>,
    omissions: &mut ProjectionOmissions,
) -> Result<Exported> {
    let mut result = Exported {
        unverified_link_targets: sources
            .is_some_and(|(decoded, _)| decoded.unverified_occluder_links),
        ..Exported::default()
    };
    let in_interior = |record: &RawRecord| {
        record
            .cell_form_id
            .is_some_and(|cell| interior_cells.contains(&cell))
    };
    // A typed export retains incoming links from markers outside the interior snapshot.
    let mut markers: Vec<_> = master
        .values()
        .filter(|record| {
            (sources.is_some() || in_interior(record))
                && is_marker(record, &SubrecordView::new(&record.subrecords))
        })
        .collect();
    markers.sort_unstable_by_key(|record| record.form_id);
    for record in markers {
        let source = sources.and_then(|(decoded, order)| {
            decoded
                .records
                .get(&record.form_id)
                .map(|record| (record, order))
        });
        project(
            tx,
            record,
            in_interior(record),
            MarkerSource::from_typed(source),
            |id| {
                Ok(master.get(&id).map_or(id < 0x800, |record| {
                    record.record_type == *b"REFR" && !record.is_deleted()
                }))
            },
            &mut result,
            omissions,
        )?;
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum MarkerSource<'a> {
    Canonical,
    Authored,
    DamagedPlugin,
    Typed(&'a DecodedRecord, &'a LoadOrder),
}

impl<'a> MarkerSource<'a> {
    fn from_typed(source: Option<(&'a DecodedRecord, &'a LoadOrder)>) -> Self {
        source.map_or(Self::Canonical, |(record, order)| {
            Self::Typed(record, order)
        })
    }

    fn typed(self) -> Option<(&'a DecodedRecord, &'a LoadOrder)> {
        match self {
            Self::Typed(record, order) => Some((record, order)),
            _ => None,
        }
    }
}

fn project(
    tx: &Transaction<'_>,
    record: &RawRecord,
    interior: bool,
    provenance: MarkerSource<'_>,
    target_exists: impl Fn(u32) -> Result<bool>,
    result: &mut Exported,
    omissions: &mut ProjectionOmissions,
) -> Result<()> {
    let id = record.form_id;
    let view = SubrecordView::new(&record.subrecords);
    let cell = record.cell_form_id.unwrap_or(0);
    let source = provenance.typed();
    let authored_ids = !matches!(provenance, MarkerSource::Canonical);
    let source_fields = source.map(|(record, _)| record.framed_source_prefix());
    let authored = |tag: [u8; 4]| {
        source_fields
            .iter()
            .flatten()
            .filter_map(move |(signature, data)| (*signature == tag).then_some(*data))
    };
    let field = |tag: &[u8; 4]| match source {
        Some((record, _)) => {
            let mut occurrences = authored(*tag);
            let value = occurrences.next()?;
            (occurrences.next().is_none() && !record.rejected_fields.contains(tag)).then_some(value)
        }
        None => {
            let mut fields = view.find_all(tag);
            let first = fields.next()?;
            fields.next().is_none().then_some(first)
        }
    };
    let present = |tag: &[u8; 4]| {
        view.find(tag).is_some()
            || authored(*tag).next().is_some()
            || source.is_some_and(|(record, _)| record.rejected_fields.contains(tag))
    };
    let complete = match provenance {
        MarkerSource::Canonical | MarkerSource::Authored => true,
        MarkerSource::DamagedPlugin => false,
        MarkerSource::Typed(record, _) => record.payload_complete,
    };
    if !complete {
        // Truncation or plugin remapping can hide an outgoing link to another cell.
        result.unverified_link_targets = true;
        result.incomplete_cells.insert(cell);
    }

    // Walk physical occurrences, including rejected ones. Outgoing links from an
    // unusable marker still suppress independently valid targets in any cell.
    let mut link_omission = None;
    let single_target = authored_ids
        && complete
        && field(b"XORD")
            .and_then(link_targets)
            .is_some_and(|slots| slots.iter().filter(|&&target| target != 0).count() == 1);
    let xords = view
        .find_all(b"XORD")
        .filter(|_| source.is_none())
        .chain(authored(*b"XORD"));
    for (occurrence, bytes) in xords.enumerate() {
        let Some(targets) = link_targets(bytes) else {
            result.unverified_link_targets = true;
            link_omission.get_or_insert("PlaneMarker XORD must contain four FormIDs");
            continue;
        };
        for (slot, target) in targets.into_iter().enumerate() {
            if target == 0 {
                continue;
            }
            let linked = match source {
                Some((record, order)) => source_identity::header_runtime_id(
                    b"REFR",
                    target,
                    record.load_order as usize,
                    order,
                ),
                None => Some(target),
            };
            let Some(linked) = linked.filter(|&id| id != 0) else {
                link_omission.get_or_insert("PlaneMarker XORD contains an unresolved FormID");
                continue;
            };
            let live = !authored_ids || target_exists(linked)?;
            if !live {
                link_omission.get_or_insert("PlaneMarker XORD target is absent or not a live REFR");
            }
            tx.prepare_cached(
                "INSERT INTO room_occluder_links(ref_id,link_index,linked_ref,single_target_verified) VALUES(?1,?2,?3,?4)",
            )?
            .execute(params![id, occurrence * 4 + slot, linked, single_target && live])?;
        }
    }
    if let Some(reason) = link_omission {
        result.incomplete_cells.insert(cell);
        omissions.record(OmissionCategory::OccluderUnusableLinks, id, reason);
    }
    if !interior {
        return Ok(());
    }
    result.cells.insert(cell);

    let Geometry {
        shape,
        half,
        position,
        rotation,
        scale,
    } = match geometry(&view, complete, present(b"XSCL"), field) {
        Ok(geometry) => geometry,
        Err(reason) => {
            result.invalid += 1;
            result.incomplete_cells.insert(cell);
            omissions.record(OmissionCategory::OccluderUnusableGeometry, id, reason);
            return Ok(());
        }
    };
    let target = present(b"XMBR").then(|| {
        view.find(b"XMBR")
            .filter(|data| data.len() == 4)
            .and_then(|data| u32_at(data, 0))
            .unwrap_or(0)
    });
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
            present(b"XESP"),
            present(b"XORD"),
            target
        ],
    )?;
    if authored_ids
        && shape == 3
        && let Some(target) = target.filter(|&id| id != 0)
        && field(b"XMBR").is_some_and(|data| data.len() == 4 && data != [0; 4])
    {
        tx.prepare_cached("INSERT INTO room_reference_rules(reference_id,cell_id,assignment,target_ref) VALUES(?1,?2,'target',?3)")?
            .execute(params![id, cell, target])?;
    }
    result.count += 1;
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
    fn duplicate_geometry_keeps_outgoing_links_and_valid_siblings() {
        for index in [1, 2] {
            let mut bad = marker(100, 3);
            bad.subrecords.push(bad.subrecords[index].clone());
            bad.subrecords.push((
                b"XORD".to_vec(),
                [0u32, 101, 0, 0]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect(),
            ));
            let (db, report) = exported(vec![bad, marker(101, 3)]);
            assert_eq!(report.occluders, 1);
            assert_eq!(
                db.query_row("SELECT ref_id FROM room_occluders", [], |r| r
                    .get::<_, u32>(0))
                    .unwrap(),
                101
            );
            assert_eq!(
                db.query_row(
                    "SELECT ref_id, link_index, linked_ref FROM room_occluder_links",
                    [],
                    |r| Ok((
                        r.get::<_, u32>(0)?,
                        r.get::<_, u32>(1)?,
                        r.get::<_, u32>(2)?
                    ))
                )
                .unwrap(),
                (100, 1, 101)
            );
            assert!(
                !db.query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=7",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
            );
        }
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
        let mut deleted = marker(102, 3);
        deleted.flags = 0x20;
        let mut exterior = marker(103, 3);
        exterior.cell_form_id = Some(8);
        let (conn, report) = exported(vec![bad_size, deleted, exterior, marker(104, 2)]);
        assert_eq!((report.occluders, report.invalid_occluders_skipped), (0, 2));
        assert_eq!(
            conn.query_row("SELECT count(*) FROM room_occluders", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn invalid_scales_report_omissions_and_preserve_cross_cell_links() {
        let cases = [
            vec![],
            vec![0.5f32.to_le_bytes().to_vec()],
            vec![(-1.0f32).to_le_bytes().to_vec()],
            vec![0.0f32.to_le_bytes().to_vec()],
            vec![f32::NAN.to_le_bytes().to_vec()],
            vec![vec![0; 3]],
            vec![0.5f32.to_le_bytes().to_vec(); 2],
        ];
        let mut master: HashMap<_, _> = cases
            .into_iter()
            .enumerate()
            .map(|(index, scales)| {
                let id = 100 + u32::try_from(index).unwrap();
                let mut plane = marker(id, 3);
                plane
                    .subrecords
                    .extend(scales.into_iter().map(|bytes| (b"XSCL".to_vec(), bytes)));
                (id, plane)
            })
            .collect();
        let mut target = marker(200, 3);
        target.cell_form_id = Some(8);
        master.insert(200, target);
        master.get_mut(&106).unwrap().subrecords.push((
            b"XORD".to_vec(),
            [200u32, 0, 0, 0].map(u32::to_le_bytes).concat(),
        ));
        let mut conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        let mut omissions = ProjectionOmissions::default();
        let report = super::super::export_room_graph_with_sources(
            &tx,
            &master,
            &HashSet::from([7, 8]),
            None,
            None,
            None,
            &mut omissions,
        )
        .unwrap();
        assert_eq!((report.occluders, report.invalid_occluders_skipped), (3, 5));
        let omitted = &omissions.categories["occluder_unusable_geometry"];
        assert_eq!(omitted.count, 5);
        assert_eq!(
            omitted
                .samples
                .iter()
                .map(|sample| sample.form_id)
                .collect::<Vec<_>>(),
            [102, 103, 104, 105, 106]
        );
        assert!(
            omitted
                .samples
                .iter()
                .all(|sample| sample.reason.contains("XSCL"))
        );
        let scales = tx
            .prepare("SELECT ref_id,scale FROM room_occluders ORDER BY ref_id")
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, f32>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(scales, [(100, 1.0), (101, 0.5), (200, 1.0)]);
        assert_eq!(
            tx.query_row(
                "SELECT linked_ref FROM room_occluder_links WHERE ref_id=106 AND link_index=0",
                [],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            200
        );
        let complete = |cell: u32| {
            tx.query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                [cell],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        };
        assert!(!complete(7));
        assert!(complete(8));
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
