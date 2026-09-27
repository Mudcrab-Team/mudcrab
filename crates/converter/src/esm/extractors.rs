use crate::esm::{
    records::record_type::{
        AlternateTexture, ModelData, SubrecordMODT, SubrecordOBND,
        vmad::{VmadSubrecord, parse_vmad},
    },
    types::{ArchivedRecordData, ArchivedSubRecord},
};
use rkyv::{rancor::Panic, to_bytes};
use std::str::from_utf8;

pub fn extract_subrecords(data: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut subs = Vec::new();
    let mut curr = data;

    let mut extended_length = None;
    while curr.len() >= 6 {
        let tag = &curr[..4];
        let len = u16::from_le_bytes([curr[4], curr[5]]) as usize;
        curr = &curr[6..];

        if tag == b"XXXX" {
            if len == 4 && curr.len() >= 4 {
                extended_length = Some(u32::from_le_bytes(curr[..4].try_into().unwrap()) as usize);
                curr = &curr[4..];
            } else {
                break;
            }
            continue;
        }

        let payload_len = extended_length.take().unwrap_or(len);
        if payload_len > curr.len() {
            break;
        }
        let payload = curr[..payload_len].to_vec();

        subs.push((tag.to_vec(), payload));

        curr = &curr[payload_len..];
    }

    subs
}

pub fn extract_land_data(subs: &[(Vec<u8>, Vec<u8>)]) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut vhgt = Vec::new();
    let mut vtex = Vec::new();
    let mut vclr = Vec::new();
    let mut vnml = Vec::new();

    for (tag, data) in subs {
        match from_utf8(tag).unwrap_or("") {
            "VHGT" => vhgt = data.clone(),
            "VTEX" => vtex = data.clone(),
            "VCLR" => vclr = data.clone(),
            "VNML" => vnml = data.clone(),
            _ => {}
        }
    }

    (vhgt, vtex, vclr, vnml)
}

pub fn extract_cell_info(
    subs: &[(Vec<u8>, Vec<u8>)],
) -> (Option<i32>, Option<i32>, Option<String>) {
    let mut grid_x: Option<i32> = None;
    let mut grid_y: Option<i32> = None;
    let mut interior_name: Option<String> = None;

    for (tag, data) in subs {
        match from_utf8(tag).unwrap_or("") {
            "XCLC" if data.len() >= 8 => {
                grid_x = Some(i32::from_le_bytes([data[0], data[1], data[2], data[3]]));
                grid_y = Some(i32::from_le_bytes([data[4], data[5], data[6], data[7]]));
            }
            "EDID" => {
                // Editor IDs are zero-terminated strings; the terminator is not part of the name.
                interior_name = Some(
                    String::from_utf8_lossy(data)
                        .trim_end_matches('\0')
                        .to_owned(),
                );
            }
            _ => {}
        }
    }

    (grid_x, grid_y, interior_name)
}

pub fn serialize_subrecords(subs: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let record_data = ArchivedRecordData {
        subrecords: subs
            .iter()
            .map(|(tag, data)| {
                let mut tag_arr = [0u8; 4];
                tag_arr.copy_from_slice(&tag[..4.min(tag.len())]);
                ArchivedSubRecord {
                    tag: tag_arr,
                    data: data.clone(),
                }
            })
            .collect(),
    };

    to_bytes::<Panic>(&record_data).unwrap().to_vec()
}

pub struct SubrecordView<'a> {
    subs: &'a [(Vec<u8>, Vec<u8>)],
}

impl<'a> SubrecordView<'a> {
    pub fn new(subs: &'a [(Vec<u8>, Vec<u8>)]) -> Self {
        Self { subs }
    }

    /// Read a string (EDID, FULL, MODL)
    pub fn get_string(&self, target_tag: &[u8; 4]) -> Option<String> {
        self.find(target_tag)
            .map(|d| String::from_utf8_lossy(d).trim_matches('\0').to_string())
    }

    /// Read a FormID (4 bytes LE)
    pub fn get_form_id(&self, target_tag: &[u8; 4]) -> Option<u32> {
        self.find(target_tag)
            .filter(|d| d.len() >= 4)
            .map(|d| u32::from_le_bytes([d[0], d[1], d[2], d[3]]))
    }

    /// Read an i32 (4 bytes LE)
    pub fn get_i32(&self, target_tag: &[u8; 4]) -> Option<i32> {
        self.find(target_tag)
            .filter(|d| d.len() >= 4)
            .map(|d| i32::from_le_bytes([d[0], d[1], d[2], d[3]]))
    }

    /// Read a float vector / transform (e.g. DATA in REFR: 3 pos + 3 rot = 24 bytes)
    pub fn get_f32_slice(&self, target_tag: &[u8; 4]) -> Option<Vec<f32>> {
        self.find(target_tag).map(|d| {
            d.as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        })
    }

    /// Find raw data bytes for a 4-byte subrecord tag (e.g b"HERD")
    pub fn find(&self, target_tag: &[u8; 4]) -> Option<&'a [u8]> {
        for (tag, data) in self.subs {
            if tag.len() >= 4 && &tag[..4] == target_tag {
                return Some(data);
            }
        }
        None
    }

    pub fn get_obnd(&self) -> Option<SubrecordOBND> {
        self.find(b"OBND")
            .filter(|d| d.len() >= 12)
            .map(|d| SubrecordOBND {
                x1: i16::from_le_bytes([d[0], d[1]]),
                y1: i16::from_le_bytes([d[2], d[3]]),
                z1: i16::from_le_bytes([d[4], d[5]]),
                x2: i16::from_le_bytes([d[6], d[7]]),
                y2: i16::from_le_bytes([d[8], d[9]]),
                z2: i16::from_le_bytes([d[10], d[11]]),
            })
    }

    pub fn get_vmad(&self, record_type: &[u8; 4]) -> Option<VmadSubrecord> {
        self.find(b"VMAD")
            .and_then(|bytes| parse_vmad(bytes, record_type).ok())
            .map(|(_, data)| data)
    }

    /// Parse Alternate Textures subrecord (e.g. MODS, DMDS, MO2S)
    pub fn get_alternate_textures(&self, mods_tags: &[u8; 4]) -> Vec<AlternateTexture> {
        let mut textures = Vec::new();
        let Some(data) = self.find(mods_tags) else {
            return textures;
        };

        if data.len() < 4 {
            return textures;
        }

        let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let mut offset = 4;

        for _ in 0..count {
            if offset + 4 > data.len() {
                break;
            }

            let str_len = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;

            offset += 4;

            if offset + str_len + 8 > data.len() {
                break;
            }

            let name_bytes = &data[offset..offset + str_len];
            let name_3d = String::from_utf8_lossy(name_bytes)
                .trim_matches('\0')
                .to_string();

            offset += str_len;

            let texture_form_id = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);

            offset += 4;

            let index_3d = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);

            offset += 4;

            textures.push(AlternateTexture {
                name_3d,
                texture_form_id,
                index_3d,
            });
        }

        textures
    }

    /// Helper to parse a model triplet given custom subrecord tags (e.g. MODL, MODT, MODS)
    pub fn get_model_with_tags(
        &self,
        modl_tag: &[u8; 4],
        modt_tag: &[u8; 4],
        mods_tag: &[u8; 4],
    ) -> Option<ModelData> {
        let model_path = self.get_string(modl_tag)?;
        let texture_data = self.find(modt_tag).map(|d| SubrecordMODT {
            raw_data: d.to_vec(),
        });

        let alternate_textures = self.get_alternate_textures(mods_tag);

        Some(ModelData {
            model_path,
            texture_data,
            alternate_textures,
        })
    }

    /// Helper to parse standard primary model (MODL / MODT / MODS)
    pub fn get_model(&self) -> Option<ModelData> {
        self.get_model_with_tags(b"MODL", b"MODT", b"MODS")
    }
}
