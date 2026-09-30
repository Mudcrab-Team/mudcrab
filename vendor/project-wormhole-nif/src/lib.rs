#![allow(unused)]
// This crate is vendored compatibility code. Keep workspace Clippy focused on
// OpenSkyrim changes while upstream's legacy style is incrementally replaced.
#![allow(clippy::all)]
pub mod export;
pub mod legacy;

pub mod bs;
pub mod model;
pub mod nif_block;
pub mod nif_enum;
pub mod nif_file;
pub mod nif_flags;
pub mod nif_header;
pub mod nif_types;

#[cfg(test)]
mod tests;

mod dev {
    pub use half::prelude::*;
    pub use log::*;
    pub use nom::{bytes::complete::take, multi::count, number::complete::*, IResult};
    pub use nom_derive::nom;
    pub use nom_derive::{NomLE, Parse};
    pub use project_wormhole_esm::structs::strings::*;
    pub use project_wormhole_shared::prelude::*;
    pub use std::collections::{BTreeMap, HashSet};
    pub use std::io::{Read, Seek, SeekFrom};

    pub use super::bs::prelude::*;
    pub use super::legacy::*;
    pub use super::nif_block::*;
    pub use super::nif_enum::*;
    pub use super::nif_file::*;
    pub use super::nif_flags::*;
    pub use super::nif_header::*;
    pub use super::nif_types::*;
}
