//! Readers for Skyrim's retail distant-LOD files (D-03: import the retail object and tree LOD).
//!
//! Reader only. Nothing here writes a pack, changes a schema or is called by the pipeline yet:
//! it turns the bytes of one retail file into plain data so the importer (a later change) can
//! decide what to emit. Retail bytes are never committed; the unit tests build tiny synthetic
//! files, and the tests that read real files are `#[ignore]` and read a local install.
//!
//! | Kind | Reader | Container |
//! |---|---|---|
//! | `.bto` object LOD | [`bto::read_object_lod`] | NIF 20.2.0.7 (`BSMultiBoundNode`, `BSSubIndexTriShape`) |
//! | `.btr` terrain LOD | [`btr::read_terrain_lod`] | NIF 20.2.0.7 (`BSTriShape` land, `BSTriShape`/`BSSubIndexTriShape` water) |
//! | `.btt` tree LOD | [`btt::parse_tree_block`] | not a NIF: counted groups of 32-byte billboards |
//! | `.lst` tree list | [`lst::parse_tree_list`] | `i32` count, 32-byte entries |
//! | object and tree atlas | [`atlas::read_dds_info`] | DDS header |
//!
//! File-name and grid encoding is in [`name`].
//!
//! Large references (`BSDistantObjectLargeRefExtraData`) are kept as opaque bytes: their
//! semantics are not decoded yet.

pub mod atlas;
pub mod bto;
pub mod btr;
pub mod btt;
pub mod lst;
pub mod name;
