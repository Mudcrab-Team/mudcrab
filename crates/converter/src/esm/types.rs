use std::collections::HashMap;

pub use shared::records::{
    ArchivedArchivedRecordData, ArchivedArchivedSubRecord, ArchivedRecordData, ArchivedSubRecord,
};

#[derive(Debug, Clone)]
pub struct GroupHeader {
    pub data_size: u32,
    pub label: u32,
    pub group_type: i32,
}

/// A cell-child GRUP identifies its cell once the label and group kind are readable.
pub(crate) fn group_child_cell(header: &[u8]) -> Option<u32> {
    let [tag, _, label, kind, ..] = header.as_chunks::<4>().0 else {
        return None;
    };
    (tag == b"GRUP" && matches!(i32::from_le_bytes(*kind), 6 | 8 | 9 | 10))
        .then(|| u32::from_le_bytes(*label))
}

/// Skyrim Record Header (24 Bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordHeader {
    pub type_tag: [u8; 4], // e.g. "TES4", "CELL", "REFR", "NPC_"
    pub data_size: u32,
    pub flags: u32,
    pub form_id: u32,
    pub version_control: u32,
    pub version: u16,
    pub unknown: u16,
}

/// Parsed Placement Reference (REFR)
#[derive(Debug, Clone, PartialEq)]
pub struct WorldReference {
    pub form_id: u32,
    pub base_form_id: u32, // Points to Mesh/NPC/Item template
    pub pos_x: f32,
    pub pos_y: f32,
    pub pos_z: f32,
    pub rot_x: f32,
    pub rot_y: f32,
    pub rot_z: f32,
    pub cell_form_id: u32,
}

pub type CellInfoMap = HashMap<u32, (Option<i32>, Option<i32>, Option<String>)>;
