pub mod aact;
pub mod achr;
pub mod clfm;
pub mod record_type;
pub mod tes4;
pub mod tree;

use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone)]
pub struct RawRecord {
    pub form_id: u32,
    pub record_type: [u8; 4],
    pub flags: u32,
    pub subrecords: Vec<(Vec<u8>, Vec<u8>)>,
    pub cell_form_id: Option<u32>,
    pub worldspace_form_id: Option<u32>,
    pub load_order: u32,
}

impl RawRecord {
    pub fn is_deleted(&self) -> bool {
        self.flags & 0x00000020 != 0
    }
}

pub(crate) struct LandTieSummary {
    pub cells: usize,
    pub extra_forms: usize,
    pub first_cell: u32,
    pub retained_form: u32,
    pub ignored_form: u32,
}

pub(crate) struct LandSelection<'a> {
    pub cells: BTreeMap<u32, &'a RawRecord>,
    pub ties_by_plugin: BTreeMap<u32, LandTieSummary>,
}

/// Resolve LAND by highest plugin priority, then lowest FormID for a tie.
/// Merged RawRecord values do not retain physical record order, so the ID rule
/// is a deterministic recovery policy, not a claim about native record ordering.
/// Raw records remain available; both terrain projections use this winner.
/// Selection stays silent: the exporter reports each plugin's ties once, while
/// the later cache projection reuses the same policy without duplicate warnings.
pub(crate) fn land_by_cell(records: &HashMap<u32, RawRecord>) -> LandSelection<'_> {
    let mut lands: Vec<_> = records
        .values()
        .filter(|record| record.record_type == *b"LAND")
        .collect();
    lands.sort_unstable_by_key(|record| (std::cmp::Reverse(record.load_order), record.form_id));
    let mut winners = BTreeMap::<u32, &RawRecord>::new();
    let mut ties_by_plugin = BTreeMap::<u32, LandTieSummary>::new();
    let mut tied_cells = BTreeSet::new();
    for record in lands {
        let cell = record.cell_form_id.unwrap_or(record.form_id);
        if let Some(winner) = winners.get(&cell) {
            // Lower-priority alternatives are ordinary overrides, including
            // ties that no longer affect the winning plugin's projection.
            if winner.load_order == record.load_order && winner.form_id != record.form_id {
                let summary = ties_by_plugin
                    .entry(record.load_order)
                    .or_insert(LandTieSummary {
                        cells: 0,
                        extra_forms: 0,
                        first_cell: cell,
                        retained_form: winner.form_id,
                        ignored_form: record.form_id,
                    });
                summary.extra_forms += 1;
                if tied_cells.insert(cell) {
                    summary.cells += 1;
                    if cell < summary.first_cell {
                        summary.first_cell = cell;
                        summary.retained_form = winner.form_id;
                        summary.ignored_form = record.form_id;
                    }
                }
            }
        } else {
            winners.insert(cell, record);
        }
    }
    LandSelection {
        cells: winners,
        ties_by_plugin,
    }
}

pub trait EsmRecord: Sized {
    const RECORD_TYPE: &'static [u8; 4];
    fn parse(raw: &RawRecord) -> Option<Self>;
}
