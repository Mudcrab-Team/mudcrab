//! Offline LOD chunk compiler: `references` + `statics` + cell cache into
//! spatial GLB chunks with a database index (ADR-0010, `lod-compiler.md`).
//!
//! Phase 1 compiles terrain only. Phase 2 adds eligible static objects into
//! the same chunk files beneath per-cell `objects` groups.

pub mod albedo;
pub(crate) mod generation;
pub mod near_match;
pub mod object_stage;
pub mod objects;
pub mod retail;
pub mod retail_terrain;
pub(crate) mod retail_terrain_import;
pub mod retail_terrain_source;
pub(crate) mod reuse;
pub mod terrain;

/// Explicit LOD producer revision; ordinary conversion bytes are unchanged.
pub(crate) const TERRAIN_COMPILER_VERSION: u32 = 4;
