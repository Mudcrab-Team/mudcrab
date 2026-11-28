//! Stable data contracts shared by the offline converter and the runtime.

pub mod actor_idle_catalog;
pub mod actor_outfits;
pub mod asset_lock;
pub mod collision;
pub mod coordinates;
pub mod creature_idle;
pub mod extensions;
pub mod form_key;
pub mod grass;
pub mod grass_settings;
pub mod initial_enable;
pub mod legacy_id_view;
pub mod lod;
pub mod model_visibility;
pub mod nif_alpha;
pub mod plugin_selection;
pub mod plugin_table;
pub mod records;
pub mod room_assignment;
pub mod source_form_id;
pub mod streaming_costs;
pub mod terrain_mesh;
pub mod texture_aliases;
pub mod world_assets;

use rkyv::{Archive, Deserialize, Serialize};

/// The line every Mudcrab binary prints for `--version`.
pub const VERSION_LINE: &str = concat!("mudcrab ", env!("CARGO_PKG_VERSION"));

/// True when the arguments ask for the version.
///
/// The engine parses this in its own option table; the other binaries call this
/// before they touch the file system, so `--version` never needs game data.
pub fn wants_version<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    args.into_iter()
        .any(|argument| argument.as_ref() == "--version" || argument.as_ref() == "-V")
}

/// Current combined database contract: corrected TXST roles and one-byte XESP flags.
pub const WORLD_DATABASE_SCHEMA_VERSION: u32 = include!("../world-database-schema-version.txt");

/// Current Papyrus script emission ABI: case-insensitive identifiers and permissive initial states.
///
/// 4: the shared `papyrus_runtime.luau` gained the helper that searches active-state methods across
/// the ancestry first (#764). Emitted scripts are unchanged; the runtime is part of the ABI because
/// the converter copies it into each pack and hashes it. Raising the number makes `--reuse-assets`
/// rebuild only the scripts and rewrite the runtime copy.
///
/// 5: the same runtime gained inherited-native lookup, fault-and-continue, a scheduler and
/// per-frame budgets (#703). Both changes reach packs through the same file, so ABI 4 packs made
/// on either side must rebuild their script kind.
///
/// Existing packs still load: the engine never reads a pack's ABI or its runtime copy. It runs
/// scripts only with `--papyrus-scripts`, and then uses its built-in runtime.
pub const PAPYRUS_SCRIPT_ABI_VERSION: u32 = 5;
/// Latest producer required when a package advertises the combined LOD contract.
/// 27: collision coverage (sphere shapes as zero-length capsules, multi-sphere and cylinder
/// shapes) changes GLB collision extras.
/// 28: KTX2 texture containers store mip level data smallest-first. Uncompressed
/// levels use exact `lcm(texel block bytes, 4)` alignment; supercompressed levels
/// are contiguous (KTX 2.0 sections 3.7 and 3.9.7). Producer-27
/// texture bytes keep the largest-first, 16-byte-padded layout and must be regenerated.
/// GLB bytes are unchanged, so producer-27 meshes stay reusable.
pub const LOD_CONVERTER_SCHEMA_VERSION: u32 = include!("../converter-schema-version.txt");
pub const WORLD_DATABASE_LOD_SCHEMA_VERSION: u32 = 6;

/// The oldest world database schema the runtime (the engine and `world-inspect`) still reads, and
/// so the oldest the launcher calls ready. Schemas 4, 5 and 8 only added tables and columns
/// (`lights`, `references.radius_override`, the movement tables, water fresnel columns, grass tables,
/// `references.emittance_id` and `emittance`), and every runtime query probes for them, so a schema
/// 3 database still loads.
pub const MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION: u32 = 3;

/// The oldest converter manifest schema the runtime still loads, and so the oldest the launcher
/// calls ready. Converter schema 15 wrote world database schema 3, which the runtime reads (see
/// [`MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION`]). The newest is the converter's own
/// `CONVERTER_SCHEMA_VERSION`, which lives in the converter crate.
pub const MIN_RUNTIME_CONVERTER_SCHEMA_VERSION: u32 = 15;

/// Whether the runtime reads a world database of this schema: from
/// [`MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION`] through [`WORLD_DATABASE_SCHEMA_VERSION`].
pub fn supports_runtime_world_database_schema(version: u32) -> bool {
    (MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION..=WORLD_DATABASE_SCHEMA_VERSION).contains(&version)
}

pub const CELL_CACHE_VERSION: u32 = 3;
pub const LAND_SIDE: u16 = 33;

/// Default LAND texture repeats per full cell per axis. Skyrim advances texture UVs
/// by `fLandTextureTilingMult / 4` per sample interval; the vanilla default is 3,
/// so 32 intervals give `32 * (3 / 4) = 24` repeats per cell.
/// Apply this to diffuse/normal sampling, never to the LAND blend-weight grid.
/// See [landscape texture scale](../../../docs/specs/engine/landscape-texture-scale.md)
/// for sources and verification.
pub const LAND_TEXTURE_REPEATS_PER_CELL: f32 = 32.0 * (3.0 / 4.0);

#[derive(Debug, Clone, Archive, Serialize, Deserialize, PartialEq)]
#[rkyv(bytecheck())]
pub struct TerrainLayer {
    pub texture_form_id: u32,
    pub quadrant: u8,
    pub layer: u16,
    pub is_base: bool,
    pub weights: Vec<TerrainWeight>,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, PartialEq)]
#[rkyv(bytecheck())]
pub struct TerrainWeight {
    pub vertex: u16,
    pub opacity: f32,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, PartialEq)]
#[rkyv(bytecheck())]
pub struct CachedLand {
    pub cell_id: u32,
    pub width: u16,
    pub height: u16,
    /// Row-major, absolute Creation Engine height units.
    pub heights: Vec<f32>,
    /// Packed signed XYZ normals, three bytes per vertex.
    pub normals: Vec<i8>,
    /// Packed RGB colors, three bytes per vertex.
    pub vertex_colors: Vec<u8>,
    pub layers: Vec<TerrainLayer>,
    pub water_height: Option<f32>,
    pub water_type_form_id: Option<u32>,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, PartialEq)]
#[rkyv(bytecheck())]
pub struct CellCache {
    pub version: u32,
    pub cells: Vec<CachedLand>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds3 {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds3 {
    pub const UNIT: Self = Self {
        min: [-0.5; 3],
        max: [0.5; 3],
    };

    pub fn is_finite_and_ordered(self) -> bool {
        self.min
            .iter()
            .chain(self.max.iter())
            .all(|value| value.is_finite())
            && (0..3).all(|axis| self.min[axis] <= self.max[axis])
    }
}

/// Result of inspecting a `CELL` `XCLC` subrecord for SSE land-hide flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XclcLandHide {
    /// Low nibble (`bit q` hides quadrant `q` in `0..4`). Zero when absent or not 12 bytes.
    pub flags: u8,
    /// Non-zero upper-nibble bits at byte 8 when `XCLC` is at least 9 bytes.
    pub upper_nibble_bits: u8,
    /// Present when `XCLC` is shorter than 9 bytes, not 12 bytes, or has upper-nibble bits set.
    pub issue: Option<XclcIssue>,
}

/// Non-standard `XCLC` layout observed while reading land-hide flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XclcIssue {
    Truncated(usize),
    LegacyNoFlags,
    UnexpectedLength(usize),
    UpperNibbleBits(u8),
}

/// Parse the low-nibble quadrant-hide bitmask from a `CELL` `XCLC` payload.
///
/// An SSE `XCLC` is 12 bytes with land-hide flags at byte 8 (`bit q` hides quadrant `q`, `0..4`).
/// Legacy 8-byte `XCLC` fields carry only `(grid_x, grid_y)` and hide nothing.
pub fn xclc_land_hide_flags(xclc: &[u8]) -> XclcLandHide {
    if xclc.len() < 8 {
        return XclcLandHide {
            flags: 0,
            upper_nibble_bits: 0,
            issue: Some(XclcIssue::Truncated(xclc.len())),
        };
    }
    if xclc.len() == 8 {
        return XclcLandHide {
            flags: 0,
            upper_nibble_bits: 0,
            issue: Some(XclcIssue::LegacyNoFlags),
        };
    }
    let raw = xclc[8];
    let upper_nibble_bits = raw & 0xF0;
    if xclc.len() != 12 {
        return XclcLandHide {
            flags: 0,
            upper_nibble_bits,
            issue: Some(XclcIssue::UnexpectedLength(xclc.len())),
        };
    }
    let flags = raw & 0x0F;
    let issue = (upper_nibble_bits != 0).then_some(XclcIssue::UpperNibbleBits(upper_nibble_bits));
    XclcLandHide {
        flags,
        upper_nibble_bits,
        issue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xclc_land_hide_flags_parses_sse_and_rejects_short_payloads() {
        let mut sse = [0u8; 12];
        sse[8] = 0x04;
        assert_eq!(
            xclc_land_hide_flags(&sse),
            XclcLandHide {
                flags: 0x04,
                upper_nibble_bits: 0,
                issue: None
            }
        );

        sse[8] = 0xF4;
        assert_eq!(
            xclc_land_hide_flags(&sse),
            XclcLandHide {
                flags: 0x04,
                upper_nibble_bits: 0xF0,
                issue: Some(XclcIssue::UpperNibbleBits(0xF0))
            }
        );

        let mut wrong_len = [0u8; 10];
        wrong_len[8] = 0xF4;
        assert_eq!(
            xclc_land_hide_flags(&wrong_len),
            XclcLandHide {
                flags: 0,
                upper_nibble_bits: 0xF0,
                issue: Some(XclcIssue::UnexpectedLength(10))
            }
        );

        assert_eq!(
            xclc_land_hide_flags(&[0u8; 8]),
            XclcLandHide {
                flags: 0,
                upper_nibble_bits: 0,
                issue: Some(XclcIssue::LegacyNoFlags)
            }
        );
        assert_eq!(
            xclc_land_hide_flags(&[0u8; 4]),
            XclcLandHide {
                flags: 0,
                upper_nibble_bits: 0,
                issue: Some(XclcIssue::Truncated(4))
            }
        );
    }

    #[test]
    fn version_line_and_flags_are_stable() {
        assert_eq!(VERSION_LINE, concat!("mudcrab ", env!("CARGO_PKG_VERSION")));
        assert!(wants_version(["--version"]));
        assert!(wants_version(["-V"]));
        assert!(!wants_version(["--verbose"]));
        assert!(!wants_version(["--help"]));
    }

    #[test]
    fn runtime_world_database_schemas_run_from_the_oldest_to_the_current() {
        let oldest = MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION;
        assert!(!supports_runtime_world_database_schema(oldest - 1));
        assert!(supports_runtime_world_database_schema(oldest));
        assert!(supports_runtime_world_database_schema(
            WORLD_DATABASE_SCHEMA_VERSION
        ));
        assert!(!supports_runtime_world_database_schema(
            WORLD_DATABASE_SCHEMA_VERSION + 1
        ));
    }
}
