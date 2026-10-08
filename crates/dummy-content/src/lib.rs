//! Deterministic, procedurally generated Skyrim asset fixtures.
//!
//! Every byte produced by this crate is synthesized from a caller-provided
//! seed. No proprietary game data is read, copied, or required, which lets
//! contributors exercise the converter and runtime without a local game
//! installation.
#![forbid(unsafe_code)]

pub mod ba2;
pub mod bsa;
mod bytes;
pub mod dds;
pub mod esm;
pub mod inhouse_actors;
pub mod inhouse_audio_extras;
pub mod inhouse_items;
pub mod inhouse_magic;
pub mod inhouse_visual_extras;
pub mod inhouse_world_extras;
pub mod layout;
pub mod nif;
mod path;
pub mod pex;
pub mod rng;

/// A named binary payload inside a generated archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    /// Archive-relative path, for example `textures/generated.dds`.
    pub name: &'a str,
    /// Raw payload bytes stored under `name`.
    pub data: &'a [u8],
}

impl<'a> Entry<'a> {
    /// Creates an archive entry.
    #[must_use]
    pub const fn new(name: &'a str, data: &'a [u8]) -> Self {
        Self { name, data }
    }
}
