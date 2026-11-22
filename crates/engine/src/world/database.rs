use super::enable_state::{EnableStateError, EnableStateResolver, InitialEnableInputs};
use bevy::prelude::Resource;
use color_eyre::{Result, eyre::WrapErr};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use shared::lod::{
    ChunkAnchor, ChunkKey, LodOrigin, LodTier, chunk_payload_path, object_chunk_payload_path,
};
use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

pub(crate) const EXTERIOR_CELL_ID_SQL: &str = "SELECT c.id FROM cells c
     LEFT JOIN land l ON l.cell_id=c.id
     WHERE c.worldspace_id=?1 AND c.grid_x=?2 AND c.grid_y=?3
     ORDER BY (l.cell_id IS NOT NULL) DESC, c.id DESC
     LIMIT 1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellKey {
    Exterior {
        worldspace_id: u32,
        grid_x: i32,
        grid_y: i32,
    },
    Interior(u32),
}

#[derive(Debug, Clone)]
pub struct ReferenceRow {
    pub form_id: u32,
    /// New-game enable-state snapshot, including parent chains. All rows remain available;
    /// this bootstrap value does not represent subsequent script/quest/save state.
    pub initially_enabled: bool,
    /// The placed reference carries the Multibound flag. Its authored policy, including `NONE`,
    /// must not be replaced with geometric room membership from an older conversion.
    pub has_multibound_override: bool,
    /// A versioned interior room rule initially attaches this reference to a named room.
    pub named_room_target: bool,
    /// The winning `ACHR` header's `Starts Dead` flag (`0x200`). Only actor references read it;
    /// other record types assign that bit another meaning. The engine has no death pose or
    /// ragdoll, so a corpse keeps its bind pose instead of playing a standing idle.
    pub starts_dead: bool,
    pub cell_id: u32,
    pub base_form_id: u32,
    /// Authoritative base record type; `statics` also contains movable clutter.
    pub base_record_type: Option<String>,
    /// Whether the base record carries the `Is Marker` header flag: an editor-only object, such as
    /// a map or furniture marker, that the game never draws.
    pub base_is_marker: bool,
    pub model_path: Option<String>,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub bounds_valid: bool,
    /// The `lights` row of the reference's base record, when the database has one and the record is
    /// a `LIGH`. `None` for every other reference, and for every reference in a database converted
    /// before lights were exported.
    pub light: Option<LightRow>,
    /// The reference's signed light-radius offset (`XRDS`), added to [`LightRow::radius`]. `None` when
    /// the reference carries no override, and in a database converted before the column existed.
    pub light_radius_override: Option<f32>,
    /// The 8-bit sRGB colour the reference's `XEMI` light or region lends its model's
    /// `External_Emittance` surfaces by day, the engine's one time of day (see `sky.rs`). `None`
    /// when the reference names no source the converter could resolve, and in a database
    /// converted before the `emittance` table existed.
    pub emittance: Option<[u8; 3]>,
    /// The reference's `door_links` row, when the database has the table and the reference is a
    /// load door. `None` for every other reference, and for every reference in a database
    /// converted before door links were exported.
    pub door: Option<DoorLinkRow>,
}

/// A `door_links` row as the world database stores it: where a load door leads, from the door's
/// `XTEL` subrecord.
#[derive(Debug, Clone, PartialEq)]
pub struct DoorLinkRow {
    /// The destination door reference (`XTEL` bytes 0..4, after plugin FormID remapping).
    pub destination_ref_id: u32,
    /// The destination reference's cell, `None` when the converter could not resolve it.
    pub destination_cell_id: Option<u32>,
    /// The destination reference's worldspace; `None` means the destination is an interior.
    pub destination_worldspace_id: Option<u32>,
    /// Arrival position in Creation units (`XTEL` bytes 4..16). Not the destination door's own
    /// position.
    pub arrival_position: [f32; 3],
    /// Arrival rotation in Creation-engine radians (`XTEL` bytes 16..28).
    pub arrival_rotation: [f32; 3],
}

/// A `lights` row as the converted database stores it
/// (`docs/specs/converters/db-schema.md`, §12): one row per `LIGH` record, with or without a model.
#[derive(Debug, Clone, PartialEq)]
pub struct LightRow {
    /// Radius in Creation units, from `DATA`'s radius.
    pub radius: f32,
    /// `DATA`'s colour bytes, red first.
    pub color: [u8; 3],
    /// `DATA`'s flags, uninterpreted; see the flag bits in [`crate::lights`].
    pub flags: u32,
    /// Authored FNAM; absent/invalid values use a labelled compatibility fallback.
    pub fade: Option<f32>,
    /// 0 absent, 1 valid (including zero), 2 invalid; None for older projections.
    pub fade_status: Option<u8>,
    /// Authored DATA falloff, retained even where the backend cannot evaluate it.
    pub falloff: Option<f32>,
    /// The placing reference's XLIG projection, absent in older packages.
    pub reference_data: Option<ReferenceLightRow>,
    /// Placing reference's XRDS state: 0 absent, 1 valid signed offset, 2 malformed.
    pub radius_status: Option<u8>,
}

/// Raw XLIG inputs, not effective multipliers. NULL fields retain malformed/non-finite status.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceLightRow {
    pub data_size: u32,
    pub fov_offset: Option<f32>,
    pub fade_offset: Option<f32>,
    pub end_distance_cap: Option<f32>,
    pub shadow_depth_bias: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct CellPayload {
    pub generation: u64,
    pub key: CellKey,
    pub cell_id: u32,
    pub references: Vec<ReferenceRow>,
}

#[derive(Debug)]
pub enum DatabaseRequest {
    Load {
        generation: u64,
        key: CellKey,
        queued_at: Instant,
    },
    LoadLodChunks {
        generation: u64,
        query: LodChunkQuery,
        queued_at: Instant,
    },
    /// All object LOD chunks of a worldspace (the additive `lod_object_chunks` table).
    LoadLodObjectChunks {
        generation: u64,
        worldspace_id: u32,
        queued_at: Instant,
    },
    Shutdown,
}

#[derive(Debug)]
pub struct DatabaseResponse {
    pub generation: u64,
    pub key: CellKey,
    pub result: std::result::Result<CellPayload, String>,
    pub query_micros: u64,
    pub queue_wait_micros: u64,
    pub total_request_micros: u64,
    pub row_count: usize,
}

/// World-space AABB used to discover LOD chunks. Bounds are XY Creation units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodChunkQuery {
    pub worldspace_id: u32,
    pub tier: LodTier,
    pub bounds_min: [f64; 2],
    pub bounds_max: [f64; 2],
}

/// Validated world-space bounds stored for an LOD chunk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodChunkBounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// Database metadata needed to validate and place one file-backed LOD chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct LodChunkMetadata {
    pub key: ChunkKey,
    /// Canonical path relative to the converted assets root.
    pub payload_path: String,
    /// Lowercase SHA-256 of the published GLB payload.
    pub content_hash: String,
    pub bounds: LodChunkBounds,
    /// Source exterior cells as absolute `(grid_x, grid_y)` coordinates.
    pub source_cells: Vec<[i32; 2]>,
    pub build_identity: String,
    pub origin: LodOrigin,
}

/// Generation-tagged result of an asynchronous LOD range query.
#[derive(Debug)]
pub struct LodChunkResponse {
    pub generation: u64,
    pub query: LodChunkQuery,
    pub result: std::result::Result<Vec<LodChunkMetadata>, LodQueryFailure>,
    pub query_micros: u64,
    pub queue_wait_micros: u64,
    pub total_request_micros: u64,
    pub chunk_count: usize,
}

/// Generation-tagged result of loading a worldspace's object LOD chunks. It has its own channel,
/// so terrain LOD intake never sees an object response.
#[derive(Debug)]
pub struct LodObjectResponse {
    pub generation: u64,
    pub worldspace_id: u32,
    pub result: std::result::Result<Vec<LodChunkMetadata>, LodQueryFailure>,
    /// The worldspace's shared atlas, when its chunks reference one (format 3). `Ok(None)` for a
    /// pack whose chunks embed the atlas.
    pub atlas: std::result::Result<Option<LodObjectAtlas>, LodQueryFailure>,
}

/// The shared object LOD atlas of a worldspace: a KTX2 file every object chunk of the worldspace
/// references (the additive `lod_object_atlases` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LodObjectAtlas {
    pub worldspace_id: u32,
    pub payload_path: String,
    pub content_hash: String,
}

/// Whether an asynchronous query failed temporarily or rejected invalid metadata.
#[derive(Debug)]
pub struct LodQueryFailure {
    pub transient: bool,
    pub reason: String,
}

impl LodQueryFailure {
    fn from_error(error: color_eyre::Report) -> Self {
        let transient = error.downcast_ref::<rusqlite::Error>().is_some_and(|error| {
            matches!(error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked | rusqlite::ErrorCode::SystemIoFailure))
        });
        Self {
            transient,
            reason: format!("{error:#}"),
        }
    }
}

impl std::fmt::Display for LodQueryFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

/// Which door-link contract the worker may read. Core packs discover the optional table;
/// legacy extension packs require a valid declaration and may explicitly disable the reader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DoorLinksAccess {
    /// Upstream core output; older core packs may have no door table.
    #[default]
    Core,
    /// A validated legacy extension declaration requires a complete door table.
    LegacyExtension,
    /// A stale or unsupported legacy declaration must not be read as core data.
    Disabled,
}

/// Dataset capabilities used by the world database worker (`crate::extensions`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldDatabaseFeatures {
    /// Ownership and validation policy for the promoted `door_links` table.
    pub door_links: DoorLinksAccess,
}

#[derive(Resource)]
pub struct WorldDatabase {
    requests: Sender<DatabaseRequest>,
    responses: Receiver<DatabaseResponse>,
    lod_responses: Receiver<LodChunkResponse>,
    lod_object_responses: Receiver<LodObjectResponse>,
    worker: Option<thread::JoinHandle<()>>,
    worker_stopped: Arc<AtomicBool>,
}

/// A water's above-water fog, read from its WATR `DNAM` in the record bytes (`waters.data`), so it
/// needs no column and no reconversion: the view into the water starts to cloud
/// `near` world units behind the surface, is fully opaque at `far`, and `amount` is the exponent of
/// the curve between them (0 leaves the body clear).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterFog {
    pub near: f32,
    pub far: f32,
    pub amount: f32,
}

impl Default for WaterFog {
    /// Skyrim's DefaultWater after Update.esm: fog near -10, far 150, amount 0.7.
    fn default() -> Self {
        Self {
            near: -10.0,
            far: 150.0,
            amount: 0.7,
        }
    }
}

impl WaterFog {
    fn is_usable(self) -> bool {
        self.near.is_finite() && self.far.is_finite() && self.amount.is_finite()
    }
}

/// Byte offsets of the above-water fog in a WATR `DNAM`, three little-endian `f32`s (see
/// `docs/research/water.md`). The amount sits at 132, so a `DNAM` shorter than 136 bytes has none.
const WATER_DNAM_FOG_NEAR: usize = 32;
const WATER_DNAM_FOG_FAR: usize = 36;
const WATER_DNAM_FOG_AMOUNT: usize = 132;

/// The parts of a `waters.data` blob the engine reads: the WATR `DNAM` bytes and the three noise
/// texture paths (`NAM2` to `NAM4`). The blob is the converter's archived list of the record's
/// subrecords. `None` when it does not decode or has no `DNAM`.
struct WaterRecord {
    dnam: Vec<u8>,
    /// `FNAM` (one byte, `TESWaterForm` +0xB9): bit 0 dangerous, bit 3 flow map, bit 4 blend
    /// normals (RF-WATER-013).
    flags: Option<u8>,
    noise_paths: [Option<String>; 3],
}

fn parse_water_record(record: &[u8]) -> Option<WaterRecord> {
    // SQLite gives no alignment guarantee, and the archive is validated before it is read.
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(record.len());
    aligned.extend_from_slice(record);
    let record =
        rkyv::from_bytes::<shared::records::ArchivedRecordData, rkyv::rancor::Error>(&aligned)
            .ok()?;
    let find = |tag: &[u8; 4]| {
        record
            .subrecords
            .iter()
            .find(|subrecord| &subrecord.tag == tag)
            .map(|subrecord| subrecord.data.as_slice())
    };
    let path = |tag: &[u8; 4]| {
        let bytes = find(tag)?;
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
        (!text.is_empty()).then_some(text)
    };
    Some(WaterRecord {
        dnam: find(b"DNAM")?.to_vec(),
        flags: find(b"FNAM").and_then(|bytes| bytes.first().copied()),
        noise_paths: [path(b"NAM2"), path(b"NAM3"), path(b"NAM4")],
    })
}

/// A little-endian `f32` at `offset` of a `DNAM`, if it is there and finite.
fn dnam_float(dnam: &[u8], offset: usize) -> Option<f32> {
    let bytes = dnam.get(offset..offset + 4)?;
    Some(f32::from_le_bytes(bytes.try_into().ok()?)).filter(|value| value.is_finite())
}

fn water_fog_from_dnam(dnam: &[u8]) -> Option<WaterFog> {
    let fog = WaterFog {
        near: dnam_float(dnam, WATER_DNAM_FOG_NEAR)?,
        far: dnam_float(dnam, WATER_DNAM_FOG_FAR)?,
        amount: dnam_float(dnam, WATER_DNAM_FOG_AMOUNT)?,
    };
    fog.is_usable().then_some(fog)
}

/// Decodes a water's fog from the `waters.data` blob. `None` when the blob does not decode, has no
/// `DNAM`, the `DNAM` is too short, or a value is not finite; the caller then uses
/// [`WaterFog::default`] and keeps the water's colours.
#[cfg(test)]
fn water_fog_from_record(record: &[u8]) -> Option<WaterFog> {
    water_fog_from_dnam(&parse_water_record(record)?.dnam)
}

/// The surface of a water, as its WATR record gives it: three noise normal maps, each with a UV
/// scale and an amplitude, and the depth property that flattens them in shallow water. Skyrim's
/// water shader blends the three maps as `normalize((0,0,1) + a0*n0 + a1*n1 + a2*n2)`
/// (Community Shaders `Water.hlsl`, `GetWaterNormal`); `docs/research/water.md` has the sources.
#[derive(Debug, Clone, PartialEq)]
pub struct WaterNoise {
    /// The converted texture of each layer (`NAM2` to `NAM4`); `None` for a layer with no path.
    pub textures: [Option<String>; 3],
    /// World units per texture tile (`DNAM` `uvScaleA`).
    pub uv_scale: [f32; 3],
    /// How much each layer tilts the normal (`DNAM` `amplitudeA`).
    pub amplitude: [f32; 3],
    /// `DNAM` depth properties: reflections, refraction, normals and specular lighting.
    pub depth_properties: [f32; 4],
    /// `DNAM` `noiseWindDirectionA`, degrees: the compass bearing the wind of each layer comes from
    /// (0 is north, 90 is east).
    pub wind_direction: [f32; 3],
    /// `DNAM` `noiseWindSpeedA`: noise-tile repeats per second.
    pub wind_speed: [f32; 3],
    /// `DNAM` `noiseFalloff` (+0x60), world units: the distance the reflection weight blends over
    /// (RF-SHADER-008).
    pub noise_falloff: f32,
    /// The sun terms of the retail water shader (RF-WATER-010): sun specular power (+0x10), sun
    /// specular magnitude (+0xCC), sparkle power (+0xE0), sparkle magnitude (+0xC8).
    pub sun: [f32; 4],
    /// `DNAM` refraction magnitude (+0x98), world units: how far the wave normal moves the point
    /// the bed is seen through (RF-WATER-049; 40 for the river records, 8 for the creek).
    pub refraction_magnitude: f32,
}

/// Byte offsets in a WATR `DNAM` (`docs/research/water.md`, section 1.2).
const WATER_DNAM_REFLECTION_MAGNITUDE: usize = 0xC4;
const WATER_DNAM_UV_SCALE: usize = 0xAC;
const WATER_DNAM_AMPLITUDE: usize = 0xB8;
const WATER_DNAM_DEPTH_PROPERTIES: usize = 0xD0;
const WATER_DNAM_WIND_DIRECTION: usize = 0x64;
const WATER_DNAM_WIND_SPEED: usize = 0x70;
const WATER_DNAM_NOISE_FALLOFF: usize = 0x60;
const WATER_DNAM_UNDERWATER_FOG_NEAR: usize = 0x90;
const WATER_DNAM_UNDERWATER_FOG_FAR: usize = 0x94;
const WATER_DNAM_SUN_SPECULAR_POWER: usize = 0x10;
const WATER_DNAM_SUN_SPECULAR_MAGNITUDE: usize = 0xCC;
const WATER_DNAM_SUN_SPARKLE_POWER: usize = 0xE0;
const WATER_DNAM_SUN_SPARKLE_MAGNITUDE: usize = 0xC8;
const WATER_DNAM_REFRACTION_MAGNITUDE: usize = 0x98;

/// Skyrim's DefaultWater reflection magnitude (`DNAM` +0xC4, 0.42 in the retail records read for
/// this PR), used when a water's record cannot be read.
pub const DEFAULT_WATER_REFLECTION_MAGNITUDE: f32 = 0.42;

fn water_noise_from_record(record: &WaterRecord) -> Option<WaterNoise> {
    let triple = |offset: usize| {
        Some([
            dnam_float(&record.dnam, offset)?,
            dnam_float(&record.dnam, offset + 4)?,
            dnam_float(&record.dnam, offset + 8)?,
        ])
    };
    let uv_scale = triple(WATER_DNAM_UV_SCALE)?;
    let amplitude = triple(WATER_DNAM_AMPLITUDE)?;
    // A scale of zero or less has no tile size; the amplitude may be zero, which silences a layer.
    if uv_scale.iter().any(|scale| *scale <= 0.0) || amplitude.iter().any(|amount| *amount < 0.0) {
        return None;
    }
    let depth = |index: usize| dnam_float(&record.dnam, WATER_DNAM_DEPTH_PROPERTIES + 4 * index);
    Some(WaterNoise {
        textures: record
            .noise_paths
            .clone()
            .map(|path| path.and_then(record_texture_path)),
        uv_scale,
        amplitude,
        depth_properties: [depth(0)?, depth(1)?, depth(2)?, depth(3)?],
        wind_direction: triple(WATER_DNAM_WIND_DIRECTION)?,
        wind_speed: triple(WATER_DNAM_WIND_SPEED)?,
        noise_falloff: dnam_float(&record.dnam, WATER_DNAM_NOISE_FALLOFF)?,
        sun: [
            dnam_float(&record.dnam, WATER_DNAM_SUN_SPECULAR_POWER)?,
            dnam_float(&record.dnam, WATER_DNAM_SUN_SPECULAR_MAGNITUDE)?,
            dnam_float(&record.dnam, WATER_DNAM_SUN_SPARKLE_POWER)?,
            dnam_float(&record.dnam, WATER_DNAM_SUN_SPARKLE_MAGNITUDE)?,
        ],
        refraction_magnitude: dnam_float(&record.dnam, WATER_DNAM_REFRACTION_MAGNITUDE)?,
    })
}

/// A water's Skyrim colours, reflectivity factors, opacity and fog. The colours and factors are
/// decoded by the converter from its WATR record's `DNAM` into `waters` columns
/// (`crates/converter/src/esm/exporter.rs`); a database converted before those columns existed
/// lacks them, so [`AssetCatalog::water_colors`] returns `None` for every water rather than
/// erroring, and callers fall back to Skyrim's DefaultWater values (see
/// `render::DEFAULT_WATER_FRESNEL`, `render::DEFAULT_WATER_REFLECTIVITY` and
/// [`WaterFog::default`]). `fog` is read from the record bytes in `waters.data`, per record: a
/// water whose `DNAM` cannot be read reads as `None` while its colours stay usable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterColors {
    pub shallow: [u8; 3],
    pub deep: [u8; 3],
    pub reflection: [u8; 3],
    pub fresnel: f32,
    pub reflectivity: f32,
    /// The water's `ANAM` opacity, 0-100: how much of the surface covers the bed. A water row with
    /// no opacity reads as Skyrim's DefaultWater 50.
    pub opacity: u8,
    pub fog: Option<WaterFog>,
    /// `DNAM` Reflection Magnitude: Skyrim's water shader reflects along
    /// `reflect(view, magnitude * normal + (0, 0, 1 - magnitude))`, so it is how much of the wave
    /// normal reaches the reflection. Read from the record bytes in `waters.data`.
    pub reflection_magnitude: Option<f32>,
    /// The record's `FNAM` byte, read from the record bytes: `waters.flags` holds the record header
    /// flags, not this (RF-WATER-013). Bit 3 selects retail's flow-map program, bit 4 blends the
    /// normals. `None` when the record bytes cannot be read.
    pub fnam: Option<u8>,
    /// `DNAM` underwater fog near (+0x90) and far (+0x94): the distances the underwater fog blends
    /// over (RF-WATER-031). `DNAM` +0x8C, the fog amount, is passed to the effect but no program
    /// reads it, so it is not read here.
    pub underwater_fog: Option<[f32; 2]>,
}

impl WaterColors {
    /// FNAM bit 3: retail draws this water with its flow-map program (RF-WATER-013, RF-WATER-019)
    /// in a cell that has a flow map (RF-WATER-036).
    pub fn uses_flow_map(&self) -> bool {
        self.fnam.is_some_and(|flags| flags & 0x08 != 0)
    }

    /// FNAM bit 4: blend normals (RF-WATER-013, RF-WATER-023).
    pub fn blends_normals(&self) -> bool {
        self.fnam.is_some_and(|flags| flags & 0x10 != 0)
    }
}

fn unpack_color(value: u32) -> [u8; 3] {
    let bytes = value.to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

#[derive(Resource, Default)]
pub struct AssetCatalog {
    object_texture_swaps: crate::object_materials::ObjectTextureCatalog,
    landscape_diffuse: std::collections::HashMap<u32, String>,
    landscape_normal: std::collections::HashMap<u32, String>,
    default_land: (Option<String>, Option<String>),
    water_flow: std::collections::HashMap<u32, String>,
    water_colors: std::collections::HashMap<u32, WaterColors>,
    water_noise: std::collections::HashMap<u32, WaterNoise>,
    /// The cells whose pack holds a per-cell flow map (RF-WATER-036), read from the pack's
    /// `textures/water/<plugin>/` folder when the catalog opens.
    water_flow_maps: std::collections::HashSet<(i32, i32)>,
}

/// The plugin folder the retail game reads a cell's flow map from: it formats the cell's own
/// plugin file name (RF-WATER-036). Every one of the 641 flow maps is under `skyrim.esm`, and no
/// other archive has one, so the engine reads that folder.
pub const WATER_FLOW_MAP_PLUGIN: &str = "skyrim.esm";

/// The cells with a flow map in `<pack>/textures/water/<plugin>/`: the file names are
/// `flow.<cellX>.<cellY>.ktx2`. A pack with no such folder has none.
fn read_flow_map_cells(pack: &Path) -> std::collections::HashSet<(i32, i32)> {
    let folder = pack
        .join("textures")
        .join("water")
        .join(WATER_FLOW_MAP_PLUGIN);
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Default::default();
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| flow_map_cell(&entry.file_name().to_string_lossy()))
        .collect()
}

/// The cell a flow map file name `flow.<x>.<y>.ktx2` belongs to.
fn flow_map_cell(file_name: &str) -> Option<(i32, i32)> {
    let stem = file_name.to_ascii_lowercase();
    let stem = stem.strip_prefix("flow.")?.strip_suffix(".ktx2")?;
    let (x, y) = stem.split_once('.')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

impl AssetCatalog {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut statement = connection.prepare(
            "SELECT l.id,t.diffuse_path FROM landscape_textures l JOIN texture_sets t ON t.id=l.texture_set_id WHERE t.diffuse_path IS NOT NULL",
        )?;
        let landscape_diffuse = statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(std::result::Result::ok)
            .filter_map(|(id, path)| converted_texture_path(path).map(|path| (id, path)))
            .collect();
        drop(statement);
        // Each landscape texture's normal map, where its texture set has one. A database written
        // before `texture_sets` carried `normal_path` (the in-memory fixtures) has none, and the
        // terrain then draws with the geometric normal alone.
        let landscape_normal = connection
            .prepare(
                "SELECT l.id,t.normal_path FROM landscape_textures l JOIN texture_sets t ON t.id=l.texture_set_id WHERE t.normal_path IS NOT NULL AND t.normal_path <> ''",
            )
            .map(|mut statement| {
                statement
                    .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)))
                    .map(|rows| {
                        rows.filter_map(std::result::Result::ok)
                            .filter_map(|(id, path)| converted_texture_path(path).map(|path| (id, path)))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let mut statement = connection.prepare(
            "SELECT id,flow_normal_path FROM waters WHERE flow_normal_path IS NOT NULL AND flow_normal_path <> ''",
        )?;
        let water_flow = statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(std::result::Result::ok)
            .filter_map(|(id, path)| converted_texture_path(path).map(|path| (id, path)))
            .collect();
        drop(statement);
        // A database converted before `fresnel`/`reflectivity` existed has no such columns, and
        // `prepare` fails with a "no such column" error rather than an empty result. Treat that
        // the same as no water colours at all instead of refusing to open the catalog. The fog is
        // not a column: it is read from the record bytes in `data`, so a database of any age that
        // still has them yields fog, and one that does not yields the default.
        let has_column = |name: &str| {
            connection
                .prepare("SELECT COUNT(*) FROM pragma_table_info('waters') WHERE name=?1")
                .and_then(|mut statement| statement.query_row([name], |row| row.get::<_, i64>(0)))
                .unwrap_or(0)
                > 0
        };
        let opacity_column = if has_column("opacity") {
            "COALESCE(opacity,50)"
        } else {
            "50"
        };
        let record_column = if has_column("data") { "data" } else { "NULL" };
        let water_colors_query = format!(
            "SELECT id,shallow_color,deep_color,reflection_color,fresnel,reflectivity,{opacity_column},{record_column} FROM waters \
             WHERE shallow_color IS NOT NULL AND deep_color IS NOT NULL \
             AND reflection_color IS NOT NULL AND fresnel IS NOT NULL AND reflectivity IS NOT NULL"
        );
        let water_rows: Vec<(u32, WaterColors, Option<WaterNoise>)> = connection
            .prepare(&water_colors_query)
            .ok()
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| {
                        let parsed = row
                            .get::<_, Option<Vec<u8>>>(7)?
                            .and_then(|record| parse_water_record(&record));
                        let fog = parsed
                            .as_ref()
                            .and_then(|record| water_fog_from_dnam(&record.dnam));
                        let reflection_magnitude = parsed.as_ref().and_then(|record| {
                            dnam_float(&record.dnam, WATER_DNAM_REFLECTION_MAGNITUDE)
                        });
                        let noise = parsed.as_ref().and_then(water_noise_from_record);
                        Ok((
                            row.get::<_, u32>(0)?,
                            WaterColors {
                                shallow: unpack_color(row.get::<_, u32>(1)?),
                                deep: unpack_color(row.get::<_, u32>(2)?),
                                reflection: unpack_color(row.get::<_, u32>(3)?),
                                fresnel: row.get::<_, f32>(4)?,
                                reflectivity: row.get::<_, f32>(5)?,
                                opacity: row.get::<_, i64>(6)?.clamp(0, 100) as u8,
                                fog,
                                reflection_magnitude,
                                fnam: parsed.as_ref().and_then(|record| record.flags),
                                underwater_fog: parsed.as_ref().and_then(|record| {
                                    Some([
                                        dnam_float(&record.dnam, WATER_DNAM_UNDERWATER_FOG_NEAR)?,
                                        dnam_float(&record.dnam, WATER_DNAM_UNDERWATER_FOG_FAR)?,
                                    ])
                                }),
                            },
                            noise,
                        ))
                    })
                    .ok()
                    .map(|rows| rows.filter_map(std::result::Result::ok).collect())
            })
            .unwrap_or_default();
        let mut water_colors = std::collections::HashMap::new();
        let mut water_noise = std::collections::HashMap::new();
        for (id, colors, noise) in water_rows {
            water_colors.insert(id, colors);
            if let Some(noise) = noise {
                water_noise.insert(id, noise);
            }
        }
        let has_default_land: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='terrain_default_texture')",
            [],
            |row| row.get(0),
        )?;
        let default_land = if has_default_land {
            match connection.query_row(
                "SELECT diffuse_path,normal_path FROM terrain_default_texture WHERE id=0",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            ) {
                Ok((diffuse, normal)) => {
                    let valid = |path: String| {
                        if !is_safe_relative_asset_path(&path)
                            || path.contains([':', '\\'])
                            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
                            || path != path.to_ascii_lowercase()
                            || !path.ends_with(".ktx2")
                        {
                            bevy::log::warn!("Invalid default land path; using white: {path}");
                            return None;
                        }
                        Some(path)
                    };
                    let diffuse = diffuse.and_then(valid);
                    let normal = diffuse.as_ref().and(normal).and_then(valid);
                    (diffuse, normal)
                }
                Err(error) => {
                    bevy::log::warn!("Invalid default land metadata; using white: {error}");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        Ok(Self {
            default_land,
            object_texture_swaps: crate::object_materials::ObjectTextureCatalog::read(&connection)?,
            landscape_diffuse,
            landscape_normal,
            water_flow,
            water_colors,
            water_noise,
            water_flow_maps: path.parent().map(read_flow_map_cells).unwrap_or_default(),
        })
    }

    /// The converted per-cell flow map of cell (`x`, `y`) (RF-WATER-036), when the pack has one.
    pub fn water_flow_map(&self, x: i32, y: i32) -> Option<String> {
        self.water_flow_maps
            .contains(&(x, y))
            .then(|| format!("textures/water/{WATER_FLOW_MAP_PLUGIN}/flow.{x}.{y}.ktx2"))
    }

    /// The converted LTEX diffuse, or the pack's configured default for the zero base ID.
    pub fn landscape_diffuse(&self, form_id: u32) -> Option<&str> {
        if form_id == shared::terrain_mesh::DEFAULT_LAND_TEXTURE_FORM_ID {
            return self.default_land.0.as_deref();
        }
        self.landscape_diffuse.get(&form_id).map(String::as_str)
    }

    pub(crate) fn object_texture_swaps(
        &self,
        base: u32,
    ) -> Option<crate::object_materials::ReferenceTextureSwaps> {
        self.object_texture_swaps.get(base)
    }

    /// The converted LTEX normal, or the pack's configured default for the zero base ID.
    pub fn landscape_normal(&self, form_id: u32) -> Option<&str> {
        if form_id == shared::terrain_mesh::DEFAULT_LAND_TEXTURE_FORM_ID {
            return self.default_land.1.as_deref();
        }
        self.landscape_normal.get(&form_id).map(String::as_str)
    }

    pub fn water_flow(&self, form_id: u32) -> Option<&str> {
        self.water_flow.get(&form_id).map(String::as_str)
    }

    /// The noise normal maps and their scales of water `form_id`, when its record bytes give them.
    pub fn water_noise(&self, form_id: u32) -> Option<&WaterNoise> {
        self.water_noise.get(&form_id)
    }

    pub fn water_colors(&self, form_id: u32) -> Option<WaterColors> {
        self.water_colors.get(&form_id).copied()
    }
}

/// The converted texture of a path as a record's own subrecord spells it, for example
/// `Data\\Textures\\Water\\DefaultWater.dds`: the converter's `waters` columns hold such paths
/// already without the `Data` folder, the record bytes do not.
fn record_texture_path(path: String) -> Option<String> {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let without_data = normalized.strip_prefix("data/").unwrap_or(&normalized);
    converted_texture_path(without_data.to_owned())
}

/// Maps a texture path stored in the world database to the converted `.ktx2` under `textures/`.
///
/// The database is input the engine did not write, so a path that could leave `textures/` - a
/// `..` or `.` segment, an absolute or drive-prefixed path, an empty segment, or a `:` (a Windows
/// drive or stream, or an asset source such as `embedded://`) - is refused rather than loaded.
pub(crate) fn converted_texture_path(path: String) -> Option<String> {
    shared::texture_aliases::converted_record_texture_path(&path)
}

/// Rejects a database-supplied relative path that could escape the assets
/// root once re-rooted under `textures/` or `meshes/` with `PathBuf::join`.
/// A `..` segment walks back out of the base directory, and a rooted or
/// drive-prefixed path makes `PathBuf::join` replace the base entirely
/// instead of appending to it (see the `std::path::PathBuf::push` docs).
/// Every component must therefore be a plain, non-empty path segment.
fn is_safe_relative_asset_path(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
}

impl WorldDatabase {
    #[cfg(test)]
    pub(crate) fn channel_fixture(
        capacity: usize,
    ) -> (Self, Receiver<DatabaseRequest>, Sender<DatabaseResponse>) {
        let (requests, request_rx) = bounded(capacity);
        let (response_tx, responses) = unbounded();
        let (_lod_tx, lod_responses) = unbounded();
        let (_lod_object_tx, lod_object_responses) = unbounded();
        (
            Self {
                requests,
                responses,
                lod_responses,
                lod_object_responses,
                worker: None,
                worker_stopped: Arc::new(AtomicBool::new(true)),
            },
            request_rx,
            response_tx,
        )
    }

    /// [`Self::channel_fixture`] that keeps the object LOD response sender, so a test can answer
    /// an object chunk request itself.
    #[cfg(test)]
    pub(crate) fn channel_fixture_with_lod_objects(
        capacity: usize,
    ) -> (Self, Receiver<DatabaseRequest>, Sender<LodObjectResponse>) {
        let (requests, request_rx) = bounded(capacity);
        let (_response_tx, responses) = unbounded();
        let (_lod_tx, lod_responses) = unbounded();
        let (lod_object_tx, lod_object_responses) = unbounded();
        (
            Self {
                requests,
                responses,
                lod_responses,
                lod_object_responses,
                worker: None,
                worker_stopped: Arc::new(AtomicBool::new(true)),
            },
            request_rx,
            lod_object_tx,
        )
    }

    /// Opens available core doors, including legacy packages with no door table.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, WorldDatabaseFeatures::default())
    }

    /// Opens the ordinary database with a validated door capability policy.
    pub fn open_with(path: &Path, features: WorldDatabaseFeatures) -> Result<Self> {
        validate(path)?;
        if features.door_links == DoorLinksAccess::LegacyExtension {
            require_table(path, "door_links", crate::extensions::Feature::DoorLinks)?;
            let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            color_eyre::eyre::ensure!(
                has_door_links(&connection)?,
                "extension {} is listed but its door_links table lacks columns",
                shared::extensions::DOOR_LINKS
            );
        }
        let path = path.to_owned();
        let (request_tx, request_rx) = bounded(128);
        // Responses must not block shutdown if the main world stops polling.
        let (response_tx, response_rx) = unbounded();
        let (lod_response_tx, lod_response_rx) = unbounded();
        let (lod_object_tx, lod_object_rx) = unbounded();
        let worker_stopped = Arc::new(AtomicBool::new(false));
        let stopped = worker_stopped.clone();
        let worker = thread::Builder::new()
            .name("mudcrab-world-db".into())
            .spawn(move || {
                worker(
                    path,
                    features,
                    request_rx,
                    response_tx,
                    lod_response_tx,
                    lod_object_tx,
                );
                stopped.store(true, Ordering::Release);
            })
            .wrap_err("failed to start world database worker")?;
        Ok(Self {
            requests: request_tx,
            responses: response_rx,
            lod_responses: lod_response_rx,
            lod_object_responses: lod_object_rx,
            worker: Some(worker),
            worker_stopped,
        })
    }

    pub fn request(&self, request: DatabaseRequest) -> Result<()> {
        self.requests
            .send(request)
            .wrap_err("world database worker stopped")
    }

    /// Ordinary streaming intake must never wait for the worker on the main thread.
    pub(crate) fn try_request(&self, request: DatabaseRequest) -> Result<()> {
        enqueue_request(&self.requests, request)
    }

    pub fn try_response(&self) -> Option<DatabaseResponse> {
        self.responses.try_recv().ok()
    }

    /// Includes returned payloads that the main world has not polled yet.
    pub(crate) fn pending_cell_responses(&self) -> usize {
        self.responses.len()
    }

    /// Queue a world-space LOD chunk range query on the existing database worker.
    pub fn request_lod_chunks(&self, generation: u64, query: LodChunkQuery) -> Result<()> {
        enqueue_lod_query(&self.requests, generation, query)
    }

    /// Read the next LOD response without consuming ordinary cell responses.
    pub fn try_lod_response(&self) -> Option<LodChunkResponse> {
        self.lod_responses.try_recv().ok()
    }

    /// Queue a load of every object LOD chunk of `worldspace_id` on the existing worker.
    pub fn request_lod_object_chunks(&self, generation: u64, worldspace_id: u32) -> Result<()> {
        enqueue_request(
            &self.requests,
            DatabaseRequest::LoadLodObjectChunks {
                generation,
                worldspace_id,
                queued_at: Instant::now(),
            },
        )
    }

    /// Read the next object LOD response.
    pub fn try_lod_object_response(&self) -> Option<LodObjectResponse> {
        self.lod_object_responses.try_recv().ok()
    }
}

fn enqueue_lod_query(
    requests: &Sender<DatabaseRequest>,
    generation: u64,
    query: LodChunkQuery,
) -> Result<()> {
    let request = DatabaseRequest::LoadLodChunks {
        generation,
        query,
        queued_at: Instant::now(),
    };
    enqueue_request(requests, request)
}

fn enqueue_request(requests: &Sender<DatabaseRequest>, request: DatabaseRequest) -> Result<()> {
    match requests.try_send(request) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(color_eyre::eyre::eyre!(
            "world database request queue is full"
        )),
        Err(TrySendError::Disconnected(_)) => {
            Err(color_eyre::eyre::eyre!("world database worker stopped"))
        }
    }
}

impl Drop for WorldDatabase {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.requests.send(DatabaseRequest::Shutdown);
            let _ = worker.join();
        }
        debug_assert!(self.worker_stopped.load(Ordering::Acquire));
    }
}

fn require_table(path: &Path, table: &str, feature: crate::extensions::Feature) -> Result<()> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .wrap_err_with(|| format!("failed to open {}", path.display()))?;
    let found: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1 COLLATE NOCASE",
        [table],
        |row| row.get(0),
    )?;
    color_eyre::eyre::ensure!(
        found > 0,
        "extension {} is listed but {} has no `{table}` table",
        feature.extension(),
        path.display()
    );
    Ok(())
}

fn validate(path: &Path) -> Result<()> {
    shared::world_assets::validate_world_database(path).map(|_| ())
}

pub(crate) use shared::world_assets::validate_lod_build_contract;

fn worker(
    path: std::path::PathBuf,
    features: WorldDatabaseFeatures,
    requests: Receiver<DatabaseRequest>,
    responses: Sender<DatabaseResponse>,
    lod_responses: Sender<LodChunkResponse>,
    lod_object_responses: Sender<LodObjectResponse>,
) {
    // Which optional tables and columns this database has does not change while it is open, so
    // the reference query is built once for the connection. If the database cannot be opened or
    // probed, every request is still answered, with that error, so the cells fail visibly instead
    // of waiting forever.
    let mut setup = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(color_eyre::eyre::Report::from)
    .and_then(|connection| {
        let query = ReferenceQuery::for_connection(&connection, features)?;
        Ok((connection, query))
    })
    .map_err(|error| format!("world database {} is unusable: {error:#}", path.display()));
    while let Ok(request) = requests.recv() {
        match request {
            DatabaseRequest::Shutdown => break,
            DatabaseRequest::Load {
                generation,
                key,
                queued_at,
            } => {
                let queue_wait_micros = elapsed_micros(queued_at);
                let started = Instant::now();
                let result = match &mut setup {
                    Ok((connection, query)) => load_cell(connection, query, generation, key)
                        .map_err(|error| format!("{error:#}")),
                    Err(error) => Err(error.clone()),
                };
                let query_micros = elapsed_micros(started);
                let row_count = result
                    .as_ref()
                    .map_or(0, |payload| payload.references.len());
                if responses
                    .send(DatabaseResponse {
                        generation,
                        key,
                        result,
                        query_micros,
                        queue_wait_micros,
                        total_request_micros: elapsed_micros(queued_at),
                        row_count,
                    })
                    .is_err()
                {
                    break;
                }
            }
            DatabaseRequest::LoadLodChunks {
                generation,
                query,
                queued_at,
            } => {
                let queue_wait_micros = elapsed_micros(queued_at);
                let started = Instant::now();
                let result = match &setup {
                    Ok((connection, _)) => {
                        load_lod_chunks(connection, query).map_err(LodQueryFailure::from_error)
                    }
                    Err(error) => Err(LodQueryFailure {
                        transient: false,
                        reason: error.clone(),
                    }),
                };
                let query_micros = elapsed_micros(started);
                let chunk_count = result.as_ref().map_or(0, Vec::len);
                if lod_responses
                    .send(LodChunkResponse {
                        generation,
                        query,
                        result,
                        query_micros,
                        queue_wait_micros,
                        total_request_micros: elapsed_micros(queued_at),
                        chunk_count,
                    })
                    .is_err()
                {
                    break;
                }
            }
            DatabaseRequest::LoadLodObjectChunks {
                generation,
                worldspace_id,
                queued_at: _,
            } => {
                let (result, atlas) = match &setup {
                    Ok((connection, _)) => (
                        load_lod_object_chunks(connection, worldspace_id)
                            .map_err(LodQueryFailure::from_error),
                        load_lod_object_atlas(connection, worldspace_id)
                            .map_err(LodQueryFailure::from_error),
                    ),
                    Err(error) => (
                        Err(LodQueryFailure {
                            transient: false,
                            reason: error.clone(),
                        }),
                        Err(LodQueryFailure {
                            transient: false,
                            reason: error.clone(),
                        }),
                    ),
                };
                if lod_object_responses
                    .send(LodObjectResponse {
                        generation,
                        worldspace_id,
                        result,
                        atlas,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn load_lod_chunks(connection: &Connection, query: LodChunkQuery) -> Result<Vec<LodChunkMetadata>> {
    validate_lod_query(query)?;

    if !has_lod_table(connection)? {
        let version: u32 =
            connection.query_row("SELECT version FROM schema_info", [], |row| row.get(0))?;
        color_eyre::eyre::ensure!(
            version < shared::WORLD_DATABASE_LOD_SCHEMA_VERSION,
            "world database has no LOD chunk table"
        );
        return Ok(Vec::new());
    }
    let has_chunks: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM lod_chunks WHERE worldspace_id=?1)",
        [query.worldspace_id],
        |row| row.get(0),
    )?;
    if !has_chunks {
        return Ok(Vec::new());
    }

    let origin = lod_worldspace_origin(connection, query.worldspace_id)?;
    let build_identity = lod_build_identity(connection)?;

    let mut statement = connection.prepare_cached(
        "SELECT DISTINCT c.worldspace_id,c.tier,c.anchor_x,c.anchor_y,c.payload_path,c.content_hash,\
         c.bounds_min_x,c.bounds_min_y,c.bounds_min_z,c.bounds_max_x,c.bounds_max_y,c.bounds_max_z,\
         c.source_cells FROM lod_chunks_spatial r \
         JOIN lod_chunks c ON c.worldspace_id=r.worldspace_id AND c.tier=r.tier \
         AND c.anchor_x=r.anchor_x AND c.anchor_y=r.anchor_y \
         WHERE r.worldspace_id=?1 AND r.tier=?2 \
         AND r.minX<=?4 AND r.maxX>=?3 AND r.minY<=?6 AND r.maxY>=?5",
    )?;
    let rows = statement.query_map(
        params![
            query.worldspace_id,
            query.tier.side_cells(),
            query.bounds_min[0],
            query.bounds_max[0],
            query.bounds_min[1],
            query.bounds_max[1],
        ],
        RawLodChunk::from_row,
    )?;

    rows.map(|row| {
        let raw = row?;
        validate_lod_chunk(
            raw,
            query.worldspace_id,
            origin,
            &build_identity,
            chunk_payload_path,
        )
    })
    .collect()
}

/// The LOD grid origin of a worldspace, from its `worldspaces` row.
fn lod_worldspace_origin(connection: &Connection, worldspace_id: u32) -> Result<LodOrigin> {
    let (origin_x, origin_y): (Option<i32>, Option<i32>) = connection
        .query_row(
            "SELECT lod_origin_x,lod_origin_y FROM worldspaces WHERE id=?1",
            [worldspace_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .wrap_err_with(|| {
            format!(
                "LOD worldspace {worldspace_id} is missing from worldspaces or has invalid origin columns"
            )
        })?;
    Ok(LodOrigin::new(
        origin_x.ok_or_else(|| {
            color_eyre::eyre::eyre!("LOD worldspace {worldspace_id} has no valid X origin")
        })?,
        origin_y.ok_or_else(|| {
            color_eyre::eyre::eyre!("LOD worldspace {worldspace_id} has no valid Y origin")
        })?,
    ))
}

/// The pack's LOD build identity (`lod_build`), which every chunk row is checked against.
fn lod_build_identity(connection: &Connection) -> Result<String> {
    let build_identity: String = connection
        .query_row(
            "SELECT build_identity FROM lod_build WHERE id=1",
            [],
            |row| row.get(0),
        )
        .wrap_err("LOD build identity is missing from lod_build")?;
    color_eyre::eyre::ensure!(
        is_canonical_sha256(&build_identity),
        "LOD build identity is not a lowercase SHA-256 digest"
    );
    Ok(build_identity)
}

/// Object LOD chunks of a worldspace, from the additive `lod_object_chunks` table.
///
/// Returns no chunks for a pack without that table, so a pack converted before object LOD loads
/// exactly as before. A pack that has the table is held to the terrain contract: a row with a
/// non-canonical payload path, a bad hash, bad bounds or an identity that does not match
/// `lod_build` is an error, never a skipped row. Use [`verify_lod_object_payloads`] to check that
/// every listed file exists and matches its hash.
pub fn load_lod_object_chunks(
    connection: &Connection,
    worldspace_id: u32,
) -> Result<Vec<LodChunkMetadata>> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_object_chunks')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(Vec::new());
    }
    let has_chunks: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM lod_object_chunks WHERE worldspace_id=?1)",
        [worldspace_id],
        |row| row.get(0),
    )?;
    if !has_chunks {
        return Ok(Vec::new());
    }
    let origin = lod_worldspace_origin(connection, worldspace_id)?;
    let build_identity = lod_build_identity(connection)?;
    let mut statement = connection.prepare_cached(
        "SELECT worldspace_id,tier,anchor_x,anchor_y,payload_path,content_hash,\
         bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,\
         source_cells FROM lod_object_chunks WHERE worldspace_id=?1 \
         ORDER BY tier,anchor_x,anchor_y",
    )?;
    let rows = statement.query_map([worldspace_id], RawLodChunk::from_row)?;
    rows.map(|row| {
        validate_lod_chunk(
            row?,
            worldspace_id,
            origin,
            &build_identity,
            object_chunk_payload_path,
        )
    })
    .collect()
}

/// The shared object LOD atlas of a worldspace, from the additive `lod_object_atlases` table.
///
/// `Ok(None)` for a pack without that table or without a row for the worldspace: its chunks embed
/// the atlas. A row with a non-canonical path or hash is an error.
pub fn load_lod_object_atlas(
    connection: &Connection,
    worldspace_id: u32,
) -> Result<Option<LodObjectAtlas>> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_object_atlases')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT payload_path, content_hash FROM lod_object_atlases WHERE worldspace_id=?1",
            [worldspace_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((payload_path, content_hash)) = row else {
        return Ok(None);
    };
    let canonical = shared::lod::object_atlas_path(worldspace_id);
    color_eyre::eyre::ensure!(
        is_safe_relative_asset_path(&payload_path) && payload_path == canonical,
        "object LOD atlas of worldspace {worldspace_id} has unsafe or non-canonical path {payload_path:?}; expected {canonical:?}"
    );
    color_eyre::eyre::ensure!(
        is_canonical_sha256(&content_hash),
        "object LOD atlas of worldspace {worldspace_id} has an invalid content hash"
    );
    Ok(Some(LodObjectAtlas {
        worldspace_id,
        payload_path,
        content_hash,
    }))
}

/// Checks that the shared atlas file exists under `assets` and matches its recorded SHA-256. A
/// missing or damaged atlas is an error that names the file.
pub fn verify_lod_object_atlas(assets: &Path, atlas: &LodObjectAtlas) -> Result<()> {
    crate::streaming::lod::verify_payload_hash(
        assets.join(&atlas.payload_path),
        atlas.content_hash.clone(),
    )
    .map_err(|failure| {
        color_eyre::eyre::eyre!(
            "the shared object LOD atlas {} failed its integrity check: {}",
            atlas.payload_path,
            failure.reason()
        )
    })
}

/// Checks that every object LOD chunk's file exists under `assets` and matches its recorded
/// SHA-256. A listed chunk whose file is missing or damaged is an error that names the chunk: the
/// pack fails closed instead of drawing without it.
pub fn verify_lod_object_payloads(assets: &Path, chunks: &[LodChunkMetadata]) -> Result<()> {
    for chunk in chunks {
        // The terrain loader's own check: it reads the file, hashes it and reports a missing
        // file or a different SHA-256 as an error.
        crate::streaming::lod::verify_payload_hash(
            assets.join(&chunk.payload_path),
            chunk.content_hash.clone(),
        )
        .map_err(|failure| {
            color_eyre::eyre::eyre!(
                "object LOD chunk {:?} failed its integrity check: {}",
                chunk.key,
                failure.reason()
            )
        })?;
    }
    Ok(())
}

fn has_lod_table(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_chunks')",
        [],
        |row| row.get(0),
    )?)
}

fn validate_lod_query(query: LodChunkQuery) -> Result<()> {
    for (axis, min, max) in [
        ("X", query.bounds_min[0], query.bounds_max[0]),
        ("Y", query.bounds_min[1], query.bounds_max[1]),
    ] {
        color_eyre::eyre::ensure!(
            min.is_finite() && max.is_finite() && min <= max,
            "LOD query has non-finite or unordered {axis} bounds: {min}..{max}"
        );
    }
    Ok(())
}

struct RawLodChunk {
    worldspace_id: i64,
    tier: i64,
    anchor_x: i64,
    anchor_y: i64,
    payload_path: String,
    content_hash: String,
    bounds_min: [f64; 3],
    bounds_max: [f64; 3],
    source_cells: String,
}

impl RawLodChunk {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            worldspace_id: row.get(0)?,
            tier: row.get(1)?,
            anchor_x: row.get(2)?,
            anchor_y: row.get(3)?,
            payload_path: row.get(4)?,
            content_hash: row.get(5)?,
            bounds_min: [row.get(6)?, row.get(7)?, row.get(8)?],
            bounds_max: [row.get(9)?, row.get(10)?, row.get(11)?],
            source_cells: row.get(12)?,
        })
    }
}

fn validate_lod_chunk(
    raw: RawLodChunk,
    requested_worldspace: u32,
    origin: LodOrigin,
    build_identity: &str,
    canonical_payload_path: fn(ChunkKey) -> String,
) -> Result<LodChunkMetadata> {
    let worldspace_id = u32::try_from(raw.worldspace_id)
        .wrap_err_with(|| format!("LOD chunk has invalid worldspace id {}", raw.worldspace_id))?;
    color_eyre::eyre::ensure!(
        worldspace_id == requested_worldspace,
        "LOD chunk worldspace {worldspace_id} does not match requested worldspace {requested_worldspace}"
    );
    let tier = lod_tier_from_database(raw.tier)?;
    let anchor_x = i32::try_from(raw.anchor_x)
        .wrap_err_with(|| format!("LOD chunk has invalid anchor X {}", raw.anchor_x))?;
    let anchor_y = i32::try_from(raw.anchor_y)
        .wrap_err_with(|| format!("LOD chunk has invalid anchor Y {}", raw.anchor_y))?;
    let key = ChunkKey::new(worldspace_id, tier, ChunkAnchor::new(anchor_x, anchor_y));

    let canonical_path = canonical_payload_path(key);
    color_eyre::eyre::ensure!(
        is_safe_relative_asset_path(&raw.payload_path) && raw.payload_path == canonical_path,
        "LOD chunk {key:?} has unsafe or non-canonical payload path {:?}; expected {canonical_path:?}",
        raw.payload_path
    );
    color_eyre::eyre::ensure!(
        is_canonical_sha256(&raw.content_hash),
        "LOD chunk {key:?} has an invalid content hash"
    );
    for axis in 0..3 {
        color_eyre::eyre::ensure!(
            raw.bounds_min[axis].is_finite()
                && raw.bounds_max[axis].is_finite()
                && raw.bounds_min[axis] <= raw.bounds_max[axis],
            "LOD chunk {key:?} has non-finite or unordered world-space bounds on axis {axis}"
        );
    }

    Ok(LodChunkMetadata {
        key,
        payload_path: raw.payload_path,
        content_hash: raw.content_hash,
        bounds: LodChunkBounds {
            min: raw.bounds_min,
            max: raw.bounds_max,
        },
        source_cells: parse_source_cells(&raw.source_cells)
            .wrap_err_with(|| format!("LOD chunk {key:?} has invalid source_cells metadata"))?,
        build_identity: build_identity.to_owned(),
        origin,
    })
}

fn lod_tier_from_database(value: i64) -> Result<LodTier> {
    let side_cells =
        i32::try_from(value).wrap_err_with(|| format!("LOD chunk has invalid tier {value}"))?;
    LodTier::from_side_cells(side_cells)
        .ok_or_else(|| color_eyre::eyre::eyre!("LOD chunk has unsupported tier {side_cells}"))
}

fn parse_source_cells(value: &str) -> Result<Vec<[i32; 2]>> {
    color_eyre::eyre::ensure!(!value.is_empty(), "source_cells is empty");
    let mut seen = BTreeSet::new();
    let mut cells = Vec::new();
    for pair in value.split(';') {
        let (x, y) = pair
            .split_once(',')
            .ok_or_else(|| color_eyre::eyre::eyre!("expected x,y cell coordinate, got {pair:?}"))?;
        let cell = [
            x.parse::<i32>()
                .wrap_err_with(|| format!("invalid source cell X coordinate {x:?}"))?,
            y.parse::<i32>()
                .wrap_err_with(|| format!("invalid source cell Y coordinate {y:?}"))?,
        ];
        color_eyre::eyre::ensure!(seen.insert(cell), "duplicate source cell {cell:?}");
        cells.push(cell);
    }
    Ok(cells)
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The reference columns [`map_reference`] reads, in order: the placement itself and the base
/// object's model and bounds. Every table this query can be missing is joined on after them, so the
/// indices below are the same whichever tables a database has.
const REFERENCE_COLUMNS: &str = "r.id,r.cell_id,r.base_form_id,s.model_path,r.pos_x,r.pos_y,r.pos_z,\
     r.rot_x,r.rot_y,r.rot_z,r.scale,\
     COALESCE(s.bounds_min_x,-64),COALESCE(s.bounds_min_y,-64),COALESCE(s.bounds_min_z,-64),\
     COALESCE(s.bounds_max_x,64),COALESCE(s.bounds_max_y,64),COALESCE(s.bounds_max_z,64),\
     COALESCE(s.bounds_valid,0)";

const REFERENCE_JOIN: &str = " LEFT JOIN statics s ON s.id=r.base_form_id";

/// The `door_links` row of the reference, in the order [`map_reference`] reads them.
const DOOR_COLUMNS: &str = "d.destination_ref_id,d.destination_cell_id,d.destination_worldspace_id,\
     d.pos_x,d.pos_y,d.pos_z,d.rot_x,d.rot_y,d.rot_z";

/// Stand-in for [`DOOR_COLUMNS`] in a database without the `door_links` table: every reference
/// reads as a non-door, with the column order unchanged.
const ABSENT_DOOR_COLUMNS: &str = "NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL";

const DOOR_JOIN: &str = " LEFT JOIN door_links d ON d.ref_id=r.id";

/// The `lights` row of the reference's base record, in the order [`map_reference`] reads them.
const LIGHT_COLUMNS: &str = "l.radius,l.color_r,l.color_g,l.color_b,l.flags";

/// Stand-in for [`LIGHT_COLUMNS`] in a database that predates the `lights` table: every reference
/// reads as unlit, with the column order unchanged.
const ABSENT_LIGHT_COLUMNS: &str = "NULL,NULL,NULL,NULL,NULL";

/// The reference's signed XRDS radius offset, added to the base radius. The column arrived with
/// the `lights` table in conversion schema 4; a database from before it has no such column.
const RADIUS_OVERRIDE_COLUMN: &str = "r.radius_override";
const ABSENT_RADIUS_OVERRIDE_COLUMN: &str = "NULL";

// PR #128 owns door decoding at 25..=33 and emittance at 34..=36. Enable state follows that
// reserved suffix so combining the optional door/emittance projections with enable state cannot
// reinterpret either feature.
const REFERENCE_ENABLE_START: usize = 37;
/// `ACHR` record header flag: the actor is placed as a corpse.
const ACTOR_STARTS_DEAD: u32 = 0x200;

/// The `lights` row of the reference's base record: one `LIGH` record can be placed many times,
/// each reference lighting the space at its own radius.
const LIGHT_JOIN: &str = " LEFT JOIN lights l ON l.id=r.base_form_id";

/// The day colour of the reference's `XEMI` source (`docs/specs/converters/db-schema.md`). The
/// `emittance` table and `references.emittance_id` are optional: an older database has neither.
const EMITTANCE_COLUMNS: &str = "e.day_r,e.day_g,e.day_b";
const ABSENT_EMITTANCE_COLUMNS: &str = "NULL,NULL,NULL";
const EMITTANCE_JOIN: &str = " LEFT JOIN emittance e ON e.id=r.emittance_id";

/// The base record's header flags, which [`map_reference`] reads for [`MARKER_RECORD_FLAG`].
/// `NULL` for a reference whose base has no `statics` row.
const STATIC_FLAGS_COLUMN: &str = "s.flags";
/// Appended after the three enable-state columns so it never shifts their positions.
const STATIC_FLAGS_INDEX: usize = REFERENCE_ENABLE_START + 3;

/// The record header flag Skyrim sets on editor-only base records (`0x00800000`, "Is Marker").
const MARKER_RECORD_FLAG: u32 = 0x0080_0000;

fn has_records(connection: &Connection) -> Result<bool> {
    let count: i64 = connection
        .prepare_cached("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='records'")?
        .query_row([], |row| row.get(0))?;
    Ok(count > 0)
}

/// Whether the database carries the `lights` table. A database converted before lights were
/// exported still loads; every reference then reads as unlit.
fn has_lights(connection: &Connection) -> Result<bool> {
    let count: i64 = connection
        .prepare_cached("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='lights'")?
        .query_row([], |row| row.get(0))?;
    Ok(count > 0)
}

/// Every `door_links` column [`DOOR_COLUMNS`] and [`DOOR_JOIN`] read.
const DOOR_LINK_COLUMN_NAMES: [&str; 10] = [
    "ref_id",
    "destination_ref_id",
    "destination_cell_id",
    "destination_worldspace_id",
    "pos_x",
    "pos_y",
    "pos_z",
    "rot_x",
    "rot_y",
    "rot_z",
];

/// Whether the database carries a `door_links` table with every column the reference query reads.
/// It is optional: a database converted before doors were exported still loads, and no reference
/// in it is a load door. A table of another shape is treated the same way (and logged) rather than
/// failing every cell's query, like [`has_radius_override`] checks its column.
pub(crate) fn has_door_links(connection: &Connection) -> Result<bool> {
    let mut statement =
        connection.prepare_cached("SELECT name FROM pragma_table_info('door_links')")?;
    let columns: BTreeSet<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if columns.is_empty() {
        return Ok(false);
    }
    let missing: Vec<&str> = DOOR_LINK_COLUMN_NAMES
        .into_iter()
        .filter(|name| !columns.contains(*name))
        .collect();
    if !missing.is_empty() {
        bevy::log::warn!(
            ?missing,
            "world database door_links table lacks columns; no reference is a load door"
        );
        return Ok(false);
    }
    Ok(true)
}

/// Whether `"references"` carries the `XRDS` light radius override. It arrived with the `lights`
/// table, but the two are detected separately: the override is a reference column, and a database
/// with the table and without the column must still load.
fn has_radius_override(connection: &Connection) -> Result<bool> {
    let count: i64 = connection
        .prepare_cached(
            "SELECT COUNT(*) FROM pragma_table_info('references') WHERE name='radius_override'",
        )?
        .query_row([], |row| row.get(0))?;
    Ok(count > 0)
}

/// Whether the database carries both `references.emittance_id` and the `emittance` table it names.
fn has_emittance(connection: &Connection) -> Result<bool> {
    let count: i64 = connection
        .prepare_cached(
            "SELECT (SELECT COUNT(*) FROM pragma_table_info('references') WHERE name='emittance_id')
                  * (SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='emittance')",
        )?
        .query_row([], |row| row.get(0))?;
    Ok(count > 0)
}

/// The reference query's column list and joins for one database: optional `records`, `lights`,
/// `door_links`, and `emittance` data read as `NULL` when absent, keeping [`map_reference`]'s
/// column indices stable across database versions. Enable-state metadata is optional for legacy
/// packages; the resolver survives cell unloads on this worker.
struct ReferenceQuery {
    columns: String,
    joins: String,
    parent_sql: String,
    has_named_room_targets: bool,
    enable_states: EnableStateResolver,
    enable_errors: HashSet<EnableStateError>,
}

impl ReferenceQuery {
    fn exterior_sql(&self) -> String {
        let columns = &self.columns;
        let joins = &self.joins;
        format!(
            "SELECT {columns} FROM exterior_spatial x JOIN \"references\" r ON r.id=x.id{joins} \
             WHERE x.worldspace_id=?1 AND x.minX>=?2 AND x.minX<?3 AND x.minY>=?4 AND x.minY<?5"
        )
    }

    fn interior_sql(&self) -> String {
        let columns = &self.columns;
        let joins = &self.joins;
        format!("SELECT {columns} FROM \"references\" r{joins} WHERE r.cell_id=?1")
    }

    fn for_connection(connection: &Connection, features: WorldDatabaseFeatures) -> Result<Self> {
        let has_records = has_records(connection)?;
        let record_column = if has_records { "b.record_type" } else { "NULL" };
        let has_lights = has_lights(connection)?;
        let light_columns = if has_lights {
            LIGHT_COLUMNS
        } else {
            ABSENT_LIGHT_COLUMNS
        };
        let override_column = if has_radius_override(connection)? {
            RADIUS_OVERRIDE_COLUMN
        } else {
            ABSENT_RADIUS_OVERRIDE_COLUMN
        };
        let has_door_links =
            features.door_links != DoorLinksAccess::Disabled && has_door_links(connection)?;
        let door_columns = if has_door_links {
            DOOR_COLUMNS
        } else {
            bevy::log::info!("world database has no door_links table; no reference is a load door");
            ABSENT_DOOR_COLUMNS
        };
        let has_emittance = has_emittance(connection)?;
        let emittance_columns = if has_emittance {
            EMITTANCE_COLUMNS
        } else {
            ABSENT_EMITTANCE_COLUMNS
        };
        let mut joins = String::from(REFERENCE_JOIN);
        if has_records {
            joins.push_str(" LEFT JOIN records b ON b.form_id=r.base_form_id");
        }
        if has_lights {
            joins.push_str(LIGHT_JOIN);
        }
        if has_door_links {
            joins.push_str(DOOR_JOIN);
        }
        if has_emittance {
            joins.push_str(EMITTANCE_JOIN);
        }
        let enable_column_count: usize = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('references') \
             WHERE name IN ('header_flags','enable_parent_id','enable_parent_flags')",
            [],
            |row| row.get(0),
        )?;
        let enable_columns = match enable_column_count {
            0 => {
                bevy::log::warn!(
                    "world database has no reference enable-state metadata; references default to enabled; reconvert assets to filter initially disabled references"
                );
                "0,NULL,0"
            }
            3 => "header_flags,enable_parent_id,enable_parent_flags",
            _ => color_eyre::eyre::bail!(
                "incomplete reference enable-state columns: expected header_flags, enable_parent_id and enable_parent_flags"
            ),
        };
        let reference_enable_columns = match enable_column_count {
            0 => "0,NULL,0",
            _ => "r.header_flags,r.enable_parent_id,r.enable_parent_flags",
        };
        let optional_light_column = |name: &str| -> Result<String> {
            let present: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('lights') WHERE name=?1)",
                [name],
                |row| row.get(0),
            )?;
            Ok(if present {
                format!("l.{name}")
            } else {
                "NULL".into()
            })
        };
        let has_radius_status: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('references') WHERE name='radius_status')",
            [], |row| row.get(0),
        )?;
        let radius_status_column = if has_radius_status {
            "r.radius_status"
        } else {
            "NULL"
        };
        let fade_column = optional_light_column("fade")?;
        let falloff_column = optional_light_column("falloff")?;
        let fade_status_column = optional_light_column("fade_status")?;
        let light_data_count: usize = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('reference_light_data') WHERE name IN \
             ('reference_id','data_size','fov_offset','fade_offset','end_distance_cap','shadow_depth_bias')",
            [], |row| row.get(0),
        )?;
        let reference_light_columns = if light_data_count == 6 {
            joins.push_str(" LEFT JOIN reference_light_data rl ON rl.reference_id=r.id");
            "rl.data_size,rl.fov_offset,rl.fade_offset,rl.end_distance_cap,rl.shadow_depth_bias"
        } else {
            "NULL,NULL,NULL,NULL,NULL"
        };
        let has_assignment_columns: bool = connection.query_row(
            "SELECT (SELECT count(*) FROM pragma_table_info('room_graph_stats')
                         WHERE name IN ('name','value'))=2
                AND (SELECT count(*) FROM pragma_table_info('room_reference_rules')
                         WHERE name IN ('reference_id','cell_id','assignment','target_ref'))=4",
            [],
            |row| row.get(0),
        )?;
        let has_named_room_targets = has_assignment_columns
            && connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='visibility_version' AND value=2)
                    AND EXISTS(SELECT 1 FROM room_graph_stats WHERE name='assignment_version' AND value=1)",
                [],
                |row| row.get(0),
            )?;
        Ok(Self {
            columns: format!(
                "{REFERENCE_COLUMNS},{record_column},{light_columns},{override_column},\
                 {door_columns},{emittance_columns},{reference_enable_columns},\
                 {STATIC_FLAGS_COLUMN},{fade_column},{falloff_column},{reference_light_columns},{fade_status_column},{radius_status_column}"
            ),
            joins,
            parent_sql: format!("SELECT {enable_columns} FROM \"references\" WHERE id=?1"),
            has_named_room_targets,
            enable_states: EnableStateResolver::default(),
            enable_errors: HashSet::new(),
        })
    }
}

fn load_cell(
    connection: &Connection,
    query: &mut ReferenceQuery,
    generation: u64,
    key: CellKey,
) -> Result<CellPayload> {
    let cell_id: u32 = match key {
        CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        } => connection.query_row(
            EXTERIOR_CELL_ID_SQL,
            params![worldspace_id, grid_x, grid_y],
            |row| row.get(0),
        )?,
        CellKey::Interior(cell_id) => cell_id,
    };
    let rows = match key {
        CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        } => {
            let sql = query.exterior_sql();
            let min_x = grid_x as f32 * 4096.0;
            let min_y = grid_y as f32 * 4096.0;
            connection
                .prepare_cached(&sql)?
                .query_map(
                    params![worldspace_id, min_x, min_x + 4096.0, min_y, min_y + 4096.0],
                    map_reference,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        }
        CellKey::Interior(_) => {
            let sql = query.interior_sql();
            connection
                .prepare_cached(&sql)?
                .query_map([cell_id], map_reference)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        }
    };
    let named_room_targets = if matches!(key, CellKey::Interior(_)) && query.has_named_room_targets
    {
        connection
            .prepare_cached(
                "SELECT r.id FROM room_reference_rules p
                 JOIN \"references\" r ON r.id=p.reference_id AND r.cell_id=p.cell_id
                 WHERE p.cell_id=?1 AND p.assignment='target' AND p.target_ref IS NOT NULL",
            )?
            .query_map([cell_id], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<u32>>>()?
    } else {
        HashSet::new()
    };
    let mut references = Vec::with_capacity(rows.len());
    for (mut reference, inputs) in rows {
        reference.named_room_target = named_room_targets.contains(&reference.form_id);
        let state = query
            .enable_states
            .resolve(reference.form_id, inputs, |parent| {
                connection
                    .prepare_cached(&query.parent_sql)?
                    .query_row([parent], |row| map_enable_inputs(row, 0))
                    .optional()
            })
            .wrap_err_with(|| format!("enable state of reference {:08X}", reference.form_id))?;
        reference.initially_enabled = match state {
            Ok(enabled) => enabled,
            Err(error) => {
                if query.enable_errors.insert(error) {
                    bevy::log::warn!(
                        "reference {:08X}: {error}; unresolved references will not spawn",
                        reference.form_id
                    );
                }
                false
            }
        };
        references.push(reference);
    }
    Ok(CellPayload {
        generation,
        key,
        cell_id,
        references,
    })
}

fn map_enable_inputs(
    row: &rusqlite::Row<'_>,
    start: usize,
) -> rusqlite::Result<InitialEnableInputs> {
    let parent_id: Option<u32> = row.get(start + 1)?;
    let parent_flags: Option<u32> = row.get(start + 2)?;
    if parent_id.is_some_and(|id| id != 0) && parent_flags.is_none() {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            start + 2,
            rusqlite::types::Type::Null,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "enable parent has no XESP flags",
            )),
        ));
    }
    Ok(InitialEnableInputs {
        header_flags: row.get(start)?,
        parent_id,
        parent_flags: parent_flags.unwrap_or(0),
    })
}

fn map_reference(row: &rusqlite::Row<'_>) -> rusqlite::Result<(ReferenceRow, InitialEnableInputs)> {
    let base_record_type: Option<String> = row.get(18)?;
    let radius: Option<f32> = row.get(19)?;
    let light = match radius {
        Some(radius) => Some(LightRow {
            radius,
            color: [row.get(20)?, row.get(21)?, row.get(22)?],
            flags: row.get(23)?,
            radius_status: row.get(STATIC_FLAGS_INDEX + 9)?,
            fade: row.get(STATIC_FLAGS_INDEX + 1)?,
            fade_status: row.get(STATIC_FLAGS_INDEX + 8)?,
            falloff: row.get(STATIC_FLAGS_INDEX + 2)?,
            reference_data: row
                .get::<_, Option<u32>>(STATIC_FLAGS_INDEX + 3)?
                .map(|data_size| {
                    Ok::<_, rusqlite::Error>(ReferenceLightRow {
                        data_size,
                        fov_offset: row.get(STATIC_FLAGS_INDEX + 4)?,
                        fade_offset: row.get(STATIC_FLAGS_INDEX + 5)?,
                        end_distance_cap: row.get(STATIC_FLAGS_INDEX + 6)?,
                        shadow_depth_bias: row.get(STATIC_FLAGS_INDEX + 7)?,
                    })
                })
                .transpose()?,
        }),
        None => None,
    };
    let destination_ref_id: Option<u32> = row.get(25)?;
    let door = match destination_ref_id {
        Some(destination_ref_id) => {
            // A row whose arrival point is NULL (a link the converter could not finish) is no
            // link; it must not fail the cell.
            let mut arrival = [None::<f32>; 6];
            for (slot, column) in arrival.iter_mut().zip(28..34) {
                *slot = row.get(column)?;
            }
            if arrival.iter().all(Option::is_some) {
                let value = |index: usize| arrival[index].unwrap_or_default();
                Some(DoorLinkRow {
                    destination_ref_id,
                    destination_cell_id: row.get(26)?,
                    destination_worldspace_id: row.get(27)?,
                    arrival_position: [value(0), value(1), value(2)],
                    arrival_rotation: [value(3), value(4), value(5)],
                })
            } else {
                bevy::log::debug!(
                    reference = format_args!("{:08X}", row.get::<_, u32>(0)?),
                    "door_links row has a NULL arrival position or rotation; the reference is no load door"
                );
                None
            }
        }
        None => None,
    };
    let emittance = match row.get::<_, Option<u8>>(34)? {
        Some(red) => Some([red, row.get(35)?, row.get(36)?]),
        None => None,
    };
    let base_flags: Option<u32> = row.get(STATIC_FLAGS_INDEX)?;
    let enable_inputs = map_enable_inputs(row, REFERENCE_ENABLE_START)?;
    Ok((
        ReferenceRow {
            form_id: row.get(0)?,
            initially_enabled: true,
            has_multibound_override: enable_inputs.header_flags
                & shared::records::MULTIBOUND_REFERENCE_FLAG
                != 0,
            named_room_target: false,
            starts_dead: enable_inputs.header_flags & ACTOR_STARTS_DEAD != 0,
            cell_id: row.get(1)?,
            base_form_id: row.get(2)?,
            base_record_type,
            base_is_marker: base_flags.is_some_and(|flags| flags & MARKER_RECORD_FLAG != 0),
            model_path: row.get(3)?,
            position: [row.get(4)?, row.get(5)?, row.get(6)?],
            rotation: [row.get(7)?, row.get(8)?, row.get(9)?],
            scale: row.get(10)?,
            bounds_min: [row.get(11)?, row.get(12)?, row.get(13)?],
            bounds_max: [row.get(14)?, row.get(15)?, row.get(16)?],
            bounds_valid: row.get(17)?,
            light,
            light_radius_override: row.get(24)?,
            door,
            emittance,
        },
        enable_inputs,
    ))
}

/// Archived WATR records for tests, shaped as the converter writes `waters.data`.
#[cfg(test)]
pub(crate) mod water_fixtures {
    use super::{
        WATER_DNAM_AMPLITUDE, WATER_DNAM_DEPTH_PROPERTIES, WATER_DNAM_FOG_AMOUNT,
        WATER_DNAM_FOG_FAR, WATER_DNAM_FOG_NEAR, WATER_DNAM_NOISE_FALLOFF,
        WATER_DNAM_REFLECTION_MAGNITUDE, WATER_DNAM_REFRACTION_MAGNITUDE,
        WATER_DNAM_SUN_SPARKLE_MAGNITUDE, WATER_DNAM_SUN_SPARKLE_POWER,
        WATER_DNAM_SUN_SPECULAR_MAGNITUDE, WATER_DNAM_SUN_SPECULAR_POWER, WATER_DNAM_UV_SCALE,
        WATER_DNAM_WIND_DIRECTION, WATER_DNAM_WIND_SPEED, WaterFog,
    };

    /// A WATR record's archived subrecords, with a `DNAM` of `len` bytes holding the given fog at
    /// the documented offsets, as the converter writes it into `waters.data`.
    pub(crate) fn water_record_with_fog(len: usize, fog: WaterFog) -> Vec<u8> {
        let mut dnam = vec![0u8; len];
        for (offset, value) in [
            (WATER_DNAM_FOG_NEAR, fog.near),
            (WATER_DNAM_FOG_FAR, fog.far),
            (WATER_DNAM_FOG_AMOUNT, fog.amount),
        ] {
            if let Some(slot) = dnam.get_mut(offset..offset + 4) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
        }
        water_record(vec![(*b"EDID", b"TestWater\0".to_vec()), (*b"DNAM", dnam)])
    }

    /// What a retail water record puts in the `DNAM` of a 228-byte water: the three noise layers'
    /// UV scales and amplitudes, the depth properties and the Reflection Magnitude.
    pub(crate) struct NoiseFixture {
        pub uv_scale: [f32; 3],
        pub amplitude: [f32; 3],
        pub depth_properties: [f32; 4],
        pub reflection_magnitude: f32,
        pub wind_direction: [f32; 3],
        pub wind_speed: [f32; 3],
        pub noise_falloff: f32,
        pub sun: [f32; 4],
        pub refraction_magnitude: f32,
        pub fnam: u8,
        pub textures: [&'static str; 3],
    }

    pub(crate) fn water_record_with_noise(len: usize, noise: &NoiseFixture) -> Vec<u8> {
        let mut dnam = vec![0u8; len];
        let mut put = |offset: usize, value: f32| {
            if let Some(slot) = dnam.get_mut(offset..offset + 4) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
        };
        for index in 0..3 {
            put(WATER_DNAM_UV_SCALE + 4 * index, noise.uv_scale[index]);
            put(WATER_DNAM_AMPLITUDE + 4 * index, noise.amplitude[index]);
        }
        for index in 0..4 {
            put(
                WATER_DNAM_DEPTH_PROPERTIES + 4 * index,
                noise.depth_properties[index],
            );
        }
        put(WATER_DNAM_REFLECTION_MAGNITUDE, noise.reflection_magnitude);
        put(WATER_DNAM_NOISE_FALLOFF, noise.noise_falloff);
        put(WATER_DNAM_REFRACTION_MAGNITUDE, noise.refraction_magnitude);
        for (offset, value) in [
            (WATER_DNAM_SUN_SPECULAR_POWER, noise.sun[0]),
            (WATER_DNAM_SUN_SPECULAR_MAGNITUDE, noise.sun[1]),
            (WATER_DNAM_SUN_SPARKLE_POWER, noise.sun[2]),
            (WATER_DNAM_SUN_SPARKLE_MAGNITUDE, noise.sun[3]),
        ] {
            put(offset, value);
        }
        for index in 0..3 {
            put(
                WATER_DNAM_WIND_DIRECTION + 4 * index,
                noise.wind_direction[index],
            );
            put(WATER_DNAM_WIND_SPEED + 4 * index, noise.wind_speed[index]);
        }
        let mut subrecords = vec![
            (*b"EDID", b"TestWater\0".to_vec()),
            (*b"FNAM", vec![noise.fnam]),
            (*b"DNAM", dnam),
        ];
        for (tag, path) in [b"NAM2", b"NAM3", b"NAM4"].into_iter().zip(noise.textures) {
            let mut bytes = path.as_bytes().to_vec();
            bytes.push(0);
            subrecords.push((*tag, bytes));
        }
        water_record(subrecords)
    }

    pub(super) fn water_record(subrecords: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
        let record = shared::records::ArchivedRecordData {
            subrecords: subrecords
                .into_iter()
                .map(|(tag, data)| shared::records::ArchivedSubRecord { tag, data })
                .collect(),
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&record)
            .unwrap()
            .to_vec()
    }
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod inventory_tests;

#[cfg(test)]
mod tests {
    use super::water_fixtures::{
        NoiseFixture, water_record, water_record_with_fog, water_record_with_noise,
    };
    use super::*;

    #[test]
    fn lod_query_classification_preserves_transient_sqlite_causes_through_context() {
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_IOERR,
        ] {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            let wrapped = color_eyre::Report::from(error).wrap_err("query context");
            assert!(LodQueryFailure::from_error(wrapped).transient);
        }
        assert!(
            !LodQueryFailure::from_error(color_eyre::eyre::eyre!("invalid LOD path")).transient
        );
        assert!(!LodQueryFailure::from_error(rusqlite::Error::InvalidQuery.into()).transient);
    }

    /// `load_cell` with the reference query built for `connection` as it is now, the way the
    /// worker builds it when it opens a database.
    fn load_cell(connection: &Connection, generation: u64, key: CellKey) -> Result<CellPayload> {
        super::load_cell(
            connection,
            &mut ReferenceQuery::for_connection(connection, WorldDatabaseFeatures::default())?,
            generation,
            key,
        )
    }

    fn fixture(connection: &Connection) {
        connection
            .execute_batch(
                r#"CREATE TABLE schema_info(version INTEGER NOT NULL);
                INSERT INTO schema_info VALUES(6);
                CREATE TABLE cells(id INTEGER PRIMARY KEY,worldspace_id INTEGER,grid_x INTEGER,grid_y INTEGER);
                CREATE TABLE land(cell_id INTEGER PRIMARY KEY);
                CREATE TABLE statics(id INTEGER PRIMARY KEY,model_path TEXT,bounds_min_x REAL,bounds_min_y REAL,bounds_min_z REAL,bounds_max_x REAL,bounds_max_y REAL,bounds_max_z REAL,bounds_valid INTEGER NOT NULL,flags INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE "references"(id INTEGER PRIMARY KEY,cell_id INTEGER,base_form_id INTEGER,pos_x REAL,pos_y REAL,pos_z REAL,rot_x REAL,rot_y REAL,rot_z REAL,scale REAL);
                CREATE VIRTUAL TABLE exterior_spatial USING rtree(id,minX,maxX,minY,maxY,minZ,maxZ,+cell_id,+worldspace_id);
                INSERT INTO cells VALUES(10,60,2,-3);
                INSERT INTO statics VALUES(20,'architecture/wall.nif',-1,-2,-3,1,2,3,1,0);
                INSERT INTO "references" VALUES(30,10,20,8200,-12200,50,0,0,0,1);
                INSERT INTO exterior_spatial VALUES(30,8200,8200,-12200,-12200,50,50,10,60);
                INSERT INTO "references" VALUES(31,99,20,8250,-12150,55,0,0,0,1);
                INSERT INTO exterior_spatial VALUES(31,8250,8250,-12150,-12150,55,55,99,60);"#,
            )
            .unwrap();
    }

    fn enable_columns(connection: &Connection) {
        connection
            .execute_batch(
                "ALTER TABLE \"references\" ADD COLUMN header_flags INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE \"references\" ADD COLUMN enable_parent_id INTEGER;
             ALTER TABLE \"references\" ADD COLUMN enable_parent_flags INTEGER;",
            )
            .unwrap();
    }

    #[test]
    fn disabled_legacy_doors_preserve_query_layout_without_reading_core_doors() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        add_door_links(&connection);
        let core =
            ReferenceQuery::for_connection(&connection, WorldDatabaseFeatures::default()).unwrap();
        assert!(core.joins.contains("door_links"));
        let mut denied = ReferenceQuery::for_connection(
            &connection,
            WorldDatabaseFeatures {
                door_links: DoorLinksAccess::Disabled,
            },
        )
        .unwrap();
        assert!(!denied.joins.contains("door_links"));
        assert!(denied.columns.contains(ABSENT_DOOR_COLUMNS));
        let sql = format!(
            "SELECT {} FROM \"references\" r{} WHERE r.id=30",
            denied.columns, denied.joins
        );
        connection
            .query_row(&sql, [], |row| {
                for index in 25..34 {
                    assert_eq!(row.get::<_, Option<i64>>(index)?, None);
                }
                Ok(())
            })
            .unwrap();
        let payload = super::load_cell(&connection, &mut denied, 1, CellKey::Interior(99)).unwrap();
        assert!(
            payload
                .references
                .iter()
                .all(|reference| reference.door.is_none())
        );
    }

    #[test]
    fn enable_metadata_does_not_decode_reserved_door_slots() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        enable_columns(&connection);
        connection
            .execute_batch(
                "UPDATE \"references\" SET header_flags=2048,enable_parent_id=20,enable_parent_flags=1 WHERE id=30;",
            )
            .unwrap();
        let mut query =
            ReferenceQuery::for_connection(&connection, WorldDatabaseFeatures::default()).unwrap();
        let sql = format!(
            "SELECT {} FROM \"references\" r{} WHERE r.id=30",
            query.columns, query.joins
        );
        connection
            .query_row(&sql, [], |row| {
                for index in 25..34 {
                    assert_eq!(row.get::<_, Option<i64>>(index)?, None);
                }
                for index in 34..37 {
                    assert_eq!(row.get::<_, Option<i64>>(index)?, None);
                }
                Ok(())
            })
            .unwrap();

        // Model #128's nine-column projection with distinct destination and
        // arrival values, without implementing or depending on its door tables.
        // Bounds COALESCE expressions contain commas, so replace the exact
        // suffix instead of treating the SQL text as a CSV list.
        query.columns = query.columns.replace(
            &format!(",{ABSENT_DOOR_COLUMNS},{ABSENT_EMITTANCE_COLUMNS},r.header_flags"),
            ",123,456,NULL,1.25,2.5,3.75,0.1,0.2,0.3,NULL,NULL,NULL,r.header_flags",
        );
        let sql = format!(
            "SELECT {} FROM \"references\" r{} WHERE r.id=30",
            query.columns, query.joins
        );
        connection
            .query_row(&sql, [], |row| {
                assert_eq!(row.get::<_, u32>(25)?, 123);
                assert_eq!(row.get::<_, u32>(26)?, 456);
                assert_eq!(row.get::<_, Option<u32>>(27)?, None);
                assert_eq!(row.get::<_, f32>(28)?, 1.25);
                let (_, inputs) = map_reference(row)?;
                assert_eq!(inputs.header_flags, 2048);
                assert_eq!(inputs.parent_id, Some(20));
                assert_eq!(inputs.parent_flags, 1);
                Ok(())
            })
            .unwrap();
        for key in [
            CellKey::Interior(10),
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        ] {
            let payload = super::load_cell(&connection, &mut query, 1, key).unwrap();
            let child = payload
                .references
                .iter()
                .find(|row| row.form_id == 30)
                .unwrap();
            assert!(!child.initially_enabled);
        }
    }

    #[test]
    fn starts_dead_comes_from_the_reference_header_and_defaults_off() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        let mut legacy =
            ReferenceQuery::for_connection(&connection, WorldDatabaseFeatures::default()).unwrap();
        let payload = super::load_cell(&connection, &mut legacy, 1, CellKey::Interior(10)).unwrap();
        assert!(!payload.references.is_empty());
        assert!(payload.references.iter().all(|row| !row.starts_dead));
        enable_columns(&connection);
        // 0x200 Starts Dead with Persistent (0x400); 0x800 alone is Initially Disabled.
        connection
            .execute_batch(
                "UPDATE \"references\" SET header_flags=1536 WHERE id=30;
                 UPDATE \"references\" SET header_flags=2048 WHERE id<>30;",
            )
            .unwrap();
        let mut query =
            ReferenceQuery::for_connection(&connection, WorldDatabaseFeatures::default()).unwrap();
        let payload = super::load_cell(&connection, &mut query, 1, CellKey::Interior(10)).unwrap();
        for row in &payload.references {
            assert_eq!(row.starts_dead, row.form_id == 30, "{:08X}", row.form_id);
        }
        assert!(payload.references.iter().any(|row| row.form_id == 30));
    }

    #[test]
    fn initial_enable_state_uses_parents_outside_the_loaded_cell_without_models() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        enable_columns(&connection);
        connection.execute_batch(
            "INSERT INTO \"references\"(id,cell_id,base_form_id,header_flags) VALUES(100,500,9999,2048);
             INSERT INTO \"references\"(id,cell_id,base_form_id,header_flags,enable_parent_id,enable_parent_flags)
                 VALUES(200,600,9998,0,100,1);
             UPDATE \"references\" SET header_flags=2048,enable_parent_id=200,enable_parent_flags=952044802 WHERE id=30;
             UPDATE \"references\" SET enable_parent_id=30,enable_parent_flags=1 WHERE id=31;"
        ).unwrap();
        let mut query =
            ReferenceQuery::for_connection(&connection, WorldDatabaseFeatures::default()).unwrap();
        for _ in 0..2 {
            let interior =
                super::load_cell(&connection, &mut query, 1, CellKey::Interior(99)).unwrap();
            assert_eq!(interior.references.len(), 1);
            assert!(!interior.references[0].initially_enabled);
            let exterior = super::load_cell(
                &connection,
                &mut query,
                2,
                CellKey::Exterior {
                    worldspace_id: 60,
                    grid_x: 2,
                    grid_y: -3,
                },
            )
            .unwrap();
            assert_eq!(
                exterior.references.len(),
                2,
                "hidden rows remain in the payload"
            );
            assert!(
                exterior
                    .references
                    .iter()
                    .find(|row| row.form_id == 30)
                    .unwrap()
                    .initially_enabled
            );
            assert!(
                !exterior
                    .references
                    .iter()
                    .find(|row| row.form_id == 31)
                    .unwrap()
                    .initially_enabled
            );
        }
        assert!(query.enable_errors.is_empty());
    }

    #[test]
    fn missing_and_cyclic_enable_parents_remain_hidden_and_are_diagnosed_once() {
        for parent in [99, 31] {
            let connection = Connection::open_in_memory().unwrap();
            fixture(&connection);
            enable_columns(&connection);
            connection.execute(
                "UPDATE \"references\" SET enable_parent_id=?1,enable_parent_flags=1 WHERE id=30",
                [parent],
            ).unwrap();
            connection.execute_batch(
                "UPDATE \"references\" SET enable_parent_id=30,enable_parent_flags=1 WHERE id=31;"
            ).unwrap();
            let mut query =
                ReferenceQuery::for_connection(&connection, WorldDatabaseFeatures::default())
                    .unwrap();
            for _ in 0..2 {
                let payload = super::load_cell(
                    &connection,
                    &mut query,
                    1,
                    CellKey::Exterior {
                        worldspace_id: 60,
                        grid_x: 2,
                        grid_y: -3,
                    },
                )
                .unwrap();
                assert_eq!(payload.references.len(), 2);
                assert!(payload.references.iter().all(|row| !row.initially_enabled));
                assert_eq!(query.enable_errors.len(), 1);
            }
        }
    }

    #[test]
    /// Pin the shared resolver's literal initial-state rules through real database loading.
    fn shared_initial_enable_rules_preserve_database_loading_states() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        enable_columns(&connection);
        type FixtureRow = (u32, u32, Option<u32>, Option<u32>, bool);
        let rows: &[FixtureRow] = &[
            (100, 2048, None, None, false),
            (101, 0, None, None, true),
            (102, 16384, None, None, true),
            (103, 2048, Some(0), Some(1), false),
            (104, 0, Some(0), None, true),
            (110, 2048, Some(101), Some(0), true),
            (111, 0, Some(100), Some(0), false),
            (112, 0, Some(100), Some(1), true),
            (113, 0, Some(100), Some(952044802), false),
            (114, 2048, Some(112), Some(1), false),
            (115, 0, Some(999), Some(1), false),
            (116, 0, Some(117), Some(1), false),
            (117, 0, Some(116), Some(0), false),
            (118, 0, Some(118), Some(1), false),
            (119, 2048, Some(20), Some(0), true),
            (120, 0, Some(20), Some(1), false),
        ];
        for &(id, flags, parent, parent_flags, _) in rows {
            connection.execute(
                "INSERT INTO \"references\"(id,cell_id,base_form_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,enable_parent_id,enable_parent_flags)
                 VALUES(?1,600,20,0,0,0,0,0,0,1,?2,?3,?4)",
                params![id, flags, parent, parent_flags],
            ).unwrap();
        }
        let mut query =
            ReferenceQuery::for_connection(&connection, WorldDatabaseFeatures::default()).unwrap();
        for _ in 0..2 {
            let payload =
                super::load_cell(&connection, &mut query, 1, CellKey::Interior(600)).unwrap();
            assert_eq!(payload.references.len(), 16);
            for &(id, _, _, _, enabled) in rows {
                let reference = payload
                    .references
                    .iter()
                    .find(|row| row.form_id == id)
                    .unwrap();
                assert_eq!(reference.initially_enabled, enabled, "reference {id}");
            }
            assert_eq!(query.enable_errors.len(), 3);
        }
        assert_eq!(
            query.enable_states.decision_inputs(114),
            Some(vec![
                shared::initial_enable::EnableDecisionInput::Reference {
                    form_id: 114,
                    inputs: InitialEnableInputs {
                        header_flags: 2048,
                        parent_id: Some(112),
                        parent_flags: 1
                    },
                },
                shared::initial_enable::EnableDecisionInput::Reference {
                    form_id: 112,
                    inputs: InitialEnableInputs {
                        header_flags: 0,
                        parent_id: Some(100),
                        parent_flags: 1
                    },
                },
                shared::initial_enable::EnableDecisionInput::Reference {
                    form_id: 100,
                    inputs: InitialEnableInputs {
                        header_flags: 2048,
                        parent_id: None,
                        parent_flags: 0
                    },
                },
            ])
        );
    }

    #[test]
    fn authored_multibound_override_survives_the_existing_database_projection() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        enable_columns(&connection);
        connection
            .execute(
                "UPDATE \"references\" SET header_flags=?1 WHERE id=31",
                [shared::records::MULTIBOUND_REFERENCE_FLAG],
            )
            .unwrap();
        let unrestricted = load_cell(&connection, 1, CellKey::Interior(99)).unwrap();
        assert!(unrestricted.references[0].has_multibound_override);
        let automatic = load_cell(&connection, 1, CellKey::Interior(10)).unwrap();
        assert!(!automatic.references[0].has_multibound_override);
    }

    #[test]
    fn versioned_named_room_targets_are_identified_before_streaming() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        connection.execute_batch(
            "CREATE TABLE room_graph_stats(name TEXT PRIMARY KEY, value INTEGER);
             INSERT INTO room_graph_stats VALUES('visibility_version',2),('assignment_version',1);
             CREATE TABLE room_reference_rules(reference_id INTEGER PRIMARY KEY,cell_id INTEGER,assignment TEXT,target_ref INTEGER);
             INSERT INTO room_reference_rules VALUES(30,10,'target',1),(31,99,'target',1);
             INSERT INTO \"references\" VALUES(32,99,20,0,0,0,0,0,0,1),(33,99,20,0,0,0,0,0,0,1);
             INSERT INTO room_reference_rules VALUES(32,99,'none',NULL),(33,99,'auto',NULL);",
        ).unwrap();
        let interior = load_cell(&connection, 1, CellKey::Interior(99)).unwrap();
        let targets: Vec<_> = interior
            .references
            .iter()
            .filter(|row| row.named_room_target)
            .map(|row| row.form_id)
            .collect();
        assert_eq!(targets, [31]);
        assert!(
            exterior_payload(&connection)
                .references
                .iter()
                .all(|row| !row.named_room_target)
        );

        connection
            .execute(
                "DELETE FROM room_graph_stats WHERE name='assignment_version'",
                [],
            )
            .unwrap();
        let legacy = load_cell(&connection, 1, CellKey::Interior(99)).unwrap();
        assert!(legacy.references.iter().all(|row| !row.named_room_target));
    }

    #[test]
    fn legacy_reference_columns_default_enabled_but_partial_columns_fail() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        let payload = load_cell(&connection, 1, CellKey::Interior(99)).unwrap();
        assert!(payload.references.iter().all(|row| row.initially_enabled));
        assert!(
            payload
                .references
                .iter()
                .all(|row| !row.has_multibound_override)
        );
        connection
            .execute_batch(
                "ALTER TABLE \"references\" ADD COLUMN header_flags INTEGER NOT NULL DEFAULT 0;",
            )
            .unwrap();
        let error = load_cell(&connection, 1, CellKey::Interior(99)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incomplete reference enable-state columns")
        );
    }

    #[test]
    fn corrupt_parent_flags_fail_the_cell_instead_of_inventing_enabled_state() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        enable_columns(&connection);
        connection.execute_batch(
            "INSERT INTO \"references\"(id,cell_id,base_form_id,header_flags) VALUES(100,500,9999,-1);
             UPDATE \"references\" SET enable_parent_id=100,enable_parent_flags=1 WHERE id=31;"
        ).unwrap();
        let error = load_cell(&connection, 1, CellKey::Interior(99)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("enable state of reference 0000001F")
        );
    }

    #[test]
    fn nonzero_enable_parent_requires_flags_in_the_database() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        enable_columns(&connection);
        connection
            .execute_batch("UPDATE \"references\" SET enable_parent_id=30 WHERE id=31;")
            .unwrap();
        let error = load_cell(&connection, 1, CellKey::Interior(99)).unwrap_err();
        assert!(format!("{error:#}").contains("enable parent has no XESP flags"));
    }

    fn lod_fixture(connection: &Connection) {
        connection
            .execute_batch(
                "CREATE TABLE worldspaces(id INTEGER PRIMARY KEY,lod_origin_x INTEGER,lod_origin_y INTEGER);
                 CREATE TABLE lod_build(id INTEGER PRIMARY KEY,build_identity TEXT NOT NULL);
                 CREATE TABLE lod_chunks(
                    worldspace_id INTEGER NOT NULL,tier INTEGER NOT NULL,anchor_x INTEGER NOT NULL,anchor_y INTEGER NOT NULL,
                    payload_path TEXT NOT NULL,content_hash TEXT NOT NULL,
                    bounds_min_x REAL NOT NULL,bounds_min_y REAL NOT NULL,bounds_min_z REAL NOT NULL,
                    bounds_max_x REAL NOT NULL,bounds_max_y REAL NOT NULL,bounds_max_z REAL NOT NULL,
                    source_cells TEXT NOT NULL,PRIMARY KEY(worldspace_id,tier,anchor_x,anchor_y));
                 CREATE VIRTUAL TABLE lod_chunks_spatial USING rtree(
                    id,minX,maxX,minY,maxY,+worldspace_id,+tier,+anchor_x,+anchor_y);
                 INSERT INTO worldspaces VALUES(60,-8,12);",
            )
            .unwrap();
        connection
            .execute("INSERT INTO lod_build VALUES(1,?1)", ["a".repeat(64)])
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks VALUES(60,4,-1,2,'lod/0000003c/4/cell_-1_2.glb',?1,\
                 -50000.0,50000.0,0.0,-40000.0,60000.0,100.0,'-12,20;-11,20')",
                ["b".repeat(64)],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks_spatial VALUES(1,-50000.0,-40000.0,50000.0,60000.0,60,4,-1,2)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks VALUES(60,8,-1,2,'lod/0000003c/8/cell_-1_2.glb',?1,\
                 -50000.0,50000.0,0.0,-40000.0,60000.0,100.0,'-12,20')",
                ["c".repeat(64)],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks_spatial VALUES(2,-50000.0,-40000.0,50000.0,60000.0,60,8,-1,2)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks VALUES(61,4,-1,2,'lod/0000003d/4/cell_-1_2.glb',?1,\
                 -50000.0,50000.0,0.0,-40000.0,60000.0,100.0,'-12,20')",
                ["d".repeat(64)],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks_spatial VALUES(3,-50000.0,-40000.0,50000.0,60000.0,61,4,-1,2)",
                [],
            )
            .unwrap();
    }

    fn lod_query(min: [f64; 2], max: [f64; 2]) -> LodChunkQuery {
        LodChunkQuery {
            worldspace_id: 60,
            tier: LodTier::Tier4,
            bounds_min: min,
            bounds_max: max,
        }
    }

    /// Builds a database and conversion manifest with the current combined LOD identities.
    fn lod_build_contract_fixture(path: &Path, chunks: u32, identity: &str) {
        std::fs::write(
            path.parent().unwrap().join("conversion-manifest.json"),
            serde_json::to_vec(
                &serde_json::json!({"schema_version": shared::LOD_CONVERTER_SCHEMA_VERSION}),
            )
            .unwrap(),
        )
        .unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_info(version INTEGER NOT NULL);
                 CREATE TABLE lod_chunks(id INTEGER PRIMARY KEY);
                 CREATE TABLE lod_build(id INTEGER PRIMARY KEY,build_identity TEXT NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO schema_info VALUES(?1)",
                [shared::WORLD_DATABASE_SCHEMA_VERSION],
            )
            .unwrap();
        for id in 0..chunks {
            connection
                .execute("INSERT INTO lod_chunks(id) VALUES(?1)", [id])
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO lod_build(id,build_identity) VALUES(1,?1)",
                [identity],
            )
            .unwrap();
    }

    const OBJECT_PATH: &str = "lod/0000003c/16/objects_0_0.glb";

    /// The additive object LOD table, with one level-16 chunk for worldspace 60 whose file is
    /// the bytes `payload` (its hash is computed from them).
    fn object_chunk_fixture(connection: &Connection, path: &str, payload: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        connection
            .execute_batch(
                "CREATE TABLE lod_object_chunks(
                    worldspace_id INTEGER NOT NULL,tier INTEGER NOT NULL,anchor_x INTEGER NOT NULL,anchor_y INTEGER NOT NULL,
                    payload_path TEXT NOT NULL,content_hash TEXT NOT NULL,
                    bounds_min_x REAL NOT NULL,bounds_min_y REAL NOT NULL,bounds_min_z REAL NOT NULL,
                    bounds_max_x REAL NOT NULL,bounds_max_y REAL NOT NULL,bounds_max_z REAL NOT NULL,
                    source_cells TEXT NOT NULL,PRIMARY KEY(worldspace_id,tier,anchor_x,anchor_y));",
            )
            .unwrap();
        let hash: String = Sha256::digest(payload)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        connection
            .execute(
                "INSERT INTO lod_object_chunks VALUES(60,16,0,0,?1,?2,\
                 0.0,0.0,0.0,65536.0,65536.0,500.0,'-8,12;-7,12')",
                [path, hash.as_str()],
            )
            .unwrap();
        hash
    }

    #[test]
    fn a_pack_without_the_object_lod_table_loads_no_object_chunks() {
        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);
        assert!(load_lod_object_chunks(&connection, 60).unwrap().is_empty());
        // The terrain loader is unaffected by the object table's absence.
        assert_eq!(
            load_lod_chunks(
                &connection,
                lod_query([-50_000.0, 0.0], [50_000.0, 60_000.0])
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn object_chunks_load_under_the_terrain_contract_and_leave_terrain_alone() {
        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);
        let hash = object_chunk_fixture(&connection, OBJECT_PATH, b"glb");
        let chunks = load_lod_object_chunks(&connection, 60).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].key.tier, LodTier::Tier16);
        assert_eq!(chunks[0].payload_path, OBJECT_PATH);
        assert_eq!(chunks[0].content_hash, hash);
        assert_eq!(chunks[0].build_identity, "a".repeat(64));
        assert_eq!(chunks[0].origin, LodOrigin::new(-8, 12));
        // Another worldspace has none, and the terrain rows are the same as without the table.
        assert!(load_lod_object_chunks(&connection, 61).unwrap().is_empty());
        assert_eq!(
            load_lod_chunks(
                &connection,
                lod_query([-50_000.0, 0.0], [50_000.0, 60_000.0])
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn a_damaged_object_row_fails_closed_instead_of_being_skipped() {
        // A terrain-style path, a bad hash and an unsafe path are errors, not skipped rows.
        for (path, hash_sql) in [
            ("lod/0000003c/16/cell_0_0.glb", None),
            (
                OBJECT_PATH,
                Some("UPDATE lod_object_chunks SET content_hash='xyz'"),
            ),
            ("../objects_0_0.glb", None),
        ] {
            let connection = Connection::open_in_memory().unwrap();
            lod_fixture(&connection);
            object_chunk_fixture(&connection, path, b"glb");
            if let Some(sql) = hash_sql {
                connection.execute_batch(sql).unwrap();
            }
            assert!(
                load_lod_object_chunks(&connection, 60).is_err(),
                "{path} {hash_sql:?}"
            );
        }
        // An identity that is not a SHA-256 digest fails the whole load.
        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);
        object_chunk_fixture(&connection, OBJECT_PATH, b"glb");
        connection
            .execute_batch("UPDATE lod_build SET build_identity='bad'")
            .unwrap();
        assert!(load_lod_object_chunks(&connection, 60).is_err());
    }

    #[test]
    fn a_missing_or_damaged_object_payload_fails_closed() {
        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);
        object_chunk_fixture(&connection, OBJECT_PATH, b"glb");
        let chunks = load_lod_object_chunks(&connection, 60).unwrap();
        let assets = tempfile::tempdir().unwrap();

        let missing = verify_lod_object_payloads(assets.path(), &chunks).unwrap_err();
        assert!(
            format!("{missing:#}").contains("failed to read LOD payload"),
            "{missing:#}"
        );

        let file = assets.path().join(OBJECT_PATH);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"damaged").unwrap();
        let damaged = verify_lod_object_payloads(assets.path(), &chunks).unwrap_err();
        assert!(format!("{damaged:#}").contains("expected"), "{damaged:#}");

        std::fs::write(&file, b"glb").unwrap();
        verify_lod_object_payloads(assets.path(), &chunks).unwrap();
    }

    #[test]
    fn legacy_databases_render_without_lod_but_cannot_advertise_lod() {
        for version in [3, 4, 5] {
            let directory = tempfile::tempdir().unwrap();
            let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
            connection.execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER NOT NULL); INSERT INTO schema_info VALUES({version});"
            )).unwrap();
            if version == 5 {
                connection.execute_batch("CREATE TABLE grass_types(id INTEGER PRIMARY KEY); CREATE TABLE landscape_texture_grasses(ltex_id INTEGER,gras_id INTEGER); INSERT INTO grass_types VALUES(1); INSERT INTO landscape_texture_grasses VALUES(2,1);").unwrap();
            }
            assert!(
                load_lod_chunks(&connection, lod_query([0.0, 0.0], [1.0, 1.0]))
                    .unwrap()
                    .is_empty()
            );
            validate_lod_build_contract(directory.path(), 20).unwrap();
            std::fs::write(directory.path().join("lod-manifest.json"), b"{}").unwrap();
            assert!(validate_lod_build_contract(directory.path(), 20).is_err());
        }
    }

    /// Current database schemas require LOD tables even when no LOD manifest is present.
    #[test]
    fn current_database_requires_lod_tables_even_without_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        connection
            .execute_batch("CREATE TABLE schema_info(version INTEGER NOT NULL);")
            .unwrap();
        connection
            .execute(
                "INSERT INTO schema_info VALUES(?1)",
                [shared::WORLD_DATABASE_SCHEMA_VERSION],
            )
            .unwrap();
        assert!(load_lod_chunks(&connection, lod_query([0.0, 0.0], [1.0, 1.0])).is_err());
        assert!(validate_lod_build_contract(directory.path(), 20).is_err());
    }

    #[test]
    fn combined_lod_requires_exact_latest_producer_and_database() {
        let directory = tempfile::tempdir().unwrap();
        let identity = "a".repeat(64);
        let database = directory.path().join("skyrim_world.db");
        lod_build_contract_fixture(&database, 1, &identity);
        for (producer, world, accepted) in [
            (
                shared::LOD_CONVERTER_SCHEMA_VERSION,
                shared::WORLD_DATABASE_SCHEMA_VERSION,
                true,
            ),
            (25, shared::WORLD_DATABASE_SCHEMA_VERSION, false),
            (shared::LOD_CONVERTER_SCHEMA_VERSION, 8, false),
            (25, 8, false),
            (24, shared::WORLD_DATABASE_SCHEMA_VERSION, false),
            (shared::LOD_CONVERTER_SCHEMA_VERSION, 7, false),
            (24, 7, false),
            (24, 6, false),
            (23, 7, false),
            (22, 7, false),
            (21, 7, false),
            (20, 6, false),
            (19, 7, false),
            (16, 7, false),
        ] {
            std::fs::write(
                directory.path().join("conversion-manifest.json"),
                serde_json::to_vec(&serde_json::json!({"schema_version": producer})).unwrap(),
            )
            .unwrap();
            Connection::open(&database)
                .unwrap()
                .execute("UPDATE schema_info SET version=?1", [world])
                .unwrap();
            std::fs::write(
                directory.path().join("lod-manifest.json"),
                serde_json::to_vec(&serde_json::json!({
                    "build_identity": identity, "converter_schema": producer,
                    "world_database_schema": world, "chunks": 1,
                    "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                validate_lod_build_contract(directory.path(), producer).is_ok(),
                accepted,
                "producer={producer} world={world}"
            );
        }
        Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE schema_info SET version=?1",
                [shared::WORLD_DATABASE_SCHEMA_VERSION],
            )
            .unwrap();
        std::fs::write(
            directory.path().join("lod-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "build_identity": identity, "converter_schema": 24,
                "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
                "chunks": 1, "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("conversion-manifest.json"),
            br#"{"schema_version":20}"#,
        )
        .unwrap();
        assert!(validate_lod_build_contract(directory.path(), 21).is_err());
    }

    #[test]
    fn worlds_without_compiled_chunks_do_not_require_lod_origins() {
        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);
        let mut query = lod_query([0.0, 0.0], [1.0, 1.0]);
        query.worldspace_id = 99;
        assert!(load_lod_chunks(&connection, query).unwrap().is_empty());
    }

    #[test]
    fn full_database_queue_rejects_lod_queries_without_blocking() {
        let (requests, _receiver) = bounded(1);
        requests.send(DatabaseRequest::Shutdown).unwrap();
        let error = enqueue_lod_query(&requests, 1, lod_query([0.0, 0.0], [1.0, 1.0]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("queue is full"), "{error}");
    }

    #[test]
    fn full_database_queue_rejects_cell_intake_without_blocking() {
        let (database, requests, _responses) = WorldDatabase::channel_fixture(1);
        let request = || DatabaseRequest::Load {
            generation: 1,
            key: CellKey::Interior(1),
            queued_at: Instant::now(),
        };
        database.try_request(request()).unwrap();
        let error = database.try_request(request()).unwrap_err().to_string();
        assert!(error.contains("queue is full"), "{error}");
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn runtime_lod_contract_rejects_missing_mixed_and_incomplete_manifests() {
        let directory = tempfile::tempdir().unwrap();
        let identity = "a".repeat(64);
        let database = directory.path().join("skyrim_world.db");
        lod_build_contract_fixture(&database, 1, &identity);
        let manifest_path = directory.path().join("lod-manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&serde_json::json!({
                "build_identity": identity,
                "converter_schema": shared::LOD_CONVERTER_SCHEMA_VERSION,
                "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
                "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
                "chunks": 1,
            }))
            .unwrap(),
        )
        .unwrap();
        validate_lod_build_contract(directory.path(), shared::LOD_CONVERTER_SCHEMA_VERSION)
            .unwrap();
        let current: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let mut stale = current.clone();
        stale["land_texture_repeats_per_cell"] = serde_json::json!(8.0);
        std::fs::write(&manifest_path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(
            validate_lod_build_contract(directory.path(), shared::LOD_CONVERTER_SCHEMA_VERSION)
                .unwrap_err()
                .to_string()
                .contains("texture scale is stale")
        );
        std::fs::write(&manifest_path, serde_json::to_vec(&current).unwrap()).unwrap();

        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE lod_build SET build_identity=?1 WHERE id=1",
                ["b".repeat(64)],
            )
            .unwrap();
        drop(connection);
        let error =
            validate_lod_build_contract(directory.path(), shared::LOD_CONVERTER_SCHEMA_VERSION)
                .unwrap_err()
                .to_string();
        assert!(error.contains("identities do not match"), "{error}");

        std::fs::remove_file(&manifest_path).unwrap();
        let error =
            validate_lod_build_contract(directory.path(), shared::LOD_CONVERTER_SCHEMA_VERSION)
                .unwrap_err()
                .to_string();
        assert!(error.contains("lod-manifest.json is missing"), "{error}");
    }

    #[test]
    fn runtime_lod_contract_allows_a_world_with_no_compiled_chunks() {
        let directory = tempfile::tempdir().unwrap();
        lod_build_contract_fixture(
            &directory.path().join("skyrim_world.db"),
            0,
            &"a".repeat(64),
        );
        validate_lod_build_contract(directory.path(), 16).unwrap();
    }

    #[test]
    fn loads_exterior_cell_through_spatial_index() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        let payload = load_cell(
            &connection,
            9,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap();
        assert_eq!(payload.generation, 9);
        assert_eq!(payload.cell_id, 10);
        assert_eq!(payload.references.len(), 2);
        assert!(
            payload
                .references
                .iter()
                .any(|reference| reference.cell_id == 99)
        );
        assert_eq!(
            payload.references[0].model_path.as_deref(),
            Some("architecture/wall.nif")
        );
        assert_eq!(payload.references[0].bounds_max, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn legacy_world6_lod_metadata_remains_queryable() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE schema_info(version INTEGER NOT NULL); INSERT INTO schema_info VALUES(6);").unwrap();
        lod_fixture(&connection);
        let chunks = load_lod_chunks(
            &connection,
            lod_query([-46000.0, 54000.0], [-45000.0, 55000.0]),
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].origin, LodOrigin::new(-8, 12));
        assert_eq!(chunks[0].source_cells, vec![[-12, 20], [-11, 20]]);
    }

    #[test]
    fn lod_query_uses_world_space_overlap_and_returns_validated_metadata() {
        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);

        let chunks = load_lod_chunks(
            &connection,
            lod_query([-46000.0, 54000.0], [-45000.0, 55000.0]),
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        let chunk = &chunks[0];
        assert_eq!(
            chunk.key,
            ChunkKey::new(60, LodTier::Tier4, ChunkAnchor::new(-1, 2))
        );
        assert_eq!(chunk.payload_path, "lod/0000003c/4/cell_-1_2.glb");
        assert_eq!(chunk.content_hash, "b".repeat(64));
        assert_eq!(chunk.build_identity, "a".repeat(64));
        assert_eq!(chunk.origin, LodOrigin::new(-8, 12));
        assert_eq!(chunk.bounds.min, [-50000.0, 50000.0, 0.0]);
        assert_eq!(chunk.bounds.max, [-40000.0, 60000.0, 100.0]);
        assert_eq!(chunk.source_cells, vec![[-12, 20], [-11, 20]]);

        let misses = load_lod_chunks(&connection, lod_query([0.0, 0.0], [10.0, 10.0])).unwrap();
        assert!(misses.is_empty());
    }

    #[test]
    fn lod_query_rejects_malformed_tier_path_bounds_and_query_aabb() {
        assert!(lod_tier_from_database(3).is_err());

        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);
        connection
            .execute(
                "UPDATE lod_chunks SET payload_path='../outside.glb' WHERE worldspace_id=60",
                [],
            )
            .unwrap();
        let error = load_lod_chunks(
            &connection,
            lod_query([-46000.0, 54000.0], [-45000.0, 55000.0]),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("unsafe or non-canonical payload path"),
            "{error}"
        );

        connection
            .execute(
                "UPDATE lod_chunks SET payload_path='lod/0000003c/4/cell_-1_2.glb',bounds_max_x=-60000.0 \
                 WHERE worldspace_id=60",
                [],
            )
            .unwrap();
        let error = load_lod_chunks(
            &connection,
            lod_query([-46000.0, 54000.0], [-45000.0, 55000.0]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unordered world-space bounds"), "{error}");

        let error = load_lod_chunks(&connection, lod_query([f64::NAN, 0.0], [1.0, 1.0]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("non-finite or unordered X bounds"),
            "{error}"
        );
    }

    #[test]
    fn lod_query_rejects_missing_origin_and_invalid_source_cells() {
        let connection = Connection::open_in_memory().unwrap();
        lod_fixture(&connection);
        connection
            .execute("UPDATE worldspaces SET lod_origin_x=NULL WHERE id=60", [])
            .unwrap();
        let error = load_lod_chunks(
            &connection,
            lod_query([-46000.0, 54000.0], [-45000.0, 55000.0]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no valid X origin"), "{error}");

        connection
            .execute("UPDATE worldspaces SET lod_origin_x=-8 WHERE id=60", [])
            .unwrap();
        connection
            .execute(
                "UPDATE lod_chunks SET source_cells='not-a-cell' WHERE worldspace_id=60",
                [],
            )
            .unwrap();
        let error = load_lod_chunks(
            &connection,
            lod_query([-46000.0, 54000.0], [-45000.0, 55000.0]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("invalid source_cells metadata"), "{error}");
    }

    #[test]
    fn rejects_traversal_and_rooted_texture_paths() {
        assert_eq!(
            converted_texture_path("textures/../../secrets.dds".to_owned()),
            None
        );
        assert_eq!(
            converted_texture_path("textures//etc/passwd".to_owned()),
            None
        );
        assert_eq!(
            converted_texture_path(r"textures\..\..\secrets.dds".to_owned()),
            None
        );
        // Windows treats a drive-prefixed path as absolute (and `PathBuf::join`
        // would let it replace the base path entirely); Rust's path parsing is
        // OS-native, so this case only bites on the Windows target this engine
        // ships for.
        #[cfg(windows)]
        assert_eq!(
            converted_texture_path("textures/C:/Windows/evil.dds".to_owned()),
            None
        );
        // A plain relative path is unaffected.
        assert_eq!(
            converted_texture_path("textures/land/grass.dds".to_owned()),
            Some("textures/land/grass.ktx2".to_owned())
        );
    }

    #[test]
    fn base_record_type_distinguishes_fixed_static_from_movable_model() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        connection
            .execute_batch(
                "CREATE TABLE records(form_id INTEGER PRIMARY KEY,record_type TEXT NOT NULL);
                 INSERT INTO records VALUES(20,'STAT');
                 INSERT INTO statics VALUES(22,'clutter/barrel.nif',-1,-1,-1,1,1,1,1,0);
                 INSERT INTO records VALUES(22,'MISC');
                 INSERT INTO \"references\" VALUES(40,10,22,8250,-12150,55,0,0,0,1);
                 INSERT INTO exterior_spatial VALUES(40,8250,8250,-12150,-12150,55,55,10,60);",
            )
            .unwrap();
        let payload = load_cell(
            &connection,
            1,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap();
        let fixed = payload.references.iter().find(|r| r.form_id == 30).unwrap();
        let movable = payload.references.iter().find(|r| r.form_id == 40).unwrap();
        assert_eq!(fixed.base_record_type.as_deref(), Some("STAT"));
        assert_eq!(movable.base_record_type.as_deref(), Some("MISC"));
    }

    #[test]
    fn base_is_marker_reads_the_records_marker_flag_not_its_model_name() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        let exterior = CellKey::Exterior {
            worldspace_id: 60,
            grid_x: 2,
            grid_y: -3,
        };
        // The map marker carries the flag; the Civil War map flag has "marker" in its filename
        // but no flag. A marker flag on a model without "marker" in its path is still authoritative.
        connection
            .execute_batch(
                "INSERT INTO statics VALUES(23,'marker_map.nif',-1,-1,-1,1,1,1,1,8388608);
                 INSERT INTO statics VALUES(24,'clutter/civilwar/cwmapmarkers01.nif',-1,-1,-1,1,1,1,1,0);
                 INSERT INTO \"references\" VALUES(41,10,23,8250,-12150,55,0,0,0,1);
                 INSERT INTO exterior_spatial VALUES(41,8250,8250,-12150,-12150,55,55,10,60);
                 INSERT INTO \"references\" VALUES(42,10,24,8250,-12150,55,0,0,0,1);
                 INSERT INTO exterior_spatial VALUES(42,8250,8250,-12150,-12150,55,55,10,60);
                 INSERT INTO statics VALUES(25,'architecture/wall.nif',-1,-1,-1,1,1,1,1,8388608);
                 INSERT INTO \"references\" VALUES(43,10,25,8250,-12150,55,0,0,0,1);
                 INSERT INTO exterior_spatial VALUES(43,8250,8250,-12150,-12150,55,55,10,60);
                 INSERT INTO \"references\" VALUES(44,10,999,8250,-12150,55,0,0,0,1);
                 INSERT INTO exterior_spatial VALUES(44,8250,8250,-12150,-12150,55,55,10,60);",
            )
            .unwrap();
        let payload = load_cell(&connection, 1, exterior).unwrap();
        let marker = |form_id| {
            payload
                .references
                .iter()
                .find(|r| r.form_id == form_id)
                .unwrap()
                .base_is_marker
        };
        assert!(marker(41));
        assert!(!marker(42));
        assert!(!marker(30));
        assert!(marker(43));
        assert!(!marker(44));
    }

    #[test]
    fn catalog_rewrites_landscape_texture_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT); CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER); CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT); INSERT INTO texture_sets VALUES(2,'Textures\\Landscape\\Tundra02.DDS'); INSERT INTO landscape_textures VALUES(1,2); INSERT INTO waters VALUES(9,'textures/water/flow.dds');",
            )
            .unwrap();
        drop(connection);
        let catalog = AssetCatalog::open(&path).unwrap();
        assert_eq!(
            catalog.landscape_diffuse(1),
            Some("textures/landscape/tundra02.ktx2")
        );
        assert_eq!(catalog.water_flow(9), Some("textures/water/flow.ktx2"));
        // This fixture's `waters` table predates the fresnel/reflectivity columns entirely, the
        // same shape a database converted before this change has; the catalog must tolerate it
        // rather than fail to open.
        assert_eq!(catalog.water_colors(9), None);
    }

    #[test]
    fn catalog_returns_water_colors_and_factors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT); \
                 CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER); \
                 CREATE TABLE waters(id INTEGER PRIMARY KEY,opacity INTEGER,shallow_color INTEGER,deep_color INTEGER,reflection_color INTEGER,fresnel REAL,reflectivity REAL,flow_normal_path TEXT);",
            )
            .unwrap();
        let shallow = u32::from_le_bytes([38, 39, 24, 0]);
        let deep = u32::from_le_bytes([5, 14, 18, 0]);
        let reflection = u32::from_le_bytes([119, 140, 157, 0]);
        connection
            .execute(
                "INSERT INTO waters(id,opacity,shallow_color,deep_color,reflection_color,fresnel,reflectivity,flow_normal_path) VALUES (?1,?2,?3,?4,?5,?6,?7,NULL)",
                params![18u32, 35u32, shallow, deep, reflection, 0.10f32, 0.8f32],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO waters(id,opacity,shallow_color,deep_color,reflection_color,fresnel,reflectivity,flow_normal_path) VALUES (?1,NULL,?2,?3,?4,?5,?6,NULL)",
                params![19u32, shallow, deep, reflection, 0.10f32, 0.8f32],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO waters(id,opacity,shallow_color,deep_color,reflection_color,fresnel,reflectivity,flow_normal_path) VALUES (?1,140,?2,?3,?4,?5,?6,NULL)",
                params![20u32, shallow, deep, reflection, 0.10f32, 0.8f32],
            )
            .unwrap();
        drop(connection);
        let catalog = AssetCatalog::open(&path).unwrap();
        assert_eq!(
            catalog.water_colors(18),
            Some(WaterColors {
                shallow: [38, 39, 24],
                deep: [5, 14, 18],
                reflection: [119, 140, 157],
                fresnel: 0.10,
                reflectivity: 0.8,
                opacity: 35,
                fog: None,
                reflection_magnitude: None,
                fnam: None,
                underwater_fog: None,
            })
        );
        // A waters row with no ANAM reads as DefaultWater's 50, and an out-of-range one clamps
        // rather than wrapping to a shifted alpha.
        assert_eq!(
            catalog.water_colors(19).map(|colors| colors.opacity),
            Some(50)
        );
        assert_eq!(
            catalog.water_colors(20).map(|colors| colors.opacity),
            Some(100)
        );
    }

    #[test]
    fn a_water_record_yields_its_fog_from_the_dnam() {
        let default = WaterFog::default();
        // Skyrim LE writes a 228-byte DNAM and SE 232; both hold the fog at the same offsets.
        for len in [228, 232] {
            assert_eq!(
                water_fog_from_record(&water_record_with_fog(len, default)),
                Some(default),
                "{len}-byte DNAM"
            );
        }
        // A puddle: near 0, far 10, amount 0 (a clear body with a ten-unit colour mix).
        let puddle = WaterFog {
            near: 0.0,
            far: 10.0,
            amount: 0.0,
        };
        assert_eq!(
            water_fog_from_record(&water_record_with_fog(228, puddle)),
            Some(puddle)
        );
        // A DNAM that ends before the amount at +132 has no fog; so does one with a NaN or an
        // infinity, a record with no DNAM, and bytes that are not an archive at all.
        assert_eq!(
            water_fog_from_record(&water_record_with_fog(100, default)),
            None
        );
        assert_eq!(
            water_fog_from_record(&water_record_with_fog(135, default)),
            None
        );
        for bad in [f32::NAN, f32::INFINITY] {
            let fog = WaterFog {
                amount: bad,
                ..default
            };
            assert_eq!(
                water_fog_from_record(&water_record_with_fog(228, fog)),
                None
            );
        }
        assert_eq!(
            water_fog_from_record(&water_record(vec![(*b"EDID", b"NoDnam\0".to_vec())])),
            None
        );
        assert_eq!(water_fog_from_record(b"not an archive"), None);
        assert_eq!(water_fog_from_record(&[]), None);
    }

    #[test]
    fn catalog_returns_each_waters_fog_from_its_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        // The columns a registered pack's `waters` table has today: colours, factors and the
        // record bytes, and no fog columns.
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT); \
                 CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER); \
                 CREATE TABLE waters(id INTEGER PRIMARY KEY,editor_id TEXT,opacity INTEGER,flags INTEGER NOT NULL,shallow_color INTEGER,deep_color INTEGER,reflection_color INTEGER,fresnel REAL,reflectivity REAL,flow_normal_path TEXT,data BLOB NOT NULL);",
            )
            .unwrap();
        let shallow = u32::from_le_bytes([38, 39, 24, 0]);
        let deep = u32::from_le_bytes([5, 14, 18, 0]);
        let reflection = u32::from_le_bytes([119, 140, 157, 0]);
        let insert = |id: u32, record: Vec<u8>| {
            connection
                .execute(
                    "INSERT INTO waters(id,opacity,flags,shallow_color,deep_color,reflection_color,fresnel,reflectivity,flow_normal_path,data) VALUES (?1,50,0,?2,?3,?4,0.10,0.8,NULL,?5)",
                    params![id, shallow, deep, reflection, record],
                )
                .unwrap();
        };
        let creek = WaterFog {
            near: 10.0,
            far: 90.0,
            amount: 0.71,
        };
        let puddle = WaterFog {
            near: 0.0,
            far: 10.0,
            amount: 0.0,
        };
        insert(87079, water_record_with_fog(228, creek));
        insert(1013293, water_record_with_fog(228, puddle));
        // Records the fog cannot be read from keep their colours and read as no fog.
        insert(7, water_record_with_fog(100, creek));
        insert(8, b"garbage".to_vec());
        drop(connection);

        let catalog = AssetCatalog::open(&path).unwrap();
        let fog = |id| catalog.water_colors(id).map(|colors| colors.fog);
        assert_eq!(fog(87079), Some(Some(creek)));
        // A puddle's fog amount is zero: the body stays clear while the colour mix still uses its
        // ten-unit far distance.
        assert_eq!(fog(1013293), Some(Some(puddle)));
        assert_eq!(fog(7), Some(None));
        assert_eq!(fog(8), Some(None));
        assert_eq!(
            catalog.water_colors(7).map(|colors| colors.shallow),
            Some([38, 39, 24])
        );
    }

    #[test]
    fn flow_maps_are_found_by_their_file_names() {
        // RF-WATER-036: flow.<cellX>.<cellY>.ktx2, negative coordinates included.
        assert_eq!(flow_map_cell("flow.-1.-14.ktx2"), Some((-1, -14)));
        assert_eq!(flow_map_cell("Flow.27.1.KTX2"), Some((27, 1)));
        assert_eq!(flow_map_cell("flow.27.ktx2"), None);
        assert_eq!(flow_map_cell("riverflow.ktx2"), None);
        assert_eq!(flow_map_cell("flow.a.b.ktx2"), None);
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("textures/water/skyrim.esm");
        std::fs::create_dir_all(&folder).unwrap();
        for name in ["flow.-1.-14.ktx2", "flow.27.1.ktx2", "notes.txt"] {
            std::fs::write(folder.join(name), b"x").unwrap();
        }
        let cells = read_flow_map_cells(directory.path());
        assert_eq!(cells.len(), 2);
        assert!(cells.contains(&(-1, -14)) && cells.contains(&(27, 1)));
        // A pack with no such folder has none, and the catalog then names no flow map.
        assert!(read_flow_map_cells(&directory.path().join("missing")).is_empty());
    }

    const DEFAULT_WATER_NOISE: NoiseFixture = NoiseFixture {
        // DefaultWater (0x18) as the retail Skyrim.esm and Update.esm write it.
        uv_scale: [1920.0, 6703.0, 488.0],
        amplitude: [0.6957, 0.6304, 0.4746],
        depth_properties: [0.3, 0.95, 0.1, 0.9],
        reflection_magnitude: 0.42,
        wind_direction: [270.0, 210.0, 225.0],
        wind_speed: [0.019, 0.013, 0.096],
        noise_falloff: 4007.0,
        // DefaultWater: sun specular 1021 x 4.65, sparkle 3521 x 2.89 (RF-WATER-010).
        sun: [1021.0, 4.65, 3521.0, 2.89],
        // DefaultWater (Update.esm): DNAM +0x98 = 40 (RF-WATER-056).
        refraction_magnitude: 40.0,
        fnam: 0,
        textures: [
            "Data\\Textures\\Water\\DefaultWater.dds",
            "Data\\Textures\\Water\\DefaultWater.dds",
            "Data\\Textures\\Water\\DefaultWater.dds",
        ],
    };

    #[test]
    fn a_water_record_yields_its_noise_layers() {
        let record = parse_water_record(&water_record_with_noise(228, &DEFAULT_WATER_NOISE))
            .expect("a record");
        let noise = water_noise_from_record(&record).expect("noise layers");
        assert_eq!(noise.uv_scale, [1920.0, 6703.0, 488.0]);
        assert_eq!(noise.amplitude, [0.6957, 0.6304, 0.4746]);
        assert_eq!(noise.depth_properties, [0.3, 0.95, 0.1, 0.9]);
        assert_eq!(noise.wind_direction, [270.0, 210.0, 225.0]);
        assert_eq!(noise.wind_speed, [0.019, 0.013, 0.096]);
        assert_eq!(noise.noise_falloff, 4007.0);
        assert_eq!(noise.sun, [1021.0, 4.65, 3521.0, 2.89]);
        assert_eq!(noise.refraction_magnitude, 40.0);
        // The record's own Windows path becomes the converted texture.
        let expected = Some("textures/water/defaultwater.ktx2".to_owned());
        assert_eq!(
            noise.textures,
            [expected.clone(), expected.clone(), expected]
        );
        assert_eq!(
            dnam_float(&record.dnam, WATER_DNAM_REFLECTION_MAGNITUDE),
            Some(0.42)
        );
        // A layer with no path has no texture; a `DNAM` that stops before the depth properties,
        // a zero scale and a non-finite amplitude give no noise at all.
        let mut no_path = DEFAULT_WATER_NOISE;
        no_path.textures = ["", "Data\\Textures\\Water\\DefaultWater.dds", ""];
        let record = parse_water_record(&water_record_with_noise(228, &no_path)).unwrap();
        let noise = water_noise_from_record(&record).unwrap();
        assert_eq!(noise.textures[0], None);
        assert!(noise.textures[1].is_some());
        let short =
            parse_water_record(&water_record_with_noise(200, &DEFAULT_WATER_NOISE)).unwrap();
        assert_eq!(water_noise_from_record(&short), None);
        let mut zero_scale = DEFAULT_WATER_NOISE;
        zero_scale.uv_scale[1] = 0.0;
        let record = parse_water_record(&water_record_with_noise(228, &zero_scale)).unwrap();
        assert_eq!(water_noise_from_record(&record), None);
        let mut broken = DEFAULT_WATER_NOISE;
        broken.amplitude[2] = f32::NAN;
        let record = parse_water_record(&water_record_with_noise(228, &broken)).unwrap();
        assert_eq!(water_noise_from_record(&record), None);
    }

    #[test]
    fn the_fnam_flags_come_from_the_record_bytes() {
        // RF-WATER-013: RiverWaterFlow and DefaultWaterFlow 0x08, the Blend records 0x18,
        // DLC2ApocryphaWater 0x01, every other WATR 0.
        let colors = |fnam: u8| {
            let mut fixture = DEFAULT_WATER_NOISE;
            fixture.fnam = fnam;
            let record = parse_water_record(&water_record_with_noise(228, &fixture)).unwrap();
            assert_eq!(record.flags, Some(fnam));
            WaterColors {
                shallow: [0; 3],
                deep: [0; 3],
                reflection: [0; 3],
                fresnel: 0.1,
                reflectivity: 0.8,
                opacity: 50,
                fog: None,
                reflection_magnitude: None,
                fnam: record.flags,
                underwater_fog: None,
            }
        };
        assert!(colors(0x08).uses_flow_map() && !colors(0x08).blends_normals());
        assert!(colors(0x18).uses_flow_map() && colors(0x18).blends_normals());
        assert!(!colors(0x01).uses_flow_map() && !colors(0x01).blends_normals());
        assert!(!colors(0x00).uses_flow_map());
        // A record with no FNAM has no flags, and an unreadable record neither.
        let none = parse_water_record(&water_record(vec![(*b"DNAM", vec![0u8; 228])])).unwrap();
        assert_eq!(none.flags, None);
    }

    #[test]
    fn the_catalog_serves_each_waters_noise_and_reflection_magnitude() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT); \
                 CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER); \
                 CREATE TABLE waters(id INTEGER PRIMARY KEY,opacity INTEGER,shallow_color INTEGER,deep_color INTEGER,reflection_color INTEGER,fresnel REAL,reflectivity REAL,flow_normal_path TEXT,data BLOB NOT NULL);",
            )
            .unwrap();
        let insert = |id: u32, record: Vec<u8>| {
            connection
                .execute(
                    "INSERT INTO waters VALUES (?1,50,1,2,3,0.1,0.8,NULL,?2)",
                    params![id, record],
                )
                .unwrap();
        };
        insert(24, water_record_with_noise(232, &DEFAULT_WATER_NOISE));
        insert(7, water_record_with_fog(100, WaterFog::default()));
        drop(connection);
        let catalog = AssetCatalog::open(&path).unwrap();
        let noise = catalog.water_noise(24).expect("DefaultWater's noise");
        assert_eq!(noise.uv_scale, [1920.0, 6703.0, 488.0]);
        assert_eq!(
            catalog
                .water_colors(24)
                .and_then(|colors| colors.reflection_magnitude),
            Some(0.42)
        );
        // A record that stops short of the noise fields still has its colours, no noise and no
        // magnitude; the caller then keeps the surface flat and uses DefaultWater's magnitude.
        assert!(catalog.water_noise(7).is_none());
        assert_eq!(
            catalog
                .water_colors(7)
                .and_then(|colors| colors.reflection_magnitude),
            None
        );
        assert!(catalog.water_colors(7).is_some());
    }

    #[test]
    fn a_waters_table_without_opacity_or_record_bytes_still_yields_colours() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT); \
                 CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER); \
                 CREATE TABLE waters(id INTEGER PRIMARY KEY,shallow_color INTEGER,deep_color INTEGER,reflection_color INTEGER,fresnel REAL,reflectivity REAL,flow_normal_path TEXT); \
                 INSERT INTO waters VALUES (24,1,2,3,0.1,0.8,NULL);",
            )
            .unwrap();
        drop(connection);
        let colors = AssetCatalog::open(&path).unwrap().water_colors(24).unwrap();
        assert_eq!(colors.opacity, 50);
        assert_eq!(colors.fog, None);
    }

    #[test]
    fn texture_paths_that_could_leave_the_textures_folder_are_refused() {
        for path in [
            "textures/../../secret.dds",
            "..\\..\\secret.dds",
            "textures/land/../../../secret.dds",
            "/etc/secret.dds",
            "C:/Windows/secret.dds",
            "C:secret.dds",
            "textures//grass.dds",
            "textures/./grass.dds",
            "embedded://grass.dds",
            "textures/grass.dds:stream",
            "",
            "textures/",
        ] {
            assert_eq!(converted_texture_path(path.to_owned()), None, "{path}");
        }
        assert_eq!(
            converted_texture_path("Textures\\Land\\Grass01.dds".to_owned()).as_deref(),
            Some("textures/land/grass01.ktx2")
        );
        assert_eq!(
            converted_texture_path("land/grass..old.dds".to_owned()).as_deref(),
            Some("textures/land/grass..old.ktx2")
        );
    }

    #[test]
    fn catalog_finds_landscape_normal_maps_and_tolerates_their_absence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT,normal_path TEXT); CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER); CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT); INSERT INTO texture_sets VALUES(2,'textures/land/snow.dds','textures/land/snow_n.dds'); INSERT INTO texture_sets VALUES(3,'textures/land/dirt.dds',''); INSERT INTO landscape_textures VALUES(1,2); INSERT INTO landscape_textures VALUES(4,3);",
            )
            .unwrap();
        drop(connection);
        let catalog = AssetCatalog::open(&path).unwrap();
        assert_eq!(
            catalog.landscape_normal(1),
            Some("textures/land/snow_n.ktx2")
        );
        assert_eq!(
            catalog.landscape_normal(4),
            None,
            "an empty normal path is no normal map"
        );

        // A database whose texture sets have no normal_path column at all still opens; the
        // terrain then draws with its geometric normal.
        let older = directory.path().join("older.db");
        let connection = Connection::open(&older).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT); CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER); CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT); INSERT INTO texture_sets VALUES(2,'textures/land/grass.dds'); INSERT INTO landscape_textures VALUES(1,2);",
            )
            .unwrap();
        drop(connection);
        let catalog = AssetCatalog::open(&older).unwrap();
        assert_eq!(
            catalog.landscape_diffuse(1),
            Some("textures/land/grass.ktx2")
        );
        assert_eq!(catalog.landscape_normal(1), None);
    }

    #[test]
    fn rejects_previous_database_schema() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES(2);",
            )
            .unwrap();
        drop(connection);
        assert!(validate(&path).is_err());
    }

    /// Schema-4 and schema-5 databases validate and load; a schema newer than the current one does not.
    #[test]
    fn accepts_schema_four_and_five_databases_with_legacy_query_columns() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        fixture(&connection);
        drop(connection);
        for version in [4, 5] {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute("UPDATE schema_info SET version=?1", [version])
                .unwrap();
            drop(connection);
            validate(&path).unwrap();
            let connection = Connection::open(&path).unwrap();
            let payload = load_cell(
                &connection,
                1,
                CellKey::Exterior {
                    worldspace_id: 60,
                    grid_x: 2,
                    grid_y: -3,
                },
            )
            .unwrap();
            assert_eq!(payload.references.len(), 2);
        }
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE schema_info SET version=?1",
                [shared::WORLD_DATABASE_SCHEMA_VERSION + 1],
            )
            .unwrap();
        drop(connection);
        assert!(validate(&path).is_err());
    }

    #[test]
    fn prefers_exterior_cell_with_land_over_persistent_cell_at_same_grid() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        connection
            .execute_batch("INSERT INTO cells VALUES(9,60,2,-3); INSERT INTO land VALUES(10);")
            .unwrap();

        let payload = load_cell(
            &connection,
            1,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap();

        assert_eq!(payload.cell_id, 10);
        assert_eq!(payload.references.len(), 2);
    }

    #[test]
    fn rejects_truncated_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("truncated.db");
        std::fs::write(&path, b"SQLite format 3\0truncated").unwrap();
        assert!(WorldDatabase::open(&path).is_err());
    }

    #[test]
    fn an_unusable_database_answers_every_request_with_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.db");
        let (request_tx, request_rx) = bounded(4);
        let (response_tx, response_rx) = unbounded();
        let (lod_response_tx, _lod_response_rx) = unbounded();
        let key = CellKey::Interior(99);
        for generation in 0..2 {
            request_tx
                .send(DatabaseRequest::Load {
                    generation,
                    key,
                    queued_at: Instant::now(),
                })
                .unwrap();
        }
        request_tx.send(DatabaseRequest::Shutdown).unwrap();

        worker(
            path,
            WorldDatabaseFeatures::default(),
            request_rx,
            response_tx,
            lod_response_tx,
            unbounded().0,
        );

        let responses: Vec<DatabaseResponse> = response_rx.try_iter().collect();
        assert_eq!(responses.len(), 2, "every request is answered");
        for response in responses {
            let error = response
                .result
                .expect_err("the cell fails instead of loading");
            assert!(error.contains("is unusable"), "{error}");
        }
    }

    #[test]
    fn lod_worker_preserves_generation_on_its_distinct_response_channel() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        lod_fixture(&connection);
        drop(connection);

        let (request_tx, request_rx) = bounded(2);
        let (cell_response_tx, cell_response_rx) = unbounded();
        let (lod_response_tx, lod_response_rx) = unbounded();
        request_tx
            .send(DatabaseRequest::LoadLodChunks {
                generation: 7,
                query: lod_query([-46000.0, 54000.0], [-45000.0, 55000.0]),
                queued_at: Instant::now(),
            })
            .unwrap();
        request_tx.send(DatabaseRequest::Shutdown).unwrap();

        worker(
            path,
            WorldDatabaseFeatures::default(),
            request_rx,
            cell_response_tx,
            lod_response_tx,
            unbounded().0,
        );

        let response = lod_response_rx.try_recv().unwrap();
        assert_eq!(response.generation, 7);
        assert_eq!(response.chunk_count, 1);
        assert_eq!(response.result.unwrap().len(), 1);
        assert!(cell_response_rx.try_recv().is_err());
    }

    #[test]
    fn the_worker_answers_object_chunk_requests_on_their_own_channel() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        lod_fixture(&connection);
        object_chunk_fixture(&connection, OBJECT_PATH, b"glb");
        drop(connection);

        let (request_tx, request_rx) = bounded(3);
        let (cell_response_tx, cell_response_rx) = unbounded();
        let (lod_response_tx, lod_response_rx) = unbounded();
        let (object_tx, object_rx) = unbounded();
        for worldspace_id in [60, 61] {
            request_tx
                .send(DatabaseRequest::LoadLodObjectChunks {
                    generation: 9,
                    worldspace_id,
                    queued_at: Instant::now(),
                })
                .unwrap();
        }
        request_tx.send(DatabaseRequest::Shutdown).unwrap();

        worker(
            path,
            WorldDatabaseFeatures::default(),
            request_rx,
            cell_response_tx,
            lod_response_tx,
            object_tx,
        );

        let with_chunks = object_rx.try_recv().unwrap();
        assert_eq!((with_chunks.generation, with_chunks.worldspace_id), (9, 60));
        assert_eq!(with_chunks.result.unwrap().len(), 1);
        let empty = object_rx.try_recv().unwrap();
        assert_eq!(empty.worldspace_id, 61);
        assert!(empty.result.unwrap().is_empty());
        // Terrain intake never sees an object response.
        assert!(lod_response_rx.try_recv().is_err());
        assert!(cell_response_rx.try_recv().is_err());
    }

    #[test]
    fn drop_drains_a_full_request_queue_and_joins_worker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = Connection::open(&path).unwrap();
        fixture(&connection);
        drop(connection);

        let database = WorldDatabase::open(&path).unwrap();
        let stopped = database.worker_stopped.clone();
        for generation in 0..256 {
            database
                .request(DatabaseRequest::Load {
                    generation,
                    key: CellKey::Exterior {
                        worldspace_id: 60,
                        grid_x: 2,
                        grid_y: -3,
                    },
                    queued_at: Instant::now(),
                })
                .unwrap();
        }
        drop(database);
        assert!(stopped.load(Ordering::Acquire));
    }

    #[test]
    fn loads_interior_cell_without_spatial_lookup() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        let payload = load_cell(&connection, 4, CellKey::Interior(99)).unwrap();
        assert_eq!(payload.cell_id, 99);
        assert_eq!(payload.references.len(), 1);
        assert_eq!(payload.references[0].form_id, 31);
    }

    /// A `lights` table and an `XRDS` override, with the columns the runtime reads (the converter's
    /// table carries more, all of them unread). Reference 30 is lit - its base 20 has a `lights` row
    /// and it carries an override - and reference 40 is not, because its base 22 has no row.
    fn light_fixture(connection: &Connection) {
        connection
            .execute_batch(
                r#"CREATE TABLE lights(id INTEGER PRIMARY KEY,
                    radius REAL NOT NULL,color_r INTEGER NOT NULL,color_g INTEGER NOT NULL,
                    color_b INTEGER NOT NULL,flags INTEGER NOT NULL);
                INSERT INTO lights VALUES(20,256.0,255,150,80,8);
                INSERT INTO statics(id,model_path,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) VALUES(22,'clutter/barrel.nif',-2,-3,-4,2,3,4,1);
                INSERT INTO "references" VALUES(40,10,22,8250,-12150,55,0,0,0,1);
                INSERT INTO exterior_spatial VALUES(40,8250,8250,-12150,-12150,55,55,10,60);
                ALTER TABLE "references" ADD COLUMN radius_override REAL;
                UPDATE "references" SET radius_override=850.8 WHERE id=30;"#,
            )
            .unwrap();
    }

    #[test]
    fn returns_the_light_row_of_a_lit_reference_and_its_radius_override() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        light_fixture(&connection);
        assert!(has_lights(&connection).unwrap());
        assert!(has_radius_override(&connection).unwrap());

        let payload = load_cell(
            &connection,
            1,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap();

        let lit = payload
            .references
            .iter()
            .find(|reference| reference.form_id == 30)
            .expect("reference 30 is in the cell");
        assert_eq!(
            lit.light,
            Some(LightRow {
                radius: 256.0,
                color: [255, 150, 80],
                flags: 8,
                fade: None,
                fade_status: None,
                falloff: None,
                reference_data: None,
                radius_status: None,
            })
        );
        assert_eq!(
            lit.light_radius_override,
            Some(850.8),
            "the reference's own XRDS radius comes back with it"
        );

        let unlit = payload
            .references
            .iter()
            .find(|reference| reference.form_id == 40)
            .expect("reference 40 is in the cell");
        assert_eq!(
            unlit.light, None,
            "a reference whose base has no lights row is not a light"
        );
        assert_eq!(unlit.light_radius_override, None);
        assert_eq!(
            unlit.model_path.as_deref(),
            Some("clutter/barrel.nif"),
            "and it still joins its base object"
        );
    }

    /// A reference's light and its override are separate columns of separate tables, so a database
    /// converted between the two still loads.
    #[test]
    fn loads_lights_from_a_database_without_the_radius_override_column() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        connection
            .execute_batch(
                r#"CREATE TABLE lights(id INTEGER PRIMARY KEY,
                    radius REAL NOT NULL,color_r INTEGER NOT NULL,color_g INTEGER NOT NULL,
                    color_b INTEGER NOT NULL,flags INTEGER NOT NULL);
                INSERT INTO lights VALUES(20,512.0,255,200,120,0);"#,
            )
            .unwrap();
        assert!(!has_radius_override(&connection).unwrap());

        let payload = load_cell(
            &connection,
            1,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap();

        let lit = payload
            .references
            .iter()
            .find(|reference| reference.form_id == 30)
            .expect("reference 30 is in the cell");
        assert_eq!(
            lit.light.as_ref().map(|light| light.radius),
            Some(512.0),
            "the light still loads without the override column"
        );
        assert_eq!(lit.light_radius_override, None);
    }

    /// Reference 30 names a region the converter resolved; reference 40 names one it could not.
    fn emittance_fixture(connection: &Connection) {
        connection
            .execute_batch(
                r#"INSERT INTO statics(id,model_path,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) VALUES(22,'clutter/barrel.nif',-2,-3,-4,2,3,4,1);
                INSERT INTO "references" VALUES(40,10,22,8250,-12150,55,0,0,0,1);
                INSERT INTO exterior_spatial VALUES(40,8250,8250,-12150,-12150,55,55,10,60);
                ALTER TABLE "references" ADD COLUMN emittance_id INTEGER;
                UPDATE "references" SET emittance_id=535419 WHERE id=30;
                UPDATE "references" SET emittance_id=535420 WHERE id=40;
                CREATE TABLE emittance(id INTEGER PRIMARY KEY,source_type TEXT NOT NULL,weather_id INTEGER,
                    sunrise_r INTEGER NOT NULL,sunrise_g INTEGER NOT NULL,sunrise_b INTEGER NOT NULL,
                    day_r INTEGER NOT NULL,day_g INTEGER NOT NULL,day_b INTEGER NOT NULL,
                    sunset_r INTEGER NOT NULL,sunset_g INTEGER NOT NULL,sunset_b INTEGER NOT NULL,
                    night_r INTEGER NOT NULL,night_g INTEGER NOT NULL,night_b INTEGER NOT NULL);
                INSERT INTO emittance VALUES(535419,'REGN',535418,124,70,16,12,34,56,243,118,54,242,176,55);"#,
            )
            .unwrap();
    }

    #[test]
    fn returns_the_day_colour_of_a_reference_emittance_source() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        emittance_fixture(&connection);
        assert!(has_emittance(&connection).unwrap());

        let payload = load_cell(
            &connection,
            1,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap();
        let emittance = |form_id: u32| {
            payload
                .references
                .iter()
                .find(|reference| reference.form_id == form_id)
                .expect("the reference is in the cell")
                .emittance
        };
        assert_eq!(emittance(30), Some([12, 34, 56]), "the day column only");
        assert_eq!(
            emittance(40),
            None,
            "a source without an emittance row lends nothing"
        );
    }

    /// The column alone, without the table it names, does not make the query join it.
    #[test]
    fn needs_both_the_emittance_column_and_table() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        connection
            .execute_batch(r#"ALTER TABLE "references" ADD COLUMN emittance_id INTEGER;"#)
            .unwrap();
        assert!(!has_emittance(&connection).unwrap());
        let payload = load_cell(&connection, 1, CellKey::Interior(99)).unwrap();
        assert_eq!(payload.references[0].emittance, None);
    }

    #[test]
    fn loads_a_database_whose_references_are_all_unlit() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);

        assert!(!has_lights(&connection).unwrap());
        assert!(!has_radius_override(&connection).unwrap());
        let payload = load_cell(
            &connection,
            1,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap();
        assert_eq!(payload.references.len(), 2);
        assert!(payload.references.iter().all(
            |reference| reference.light.is_none() && reference.light_radius_override.is_none()
        ));
        assert!(!has_emittance(&connection).unwrap());
        assert!(
            payload
                .references
                .iter()
                .all(|reference| reference.emittance.is_none()),
            "a database without the emittance table lends no emittance"
        );
        assert_eq!(
            payload.references[0].model_path.as_deref(),
            Some("architecture/wall.nif"),
            "the plain query still joins the base object"
        );
    }

    /// A `door_links` table and its rows: reference 30 (exterior cell 10) leads into interior cell
    /// 500, reference 31 leads out to worldspace 60 and reference 32 is a link whose destination
    /// the converter could not resolve.
    fn add_door_links(connection: &Connection) {
        connection
            .execute_batch(
                r#"CREATE TABLE door_links(ref_id INTEGER PRIMARY KEY,destination_ref_id INTEGER NOT NULL,
                    pos_x REAL NOT NULL,pos_y REAL NOT NULL,pos_z REAL NOT NULL,
                    rot_x REAL NOT NULL,rot_y REAL NOT NULL,rot_z REAL NOT NULL,
                    destination_cell_id INTEGER,destination_worldspace_id INTEGER);
                INSERT INTO "references" VALUES(32,10,20,8260,-12100,50,0,0,0,1);
                INSERT INTO exterior_spatial VALUES(32,8260,8260,-12100,-12100,50,50,10,60);
                INSERT INTO door_links VALUES(30,700,10.5,-20.25,30,0,0,1.5,500,NULL);
                INSERT INTO door_links VALUES(31,701,8200,-12200,50,0,0,3.0,10,60);
                INSERT INTO door_links VALUES(32,702,1,2,3,0,0,0,NULL,NULL);"#,
            )
            .unwrap();
    }

    fn exterior_payload(connection: &Connection) -> CellPayload {
        load_cell(
            connection,
            1,
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 2,
                grid_y: -3,
            },
        )
        .unwrap()
    }

    #[test]
    fn door_links_and_emittance_load_independently_and_together() {
        for doors in [false, true] {
            for emittance in [false, true] {
                let connection = Connection::open_in_memory().unwrap();
                fixture(&connection);
                if doors {
                    add_door_links(&connection);
                }
                if emittance {
                    emittance_fixture(&connection);
                }
                let payload = exterior_payload(&connection);
                let reference = payload
                    .references
                    .iter()
                    .find(|reference| reference.form_id == 30)
                    .unwrap();
                assert_eq!(
                    reference.door,
                    doors.then_some(DoorLinkRow {
                        destination_ref_id: 700,
                        destination_cell_id: Some(500),
                        destination_worldspace_id: None,
                        arrival_position: [10.5, -20.25, 30.0],
                        arrival_rotation: [0.0, 0.0, 1.5],
                    }),
                    "doors={doors}, emittance={emittance}"
                );
                assert_eq!(
                    reference.emittance,
                    emittance.then_some([12, 34, 56]),
                    "doors={doors}, emittance={emittance}"
                );
            }
        }
    }

    #[test]
    fn a_door_links_row_rides_on_the_reference_row() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        add_door_links(&connection);

        assert!(has_door_links(&connection).unwrap());
        let payload = exterior_payload(&connection);
        let door = |form_id: u32| {
            payload
                .references
                .iter()
                .find(|reference| reference.form_id == form_id)
                .unwrap()
                .door
                .clone()
        };
        assert_eq!(
            door(30),
            Some(DoorLinkRow {
                destination_ref_id: 700,
                destination_cell_id: Some(500),
                destination_worldspace_id: None,
                arrival_position: [10.5, -20.25, 30.0],
                arrival_rotation: [0.0, 0.0, 1.5],
            })
        );
        assert_eq!(door(31).unwrap().destination_worldspace_id, Some(60));
        assert_eq!(door(32).unwrap().destination_cell_id, None);
        assert_eq!(payload.references.len(), 3);
    }

    #[test]
    fn a_door_links_row_with_null_arrival_columns_is_no_link_and_does_not_fail_the_cell() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        connection
            .execute_batch(
                r#"CREATE TABLE door_links(ref_id INTEGER PRIMARY KEY,destination_ref_id INTEGER NOT NULL,
                    pos_x REAL,pos_y REAL,pos_z REAL,rot_x REAL,rot_y REAL,rot_z REAL,
                    destination_cell_id INTEGER,destination_worldspace_id INTEGER);
                INSERT INTO door_links VALUES(30,700,NULL,NULL,NULL,NULL,NULL,NULL,500,NULL);
                INSERT INTO door_links VALUES(31,701,8200,-12200,50,0,0,NULL,10,60);"#,
            )
            .unwrap();

        let payload = exterior_payload(&connection);
        assert_eq!(payload.references.len(), 2);
        assert!(
            payload
                .references
                .iter()
                .all(|reference| reference.door.is_none())
        );
    }

    #[test]
    fn a_door_links_table_of_another_shape_is_ignored_and_does_not_fail_the_cell() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);
        connection
            .execute_batch(
                r#"CREATE TABLE door_links(ref_id INTEGER PRIMARY KEY,destination_ref_id INTEGER NOT NULL,
                    pos_x REAL,pos_y REAL,pos_z REAL,rot_x REAL,rot_y REAL,rot_z REAL);
                INSERT INTO door_links VALUES(30,700,1,2,3,0,0,0);"#,
            )
            .unwrap();

        assert!(!has_door_links(&connection).unwrap());
        let payload = exterior_payload(&connection);
        assert_eq!(payload.references.len(), 2);
        assert!(
            payload
                .references
                .iter()
                .all(|reference| reference.door.is_none())
        );
    }

    #[test]
    fn a_database_without_door_links_yields_no_links() {
        let connection = Connection::open_in_memory().unwrap();
        fixture(&connection);

        assert!(!has_door_links(&connection).unwrap());
        let payload = exterior_payload(&connection);
        assert_eq!(payload.references.len(), 2);
        assert!(
            payload
                .references
                .iter()
                .all(|reference| reference.door.is_none())
        );
    }

    #[test]
    fn the_shared_object_atlas_row_is_read_validated_and_hash_checked() {
        let connection = Connection::open_in_memory().unwrap();
        // A pack without the table: its chunks embed the atlas.
        assert_eq!(load_lod_object_atlas(&connection, 60).unwrap(), None);
        connection
            .execute_batch(
                "CREATE TABLE lod_object_atlases(worldspace_id INTEGER PRIMARY KEY, payload_path TEXT NOT NULL, content_hash TEXT NOT NULL);",
            )
            .unwrap();
        assert_eq!(load_lod_object_atlas(&connection, 60).unwrap(), None);
        let hash = "ab".repeat(32);
        connection
            .execute(
                "INSERT INTO lod_object_atlases VALUES (60, 'lod/0000003c/objects_atlas.opensky-srgb.ktx2', ?1)",
                [&hash],
            )
            .unwrap();
        let atlas = load_lod_object_atlas(&connection, 60).unwrap().unwrap();
        assert_eq!(
            atlas.payload_path,
            "lod/0000003c/objects_atlas.opensky-srgb.ktx2"
        );
        // A missing file is an error that names the atlas, not a silent skip.
        let assets = tempfile::tempdir().unwrap();
        let message = format!(
            "{:#}",
            verify_lod_object_atlas(assets.path(), &atlas).unwrap_err()
        );
        assert!(message.contains("shared object LOD atlas"), "{message}");
        // A non-canonical path or a bad hash is refused.
        connection
            .execute("UPDATE lod_object_atlases SET payload_path='../x.ktx2'", [])
            .unwrap();
        assert!(load_lod_object_atlas(&connection, 60).is_err());
        connection
            .execute(
                "UPDATE lod_object_atlases SET payload_path='lod/0000003c/objects_atlas.opensky-srgb.ktx2', content_hash='nope'",
                [],
            )
            .unwrap();
        assert!(load_lod_object_atlas(&connection, 60).is_err());
    }
}
