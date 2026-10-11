pub mod aact;
pub mod achr;
pub mod clfm;
pub mod record_type;
pub mod tes4;
pub mod tree;

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

/// Resolve distinct LAND forms attached to the same cell using plugin priority.
/// Raw records remain available; both terrain projections use this winner.
pub(crate) fn land_by_cell(
    records: &std::collections::HashMap<u32, RawRecord>,
) -> color_eyre::Result<std::collections::BTreeMap<u32, &RawRecord>> {
    let mut lands: Vec<_> = records
        .values()
        .filter(|record| record.record_type == *b"LAND")
        .collect();
    lands.sort_unstable_by_key(|record| (std::cmp::Reverse(record.load_order), record.form_id));
    let mut winners = std::collections::BTreeMap::<u32, &RawRecord>::new();
    for record in lands {
        let cell = record.cell_form_id.unwrap_or(record.form_id);
        if let Some(winner) = winners.get(&cell) {
            color_eyre::eyre::ensure!(
                winner.load_order != record.load_order,
                "cell {cell:08X} has ambiguous LAND forms {:08X} and {:08X} at plugin priority {}",
                winner.form_id,
                record.form_id,
                record.load_order
            );
        } else {
            winners.insert(cell, record);
        }
    }
    Ok(winners)
}

pub trait EsmRecord: Sized {
    const RECORD_TYPE: &'static [u8; 4];
    fn parse(raw: &RawRecord) -> Option<Self>;
}
