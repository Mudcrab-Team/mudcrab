//! **VMAD** fields contain Papyrus script data, and are present in any record
//! that contains a script, including items, dialogues, packages, and quests.
//!
//! Information contained in the VMAD field includes:
//!
//! - The names of all scripts attached to the record, including scripts
//!   attached to individual components (e.g., quest aliases) of the record.
//! - The initial values of all properties in each of those scripts
//! - The names of all script fragments attached to the record. Script
//!   fragments are most commonly used in quests, where each stage of a
//!   quest can have an associated script fragment.
//!
//! The VMAD field contains several distinct sections. However, the entire
//! field must be processed sequentially in order to identify the sections;
//! the lengths and locations of the various sections are not provided, making
//! it impossible to skip through the field to a specific section.
//!
//! All VMAD fields contain a Primary Scripts Section and its format is the
//! same for all record types; in the majority of cases, that is the only
//! section present. However, some records also contain a Fragments Section,
//! with a format that is dependent upon the record type.
//!
//! All script names mentioned in the VMAD field are provided without an extension.
//! The game itself accesses compiled versions of each script, which are given
//! a .pex extension and are stored in one of the game's .bsa archive files.
//!
//! The source versions of the scripts, which use a .psc extension, are not
//! accessed by the game (and were not part of the original game distribution), but
//! are instead only used by the Creation Kit. The source scripts were made available
//! following the release of the Creation Kit, and all 10005 scripts (as of patch 1.5)
//! are available in the Data/Scripts/Source directory of your Skyrim installation.

use nom::{
    Err::Failure,
    IResult,
    bytes::complete::take,
    error::Error,
    number::complete::{le_f32, le_i8, le_i16, le_i32, le_u8, le_u16, le_u32},
};
use serde::Serialize;
/// VMAD Script Property Value Types
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ScriptPropertyValue {
    Object(ScriptObjectProperty),
    WString(String),
    Int(i32),
    Float(f32),
    Bool(bool),
    ArrayOfObject(Vec<ScriptObjectProperty>),
    ArrayOfWString(Vec<String>),
    ArrayOfInt(Vec<i32>),
    ArrayOfFloat(Vec<f32>),
    ArrayOfBool(Vec<bool>),
}

/// Object Property payload (Type 1 or Type 11 item)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScriptObjectProperty {
    pub form_id: u32,
    pub alias_id: u16,
}

/// A single property entry in a Papyrus script
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScriptProperty {
    pub name: String,
    pub property_type: u8,
    pub status: u8, // Present if VMAD version >= 4 (defaults to 1)
    pub value: ScriptPropertyValue,
}

/// A Papyrus script attached to a record or quest alias
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScriptEntry {
    pub name: String,
    pub status: u8, // Present if VMAD version >= 4 (defaults to 0)
    pub properties: Vec<ScriptProperty>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoFragment {
    pub unknown: i8,
    pub script_name: String,
    pub fragment_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFragment {
    pub unknown: i8,
    pub script_name: String,
    pub fragment_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerkFragment {
    pub index: u16,
    pub unknown_1: i16,
    pub unknown_2: i8,
    pub script_name: String,
    pub fragment_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QustFragment {
    pub index: u16,
    pub unknown_1: i16,
    pub log_entry: i32,
    pub unknown_2: i8,
    pub script_name: String,
    pub fragment_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QustAliasScript {
    pub object: ScriptObjectProperty,
    pub version: i16,
    pub obj_format: i16,
    pub scripts: Vec<ScriptEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenBeginEndFragment {
    pub unknown: i8,
    pub script_name: String,
    pub fragment_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenPhaseFragment {
    pub unknown_1: i8,
    pub phase: u32,
    pub unknown_2: i8,
    pub script_name: String,
    pub fragment_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VmadFragments {
    Info {
        unknown: i8,
        flags: u8,
        file_name: String,
        fragments: Vec<InfoFragment>,
    },
    Pack {
        unknown: i8,
        flags: u8,
        file_name: String,
        fragments: Vec<PackFragment>,
    },
    Perk {
        unknown: i8,
        file_name: String,
        fragments: Vec<PerkFragment>,
    },
    Qust {
        unknown: i8,
        file_name: String,
        fragments: Vec<QustFragment>,
        aliases: Vec<QustAliasScript>,
    },
    Scen {
        unknown: i8,
        flags: u8,
        file_name: String,
        begin_end_fragments: Vec<ScenBeginEndFragment>,
        phase_fragments: Vec<ScenPhaseFragment>,
    },
}

/// Parsed VMAD Subrecord Data
#[derive(Debug, Clone, PartialEq)]
pub struct VmadSubrecord {
    pub version: i16,
    pub obj_format: i16,
    pub scripts: Vec<ScriptEntry>,
    pub fragments: Option<VmadFragments>,
}

/// Parse length-prefixed WString (uint16 length + UTF-8 string)
pub fn parse_wstring(input: &[u8]) -> IResult<&[u8], String> {
    let (input, len) = le_u16(input)?;
    let (input, str_bytes) = take(len as usize)(input)?;
    let string = String::from_utf8_lossy(str_bytes).to_string();
    Ok((input, string))
}

/// Every array element takes at least one byte, so a count read from the
/// field never needs more capacity than the bytes that remain.
fn bounded_capacity(count: u32, remaining: &[u8]) -> usize {
    (count as usize).min(remaining.len())
}

/// Parse Object Property (8 bytes, format varies by obj_format)
pub fn parse_object_property(
    input: &[u8],
    obj_format: i16,
) -> IResult<&[u8], ScriptObjectProperty> {
    let (input, raw) = take(8usize)(input)?;
    let (form_id, alias_id) = if obj_format == 1 {
        let form_id = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let alias_id = u16::from_le_bytes([raw[4], raw[5]]);
        (form_id, alias_id)
    } else {
        let alias_id = u16::from_le_bytes([raw[2], raw[3]]);
        let form_id = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        (form_id, alias_id)
    };
    Ok((input, ScriptObjectProperty { form_id, alias_id }))
}

/// Parse a single Property Entry
pub fn parse_script_property(
    input: &[u8],
    version: i16,
    obj_format: i16,
) -> IResult<&[u8], ScriptProperty> {
    let (input, name) = parse_wstring(input)?;
    let (input, property_type) = le_u8(input)?;
    let (input, status) = if version >= 4 {
        le_u8(input)?
    } else {
        (input, 1u8)
    };

    let (input, value) = match property_type {
        1 => {
            let (i, obj) = parse_object_property(input, obj_format)?;
            (i, ScriptPropertyValue::Object(obj))
        }
        2 => {
            let (i, s) = parse_wstring(input)?;
            (i, ScriptPropertyValue::WString(s))
        }
        3 => {
            let (i, i32) = le_i32(input)?;
            (i, ScriptPropertyValue::Int(i32))
        }
        4 => {
            let (i, f32) = le_f32(input)?;
            (i, ScriptPropertyValue::Float(f32))
        }
        5 => {
            let (i, val) = le_i8(input)?;
            (i, ScriptPropertyValue::Bool(val != 0))
        }
        11 => {
            let (mut curr, count) = le_u32(input)?;
            let mut item = Vec::with_capacity(bounded_capacity(count, curr));
            for _ in 0..count {
                let (next, obj) = parse_object_property(curr, obj_format)?;
                item.push(obj);
                curr = next;
            }
            (curr, ScriptPropertyValue::ArrayOfObject(item))
        }
        12 => {
            let (mut curr, count) = le_u32(input)?;
            let mut item = Vec::with_capacity(bounded_capacity(count, curr));
            for _ in 0..count {
                let (next, s) = parse_wstring(curr)?;
                item.push(s);
                curr = next;
            }
            (curr, ScriptPropertyValue::ArrayOfWString(item))
        }
        13 => {
            let (mut curr, count) = le_u32(input)?;
            let mut item = Vec::with_capacity(bounded_capacity(count, curr));
            for _ in 0..count {
                let (next, i32) = le_i32(curr)?;
                item.push(i32);
                curr = next;
            }
            (curr, ScriptPropertyValue::ArrayOfInt(item))
        }
        14 => {
            let (mut curr, count) = le_u32(input)?;
            let mut item = Vec::with_capacity(bounded_capacity(count, curr));
            for _ in 0..count {
                let (next, f32) = le_f32(curr)?;
                item.push(f32);
                curr = next;
            }
            (curr, ScriptPropertyValue::ArrayOfFloat(item))
        }
        15 => {
            let (mut curr, count) = le_u32(input)?;
            let mut item = Vec::with_capacity(bounded_capacity(count, curr));
            for _ in 0..count {
                let (next, bool) = le_i8(curr)?;
                item.push(bool != 0);
                curr = next;
            }
            (curr, ScriptPropertyValue::ArrayOfBool(item))
        }
        _ => return Err(Failure(Error::new(input, nom::error::ErrorKind::Tag))),
    };

    Ok((
        input,
        ScriptProperty {
            name,
            property_type,
            status,
            value,
        },
    ))
}

/// Parse a single Script entry
pub fn parse_script_entry(
    input: &[u8],
    version: i16,
    obj_format: i16,
) -> IResult<&[u8], ScriptEntry> {
    let (input, name) = parse_wstring(input)?;
    let (input, status) = if version >= 4 {
        le_u8(input)?
    } else {
        (input, 0u8)
    };

    let (input, property_count) = le_u16(input)?;
    let mut properties = Vec::with_capacity(property_count as usize);
    let mut curr = input;

    for _ in 0..property_count {
        let (next, prop) = parse_script_property(curr, version, obj_format)?;
        properties.push(prop);
        curr = next;
    }

    Ok((
        curr,
        ScriptEntry {
            name,
            status,
            properties,
        },
    ))
}

/// Main entry point to parse a VMAD subrecord byte slice givend the record's
/// 4-byt type tag (e.g. b"QUST", b"INFO", etc.)
pub fn parse_vmad<'a>(input: &'a [u8], record_tag: &[u8; 4]) -> IResult<&'a [u8], VmadSubrecord> {
    let (input, version) = le_i16(input)?;
    let (input, obj_format) = le_i16(input)?;
    let (input, script_count) = le_u16(input)?;

    let mut curr = input;
    let mut scripts = Vec::with_capacity(script_count as usize);

    for _ in 0..script_count {
        let (next, script) = parse_script_entry(curr, version, obj_format)?;
        scripts.push(script);
        curr = next;
    }

    if curr.is_empty() {
        return Ok((
            curr,
            VmadSubrecord {
                version,
                obj_format,
                scripts,
                fragments: None,
            },
        ));
    }

    let (curr, fragments) = match record_tag {
        b"INFO" => {
            let (i, unknown) = le_i8(curr)?;
            let (i, flags) = le_u8(i)?;
            let (i, file_name) = parse_wstring(i)?;
            let flag_count = flags.count_ones() as usize;

            let mut frags = Vec::with_capacity(flag_count);
            let mut c = i;
            for _ in 0..flag_count {
                let (next, unk) = le_i8(c)?;
                let (next, s_name) = parse_wstring(next)?;
                let (next, f_name) = parse_wstring(next)?;
                frags.push(InfoFragment {
                    unknown: unk,
                    script_name: s_name,
                    fragment_name: f_name,
                });
                c = next;
            }
            (
                c,
                Some(VmadFragments::Info {
                    unknown,
                    flags,
                    file_name,
                    fragments: frags,
                }),
            )
        }
        b"PACK" => {
            let (i, unknown) = le_i8(curr)?;
            let (i, flags) = le_u8(i)?;
            let (i, file_name) = parse_wstring(i)?;
            let flag_count = flags.count_ones() as usize;

            let mut frags = Vec::with_capacity(flag_count);
            let mut c = i;
            for _ in 0..flag_count {
                let (next, unk) = le_i8(c)?;
                let (next, s_name) = parse_wstring(next)?;
                let (next, f_name) = parse_wstring(next)?;
                frags.push(PackFragment {
                    unknown: unk,
                    script_name: s_name,
                    fragment_name: f_name,
                });
                c = next;
            }
            (
                c,
                Some(VmadFragments::Pack {
                    unknown,
                    flags,
                    file_name,
                    fragments: frags,
                }),
            )
        }
        b"PERK" => {
            let (i, unknown) = le_i8(curr)?;
            let (i, file_name) = parse_wstring(i)?;
            let (i, frag_count) = le_u16(i)?;

            let mut frags = Vec::with_capacity(frag_count as usize);
            let mut c = i;
            for _ in 0..frag_count {
                let (next, index) = le_u16(c)?;
                let (next, unk1) = le_i16(next)?;
                let (next, unk2) = le_i8(next)?;
                let (next, s_name) = parse_wstring(next)?;
                let (next, f_name) = parse_wstring(next)?;
                frags.push(PerkFragment {
                    index,
                    unknown_1: unk1,
                    unknown_2: unk2,
                    script_name: s_name,
                    fragment_name: f_name,
                });
                c = next;
            }
            (
                c,
                Some(VmadFragments::Perk {
                    unknown,
                    file_name,
                    fragments: frags,
                }),
            )
        }
        b"QUST" => {
            let (i, unknown) = le_i8(curr)?;
            let (i, frag_count) = le_u16(i)?;
            let (i, file_name) = parse_wstring(i)?;

            let mut frags = Vec::with_capacity(frag_count as usize);
            let mut c = i;
            for _ in 0..frag_count {
                let (next, index) = le_u16(c)?;
                let (next, unk1) = le_i16(next)?;
                let (next, logentry) = le_i32(next)?;
                let (next, unk2) = le_i8(next)?;
                let (next, s_name) = parse_wstring(next)?;
                let (next, f_name) = parse_wstring(next)?;
                frags.push(QustFragment {
                    index,
                    unknown_1: unk1,
                    log_entry: logentry,
                    unknown_2: unk2,
                    script_name: s_name,
                    fragment_name: f_name,
                });
                c = next;
            }

            let (i, alias_count) = le_u16(c)?;
            let mut aliases = Vec::with_capacity(alias_count as usize);
            let mut c_alias = i;

            for _ in 0..alias_count {
                let (next, obj) = parse_object_property(c_alias, obj_format)?;
                let (next, a_ver) = le_i16(next)?;
                let (next, a_obj_fmt) = le_i16(next)?;
                let (next, a_script_count) = le_u16(next)?;

                let mut a_scripts = Vec::with_capacity(a_script_count as usize);
                let mut c_script = next;
                for _ in 0..a_script_count {
                    let (n, s) = parse_script_entry(c_script, a_ver, a_obj_fmt)?;
                    a_scripts.push(s);
                    c_script = n;
                }

                aliases.push(QustAliasScript {
                    object: obj,
                    version: a_ver,
                    obj_format: a_obj_fmt,
                    scripts: a_scripts,
                });
                c_alias = c_script;
            }

            (
                c_alias,
                Some(VmadFragments::Qust {
                    unknown,
                    file_name,
                    fragments: frags,
                    aliases,
                }),
            )
        }
        b"SCEN" => {
            let (i, unknown) = le_i8(curr)?;
            let (i, flags) = le_u8(i)?;
            let (i, file_name) = parse_wstring(i)?;
            let flag_count = flags.count_ones() as usize;

            let mut be_frags = Vec::with_capacity(flag_count);
            let mut c = i;
            for _ in 0..flag_count {
                let (next, unk) = le_i8(c)?;
                let (next, s_name) = parse_wstring(next)?;
                let (next, f_name) = parse_wstring(next)?;
                be_frags.push(ScenBeginEndFragment {
                    unknown: unk,
                    script_name: s_name,
                    fragment_name: f_name,
                });
                c = next;
            }

            let (i, phase_count) = le_u16(c)?;
            let mut phase_frags = Vec::with_capacity(phase_count as usize);
            let mut c_phase = i;

            for _ in 0..phase_count {
                let (next, unk1) = le_i8(c_phase)?;
                let (next, phase) = le_u32(next)?;
                let (next, unk2) = le_i8(next)?;
                let (next, s_name) = parse_wstring(next)?;
                let (next, f_name) = parse_wstring(next)?;
                phase_frags.push(ScenPhaseFragment {
                    unknown_1: unk1,
                    phase,
                    unknown_2: unk2,
                    script_name: s_name,
                    fragment_name: f_name,
                });
                c_phase = next;
            }

            (
                c_phase,
                Some(VmadFragments::Scen {
                    unknown,
                    flags,
                    file_name,
                    begin_end_fragments: be_frags,
                    phase_fragments: phase_frags,
                }),
            )
        }
        _ => (curr, None),
    };

    Ok((
        curr,
        VmadSubrecord {
            version,
            obj_format,
            scripts,
            fragments,
        },
    ))
}

/// Remaps object FormIDs in the primary VMAD scripts section in place while
/// preserving record-specific fragment bytes verbatim.
pub fn remap_primary_form_ids(
    data: &mut [u8],
    mut remap: impl FnMut(u32) -> color_eyre::Result<u32>,
) -> color_eyre::Result<()> {
    let mut cursor = 0usize;
    let version = read_i16(data, &mut cursor)?;
    let object_format = read_i16(data, &mut cursor)?;
    color_eyre::eyre::ensure!(
        matches!(object_format, 1 | 2),
        "unsupported VMAD object format {object_format}"
    );
    let script_count = read_u16(data, &mut cursor)?;
    for _ in 0..script_count {
        skip_wstring(data, &mut cursor)?;
        if version >= 4 {
            take_mut(data, &mut cursor, 1)?;
        }
        let property_count = read_u16(data, &mut cursor)?;
        for _ in 0..property_count {
            skip_wstring(data, &mut cursor)?;
            let property_type = take_mut(data, &mut cursor, 1)?[0];
            if version >= 4 {
                take_mut(data, &mut cursor, 1)?;
            }
            match property_type {
                1 => remap_object(data, &mut cursor, object_format, &mut remap)?,
                2 => skip_wstring(data, &mut cursor)?,
                3 | 4 => {
                    take_mut(data, &mut cursor, 4)?;
                }
                5 => {
                    take_mut(data, &mut cursor, 1)?;
                }
                11 => {
                    let count = read_u32(data, &mut cursor)?;
                    for _ in 0..count {
                        remap_object(data, &mut cursor, object_format, &mut remap)?;
                    }
                }
                12 => {
                    let count = read_u32(data, &mut cursor)?;
                    for _ in 0..count {
                        skip_wstring(data, &mut cursor)?;
                    }
                }
                13 | 14 => {
                    let count = read_u32(data, &mut cursor)? as usize;
                    take_mut(
                        data,
                        &mut cursor,
                        count
                            .checked_mul(4)
                            .ok_or_else(|| color_eyre::eyre::eyre!("VMAD array size overflow"))?,
                    )?;
                }
                15 => {
                    let count = read_u32(data, &mut cursor)? as usize;
                    take_mut(data, &mut cursor, count)?;
                }
                other => color_eyre::eyre::bail!("unsupported VMAD property type {other}"),
            }
        }
    }
    Ok(())
}

fn remap_object(
    data: &mut [u8],
    cursor: &mut usize,
    object_format: i16,
    remap: &mut impl FnMut(u32) -> color_eyre::Result<u32>,
) -> color_eyre::Result<()> {
    let object = take_mut(data, cursor, 8)?;
    let offset = if object_format == 1 { 0 } else { 4 };
    let form_id = u32::from_le_bytes(object[offset..offset + 4].try_into().unwrap());
    object[offset..offset + 4].copy_from_slice(&remap(form_id)?.to_le_bytes());
    Ok(())
}

fn take_mut<'a>(
    data: &'a mut [u8],
    cursor: &mut usize,
    count: usize,
) -> color_eyre::Result<&'a mut [u8]> {
    let end = cursor
        .checked_add(count)
        .ok_or_else(|| color_eyre::eyre::eyre!("VMAD offset overflow"))?;
    let value = data
        .get_mut(*cursor..end)
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated VMAD at offset {}", *cursor))?;
    *cursor = end;
    Ok(value)
}

fn read_u16(data: &mut [u8], cursor: &mut usize) -> color_eyre::Result<u16> {
    Ok(u16::from_le_bytes(
        take_mut(data, cursor, 2)?.try_into().unwrap(),
    ))
}

fn read_i16(data: &mut [u8], cursor: &mut usize) -> color_eyre::Result<i16> {
    Ok(i16::from_le_bytes(
        take_mut(data, cursor, 2)?.try_into().unwrap(),
    ))
}

fn read_u32(data: &mut [u8], cursor: &mut usize) -> color_eyre::Result<u32> {
    Ok(u32::from_le_bytes(
        take_mut(data, cursor, 4)?.try_into().unwrap(),
    ))
}

fn skip_wstring(data: &mut [u8], cursor: &mut usize) -> color_eyre::Result<()> {
    let length = read_u16(data, cursor)? as usize;
    take_mut(data, cursor, length)?;
    Ok(())
}

#[cfg(test)]
mod remap_tests {
    use super::*;

    #[test]
    fn remaps_scalar_and_array_object_properties_without_touching_fragments() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5i16.to_le_bytes());
        bytes.extend_from_slice(&2i16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(b"Test");
        bytes.push(0);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(b"One");
        bytes.extend_from_slice(&[1, 1]);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0x0100_1234u32.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(b"Many");
        bytes.extend_from_slice(&[11, 1]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0x0200_0567u32.to_le_bytes());
        bytes.extend_from_slice(b"fragment-tail");

        remap_primary_form_ids(&mut bytes, |value| Ok(value + 7)).unwrap();
        assert!(
            bytes
                .windows(4)
                .any(|value| value == 0x0100_123Bu32.to_le_bytes())
        );
        assert!(
            bytes
                .windows(4)
                .any(|value| value == 0x0200_056Eu32.to_le_bytes())
        );
        assert!(bytes.ends_with(b"fragment-tail"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VMAD with one script holding one array property of `property_type`
    /// whose element count is `count`, followed by `tail`.
    fn array_property_vmad(property_type: u8, count: u32, tail: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5i16.to_le_bytes());
        bytes.extend_from_slice(&2i16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(b"Test");
        bytes.push(0);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(b"Many");
        bytes.extend_from_slice(&[property_type, 1]);
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(tail);
        bytes
    }

    #[test]
    fn huge_array_counts_are_rejected_without_reserving_them() {
        for property_type in 11..=15 {
            let bytes = array_property_vmad(property_type, u32::MAX, &[0; 16]);
            assert!(parse_vmad(&bytes, b"STAT").is_err());
        }
    }
}
