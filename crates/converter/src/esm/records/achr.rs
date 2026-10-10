//! ACHR records hold information about 'Actors'. This is a specific NPC at a specific location potentially at a specific time (triggered by scripts or otherwise) doing a specific thing at a specified static marker, additionally with optional data like reference types.
//!
//! For all NPCs, it is the ACHR formID that you are using when you `moveto player` etc.
//!
//! Record header flags:
//!
//! - 0x200 - Starts Dead
//! - 0x400 - Persistent Reference (? not shown in the CK)
//! - 0x800 - Initially Disabled
//! - 0x2000000 - No AI Acquire
//! - 0x10000000 - Reflected By Auto Water
//! - 0x20000000 - Don't Havok Settle

use crate::esm::{
    extractors::SubrecordView,
    records::{EsmRecord, RawRecord, record_type::vmad::VmadSubrecord},
};

/// ACHR Header Flags
pub mod flags {
    pub const STARTS_DEAD: u32 = 0x0000_0200;
    pub const PERSISTENT: u32 = 0x0000_0400;
    pub const INITIALLY_DISABLED: u32 = 0x0000_0800;
    pub const NO_AI_ACQUIRE: u32 = 0x0200_0000;
    pub const REFLECTED_BY_AUTO_WATER: u32 = 0x1000_0000;
    pub const DONT_HAVOK_SETTLE: u32 = 0x2000_0000;
}

/// Activate Parent Subrecord (XAPR, 8 bytes)
#[derive(Debug)]
pub struct SubrecordXAPR {
    pub parent_ref: u32,
    pub delay: f32,
}

/// Enable Parent Subrecord: four-byte parent, one flags byte, three unused bytes.
#[derive(Debug)]
pub struct SubrecordXESP {
    pub parent_ref: u32,
    pub flags: u32,
}

impl SubrecordXESP {
    pub(crate) fn parse(data: &[u8]) -> Option<Self> {
        let data = data.get(..8)?;
        Some(Self {
            parent_ref: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            // Retail XESP padding can be non-zero; retain only the flags byte.
            flags: u32::from(data[4]),
        })
    }

    pub fn set_enable_opposite(&self) -> bool {
        (self.flags & 0x0001) != 0
    }

    pub fn pop_in(&self) -> bool {
        (self.flags & 0x0002) != 0
    }
}

/// Linked Route / Ref Subrecord (XLKR, 8 bytes)
#[derive(Debug)]
pub struct SubrecordXLKR {
    pub keyword: u32,
    pub target_ref: u32,
}

/// Patrol Topic Data Subrecord (PDTO)
#[derive(Debug)]
pub struct SubrecordPDTO {
    pub topic_type: u32,
    pub data: [u8; 4], // FormID (if topic_type == 0) or 4-char ASCII strin tag
}

/// Transform / Placement Data Subrecord (DATA, 24 bytes: 3 pos + 3 rot in radians)
#[derive(Debug)]
pub struct SubrecordDATA {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
}

/// Actor Reference Record (ACHR)
#[derive(Debug)]
pub struct AchrRecord {
    pub form_id: u32,
    pub flags: u32,
    pub editor_id: Option<String>,
    pub base_npc: u32,
    pub encounter_zone: Option<u32>,
    pub patrol_idle: Option<f32>,
    pub topic_data: Option<SubrecordPDTO>,
    pub ragdoll_data: Option<Vec<u8>>,
    pub level_modifier: Option<u32>,
    pub activate_parent_flags: Option<u8>,
    pub activate_parent: Option<SubrecordXAPR>,
    pub location_ref_types: Vec<u32>,
    pub horse_id: Option<u32>,
    pub enable_parent: Option<SubrecordXESP>,
    pub owner: Option<u32>,
    pub location: Option<u32>,
    pub location_route: Option<SubrecordXLKR>,
    pub ignored_by_sandbox: bool,
    pub scale: Option<f32>,
    pub transform: Option<SubrecordDATA>,
    pub vmad: Option<VmadSubrecord>,
}

impl AchrRecord {
    pub fn starts_dead(&self) -> bool {
        (self.flags & flags::STARTS_DEAD) != 0
    }

    pub fn is_persistent(&self) -> bool {
        (self.flags & flags::PERSISTENT) != 0
    }

    pub fn is_initially_disabled(&self) -> bool {
        (self.flags & flags::INITIALLY_DISABLED) != 0
    }
}

impl EsmRecord for AchrRecord {
    const RECORD_TYPE: &'static [u8; 4] = b"ACHR";

    fn parse(raw: &RawRecord) -> Option<Self> {
        if &raw.record_type != Self::RECORD_TYPE {
            return None;
        }

        let view = SubrecordView::new(&raw.subrecords);

        let base_npc = view.get_form_id(b"NAME")?;
        let editor_id = view.get_string(b"EDID");
        let encounter_zone = view.get_form_id(b"XEZN");
        let patrol_idle = view.get_f32_slice(b"XPRD").and_then(|v| v.first().copied());

        let topic_data = view.find(b"PDTO").filter(|d| d.len() >= 8).map(|d| {
            let topic_type = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
            let data = [d[4], d[5], d[6], d[7]];
            SubrecordPDTO { topic_type, data }
        });

        let ragdoll_data = view.find(b"XRGD").map(|d| d.to_vec());
        let level_modifier = view.get_i32(b"XLCM").map(|v| v as u32);
        let activate_parent_flags = view.find(b"XAPD").and_then(|d| d.first().copied());
        let activate_parent = view
            .find(b"XAPR")
            .filter(|d| d.len() >= 8)
            .map(|d| SubrecordXAPR {
                parent_ref: u32::from_le_bytes([d[0], d[1], d[2], d[3]]),
                delay: f32::from_le_bytes([d[4], d[5], d[6], d[7]]),
            });

        let mut location_ref_types = Vec::new();
        for (tag, data) in &raw.subrecords {
            if tag.len() >= 4 && &tag[..4] == b"XLRT" && data.len() >= 4 {
                location_ref_types.push(u32::from_le_bytes([data[0], data[1], data[2], data[3]]));
            }
        }

        let horse_id = view.get_form_id(b"XHOR");
        let enable_parent = view.find(b"XESP").and_then(SubrecordXESP::parse);

        let owner = view.get_form_id(b"XOWN");
        let location = view
            .get_form_id(b"XLCN")
            .or_else(|| view.get_form_id(b"XLRL"));
        let location_route = view
            .find(b"XLKR")
            .filter(|d| d.len() >= 8)
            .map(|d| SubrecordXLKR {
                keyword: u32::from_le_bytes([d[0], d[1], d[2], d[3]]),
                target_ref: u32::from_le_bytes([d[4], d[5], d[6], d[7]]),
            });

        let ignored_by_sandbox = view.find(b"XIS2").is_some();
        let scale = view.get_f32_slice(b"XSCL").and_then(|v| v.first().copied());
        let transform = view
            .get_f32_slice(b"DATA")
            .filter(|v| v.len() >= 6)
            .map(|v| SubrecordDATA {
                position: [v[0], v[1], v[2]],
                rotation: [v[3], v[4], v[5]],
            });
        let vmad = view.get_vmad(Self::RECORD_TYPE);

        Some(Self {
            form_id: raw.form_id,
            flags: raw.flags,
            editor_id,
            base_npc,
            encounter_zone,
            patrol_idle,
            topic_data,
            ragdoll_data,
            level_modifier,
            activate_parent_flags,
            activate_parent,
            location_ref_types,
            horse_id,
            enable_parent,
            owner,
            location,
            location_route,
            ignored_by_sandbox,
            scale,
            transform,
            vmad,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v174_achr_enable_parent_ignores_xesp_padding() {
        for flags in 0..=u8::MAX {
            let raw = RawRecord {
                form_id: 0x1000,
                record_type: *b"ACHR",
                flags: 0,
                subrecords: vec![
                    (b"NAME".to_vec(), 7u32.to_le_bytes().to_vec()),
                    (
                        b"XESP".to_vec(),
                        [0x78, 0x56, 0x34, 0x12, flags, 0x0D, 0xBF, 0x38].to_vec(),
                    ),
                ],
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 0,
            };
            let parent = AchrRecord::parse(&raw).unwrap().enable_parent.unwrap();
            assert_eq!(parent.parent_ref, 0x1234_5678);
            assert_eq!(parent.flags, u32::from(flags));
            assert_eq!(parent.set_enable_opposite(), flags & 1 != 0);
            assert_eq!(parent.pop_in(), flags & 2 != 0);
        }
    }

    #[test]
    fn v174_achr_enable_parent_requires_complete_xesp() {
        for length in 0..8 {
            let raw = RawRecord {
                form_id: 0x1000,
                record_type: *b"ACHR",
                flags: 0,
                subrecords: vec![
                    (b"NAME".to_vec(), 7u32.to_le_bytes().to_vec()),
                    (b"XESP".to_vec(), vec![0xFF; length]),
                ],
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 0,
            };
            assert!(AchrRecord::parse(&raw).unwrap().enable_parent.is_none());
        }
    }

    #[test]
    fn parses_achr_pdto_with_little_endian() {
        let raw = RawRecord {
            form_id: 0x00010000,
            record_type: *b"ACHR",
            flags: 0,
            subrecords: vec![
                (b"NAME".to_vec(), 0x00000007u32.to_le_bytes().to_vec()),
                (b"PDTO".to_vec(), vec![1, 0, 0, 0, b'T', b'O', b'P', b'I']),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        let achr = AchrRecord::parse(&raw).unwrap();
        assert_eq!(achr.base_npc, 7);
        let pdto = achr.topic_data.unwrap();
        assert_eq!(pdto.topic_type, 1);
        assert_eq!(&pdto.data, b"TOPI");
    }
}
