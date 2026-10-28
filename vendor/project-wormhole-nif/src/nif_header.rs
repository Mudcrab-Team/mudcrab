use crate::dev::*;
use log::error;

use nom::number::complete::{le_u16, le_u32, le_u8};

#[derive(Debug)]
pub struct NifHeader {
    pub file_desc: StringN,
    pub nif_version: NifFileVersion,
    pub endian_type: Endianess,
    pub user_version: u32,
    pub block_count: u32,
    pub bethesda_version: u32,
    pub author: Option<SizedString8>,
    pub process_script: Option<SizedString8>,
    pub export_script: Option<SizedString8>,
    pub max_filepath: Option<SizedString8>,
    pub block_types: Vec<SizedString32>,
    pub block_type_index: Vec<u16>,
    pub block_size_index: Vec<u32>,
    pub string_count: u32,
    pub string_max_size: u32,
    pub strings: Vec<SizedString32>,
    pub groups: Vec<u32>,
}

fn count_error(i: &[u8]) -> nom::Err<nom::error::Error<&[u8]>> {
    nom::Err::Failure(nom::error::Error::new(i, nom::error::ErrorKind::Count))
}

fn parse_optional_string8(
    i: &[u8],
) -> nom::IResult<&[u8], Option<SizedString8>, nom::error::Error<&[u8]>> {
    let (i, value) = SizedString8::parse(i)?;
    Ok((
        i,
        if value.0.is_empty() {
            None
        } else {
            Some(value)
        },
    ))
}

impl Parse<&[u8]> for NifHeader {
    fn parse(i: &[u8]) -> nom::IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, file_desc) = StringN::parse(i)?;
        let (i, nif_version) = NifFileVersion::parse(i)?;
        let (i, endian_type) = Endianess::parse(i)?;
        let (i, user_version) = le_u32(i)?;
        let (i, block_count) = le_u32(i)?;
        let (i, bethesda_version) = le_u32(i)?;

        // BSStreamHeader fields vary with the Bethesda version rather than the
        // NIF version: Skyrim's BS version 100 stream has no Unknown Int and no
        // Max Filepath, so a fixed four-string header lands in the block-type
        // table.
        let (i, author) = parse_optional_string8(i)?;
        let i = if bethesda_version > 130 {
            let (i, _unknown_int) = le_u32(i)?;
            i
        } else {
            i
        };
        let (i, process_script) = if bethesda_version < 131 {
            parse_optional_string8(i)?
        } else {
            (i, None)
        };
        let (i, export_script) = parse_optional_string8(i)?;
        let (i, max_filepath) = if bethesda_version >= 103 {
            parse_optional_string8(i)?
        } else {
            (i, None)
        };

        let (mut i, num_types) = le_u16(i)?;
        if usize::from(num_types) > i.len() / 4 {
            return Err(count_error(i));
        }
        let mut block_types = Vec::with_capacity(usize::from(num_types));
        for _ in 0..num_types {
            let (next, value) = SizedString32::parse(i)?;
            i = next;
            block_types.push(value);
        }

        if block_count as usize > i.len() / 2 {
            return Err(count_error(i));
        }
        let mut block_type_index = Vec::with_capacity(block_count as usize);
        for _ in 0..block_count {
            let (next, value) = le_u16(i)?;
            i = next;
            block_type_index.push(value);
        }

        if block_count as usize > i.len() / 4 {
            return Err(count_error(i));
        }
        let mut block_size_index = Vec::with_capacity(block_count as usize);
        for _ in 0..block_count {
            let (next, value) = le_u32(i)?;
            i = next;
            block_size_index.push(value);
        }

        let (mut i, string_count) = le_u32(i)?;
        let (mut i, string_max_size) = le_u32(i)?;
        if string_count as usize > i.len() / 4 {
            return Err(count_error(i));
        }
        let mut strings = Vec::with_capacity(string_count as usize);
        for _ in 0..string_count {
            let (next, value) = SizedString32::parse(i)?;
            i = next;
            strings.push(value);
        }

        let (mut i, group_count) = le_u32(i)?;
        if group_count as usize > i.len() / 4 {
            return Err(count_error(i));
        }
        let mut groups = Vec::with_capacity(group_count as usize);
        for _ in 0..group_count {
            let (next, value) = le_u32(i)?;
            i = next;
            groups.push(value);
        }

        Ok((
            i,
            Self {
                file_desc,
                nif_version,
                endian_type,
                user_version,
                block_count,
                bethesda_version,
                author,
                process_script,
                export_script,
                max_filepath,
                block_types,
                block_type_index,
                block_size_index,
                string_count,
                string_max_size,
                strings,
                groups,
            },
        ))
    }
}

impl NifHeader {
    pub fn get_block_type(&self, index: usize) -> Result<&str, ()> {
        if index < self.block_type_index.len() {
            let i = self.block_type_index[index] as usize;
            if i < self.block_types.len() {
                Ok(&self.block_types[i].0)
            } else {
                error!("Block type index out of range: {}", i);
                Err(())
            }
        } else {
            error!("Block type index out of range: {}", index);
            Err(())
        }
    }

    pub fn get_block_size(&self, index: usize) -> Result<u32, ()> {
        if index < self.block_size_index.len() {
            Ok(self.block_size_index[index])
        } else {
            error!("Block size index out of range: {}", index);
            Err(())
        }
    }

    pub fn get_string(&self, index: usize) -> Result<&str, String> {
        if index < self.strings.len() {
            Ok(&self.strings[index].0)
        } else {
            Err(format!("String index out of range: {}", index))
        }
    }
}

#[derive(NomLE, PartialEq)]
pub struct NifFileVersion(pub u32);

impl NifFileVersion {
    pub fn major(&self) -> u8 {
        (self.0 >> 24) as u8
    }
    pub fn minor(&self) -> u8 {
        (self.0 >> 16) as u8
    }
    pub fn build(&self) -> u8 {
        (self.0 >> 8) as u8
    }
    pub fn patch(&self) -> u8 {
        (self.0 >> 0) as u8
    }
}

impl std::fmt::Debug for NifFileVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]", self, self.0)
    }
}

impl std::fmt::Display for NifFileVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major(),
            self.minor(),
            self.build(),
            self.patch()
        )
    }
}

#[derive(Debug)]
pub enum Endianess {
    Big,
    Little,
}

impl Parse<&[u8]> for Endianess {
    fn parse(i: &[u8]) -> nom::IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, endian_type) = le_u8(i)?;
        match endian_type {
            0x00 => Ok((i, Endianess::Big)),
            0x01 => Ok((i, Endianess::Little)),
            _ => Err(nom::Err::Error(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Tag,
            ))),
        }
    }
}

pub trait NifHeaderTraits {
    fn get_block_type(&self, index: usize) -> Result<&str, ()>;
    fn get_block_size(&self, index: usize) -> Result<u32, ()>;
    fn get_string(&self, index: usize) -> Result<&str, String>;
}
