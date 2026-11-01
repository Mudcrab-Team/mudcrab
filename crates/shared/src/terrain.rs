//! Terrain geometry and seam policy shared by the offline converter and runtime.

/// Exterior cell side in Creation units.
pub const CELL_SIZE: f32 = 4096.0;

/// Largest difference between duplicate edge heights that is still welded.
/// Real measured seams reach 24 units. The 64-unit bound is half a LAND sample
/// step; larger differences represent landscapes authored apart and stay intact.
pub const MAX_WELDABLE_EDGE_DELTA: f32 = 64.0;

/// A welded sample moving no more than this retains its authored normal.
pub const EDGE_MATCH_TOLERANCE: f32 = 0.01;

/// Roughness of the opaque terrain atlas material, in the glTF JSON number domain.
pub const ATLAS_ROUGHNESS: f64 = 0.92;
