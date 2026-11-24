//! Small offline placement inputs; no grass instances are persisted.

use crate::asset_path::{AssetKind, canonical_asset_path};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use glam::{DMat4, DVec3};
use memmap2::Mmap;
use rkyv::rancor::Error;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use shared::{CachedLand, grass::*};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::Write,
    path::Path,
    sync::Arc,
};

/// The winning base record's authoritative Is Marker header flag, also used by runtime spawning.
const MARKER_RECORD_FLAG: i64 = 0x0080_0000;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GrassBakeReport {
    #[cfg(test)]
    geometry_decodes: usize,
    pub initially_hidden_references: usize,
    pub unresolved_enable_references: usize,
    pub expanded_sampling_cells: usize,
    pub expanded_sampling_quadrants: usize,
    pub baked_cells: usize,
    pub reused_cells: usize,
    pub unbounded_references: usize,
    pub skipped_obstruction_models: usize,
    pub obstruction_diagnostics: Vec<String>,
    pub skipped_rules: usize,
    /// Lowest FormID among the skipped modeled rules, named in the warning.
    pub first_invalid_rule: Option<u32>,
    /// Lists with omitted fields, unavailable TXST, or unreadable winning LTEX payloads.
    pub skipped_texture_lists: usize,
    pub first_invalid_texture: Option<u32>,
    /// Model-less types retain selection slots and draws, but emit no instances.
    pub modelless_rules: usize,
    pub skipped_cells: usize,
    pub first_invalid_cell: Option<u32>,
    /// Bad assignments, individual weights, and undecodable weight tails dropped locally.
    pub omitted_selection_items: usize,
    pub first_invalid_selection_cell: Option<u32>,
}

impl GrassBakeReport {
    /// Report lost grass definitions or cells without aborting otherwise usable output.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.skipped_rules > 0 {
            let first = self
                .first_invalid_rule
                .map(|id| format!("; first GRAS {id:08X}"))
                .unwrap_or_default();
            warnings.push(format!(
                "grass cache skipped {} incomplete or invalid grass rules{first}",
                self.skipped_rules
            ));
        }
        if self.skipped_texture_lists > 0 {
            let first = self
                .first_invalid_texture
                .map(|id| format!("; first LTEX {id:08X}"))
                .unwrap_or_default();
            warnings.push(format!(
                "grass cache omitted incomplete or unreadable LTEX grass data from {} lists{first}",
                self.skipped_texture_lists
            ));
        }
        if let Some(cell) = self.first_invalid_selection_cell {
            warnings.push(format!(
                "grass cache omitted {} malformed LAND selection items; first cell {cell:08X}",
                self.omitted_selection_items
            ));
        }
        if let Some(cell) = self.first_invalid_cell {
            warnings.push(format!(
                "grass cache skipped {} invalid terrain cells; first cell {cell:08X}",
                self.skipped_cells
            ));
        }
        warnings
    }

    /// Advisory notices for omitted optional obstruction masks, shared by full conversion and
    /// metadata rebuilds. Unavailable bounds and unsampleable geometry never make a run incomplete.
    pub fn notices(&self) -> Vec<String> {
        let mut notices = Vec::new();
        if self.initially_hidden_references > 0 {
            notices.push(format!("grass exclusion ignored {} initially hidden obstruction references ({} unresolved enable states)", self.initially_hidden_references, self.unresolved_enable_references));
        }
        if self.expanded_sampling_cells > 0 || self.expanded_sampling_quadrants > 0 {
            notices.push(format!("grass exclusion evaluated skinned bounds added {} cells and {} quadrants to sampling", self.expanded_sampling_cells, self.expanded_sampling_quadrants));
        }
        if self.modelless_rules > 0 {
            notices.push(format!(
                "grass cache retained {} non-emitting grass types without a model",
                self.modelless_rules
            ));
        }
        if self.unbounded_references > 0 {
            notices.push(format!(
                "grass exclusion skipped {} exterior references without validated model bounds",
                self.unbounded_references
            ));
        }
        if self.skipped_obstruction_models > 0 {
            notices.push(format!(
                "grass exclusion skipped {} missing or invalid obstruction models",
                self.skipped_obstruction_models
            ));
            notices.extend(self.obstruction_diagnostics.iter().cloned());
        }
        notices
    }
}

#[derive(Clone)]
struct ReferenceBounds {
    enabled: bool,
    unresolved_enable: bool,
    potential_skin: bool,
    id: u32,
    base_id: u32,
    worldspace: u32,
    min: [f64; 3],
    max: [f64; 3],
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
    model_path: String,
    source_hash: [u8; 32],
    /// Resolved NullTextureSet indices eligible for the current runtime swap path.
    hidden_shapes: Vec<u32>,
}

type Triangle = [DVec3; 3];
/// One model after the placed base's effective MODS visibility exclusions.
type GeometryKey = (String, Vec<u32>);
const GEOMETRY_BUDGET: usize = 64 * 1024 * 1024;

mod enable;
mod links;
mod selection;
mod skinning;
mod visibility;

#[cfg(test)]
#[path = "grass_cache/skinning_tests.rs"]
mod skinning_tests;

#[cfg(test)]
#[path = "grass_cache/review_tests.rs"]
mod review_tests;

#[cfg(test)]
mod placement_tests;

#[cfg(test)]
mod object_texture_tests;

/// Fingerprints and evaluated offsets survive eviction; retained triangles have a byte budget.
struct GeometryCache {
    #[cfg(test)]
    decodes: usize,
    potential_skins: HashMap<String, bool>,
    swappable_shapes: HashMap<String, std::collections::BTreeSet<u32>>,
    reference_offsets: HashMap<u32, [DVec3; 2]>,
    expanded_cells: std::collections::BTreeSet<u32>,
    expanded_quadrants: std::collections::BTreeSet<(u32, u8)>,
    root: std::path::PathBuf,
    hashes: HashMap<String, [u8; 32]>,
    failures: BTreeMap<String, String>,
    /// Read failures learned before geometry inspection remain optional model omissions.
    unavailable_models: std::collections::BTreeSet<String>,
    /// Failed gets for the cell currently being baked, including memoized failures.
    cell_failures: std::collections::BTreeSet<String>,
    meshes: HashMap<GeometryKey, (Arc<Vec<Triangle>>, u64)>,
    failed_meshes: std::collections::BTreeSet<GeometryKey>,
    bytes: usize,
    clock: u64,
    source_changed: bool,
}

impl GeometryCache {
    /// Create an empty geometry cache rooted at a canonical asset directory.
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            #[cfg(test)]
            decodes: 0,
            potential_skins: HashMap::new(),
            swappable_shapes: HashMap::new(),
            reference_offsets: HashMap::new(),
            expanded_cells: Default::default(),
            expanded_quadrants: Default::default(),
            root: root.canonicalize()?,
            hashes: HashMap::new(),
            failures: BTreeMap::new(),
            unavailable_models: Default::default(),
            cell_failures: Default::default(),
            meshes: HashMap::new(),
            failed_meshes: Default::default(),
            bytes: 0,
            clock: 0,
            source_changed: false,
        })
    }

    /// Return a stable model-content fingerprint, including unavailable geometry.
    fn fingerprint(&mut self, path: &str) -> [u8; 32] {
        if !self.hashes.contains_key(path) {
            let result = self.model_bytes(path);
            match result {
                Ok(bytes) => {
                    // Classification reads JSON only; corrupt containers stay spatially bounded.
                    let document = gltf::binary::Glb::from_slice(&bytes).ok().and_then(|glb| {
                        serde_json::from_slice::<serde_json::Value>(&glb.json).ok()
                    });
                    let skin = document
                        .as_ref()
                        .and_then(|json| json.get("nodes"))
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|nodes| nodes.iter().any(|node| node.get("skin").is_some()));
                    self.potential_skins.insert(path.to_owned(), skin);
                    self.swappable_shapes.insert(
                        path.to_owned(),
                        document
                            .as_ref()
                            .map(swappable_geometry_indices)
                            .unwrap_or_default(),
                    );
                    self.hashes
                        .insert(path.to_owned(), Sha256::digest(&bytes).into());
                }
                Err(error) => {
                    self.unavailable_models.insert(path.to_owned());
                    self.hashes.insert(
                        path.to_owned(),
                        Sha256::digest(b"unavailable obstruction model").into(),
                    );
                    self.potential_skins.insert(path.to_owned(), false);
                    self.swappable_shapes
                        .insert(path.to_owned(), Default::default());
                    self.failures.insert(path.to_owned(), format!("{error:#}"));
                }
            }
        }
        self.hashes[path]
    }

    /// Read only canonical model paths contained in the explicit asset root.
    fn model_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let canonical = canonical_asset_path(path, AssetKind::Mesh, "glb")?;
        ensure!(canonical == path, "noncanonical obstruction path");
        let file = self.root.join(path).canonicalize()?;
        ensure!(
            file.starts_with(&self.root),
            "obstruction escapes asset root"
        );
        Ok(std::fs::read(file)?)
    }

    /// Inspect the unchanged model variant in focused geometry tests.
    #[cfg(test)]
    fn get(&mut self, path: &str) -> Option<Arc<Vec<Triangle>>> {
        self.get_with_hidden(path, &[])
    }

    /// Decode one visibility variant with bounded retention and per-model diagnostics.
    fn get_with_hidden(&mut self, path: &str, hidden: &[u32]) -> Option<Arc<Vec<Triangle>>> {
        let key = (path.to_owned(), hidden.to_vec());
        self.clock += 1;
        if let Some((mesh, stamp)) = self.meshes.get_mut(&key) {
            *stamp = self.clock;
            return Some(Arc::clone(mesh));
        }
        if self.unavailable_models.contains(path) || self.failed_meshes.contains(&key) {
            self.cell_failures.insert(path.to_owned());
            return None;
        }
        let result = (|| -> Result<Arc<Vec<Triangle>>> {
            let bytes = self.model_bytes(path).inspect_err(|_| {
                if self.hashes.contains_key(path) {
                    self.source_changed = true;
                } else {
                    self.unavailable_models.insert(path.to_owned());
                }
            })?;
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            if self
                .hashes
                .get(path)
                .is_some_and(|expected| *expected != digest)
            {
                self.source_changed = true;
                color_eyre::eyre::bail!("obstruction changed during grass bake; retry conversion");
            }
            self.hashes.entry(path.to_owned()).or_insert(digest);
            #[cfg(test)]
            {
                self.decodes += 1;
            }
            Ok(Arc::new(model_triangles_with_hidden(&bytes, hidden)?))
        })();
        match result {
            Ok(mesh) => {
                let bytes = mesh.capacity() * std::mem::size_of::<Triangle>();
                while self.bytes + bytes > GEOMETRY_BUDGET {
                    let Some(oldest) = self
                        .meshes
                        .iter()
                        .min_by_key(|(_, (_, stamp))| *stamp)
                        .map(|(key, _)| key.clone())
                    else {
                        break;
                    };
                    let Some((removed, _)) = self.meshes.remove(&oldest) else {
                        break;
                    };
                    self.bytes -= removed.capacity() * std::mem::size_of::<Triangle>();
                }
                // A single oversized mesh is processed transiently, never added to the cache.
                if bytes <= GEOMETRY_BUDGET {
                    self.bytes += bytes;
                    self.meshes.insert(key, (Arc::clone(&mesh), self.clock));
                }
                Some(mesh)
            }
            Err(error) => {
                self.failed_meshes.insert(key);
                self.hashes
                    .entry(path.to_owned())
                    .or_insert_with(|| Sha256::digest(b"unavailable obstruction model").into());
                self.failures.insert(path.to_owned(), format!("{error:#}"));
                self.cell_failures.insert(path.to_owned());
                None
            }
        }
    }

    /// Confirm the inspected model sources remain consistent before archive publication.
    fn validate_sources(&self) -> Result<()> {
        ensure!(
            !self.source_changed,
            "obstruction changed during grass bake; retry conversion"
        );
        for (path, expected) in &self.hashes {
            if self.unavailable_models.contains(path) {
                continue;
            }
            let unchanged = self
                .model_bytes(path)
                .is_ok_and(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)) == *expected);
            ensure!(
                unchanged,
                "obstruction changed during grass bake ({path}); retry conversion"
            );
        }
        Ok(())
    }
}

/// Resolve the current runtime's last-entry-per-index MODS visibility from projected rows.
/// A later retexture or missing target restores the original visible shape.
fn read_mods_hidden_shapes(conn: &Connection) -> Result<HashMap<u32, Vec<u32>>> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='base_texture_swaps')",
        [], |row| row.get(0),
    )?;
    if !present {
        return Ok(HashMap::new());
    }
    let mut statement = conn.prepare(
        "SELECT base_id,shape_index,disposition FROM base_texture_swaps ORDER BY base_id,position",
    )?;
    let mut latest = BTreeMap::<u32, BTreeMap<u32, bool>>::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (base, index, disposition) = row?;
        if let Ok(index) = u32::try_from(index) {
            latest
                .entry(base)
                .or_default()
                .insert(index, disposition == "null_texture_set");
        }
    }
    Ok(latest
        .into_iter()
        .map(|(base, indices)| {
            (
                base,
                indices
                    .into_iter()
                    .filter_map(|(index, hidden)| hidden.then_some(index))
                    .collect(),
            )
        })
        .collect())
}

/// Mirror the current standard-material MODS path's known material-binding exclusions.
/// Native standard materials still participate; depth/skin/palette wrappers and external
/// emittance keep their rendered shape, as the runtime swap query does.
fn standard_mods_surface(material: Option<&serde_json::Value>) -> bool {
    let Some(material) = material else {
        return true;
    };
    let tag = material.pointer("/extras/openSkyrim");
    let flags1 = tag
        .and_then(|tag| tag.get("shaderFlags1"))
        .and_then(serde_json::Value::as_u64);
    if flags1.is_some_and(|flags| flags & (1 << 29) != 0) {
        return false;
    }
    let flags2 = tag
        .and_then(|tag| tag.get("shaderFlags2"))
        .and_then(serde_json::Value::as_u64);
    if let (Some(flags1), Some(flags2)) = (
        flags1.and_then(|flags| u32::try_from(flags).ok()),
        flags2.and_then(|flags| u32::try_from(flags).ok()),
    ) {
        let depth_test = flags1 & (1 << 31) != 0;
        let depth_write = flags2 & 1 != 0;
        let decal = flags1 & ((1 << 26) | (1 << 27)) != 0;
        let blend = material
            .get("alphaMode")
            .and_then(serde_json::Value::as_str)
            == Some("BLEND");
        if decal || !depth_test || (!depth_write && !blend) {
            return false;
        }
    }
    let model_space = tag
        .and_then(|tag| tag.get("normalConvention"))
        .and_then(serde_json::Value::as_str)
        == Some("model_space");
    if model_space && material.get("normalTexture").is_some() {
        return false;
    }
    let extension = material.pointer("/extensions/OPEN_SKYRIM_material");
    let shader = extension
        .and_then(|extension| extension.get("lightingShaderType"))
        .and_then(serde_json::Value::as_str);
    if !model_space
        && shader == Some("hair_tint")
        && extension
            .and_then(|extension| extension.get("tintColor"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|colour| {
                colour.len() >= 3
                    && colour[..3].iter().all(|channel| {
                        channel
                            .as_f64()
                            .is_some_and(|value| (value as f32).is_finite())
                    })
            })
    {
        return false;
    }
    // Converter tags name lighting shapes. Keep known palette wrappers visible when
    // independently authored tags accompany the effect-material palette contract.
    if let Some(extension) = extension
        && extension
            .get("shaderFamily")
            .and_then(serde_json::Value::as_str)
            == Some("effect")
        && extension
            .get("shaderFlags1")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|flags| flags & ((1 << 4) | (1 << 5)) != 0)
        && extension
            .get("textureSlots")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|slots| {
                slots.iter().any(|slot| {
                    slot.get("semantic").and_then(serde_json::Value::as_str) == Some("greyscale")
                        && slot
                            .get("texture")
                            .and_then(serde_json::Value::as_u64)
                            .is_some()
                })
            })
    {
        return false;
    }
    true
}

/// Read eligible geometry tags without decoding triangle or skin buffers.
fn swappable_geometry_indices(document: &serde_json::Value) -> std::collections::BTreeSet<u32> {
    let materials = document
        .get("materials")
        .and_then(serde_json::Value::as_array);
    document
        .get("meshes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|mesh| {
            mesh.get("primitives")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|primitive| {
            let index = u32::try_from(
                primitive
                    .pointer("/extras/mudcrabGeometryIndex")?
                    .as_u64()?,
            )
            .ok()?;
            let material = primitive
                .get("material")
                .and_then(serde_json::Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| materials?.get(index));
            standard_mods_surface(material).then_some(index)
        })
        .collect()
}

/// Apply proven per-shape MODS hiding through the existing invisible-primitive cleanup.
fn model_triangles_with_hidden(bytes: &[u8], hidden: &[u32]) -> Result<Vec<Triangle>> {
    if hidden.is_empty() {
        return model_triangles(bytes);
    }
    let mut container = gltf::binary::Glb::from_slice(bytes)?;
    let mut document: serde_json::Value = serde_json::from_slice(&container.json)?;
    if let Some(meshes) = document
        .get_mut("meshes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for mesh in meshes {
            if let Some(primitives) = mesh
                .get_mut("primitives")
                .and_then(serde_json::Value::as_array_mut)
            {
                for primitive in primitives {
                    let index = primitive
                        .pointer("/extras/mudcrabGeometryIndex")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|index| u32::try_from(index).ok());
                    if index.is_some_and(|index| hidden.binary_search(&index).is_ok())
                        && let Some(extras) = primitive
                            .get_mut("extras")
                            .and_then(serde_json::Value::as_object_mut)
                    {
                        let exclusion = extras
                            .entry("openSkyrim")
                            .or_insert_with(|| serde_json::json!({}));
                        if !exclusion.is_object() {
                            *exclusion = serde_json::json!({});
                        }
                        exclusion["materialExclusion"] =
                            serde_json::json!("STAT MODS NullTextureSet");
                    }
                }
            }
        }
    }
    container.json = std::borrow::Cow::Owned(serde_json::to_vec(&document)?);
    model_triangles(&container.to_vec()?)
}

/// The glTF reader assumes valid binary layouts even after JSON validation.
/// Check its count/stride arithmetic and byte ranges before constructing an iterator.
fn accessor_bytes<'a>(
    view: gltf::buffer::View<'_>,
    offset: usize,
    count: usize,
    item_size: usize,
    blob: &'a [u8],
) -> Result<&'a [u8]> {
    let stride = view.stride().unwrap_or(item_size);
    ensure!(
        count > 0 && stride >= item_size,
        "invalid accessor count or stride"
    );
    let end = stride
        .checked_mul(count - 1)
        .and_then(|bytes| offset.checked_add(bytes))
        .and_then(|bytes| bytes.checked_add(item_size))
        .ok_or_else(|| color_eyre::eyre::eyre!("accessor byte range overflows"))?;
    let view_end = view
        .offset()
        .checked_add(view.length())
        .ok_or_else(|| color_eyre::eyre::eyre!("buffer view byte range overflows"))?;
    ensure!(
        end <= view.length()
            && view_end <= view.buffer().length()
            && view.buffer().length() <= blob.len(),
        "accessor exceeds its buffer view or BIN data"
    );
    Ok(&blob[view.offset()..view_end][offset..end])
}

/// Validate backing ranges and sparse indices before dependency iterators read them.
fn validate_accessor(accessor: &gltf::Accessor<'_>, blob: &[u8]) -> Result<()> {
    // The dependency's sparse iterator uses a u32 counter.
    ensure!(
        accessor.count() > 0 && accessor.count() <= u32::MAX as usize,
        "invalid accessor count"
    );
    if let Some(view) = accessor.view() {
        accessor_bytes(
            view,
            accessor.offset(),
            accessor.count(),
            accessor.size(),
            blob,
        )?;
    } else {
        ensure!(
            accessor.offset() == 0 && accessor.sparse().is_some(),
            "accessor has no backing data"
        );
        // Limit the implicit base before sparse readers reserve its expanded length.
        ensure!(
            accessor
                .count()
                .checked_mul(accessor.size())
                .is_some_and(|bytes| bytes <= GEOMETRY_BUDGET),
            "implicit accessor exceeds geometry byte budget"
        );
    }
    if let Some(sparse) = accessor.sparse() {
        ensure!(
            sparse.count() > 0 && sparse.count() <= accessor.count(),
            "invalid sparse accessor count"
        );
        let indices = sparse.indices();
        let index_size = indices.index_type().size();
        let stride = indices.view().stride().unwrap_or(index_size);
        let bytes = accessor_bytes(
            indices.view(),
            indices.offset(),
            sparse.count(),
            index_size,
            blob,
        )?;
        let mut previous = None;
        for item in bytes.chunks(stride) {
            let index = match indices.index_type() {
                gltf::accessor::sparse::IndexType::U8 => u32::from(item[0]),
                gltf::accessor::sparse::IndexType::U16 => {
                    u32::from(u16::from_le_bytes(item[..2].try_into()?))
                }
                gltf::accessor::sparse::IndexType::U32 => u32::from_le_bytes(item[..4].try_into()?),
            };
            ensure!(
                (index as usize) < accessor.count() && previous.is_none_or(|last| last < index),
                "sparse indices must be sorted, unique and in bounds"
            );
            previous = Some(index);
        }
        let values = sparse.values();
        accessor_bytes(
            values.view(),
            values.offset(),
            sparse.count(),
            accessor.size(),
            blob,
        )?;
    }
    Ok(())
}

/// Decode supported visible GLB scene triangles, omitting blended primitives by grass policy.
fn model_triangles(bytes: &[u8]) -> Result<Vec<Triangle>> {
    let prepared = visibility::prepare(bytes)?;
    let bytes = prepared.as_ref();
    let evaluated = (|| {
        let mut glb = gltf::Gltf::from_slice_without_validation(bytes)?;
        skinning::validate_position_references(&glb.document)?;
        glb.document = gltf::Document::from_json(glb.document.into_json())?;
        scene_triangles(&glb, true)
    })();
    match evaluated {
        Ok(triangles) => Ok(triangles),
        Err(error) => {
            let glb = skinning::static_fallback(bytes)
                .wrap_err_with(|| format!("obstruction decode failed: {error:#}"))?;
            scene_triangles(&glb, false)
        }
    }
}

/// Collect one validated model transactionally, optionally evaluating its authored rest pose.
fn scene_triangles(glb: &gltf::Gltf, evaluate_skin: bool) -> Result<Vec<Triangle>> {
    let blob = glb
        .blob
        .as_deref()
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB has no BIN chunk"))?;
    ensure!(
        glb.buffers()
            .all(|buffer| matches!(buffer.source(), gltf::buffer::Source::Bin)),
        "external model buffers unsupported"
    );
    /// Traverse bounded scene nodes, preserving ordinary transforms for unskinned geometry.
    fn visit(
        node: gltf::Node<'_>,
        parent: DMat4,
        blob: &[u8],
        triangles: &mut Vec<Triangle>,
        depth: usize,
        globals: Option<&[Option<DMat4>]>,
        visited: &mut [bool],
    ) -> Result<()> {
        ensure!(depth < 128, "model hierarchy exceeds inspection depth");
        ensure!(
            !std::mem::replace(&mut visited[node.index()], true),
            "repeated model node in selected scene"
        );
        let matrix = node
            .transform()
            .matrix()
            .map(|column| column.map(f64::from));
        let transform = parent * DMat4::from_cols_array_2d(&matrix);
        ensure!(transform.is_finite(), "nonfinite model transform");
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                if primitive.material().alpha_mode() == gltf::material::AlphaMode::Blend {
                    continue;
                }
                ensure!(
                    primitive.mode() == gltf::mesh::Mode::Triangles,
                    "nontriangle obstruction unsupported"
                );
                let positions = primitive
                    .get(&gltf::Semantic::Positions)
                    .ok_or_else(|| color_eyre::eyre::eyre!("missing model positions"))?;
                ensure!(
                    positions.data_type() == gltf::accessor::DataType::F32
                        && positions.dimensions() == gltf::accessor::Dimensions::Vec3
                        && !positions.normalized(),
                    "unsupported model position format"
                );
                validate_accessor(&positions, blob).wrap_err("invalid model positions")?;
                if let Some(indices) = primitive.indices() {
                    ensure!(
                        matches!(
                            indices.data_type(),
                            gltf::accessor::DataType::U8
                                | gltf::accessor::DataType::U16
                                | gltf::accessor::DataType::U32
                        ) && indices.dimensions() == gltf::accessor::Dimensions::Scalar
                            && !indices.normalized(),
                        "unsupported model index format"
                    );
                    validate_accessor(&indices, blob).wrap_err("invalid model indices")?;
                }
                let reader = primitive.reader(|buffer| {
                    matches!(buffer.source(), gltf::buffer::Source::Bin).then_some(blob)
                });
                let positions: Vec<_> = reader
                    .read_positions()
                    .ok_or_else(|| color_eyre::eyre::eyre!("missing model positions"))?
                    .map(|p| DVec3::from_array(p.map(f64::from)))
                    .collect();
                let vertices = if let Some(skin) = node.skin() {
                    let globals = globals.ok_or_else(|| {
                        color_eyre::eyre::eyre!("missing scene transforms for skinned model")
                    })?;
                    skinning::positions(&primitive, skin, globals, blob, &positions)?
                } else {
                    positions
                        .into_iter()
                        .map(|p| transform.transform_point3(p))
                        .collect()
                };
                ensure!(
                    vertices.iter().all(|p| p.is_finite()),
                    "nonfinite model positions"
                );
                let indices: Vec<u32> = if primitive.indices().is_some() {
                    reader
                        .read_indices()
                        .ok_or_else(|| color_eyre::eyre::eyre!("unreadable model indices"))?
                        .into_u32()
                        .collect()
                } else {
                    (0..vertices.len() as u32).collect()
                };
                ensure!(indices.len().is_multiple_of(3), "incomplete triangle");
                for triangle in indices.as_chunks::<3>().0 {
                    ensure!(
                        triangle
                            .iter()
                            .all(|&index| (index as usize) < vertices.len()),
                        "triangle index out of bounds"
                    );
                    triangles.push(triangle.map(|index| vertices[index as usize]));
                }
            }
        }
        for child in node.children() {
            visit(
                child,
                transform,
                blob,
                triangles,
                depth + 1,
                globals,
                visited,
            )?;
        }
        Ok(())
    }
    let scene = glb
        .default_scene()
        .or_else(|| glb.scenes().next())
        .ok_or_else(|| color_eyre::eyre::eyre!("model has no scene"))?;
    let mut triangles = Vec::new();
    let globals = if evaluate_skin && glb.nodes().any(|node| node.skin().is_some()) {
        Some(skinning::scene_transforms(&scene, glb.nodes().len())?)
    } else {
        None
    };
    let mut visited = vec![false; glb.nodes().len()];
    for node in scene.nodes() {
        visit(
            node,
            DMat4::IDENTITY,
            blob,
            &mut triangles,
            0,
            globals.as_deref(),
            &mut visited,
        )?;
    }
    Ok(triangles)
}

/// Bake complete source terrain after model-bound finalization. The terrain archive must retain
/// the base and first five painted layers, with original opacity. A valid previous grass cache allows
/// unchanged cells to be reused.
pub fn write_grass_cache(
    conn: &Connection,
    asset_root: &Path,
    terrain_path: &Path,
    previous: Option<&Path>,
    path: &Path,
) -> Result<GrassBakeReport> {
    write_grass_cache_with_cancel(
        conn,
        asset_root,
        terrain_path,
        previous,
        path,
        &crate::pipeline::Cancellation::new(),
    )
}

/// Bake with cooperative cancellation between references, cells and quadrants.
pub fn write_grass_cache_with_cancel(
    conn: &Connection,
    asset_root: &Path,
    terrain_path: &Path,
    previous: Option<&Path>,
    path: &Path,
    cancellation: &crate::pipeline::Cancellation,
) -> Result<GrassBakeReport> {
    let check_cancel = || -> Result<()> {
        ensure!(!cancellation.is_cancelled(), "grass bake cancelled");
        Ok(())
    };
    check_cancel()?;
    let mut geometry = GeometryCache::new(asset_root)?;
    let terrain_map = super::cell_cache::validate_cell_cache(terrain_path)?;
    let terrain = rkyv::access::<shared::ArchivedCellCache, Error>(&terrain_map)?;
    let (types, skipped_rules, modelless_rules, first_invalid_rule) = read_types(conn)?;
    let valid_types: std::collections::BTreeSet<_> =
        types.iter().map(|rule| rule.form_id).collect();
    let modeled_types: std::collections::BTreeSet<_> = types
        .iter()
        .filter(|rule| !rule.model_path.is_empty())
        .map(|rule| rule.form_id)
        .collect();
    let (links, mut skipped_texture_lists, mut first_invalid_texture) =
        links::read_links(conn, &valid_types)?;
    let mut texture_bindings = BTreeMap::new();
    let mut textures = conn.prepare("SELECT l.id, t.id IS NOT NULL FROM landscape_textures l LEFT JOIN texture_sets t ON t.id=l.texture_set_id")?;
    for row in textures.query_map([], |row| {
        Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?))
    })? {
        match row {
            Ok((id, valid)) => {
                texture_bindings.insert(id, valid && id != 0);
            }
            Err(_) => {
                skipped_texture_lists += 1;
            }
        }
    }
    for &id in links.keys() {
        if !texture_bindings.get(&id).copied().unwrap_or(false) {
            skipped_texture_lists += 1;
            first_invalid_texture.get_or_insert(id);
        }
        texture_bindings.entry(id).or_insert(false);
    }
    let mut statement = conn.prepare("SELECT id,worldspace_id,grid_x,grid_y FROM cells WHERE worldspace_id IS NOT NULL AND grid_x IS NOT NULL AND grid_y IS NOT NULL AND (flags & 1)=0 ORDER BY id")?;
    let coordinates: BTreeMap<u32, (u32, i32, i32)> = statement
        .query_map([], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // Linked modeled rules require obstruction inputs; non-emitting slots keep their links.
    let has_grass_terrain = terrain.cells.iter().any(|land| {
        coordinates.contains_key(&land.cell_id.to_native())
            && land.layers.iter().any(|layer| {
                links
                    .get(&layer.texture_form_id.to_native())
                    .is_some_and(|grasses| grasses.iter().any(|id| modeled_types.contains(id)))
            })
    });
    let (mut references, unbounded_references) = if has_grass_terrain {
        read_reference_bounds(conn)?
    } else {
        (Vec::new(), 0)
    };
    let mut refs_by_cell = HashMap::<u32, Vec<&ReferenceBounds>>::new();
    let mut skinned_by_world = HashMap::<u32, Vec<&ReferenceBounds>>::new();
    let mut spatial = BTreeMap::<(u32, i32, i32), Vec<u32>>::new();
    for land in terrain.cells.iter() {
        let id = land.cell_id.to_native();
        if let Some(&coordinate) = coordinates.get(&id) {
            spatial.entry(coordinate).or_default().push(id);
        }
    }
    for reference in &mut references {
        check_cancel()?;
        geometry.fingerprint(&reference.model_path);
        reference.potential_skin = geometry.potential_skins[&reference.model_path];
        reference
            .hidden_shapes
            .retain(|index| geometry.swappable_shapes[&reference.model_path].contains(index));
    }
    for reference in &references {
        if reference.potential_skin {
            // Raw POSITION bounds cannot limit skin deformation. Keep conservative candidates
            // independent of geometry so a fully reusable bake never decodes vertices.
            skinned_by_world
                .entry(reference.worldspace)
                .or_default()
                .push(reference);
            continue;
        }
        let first_x = (reference.min[0] / f64::from(GRASS_CELL_SIZE)).floor() as i32;
        let last_x = (reference.max[0] / f64::from(GRASS_CELL_SIZE)).floor() as i32;
        let first_y = (reference.min[1] / f64::from(GRASS_CELL_SIZE)).floor() as i32;
        let last_y = (reference.max[1] / f64::from(GRASS_CELL_SIZE)).floor() as i32;
        let span = (i64::from(last_x) - i64::from(first_x) + 1)
            .saturating_mul(i64::from(last_y) - i64::from(first_y) + 1);
        if span <= 1024 {
            for y in first_y..=last_y {
                for x in first_x..=last_x {
                    if let Some(ids) = spatial.get(&(reference.worldspace, x, y)) {
                        for &id in ids {
                            refs_by_cell.entry(id).or_default().push(reference);
                        }
                    }
                }
            }
        } else {
            // Very large valid modded bounds must not allocate an unbounded grid.
            for (&id, &(world, x, y)) in &coordinates {
                if world == reference.worldspace
                    && (first_x..=last_x).contains(&x)
                    && (first_y..=last_y).contains(&y)
                {
                    refs_by_cell.entry(id).or_default().push(reference);
                }
            }
        }
    }
    let has_source_order: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='inhouse_terrain_source_order')",
        [],
        |row| row.get(0),
    )?;
    let source_sql = if has_source_order {
        "SELECT r.form_id,r.record_type,r.cell_id,r.data,r.load_order,COALESCE(s.source_record_offset,0) FROM records r LEFT JOIN inhouse_terrain_source_order s ON s.form_id=r.form_id WHERE r.record_type IN ('LAND','LTEX','GRAS') ORDER BY r.form_id"
    } else {
        "SELECT form_id,record_type,cell_id,data,load_order,0 FROM records WHERE record_type IN ('LAND','LTEX','GRAS') ORDER BY form_id"
    };
    let mut statement = conn.prepare(source_sql)?;
    let mut rule_inputs = BTreeMap::<u32, Vec<u8>>::new();
    let mut selected_lands = BTreeMap::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<u32>>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, u32>(4)?,
            row.get::<_, u64>(5)?,
        ))
    })? {
        let (id, kind, cell, bytes, load_order, source_offset) = row?;
        if kind == "LAND" {
            // Canonical SQL retains every live LAND. Match land_winners_by_cell's
            // whole-record priority; losing slots must never mix into the winner.
            let priority = (load_order, source_offset, id);
            let cell = cell.unwrap_or(id);
            if selected_lands
                .get(&cell)
                .is_none_or(|(selected, _)| priority > *selected)
            {
                selected_lands.insert(cell, (priority, bytes));
            }
        } else {
            rule_inputs.insert(id, bytes);
        }
    }
    let land_inputs: BTreeMap<_, _> = selected_lands
        .into_iter()
        .map(|(cell, (_, bytes))| (cell, vec![bytes]))
        .collect();
    // TXST deletion changes binding/merge behavior even if LTEX bytes remain identical.
    for (&id, &valid) in &texture_bindings {
        let input = rule_inputs.entry(id).or_default();
        input.extend_from_slice(b"grass-texture-binding-validity-v1\0");
        input.push(u8::from(valid));
    }
    // Invalid/old caches are recoverable: recompute rather than trusting their masks.
    let old_map = previous.and_then(|path| validate_grass_cache(path).ok());
    let old = old_map
        .as_ref()
        .and_then(|map| rkyv::access::<ArchivedGrassCache, Error>(map).ok());
    let mut report = GrassBakeReport {
        skipped_texture_lists,
        first_invalid_texture,
        initially_hidden_references: references
            .iter()
            .filter(|reference| !reference.enabled)
            .count(),
        unresolved_enable_references: references
            .iter()
            .filter(|reference| reference.unresolved_enable)
            .count(),
        unbounded_references,
        skipped_rules,
        first_invalid_rule,
        modelless_rules,
        ..Default::default()
    };
    let mut cells = Vec::new();
    for archived_land in terrain.cells.iter() {
        check_cancel()?;
        let id = archived_land.cell_id.to_native();
        let Some(&(worldspace_id, grid_x, grid_y)) = coordinates.get(&id) else {
            continue;
        };
        let mut land: CachedLand = rkyv::deserialize::<CachedLand, Error>(archived_land)?;
        // Restore before validation: malformed paint must not discard healthy layers.
        if let Some(records) = land_inputs.get(&id) {
            match selection::restore_layers(&mut land, records) {
                Ok((_, omitted)) => {
                    report.omitted_selection_items += omitted;
                    if omitted != 0 {
                        report.first_invalid_selection_cell.get_or_insert(id);
                    }
                }
                Err(error) => {
                    report.skipped_cells += 1;
                    if report.first_invalid_cell.is_none() {
                        eprintln!("grass selection skipped malformed LAND cell {id:08X}: {error}");
                    }
                    report.first_invalid_cell.get_or_insert(id);
                    continue;
                }
            }
        }
        if validate_terrain(&land).is_err() {
            report.skipped_cells += 1;
            report.first_invalid_cell.get_or_insert(id);
            continue;
        }
        let mut quadrants: Vec<_> = (0..4)
            .map(|quadrant| GrassQuadrant {
                quadrant,
                layers: selection_layers(&land, quadrant, &links, &|id| {
                    id == u32::MAX || texture_bindings.get(&id).copied().unwrap_or(false)
                }),
                exclusion: Vec::new(),
            })
            .collect();
        if quadrants.iter().all(|quadrant| quadrant.layers.is_empty()) {
            continue;
        }
        let mut refs = refs_by_cell.get(&id).cloned().unwrap_or_default();
        refs.extend(
            skinned_by_world
                .get(&worldspace_id)
                .into_iter()
                .flatten()
                .copied(),
        );
        refs.sort_unstable_by_key(|reference| reference.id);
        let mut model_inputs = Vec::with_capacity(refs.len());
        for reference in &refs {
            model_inputs.push(geometry.fingerprint(&reference.model_path));
        }
        let mut hash = input_hasher(
            &land,
            (worldspace_id, grid_x, grid_y),
            &refs,
            &links,
            &rule_inputs,
            land_inputs.get(&id),
            &model_inputs,
        )?;
        // The inspection recipe retires older masks that expanded duplicate scene nodes.
        hash.update(b"selected-scene unique-node inspection v1\0");
        let hash = hash.finalize().into();
        if let Some(previous) = old.and_then(|cache| {
            cache
                .cells
                .binary_search_by_key(&id, |cell| cell.cell_id.to_native())
                .ok()
                .map(|index| &cache.cells[index])
        }) && previous.input_hash.iter().copied().eq(hash)
        {
            let cell = rkyv::deserialize::<GrassCell, Error>(previous)?;
            for failure in &cell.obstruction_failures {
                // Reused failures memoize only an unambiguous visibility variant.
                let mut variants = refs
                    .iter()
                    .filter(|reference| reference.model_path == failure.model_path)
                    .map(|reference| &reference.hidden_shapes);
                if let Some(first) = variants.next()
                    && variants.all(|candidate| candidate == first)
                {
                    geometry
                        .failed_meshes
                        .insert((failure.model_path.clone(), first.clone()));
                }
                // A current read error is authoritative, even for an unavailable hash match.
                geometry
                    .failures
                    .entry(failure.model_path.clone())
                    .or_insert_with(|| failure.reason.clone());
            }
            if cell.expanded_sampling & 0x10 != 0 {
                geometry.expanded_cells.insert(id);
            }
            for quadrant in 0..4 {
                if cell.expanded_sampling & (1 << quadrant) != 0 {
                    geometry.expanded_quadrants.insert((id, quadrant));
                }
            }
            cells.push(cell);
            report.reused_cells += 1;
            continue;
        }
        geometry.cell_failures.clear();
        for quadrant in &mut quadrants {
            check_cancel()?;
            quadrant.exclusion = exclusion_mask(
                (id, grid_x, grid_y),
                quadrant.quadrant,
                &land.heights,
                &refs,
                &mut geometry,
            );
        }
        cells.push(GrassCell {
            cell_id: id,
            worldspace_id,
            grid_x,
            grid_y,
            input_hash: hash,
            heights: land.heights,
            normals: land.normals,
            vertex_colors: land.vertex_colors,
            water_height: land.water_height,
            quadrants,
            obstruction_failures: geometry
                .cell_failures
                .iter()
                .map(|path| GrassObstructionFailure {
                    model_path: path.clone(),
                    reason: geometry.failures[path].clone(),
                })
                .collect(),
            expanded_sampling: (u8::from(geometry.expanded_cells.contains(&id)) << 4)
                | (0..4).fold(0, |bits, quadrant| {
                    bits | (u8::from(geometry.expanded_quadrants.contains(&(id, quadrant)))
                        << quadrant)
                }),
        });
        report.baked_cells += 1;
    }
    check_cancel()?;
    cells.sort_unstable_by_key(|cell| cell.cell_id);
    let bytes = rkyv::to_bytes::<Error>(&GrassCache {
        version: GRASS_CACHE_VERSION,
        types,
        cells,
    })?;
    let archived = rkyv::access::<ArchivedGrassCache, Error>(&bytes)?;
    validate_archived_cache(archived).map_err(|message| color_eyre::eyre::eyre!(message))?;
    geometry.validate_sources()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Never mutate a previous pack's inode through a staged hard link.
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    let mut file = File::create(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    validate_grass_cache(path)?;
    report.skipped_obstruction_models = geometry.failures.len();
    report.expanded_sampling_cells = geometry.expanded_cells.len();
    report.expanded_sampling_quadrants = geometry.expanded_quadrants.len();
    #[cfg(test)]
    {
        report.geometry_decodes = geometry.decodes;
    }
    report.obstruction_diagnostics = geometry
        .failures
        .into_iter()
        .take(32)
        .map(|(path, reason)| format!("obstruction {path}: {reason}"))
        .collect();
    Ok(report)
}

/// Map and validate structural and semantic fields before direct runtime reads or incremental reuse.
pub fn validate_grass_cache(path: &Path) -> Result<Mmap> {
    let file = File::open(path)?;
    ensure!(file.metadata()?.len() > 0, "empty grass cache");
    // SAFETY: the file is opened read-only and the mapping is only read, through the validated
    // archive access below. The converter writes a new cache to a separate staging path and
    // publishes it whole, so it does not truncate a cache file it has mapped. As with any
    // mapping, another process that truncates the file in place is outside this contract.
    let map = unsafe { Mmap::map(&file)? };
    let cache = rkyv::access::<ArchivedGrassCache, Error>(&map).wrap_err("invalid grass cache")?;
    validate_archived_cache(cache).map_err(|message| color_eyre::eyre::eyre!(message))?;
    Ok(map)
}

/// Keep resolved GRAS slots even when their optional model or rule is unusable.
/// Empty model paths mark non-emitting placeholders; decodable sampling fields remain intact.
fn read_types(conn: &Connection) -> Result<(Vec<GrassType>, usize, usize, Option<u32>)> {
    let mut statement = conn.prepare("SELECT id,model_path,density,min_slope,max_slope,units_from_water,water_comparison,position_range,height_range,color_range,wave_period,flags FROM grass_types ORDER BY id")?;
    let mut rows = statement.query([])?;
    let mut types = Vec::new();
    let mut skipped = 0;
    let mut modelless = 0;
    let mut first_invalid = None;
    while let Some(row) = rows.next()? {
        let mut complete = true;
        // The reference zero-initializes absent DATA. Retain every decodable field,
        // including model-less species' water/slope filters and conditional draws.
        macro_rules! field {
            ($index:expr, $default:expr) => {
                match row.get($index) {
                    Ok(Some(value)) => value,
                    _ => {
                        complete = false;
                        $default
                    }
                }
            };
        }
        let Ok(id) = row.get::<_, u32>(0) else {
            skipped += 1;
            continue;
        };
        let model_path: String = row
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .unwrap_or_default();
        let without_model = model_path.is_empty();
        let mut rule = GrassType {
            form_id: id,
            model_path,
            density: field!(2, 0u8),
            min_slope: field!(3, 0u8),
            max_slope: field!(4, 0u8),
            units_from_water: field!(5, 0u16),
            water_comparison: field!(6, 0u32),
            position_range: field!(7, 0.0f32),
            height_range: field!(8, 0.0f32),
            color_range: field!(9, 0.0f32),
            wave_period: field!(10, 0.0f32),
            flags: field!(11, 0u8),
        };
        let valid = complete
            && id != 0
            && rule.position_range >= 0.0
            && rule.water_comparison <= 7
            && [
                rule.position_range,
                rule.height_range,
                rule.color_range,
                rule.wave_period,
            ]
            .iter()
            .all(|value| value.is_finite());
        if without_model {
            modelless += 1;
        } else if !valid {
            skipped += 1;
            first_invalid.get_or_insert(id);
        }
        if id == 0 {
            continue;
        }
        if !valid || without_model {
            rule.model_path.clear();
            // Invalid scalars cannot be sampled safely. Negative spacing already
            // yields no grid; water modes above five are unconstrained in spud.
            rule.water_comparison = rule.water_comparison.min(7);
            for value in [
                &mut rule.position_range,
                &mut rule.height_range,
                &mut rule.color_range,
                &mut rule.wave_period,
            ] {
                if !value.is_finite() {
                    *value = 0.0;
                }
            }
        }
        types.push(rule);
    }
    Ok((types, skipped, modelless, first_invalid))
}

/// Collect eligible exterior obstruction bounds and count references lacking valid bounds.
fn read_reference_bounds(conn: &Connection) -> Result<(Vec<ReferenceBounds>, usize)> {
    let hidden_shapes = read_mods_hidden_shapes(conn)?;
    let mut enable = enable::EnableStates::new(conn)?;
    let mut statement = conn.prepare("SELECT s.model_path,base.record_type FROM \"references\" r JOIN statics s ON s.id=r.base_form_id LEFT JOIN records base ON base.form_id=r.base_form_id WHERE r.is_exterior=1 AND (COALESCE(s.flags,0) & ?1)=0 AND COALESCE(s.bounds_valid,0)=0")?;
    let mut unbounded = 0;
    for row in statement.query_map([MARKER_RECORD_FLAG], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
        ))
    })? {
        let (model, kind) = row?;
        if obstruction_model_path(model.as_deref(), kind.as_deref()).is_some() {
            unbounded += 1;
        }
    }
    let mut statement = conn.prepare("SELECT r.id,r.base_form_id,COALESCE(r.worldspace_id,c.worldspace_id),r.pos_x,r.pos_y,r.pos_z,r.rot_x,r.rot_y,r.rot_z,r.scale,s.bounds_min_x,s.bounds_min_y,s.bounds_min_z,s.bounds_max_x,s.bounds_max_y,s.bounds_max_z,rec.data,s.model_path,base.record_type FROM \"references\" r JOIN statics s ON s.id=r.base_form_id JOIN cells c ON c.id=r.cell_id LEFT JOIN records rec ON rec.form_id=r.id LEFT JOIN records base ON base.form_id=r.base_form_id WHERE r.is_exterior=1 AND (COALESCE(s.flags,0) & ?1)=0 AND s.bounds_valid=1 AND COALESCE(r.worldspace_id,c.worldspace_id) IS NOT NULL ORDER BY r.id")?;
    let mut references = Vec::new();
    for row in statement.query_map([MARKER_RECORD_FLAG], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            [row.get::<_, f32>(3)?, row.get(4)?, row.get(5)?],
            [row.get::<_, f32>(6)?, row.get(7)?, row.get(8)?],
            row.get::<_, f32>(9)?,
            [row.get::<_, f32>(10)?, row.get(11)?, row.get(12)?],
            [row.get::<_, f32>(13)?, row.get(14)?, row.get(15)?],
            row.get::<_, Option<Vec<u8>>>(16)?,
            row.get::<_, Option<String>>(17)?,
            row.get::<_, Option<String>>(18)?,
        ))
    })? {
        let (id, base_id, worldspace, position, rotation, scale, min, max, raw, model, kind) = row?;
        let Some(model_path) = obstruction_model_path(model.as_deref(), kind.as_deref()) else {
            continue;
        };
        let local = shared::Bounds3 { min, max };
        if !local.is_finite_and_ordered()
            || position
                .iter()
                .chain(rotation.iter())
                .any(|value| !value.is_finite())
            || !scale.is_finite()
            || scale == 0.0
        {
            continue;
        }
        let mut hash = Sha256::new();
        let (enabled, enable_hash, unresolved_enable) = enable.reference(id);
        hash.update(enable_hash);
        for value in position
            .iter()
            .chain(rotation.iter())
            .chain(local.min.iter())
            .chain(local.max.iter())
            .chain(std::iter::once(&scale))
        {
            hash.update(value.to_le_bytes());
        }
        if let Some(raw) = raw {
            hash_bytes(&mut hash, &raw);
        }
        let source_hash = hash.finalize().into();
        let (min, max) = transform_bounds(local, position, rotation, scale);
        if min.iter().chain(max.iter()).any(|value| !value.is_finite()) {
            continue;
        }
        references.push(ReferenceBounds {
            enabled,
            unresolved_enable,
            potential_skin: false,
            id,
            base_id,
            worldspace,
            min,
            max,
            position,
            rotation,
            scale,
            model_path,
            source_hash,
            hidden_shapes: if kind.as_deref() == Some("STAT") {
                hidden_shapes.get(&base_id).cloned().unwrap_or_default()
            } else {
                Vec::new()
            },
        });
    }
    Ok((references, unbounded))
}

/// Resolve only static-like obstruction bases after authoritative header filtering.
fn obstruction_model_path(model: Option<&str>, kind: Option<&str>) -> Option<String> {
    if !matches!(
        kind,
        Some("STAT" | "MSTT" | "ACTI" | "FURN" | "CONT" | "DOOR")
    ) {
        return None;
    }
    let path = canonical_asset_path(model?, AssetKind::Mesh, "glb").ok()?;
    // The queries already exclude Is Marker using the winning statics.flags header.
    // Keep path and primitive visibility exclusions for unflagged editor geometry too.
    (!path.starts_with("meshes/markers/") && !path.starts_with("meshes/effects/")).then_some(path)
}

/// Rotate a vector by the runtime quaternion without changing its coordinate basis.
fn rotate_vector(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let t = cross([q[0], q[1], q[2]], v).map(|value| value * 2.0);
    let c = cross([q[0], q[1], q[2]], t);
    std::array::from_fn(|axis| v[axis] + q[3] * t[axis] + c[axis])
}

/// Database bounds are glTF Y-up; apply the exact runtime quaternion then return Creation XY/Z.
fn transform_bounds(
    bounds: shared::Bounds3,
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
) -> ([f64; 3], [f64; 3]) {
    let q = shared::coordinates::creation_euler_to_runtime_quaternion(rotation);
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for bits in 0..8 {
        let v: [f32; 3] = std::array::from_fn(|axis| {
            if bits & (1 << axis) == 0 {
                bounds.min[axis] * scale
            } else {
                bounds.max[axis] * scale
            }
        });
        let rotated = rotate_vector(q, v);
        let creation = shared::coordinates::runtime_to_creation_vector(rotated);
        for axis in 0..3 {
            // Preserve a small footprint when the absolute world position has a large f32 ULP.
            let value = f64::from(creation[axis]) + f64::from(position[axis]);
            min[axis] = min[axis].min(value);
            max[axis] = max[axis].max(value);
        }
    }
    (min, max)
}

/// Check cached terrain dimensions, finite heights and valid blend samples before baking.
fn validate_terrain(land: &CachedLand) -> Result<()> {
    let side = usize::from(shared::LAND_SIDE);
    ensure!(
        land.width == shared::LAND_SIDE
            && land.height == shared::LAND_SIDE
            && land.heights.len() == side * side
            && land.heights.iter().all(|value| value.is_finite())
            && land.water_height.is_none_or(f32::is_finite)
            && (land.normals.is_empty() || land.normals.len() == side * side * 3)
            && (land.vertex_colors.is_empty() || land.vertex_colors.len() == side * side * 3),
        "invalid terrain height field"
    );
    for layer in &land.layers {
        ensure!(
            layer.quadrant < 4
                && layer.weights.iter().all(|weight| usize::from(weight.vertex)
                    < GRASS_BLEND_SIDE * GRASS_BLEND_SIDE
                    && weight.opacity.is_finite()
                    && (0.0..=1.0).contains(&weight.opacity)),
            "invalid terrain blend field"
        );
    }
    Ok(())
}

/// Preserve transformed painted opacity and base-first selection, following spud (MIT, chasmlol/spud).
/// Five closed bindings use separately merged weights, including grassless texture coverage.
/// See third-party/spud/LICENSE for the ported selection behavior notice.
#[cfg(test)]
fn blend_layers(
    land: &CachedLand,
    quadrant: u8,
    links: &BTreeMap<u32, Vec<u32>>,
) -> Vec<GrassLayer> {
    selection_layers(land, quadrant, links, &|id| id != 0)
}

/// Project bindings only for LTEX records that name an available TXST.
fn selection_layers(
    land: &CachedLand,
    quadrant: u8,
    links: &BTreeMap<u32, Vec<u32>>,
    has_texture_set: &impl Fn(u32) -> bool,
) -> Vec<GrassLayer> {
    let samples = GRASS_BLEND_SIDE * GRASS_BLEND_SIDE;
    let base = land.layers.iter().find(|layer| {
        layer.quadrant == quadrant && layer.is_base && has_texture_set(layer.texture_form_id)
    });
    // Follow spud's importer quadrant transformation (MIT, chasmlol/spud).
    // Close slots first; bindings retain that order while weights merge on a copy.
    let mut source: Vec<_> = land
        .layers
        .iter()
        .filter(|layer| layer.quadrant == quadrant && !layer.is_base)
        .collect();
    source.sort_by_key(|layer| layer.layer);
    source.truncate(6);
    let mut bound: Vec<_> = source.iter().map(|layer| layer.texture_form_id).collect();
    bound.resize(6, 0);
    let mut fields = vec![vec![0u8; samples]; 6];
    for (slot, layer) in source.iter().enumerate() {
        // Slot 5 can bind a texture but carries no authored VTXT weights.
        if layer.layer < 5 {
            for weight in &layer.weights {
                fields[slot][usize::from(weight.vertex)] = (weight.opacity * 255.0) as u8;
            }
        }
    }
    let mut merged = bound.clone();
    let mut index = 0;
    while index < 6 {
        if merged[index] == 0 || !has_texture_set(merged[index]) {
            index += 1;
            continue;
        }
        let mut later = index + 1;
        while index < 5 && later < 6 {
            if merged[later] == merged[index] {
                let (before, after) = fields.split_at_mut(later);
                for (target, source) in before[index].iter_mut().zip(&after[0]) {
                    *target = target.wrapping_add(*source);
                }
                merged.remove(later);
                merged.push(0);
                fields.remove(later);
                fields.push(vec![0; samples]);
            } else {
                later += 1;
            }
        }
        if base.is_some_and(|base| base.texture_form_id == merged[index]) {
            merged.remove(index);
            merged.push(0);
            fields.remove(index);
            fields.push(vec![0; samples]);
        } else {
            index += 1;
        }
    }
    let mut layers = Vec::new();
    if let Some(base) = base {
        let weights = (0..samples)
            .map(|vertex| {
                let sum: u32 = fields
                    .iter()
                    .take(5)
                    .map(|field| u32::from(field[vertex]))
                    .sum();
                255u32.saturating_sub(sum) as f32 / 255.0
            })
            .collect();
        layers.push(GrassLayer {
            texture_form_id: base.texture_form_id,
            is_base: true,
            grass_ids: links
                .get(&base.texture_form_id)
                .cloned()
                .unwrap_or_default(),
            weights,
        });
    }
    for (slot, texture) in bound.into_iter().take(5).enumerate() {
        let texture = if has_texture_set(texture) { texture } else { 0 };
        layers.push(GrassLayer {
            texture_form_id: texture,
            is_base: false,
            grass_ids: links.get(&texture).cloned().unwrap_or_default(),
            weights: fields[slot]
                .iter()
                .map(|weight| f32::from(*weight) / 255.0)
                .collect(),
        });
    }
    layers.retain(|layer| {
        !layer.grass_ids.is_empty()
            && (layer.is_base || layer.weights.iter().any(|&weight| weight > 0.0))
    });
    layers
}

/// Sample transformed solid triangles at tile centers above rendered terrain heights.
fn exclusion_mask(
    cell: (u32, i32, i32),
    quadrant: u8,
    heights: &[f32],
    refs: &[&ReferenceBounds],
    geometry: &mut GeometryCache,
) -> Vec<u8> {
    let (cell_id, grid_x, grid_y) = cell;
    let mut mask = vec![0; GRASS_EXCLUSION_SIDE * GRASS_EXCLUSION_SIDE / 8];
    let size = f64::from(GRASS_CELL_SIZE) / 2.0;
    let cell_origin = [
        f64::from(grid_x) * f64::from(GRASS_CELL_SIZE),
        f64::from(grid_y) * f64::from(GRASS_CELL_SIZE),
    ];
    let origin = [
        f64::from(quadrant % 2) * size,
        f64::from(quadrant / 2) * size,
    ];
    // The 64 by 64 grid (32 Creation units per sample) is an engineering approximation of the
    // mask resolution, not a value taken from the retail engine.
    let step = size / GRASS_EXCLUSION_SIDE as f64;
    for reference in refs {
        if !reference.enabled {
            continue;
        }
        let raw_misses = |width: f64, local: [f64; 2]| {
            (0..2).any(|axis| {
                reference.max[axis] < cell_origin[axis] + local[axis]
                    || reference.min[axis] > cell_origin[axis] + local[axis] + width
            })
        };
        // The offsets are minima/maxima of exactly the f64 operations used below. Adding
        // the same position then subtracting the same CELL origin preserves enclosure.
        let misses = |bounds: &[DVec3; 2]| {
            (0..2).any(|axis| {
                bounds[1][axis] + f64::from(reference.position[axis]) - cell_origin[axis]
                    < origin[axis]
                    || bounds[0][axis] + f64::from(reference.position[axis]) - cell_origin[axis]
                        > origin[axis] + size
            })
        };
        if !reference.potential_skin && raw_misses(size, origin) {
            continue;
        }
        if reference.potential_skin
            && geometry
                .reference_offsets
                .get(&reference.id)
                .is_some_and(&misses)
        {
            continue;
        }
        let Some(triangles) =
            geometry.get_with_hidden(&reference.model_path, &reference.hidden_shapes)
        else {
            continue;
        };
        let q = shared::coordinates::creation_euler_to_runtime_quaternion(reference.rotation);
        let rotation = glam::DQuat::from_array(q.map(f64::from));
        if reference.potential_skin {
            let bounds = geometry
                .reference_offsets
                .entry(reference.id)
                .or_insert_with(|| {
                    let mut bounds = [DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)];
                    for &p in triangles.iter().flatten() {
                        let p = rotation * (p * f64::from(reference.scale));
                        let p = DVec3::new(p.x, -p.z, p.y);
                        bounds[0] = bounds[0].min(p);
                        bounds[1] = bounds[1].max(p);
                    }
                    bounds
                });
            if misses(bounds) {
                continue;
            }
            if raw_misses(f64::from(GRASS_CELL_SIZE), [0.0; 2]) {
                geometry.expanded_cells.insert(cell_id);
            }
            if raw_misses(size, origin) {
                geometry.expanded_quadrants.insert((cell_id, quadrant));
            }
        }
        for triangle in triangles.iter() {
            // Work relative to this CELL before adding local vertices, preserving far-world detail.
            let triangle = triangle.map(|p| {
                let p = rotation * (p * f64::from(reference.scale));
                DVec3::new(
                    p.x + f64::from(reference.position[0]) - cell_origin[0],
                    -p.z + f64::from(reference.position[1]) - cell_origin[1],
                    p.y + f64::from(reference.position[2]),
                )
            });
            let min = triangle[0].min(triangle[1]).min(triangle[2]);
            let max = triangle[0].max(triangle[1]).max(triangle[2]);
            let low = [min.x, min.y];
            let high = [max.x, max.y];
            let first: [usize; 2] = std::array::from_fn(|axis| {
                ((low[axis] - origin[axis]) / step - 0.5)
                    .ceil()
                    .clamp(0.0, GRASS_EXCLUSION_SIDE as f64) as usize
            });
            let end: [usize; 2] = std::array::from_fn(|axis| {
                (((high[axis] - origin[axis]) / step - 0.5).floor() + 1.0)
                    .clamp(0.0, GRASS_EXCLUSION_SIDE as f64) as usize
            });
            for y in first[1]..end[1] {
                for x in first[0]..end[0] {
                    let bit = y * GRASS_EXCLUSION_SIDE + x;
                    if mask[bit / 8] & (1 << (bit % 8)) != 0 {
                        continue;
                    }
                    let px = origin[0] + (x as f64 + 0.5) * step;
                    let py = origin[1] + (y as f64 + 0.5) * step;
                    let pz = f64::from(terrain_height(heights, px as f32, py as f32));
                    if triangle_covers(triangle, DVec3::new(px, py, pz)) {
                        mask[bit / 8] |= 1 << (bit % 8);
                    }
                }
            }
        }
    }
    mask
}

/// Old precacher upward-ray policy, expressed in Creation XY/Z coordinates.
/// No retail trace or REA registry entry (RF-GRASS) sources the -8 and +8192 window. The values
/// come from the earlier precacher's policy and stay unverified against the retail engine.
fn triangle_covers(t: Triangle, p: DVec3) -> bool {
    let a = t[1] - t[0];
    let b = t[2] - t[0];
    let c = p - t[0];
    let det = a.x * b.y - a.y * b.x;
    if det.abs() < 1e-5 {
        return false;
    }
    let u = (c.x * b.y - c.y * b.x) / det;
    let v = (a.x * c.y - a.y * c.x) / det;
    if u < -0.0001 || v < -0.0001 || u + v > 1.0001 {
        return false;
    }
    let height = t[0].z + u * a.z + v * b.z;
    height >= p.z - 8.0 && height <= p.z + 8192.0
}

/// Interpolate the rendered LAND triangle topology at a cell-relative Creation XY position.
fn terrain_height(heights: &[f32], x: f32, y: f32) -> f32 {
    let side = 33;
    let sample = [x, y].map(|value| (value / (GRASS_CELL_SIZE / 32.0)).clamp(0.0, 32.0));
    let [ix, iy] = sample.map(|value| (value as usize).min(31));
    let [fx, fy] = [sample[0] - ix as f32, sample[1] - iy as f32];
    let a = heights[iy * side + ix];
    let b = heights[iy * side + ix + 1];
    let c = heights[(iy + 1) * side + ix];
    let d = heights[(iy + 1) * side + ix + 1];
    // The same [a,b,c, b,d,c] topology as the rendered LAND mesh and runtime placement.
    if fx + fy <= 1.0 {
        a + fx * (b - a) + fy * (c - a)
    } else {
        d + (1.0 - fx) * (c - d) + (1.0 - fy) * (b - d)
    }
}

/// Start a hash of terrain, linked rules and conservative candidate identities and contents.
fn input_hasher(
    land: &CachedLand,
    coordinate: (u32, i32, i32),
    refs: &[&ReferenceBounds],
    links: &BTreeMap<u32, Vec<u32>>,
    inputs: &BTreeMap<u32, Vec<u8>>,
    raw_land: Option<&Vec<Vec<u8>>>,
    models: &[[u8; 32]],
) -> Result<Sha256> {
    let mut hash = Sha256::new();
    hash.update(b"MudCrab grass cell inputs\0");
    hash.update(b"spud-original-slots-placeholder-rules-local-item-recovery-v5");
    hash.update(GRASS_CACHE_VERSION.to_le_bytes());
    hash.update(coordinate.0.to_le_bytes());
    hash.update(coordinate.1.to_le_bytes());
    hash.update(coordinate.2.to_le_bytes());
    hash_bytes(&mut hash, &rkyv::to_bytes::<Error>(land)?);
    hash.update((raw_land.map_or(0, Vec::len) as u64).to_le_bytes());
    if let Some(records) = raw_land {
        for bytes in records {
            hash_bytes(&mut hash, bytes);
        }
    }
    let textures: std::collections::BTreeSet<_> = land
        .layers
        .iter()
        .map(|layer| layer.texture_form_id)
        .collect();
    hash.update((textures.len() as u64).to_le_bytes());
    for id in textures {
        hash.update(id.to_le_bytes());
        hash.update([u8::from(inputs.contains_key(&id))]);
        if let Some(bytes) = inputs.get(&id) {
            hash_bytes(&mut hash, bytes);
        }
        hash.update((links.get(&id).map_or(0, Vec::len) as u64).to_le_bytes());
        if let Some(grasses) = links.get(&id) {
            for grass in grasses {
                hash.update(grass.to_le_bytes());
                hash.update([u8::from(inputs.contains_key(grass))]);
                if let Some(bytes) = inputs.get(grass) {
                    hash_bytes(&mut hash, bytes);
                }
            }
        }
    }
    hash.update((refs.len() as u64).to_le_bytes());
    for (reference, model_hash) in refs.iter().zip(models) {
        hash_bytes(&mut hash, reference.model_path.as_bytes());
        hash.update(model_hash);
        hash.update(reference.id.to_le_bytes());
        hash.update(reference.base_id.to_le_bytes());
        hash.update(reference.source_hash);
        hash.update((reference.hidden_shapes.len() as u64).to_le_bytes());
        for index in &reference.hidden_shapes {
            hash.update(index.to_le_bytes());
        }
        for value in reference.min.iter().chain(reference.max.iter()) {
            hash.update(value.to_le_bytes());
        }
    }
    Ok(hash)
}

/// Hash a length-prefixed byte sequence to preserve input boundaries.
fn hash_bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::{exporter::create_tables, records::RawRecord};

    /// Write an unindexed opaque or blended triangle fixture.
    pub(super) fn write_mesh(root: &Path, triangles: &[[[f32; 3]; 3]], alpha: &str) {
        write_mesh_options(root, triangles, alpha, false);
    }

    /// Write triangle geometry with optional reversed indices and nested node transforms.
    fn write_mesh_options(root: &Path, triangles: &[[[f32; 3]; 3]], alpha: &str, indexed: bool) {
        let positions: Vec<_> = triangles.iter().flatten().copied().collect();
        let min: [f32; 3] = std::array::from_fn(|axis| {
            positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::INFINITY, f32::min)
        });
        let max: [f32; 3] = std::array::from_fn(|axis| {
            positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::NEG_INFINITY, f32::max)
        });
        let mut bin: Vec<_> = positions
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut value = serde_json::json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"meshes":[{"primitives":[{"attributes":{"POSITION":0},"material":0}]}],"materials":[{"alphaMode":alpha}],"buffers":[{"byteLength":bin.len()}],"bufferViews":[{"buffer":0,"byteLength":bin.len()}],"accessors":[{"bufferView":0,"componentType":5126,"count":positions.len(),"type":"VEC3","min":min,"max":max}]});
        if indexed {
            let offset = bin.len();
            for index in (0..positions.len() as u32).rev() {
                bin.extend(index.to_le_bytes());
            }
            value["buffers"][0]["byteLength"] = bin.len().into();
            value["bufferViews"].as_array_mut().unwrap().push(
                serde_json::json!({"buffer":0,"byteOffset":offset,"byteLength":positions.len()*4}),
            );
            value["accessors"].as_array_mut().unwrap().push(serde_json::json!({"bufferView":1,"componentType":5125,"count":positions.len(),"type":"SCALAR"}));
            value["meshes"][0]["primitives"][0]["indices"] = 1.into();
            value["nodes"] = serde_json::json!([{"translation":[3,5,-7],"children":[1]},{"mesh":0,"scale":[2,2,2]}]);
        }
        write_glb(root, &value, bin);
    }

    /// Serialize a synthetic GLB with padded JSON and embedded binary chunks.
    fn write_glb(root: &Path, value: &serde_json::Value, mut bin: Vec<u8>) {
        let mut json = serde_json::to_vec(value).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"glTF");
        bytes.extend(2u32.to_le_bytes());
        bytes.extend(((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        bytes.extend((json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"JSON");
        bytes.extend(json);
        bytes.extend((bin.len() as u32).to_le_bytes());
        bytes.extend(0x004e4942u32.to_le_bytes());
        bytes.extend(bin);
        std::fs::create_dir_all(root.join("meshes")).unwrap();
        std::fs::write(root.join("meshes/blocker.glb"), bytes).unwrap();
    }

    /// Modify a generated GLB document and binary payload while preserving container framing.
    pub(super) fn edit_mesh(root: &Path, edit: impl FnOnce(&mut serde_json::Value, &mut Vec<u8>)) {
        let bytes = std::fs::read(root.join("meshes/blocker.glb")).unwrap();
        let glb = gltf::binary::Glb::from_slice(&bytes).unwrap();
        let mut json = serde_json::from_slice(&glb.json).unwrap();
        let mut bin = glb.bin.unwrap().into_owned();
        edit(&mut json, &mut bin);
        write_glb(root, &json, bin);
    }

    /// Add two reordered joints with distinct bind transforms and two weighted influences.
    pub(super) fn add_skin(root: &Path, offset_positions: bool) {
        edit_mesh(root, |json, bin| {
            let count = json["accessors"][0]["count"].as_u64().unwrap() as usize;
            if offset_positions {
                for position in bin[..count * 12].as_chunks_mut::<12>().0 {
                    for (bytes, offset) in position
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip([17.0, 11.0, -9.0])
                    {
                        let value = f32::from_le_bytes(*bytes) - offset;
                        bytes.copy_from_slice(&value.to_le_bytes());
                    }
                }
                for key in ["min", "max"] {
                    for (axis, offset) in [17.0, 11.0, -9.0].into_iter().enumerate() {
                        let value = json["accessors"][0][key][axis].as_f64().unwrap() - offset;
                        json["accessors"][0][key][axis] = value.into();
                    }
                }
            }
            let joint_offset = bin.len();
            bin.extend(
                [0u16, 1, 0, 0]
                    .repeat(count)
                    .into_iter()
                    .flat_map(u16::to_le_bytes),
            );
            let weight_offset = bin.len();
            bin.extend(
                [0.25f32, 0.75, 0.0, 0.0]
                    .repeat(count)
                    .into_iter()
                    .flat_map(f32::to_le_bytes),
            );
            let bind_offset = bin.len();
            // Global joint translations are [20,10,-6] and [-12,23,4]. The weighted
            // joint * inverse-bind translations are .25*[5,2,-3]+.75*[21,14,-11].
            for translation in [[-15.0, -8.0, 3.0], [33.0, -9.0, -15.0]] {
                bin.extend(
                    glam::Mat4::from_translation(glam::Vec3::from_array(translation))
                        .to_cols_array()
                        .into_iter()
                        .flat_map(f32::to_le_bytes),
                );
            }
            json["bufferViews"].as_array_mut().unwrap().extend([
                serde_json::json!({"buffer":0,"byteOffset":joint_offset,"byteLength":count*8}),
                serde_json::json!({"buffer":0,"byteOffset":weight_offset,"byteLength":count*16}),
                serde_json::json!({"buffer":0,"byteOffset":bind_offset,"byteLength":128}),
            ]);
            json["accessors"].as_array_mut().unwrap().extend([
                serde_json::json!({"bufferView":1,"componentType":5123,"count":count,"type":"VEC4"}),
                serde_json::json!({"bufferView":2,"componentType":5126,"count":count,"type":"VEC4"}),
                serde_json::json!({"bufferView":3,"componentType":5126,"count":2,"type":"MAT4"}),
            ]);
            json["buffers"][0]["byteLength"] = bin.len().into();
            json["meshes"][0]["primitives"][0]["attributes"]["JOINTS_0"] = 1.into();
            json["meshes"][0]["primitives"][0]["attributes"]["WEIGHTS_0"] = 2.into();
            json["nodes"] = serde_json::json!([
                {"mesh":0,"skin":0,"translation":if offset_positions {[600,700,800]} else {[0,0,0]}},
                {"translation":[3,5,7],"children":[2,3]},
                {"translation":[-15,18,-3]}, {"translation":[17,5,-13]}
            ]);
            json["scenes"][0]["nodes"] = serde_json::json!([0, 1]);
            json["skins"] = serde_json::json!([{"joints":[3,2],"inverseBindMatrices":3}]);
        });
    }

    /// Skinning must use the rest joints and inverse binds, ignoring the mesh node transform.
    #[test]
    fn skinned_obstruction_matches_equivalent_static_masks() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        let expected = read(&output);
        let path = dir.path().join("meshes/blocker.glb");
        let static_triangles = model_triangles(&std::fs::read(&path).unwrap()).unwrap();
        add_skin(dir.path(), true);
        let report = write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        assert_eq!(
            report.skipped_obstruction_models, 0,
            "valid skin must obstruct"
        );
        assert_eq!(
            model_triangles(&std::fs::read(&path).unwrap()).unwrap(),
            static_triangles
        );
        for (actual, expected) in read(&output).cells.iter().zip(&expected.cells) {
            assert_eq!(actual.quadrants, expected.quadrants);
        }
        assert_ne!(expected.cells[1].quadrants[0].exclusion[16] & 1, 0);
        let bytes = std::fs::read(&output).unwrap();
        write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
    }

    /// Unusable skin data falls back to static triangles; a bad model never stops neighbours.
    #[test]
    fn skinned_obstruction_bad_skin_falls_back_and_bad_positions_skip_locally() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        let expected = read(&output);
        for case in 0..10 {
            write_box(dir.path(), [-64.0, -16.0, -48.0], [64.0, 32.0, 48.0]);
            add_skin(dir.path(), false);
            edit_mesh(dir.path(), |json, bin| match case {
                0 => json["skins"][0]["joints"] = serde_json::json!([999]),
                1 => {
                    let offset = json["bufferViews"][3]["byteOffset"].as_u64().unwrap() as usize;
                    bin[offset..offset + 64].fill(0);
                }
                2 => {
                    let offset = json["bufferViews"][1]["byteOffset"].as_u64().unwrap() as usize;
                    bin[offset..offset + 2].copy_from_slice(&99u16.to_le_bytes());
                }
                3 => json["bufferViews"][2]["byteLength"] = 1.into(),
                4 => {
                    json["skins"][0].as_object_mut().unwrap().remove("joints");
                }
                5 => json["accessors"][1]["bufferView"] = 999.into(),
                6 => json["accessors"][2]["count"] = 0.into(),
                // A shared dependency must stay intact: POSITION cannot be removed just
                // because a malformed JOINTS semantic also references its accessor.
                7 => json["meshes"][0]["primitives"][0]["attributes"]["JOINTS_0"] = 0.into(),
                8 => {
                    json["accessors"][1]
                        .as_object_mut()
                        .unwrap()
                        .remove("componentType");
                }
                9 => json["bufferViews"][1]["buffer"] = 999.into(),
                _ => unreachable!(),
            });
            let report = write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
            assert_eq!(
                (report.baked_cells, report.skipped_obstruction_models),
                (3, 0),
                "fallback case {case}"
            );
            for (actual, expected) in read(&output).cells.iter().zip(&expected.cells) {
                assert_eq!(actual.quadrants, expected.quadrants, "fallback case {case}");
            }
        }
        // Retain a healthy second model when neither the skin nor static positions are usable.
        write_box(dir.path(), [-64.0, -16.0, -48.0], [64.0, 32.0, 48.0]);
        std::fs::copy(
            dir.path().join("meshes/blocker.glb"),
            dir.path().join("meshes/healthy.glb"),
        )
        .unwrap();
        conn.execute_batch("INSERT INTO records(form_id,record_type,load_order,data) VALUES(41,'STAT',0,x'00');
            INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) VALUES(41,'meshes/healthy.glb',0,-64,-16,-48,64,32,48,1);
            INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(51,11,1,41,1,4090,64,0,0,0,0,2);").unwrap();
        add_skin(dir.path(), false);
        edit_mesh(dir.path(), |json, _| {
            json["skins"][0]["joints"] = serde_json::json!([999]);
            json["bufferViews"][0]["byteLength"] = 1.into();
        });
        let report = write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        assert_eq!(
            (report.baked_cells, report.skipped_obstruction_models),
            (3, 1)
        );
        assert_eq!(report.obstruction_diagnostics.len(), 1);
        assert!(report.warnings().is_empty());
        for (actual, expected) in read(&output).cells.iter().zip(&expected.cells) {
            assert_eq!(actual.quadrants, expected.quadrants);
        }
    }

    /// Version-three masks with identical model bytes must rebuild, then reuse deterministically.
    #[test]
    fn skinned_obstruction_version_three_masks_rebuild_before_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        add_skin(dir.path(), true);
        let terrain = dir.path().join("terrain.rkyv");
        let previous = dir.path().join("previous.rkyv");
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        let expected = std::fs::read(&output).unwrap();
        let mut obsolete = read(&output);
        obsolete.version = 3;
        for cell in &mut obsolete.cells {
            for quadrant in &mut cell.quadrants {
                quadrant.exclusion.fill(0);
            }
        }
        std::fs::write(&previous, rkyv::to_bytes::<Error>(&obsolete).unwrap()).unwrap();
        let report =
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &output).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (3, 0));
        assert_eq!(std::fs::read(&output).unwrap(), expected);
        std::fs::copy(&output, &previous).unwrap();
        let report =
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &output).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (0, 3));
        assert_eq!(std::fs::read(&output).unwrap(), expected);
    }

    /// An unreadable declared index array must produce a diagnostic rather than invented geometry.
    #[test]
    fn unreadable_declared_indices_are_reported_instead_of_fabricating_triangles() {
        let dir = tempfile::tempdir().unwrap();
        write_mesh_options(
            dir.path(),
            &[[[1.0, 2.0, 3.0], [4.0, 6.0, 8.0], [10.0, 12.0, 14.0]]],
            "OPAQUE",
            true,
        );
        let path = dir.path().join("meshes/blocker.glb");
        assert_eq!(
            model_triangles(&std::fs::read(&path).unwrap())
                .unwrap()
                .len(),
            1
        );
        edit_mesh(dir.path(), |json, _| {
            json["bufferViews"][1]["byteLength"] = 4.into()
        });
        // The container and JSON are valid; the declared three indices do not fit their view.
        assert!(gltf::Gltf::from_slice(&std::fs::read(&path).unwrap()).is_ok());
        let mut geometry = GeometryCache::new(dir.path()).unwrap();
        assert!(geometry.get("meshes/blocker.glb").is_none());
        assert_eq!(geometry.failures.len(), 1);
    }

    /// Reject a position stride smaller than one element without panicking.
    #[test]
    fn malformed_position_stride_is_reported_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        write_mesh(dir.path(), &hollow_triangles(), "OPAQUE");
        let path = dir.path().join("meshes/blocker.glb");
        assert_eq!(
            model_triangles(&std::fs::read(&path).unwrap())
                .unwrap()
                .len(),
            8
        );
        edit_mesh(dir.path(), |json, _| {
            json["bufferViews"][0]["byteStride"] = 4.into()
        });
        assert!(gltf::Gltf::from_slice(&std::fs::read(&path).unwrap()).is_ok());
        let mut geometry = GeometryCache::new(dir.path()).unwrap();
        assert!(geometry.get("meshes/blocker.glb").is_none());
        assert_eq!(geometry.failures.len(), 1);
    }

    /// Retain valid interleaved and sparse positions, including sparse-only backing data.
    #[test]
    fn accessor_validation_keeps_interleaved_and_sparse_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let triangle = [[1.0, 2.0, 3.0], [4.0, 6.0, 8.0], [10.0, 12.0, 14.0]];
        write_mesh(dir.path(), &[triangle], "OPAQUE");
        edit_mesh(dir.path(), |json, bin| {
            *bin = bin
                .as_chunks::<12>()
                .0
                .iter()
                .flat_map(|position| position.iter().copied().chain([0; 4]))
                .collect();
            json["bufferViews"][0]["byteStride"] = 16.into();
            json["bufferViews"][0]["byteLength"] = bin.len().into();
            json["buffers"][0]["byteLength"] = bin.len().into();
        });
        let path = dir.path().join("meshes/blocker.glb");
        let expected = triangle.map(|p| DVec3::from_array(p.map(f64::from)));
        assert_eq!(
            model_triangles(&std::fs::read(&path).unwrap()).unwrap(),
            [expected]
        );

        for implicit_base in [false, true] {
            write_mesh(dir.path(), &[triangle], "OPAQUE");
            edit_mesh(dir.path(), |json, bin| {
                bin.extend(1u32.to_le_bytes());
                bin.extend([5.0f32, 8.0, 9.0].into_iter().flat_map(f32::to_le_bytes));
                json["buffers"][0]["byteLength"] = bin.len().into();
                json["bufferViews"].as_array_mut().unwrap().extend([
                    serde_json::json!({"buffer":0,"byteOffset":36,"byteLength":4}),
                    serde_json::json!({"buffer":0,"byteOffset":40,"byteLength":12}),
                ]);
                json["accessors"][0]["sparse"] = serde_json::json!({"count":1,
                    "indices":{"bufferView":1,"componentType":5125},
                    "values":{"bufferView":2}});
                if implicit_base {
                    json["accessors"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("bufferView");
                }
            });
            let mut expected = if implicit_base {
                [DVec3::ZERO; 3]
            } else {
                expected
            };
            expected[1] = DVec3::new(5.0, 8.0, 9.0);
            assert_eq!(
                model_triangles(&std::fs::read(&path).unwrap()).unwrap(),
                [expected]
            );
        }
    }

    /// Reject malformed accessor ranges and sparse indices before reading triangles.
    #[test]
    fn malformed_accessor_ranges_and_sparse_data_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let triangle = [[1.0, 2.0, 3.0], [4.0, 6.0, 8.0], [10.0, 12.0, 14.0]];
        for case in 0..12 {
            write_mesh(dir.path(), &[triangle], "OPAQUE");
            edit_mesh(dir.path(), |json, bin| match case {
                0 => json["accessors"][0]["count"] = 0.into(),
                1 => json["accessors"][0]["count"] = u64::MAX.into(),
                2 => json["accessors"][0]["byteOffset"] = u64::MAX.into(),
                3 => json["bufferViews"][0]["byteOffset"] = u64::MAX.into(),
                4 => json["buffers"][0]["byteLength"] = 1000.into(),
                5 => json["accessors"][0]["type"] = "VEC2".into(),
                _ => {
                    bin.extend([0u32, 1, 2].into_iter().flat_map(u32::to_le_bytes));
                    bin.extend([1.0f32; 9].into_iter().flat_map(f32::to_le_bytes));
                    json["buffers"][0]["byteLength"] = bin.len().into();
                    json["bufferViews"].as_array_mut().unwrap().extend([
                        serde_json::json!({"buffer":0,"byteOffset":36,"byteLength":12}),
                        serde_json::json!({"buffer":0,"byteOffset":48,"byteLength":36}),
                    ]);
                    json["accessors"][0]["sparse"] = serde_json::json!({"count":3,
                            "indices":{"bufferView":1,"componentType":5125},
                            "values":{"bufferView":2}});
                    match case {
                        6 => json["accessors"][0]["sparse"]["count"] = 0.into(),
                        7 => json["accessors"][0]["sparse"]["count"] = 4.into(),
                        8 => bin[44..48].copy_from_slice(&3u32.to_le_bytes()),
                        9 => bin[40..44].copy_from_slice(&0u32.to_le_bytes()),
                        10 => json["bufferViews"][2]["byteStride"] = 4.into(),
                        11 => {
                            json["accessors"][0]["sparse"]["values"]["byteOffset"] = u64::MAX.into()
                        }
                        _ => unreachable!(),
                    }
                }
            });
            let bytes = std::fs::read(dir.path().join("meshes/blocker.glb")).unwrap();
            assert!(
                model_triangles(&bytes).is_err(),
                "malformed accessor case {case}"
            );
        }
    }

    /// Replacing a staged cache must leave the previous package inode and bytes unchanged.
    #[test]
    fn replacing_a_staged_hard_link_preserves_the_previous_pack() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let previous = dir.path().join("previous.rkyv");
        let staged = dir.path().join("staged.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &previous).unwrap();
        let original = std::fs::read(&previous).unwrap();
        crate::cache::link_or_copy(&previous, &staged).unwrap();
        conn.execute("DELETE FROM landscape_texture_grasses", [])
            .unwrap();
        write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &staged).unwrap();
        assert!(read(&staged).cells.is_empty());
        assert_eq!(std::fs::read(&previous).unwrap(), original);
    }

    /// Skip invalid cached terrain weights while retaining neighboring cells.
    #[test]
    fn malformed_terrain_weights_skip_only_the_bad_cell() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        let map = super::super::cell_cache::validate_cell_cache(&terrain).unwrap();
        let mut cache: shared::CellCache = rkyv::deserialize::<_, Error>(
            rkyv::access::<shared::ArchivedCellCache, Error>(&map).unwrap(),
        )
        .unwrap();
        drop(map);
        cache.cells[0].layers.push(shared::TerrainLayer {
            texture_form_id: 30,
            quadrant: 0,
            layer: 1,
            is_base: false,
            weights: vec![shared::TerrainWeight {
                vertex: 289,
                opacity: 0.5,
            }],
        });
        std::fs::write(&terrain, rkyv::to_bytes::<Error>(&cache).unwrap()).unwrap();
        let report = write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        assert_eq!(report.skipped_cells, 1);
        assert_eq!(report.first_invalid_cell, Some(10));
        assert_eq!(
            read(&output)
                .cells
                .iter()
                .map(|cell| cell.cell_id)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert!(report.warnings()[0].contains("0000000A"));
    }

    /// Publish healthy neighbors despite malformed LAND sampling fields; hash valid edits locally.
    #[test]
    fn land_sampling_fields_roundtrip_invalidate_and_skip_locally() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let first = dir.path().join("first.rkyv");
        let next = dir.path().join("next.rkyv");
        let map = super::super::cell_cache::validate_cell_cache(&terrain).unwrap();
        let mut cache: shared::CellCache = rkyv::deserialize::<_, Error>(
            rkyv::access::<shared::ArchivedCellCache, Error>(&map).unwrap(),
        )
        .unwrap();
        drop(map);
        for land in &mut cache.cells {
            land.normals = [12, -34, 117].repeat(33 * 33);
            land.vertex_colors = [43, 89, 173].repeat(33 * 33);
        }
        std::fs::write(&terrain, rkyv::to_bytes::<Error>(&cache).unwrap()).unwrap();
        write_grass_cache(&conn, dir.path(), &terrain, None, &first).unwrap();
        let output = read(&first);
        assert_eq!(output.cells[0].normals, [12, -34, 117].repeat(33 * 33));
        assert_eq!(output.cells[0].vertex_colors, [43, 89, 173].repeat(33 * 33));
        for colors in [false, true] {
            let land = cache
                .cells
                .iter_mut()
                .find(|land| land.cell_id == 10)
                .unwrap();
            if colors {
                land.vertex_colors[0] += 1;
            } else {
                land.normals[0] += 1;
            }
            std::fs::write(&terrain, rkyv::to_bytes::<Error>(&cache).unwrap()).unwrap();
            let report =
                write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next).unwrap();
            assert_eq!((report.baked_cells, report.reused_cells), (1, 2));
            std::fs::copy(&next, &first).unwrap();
        }
        for colors in [false, true] {
            let mut bad = cache.clone();
            let land = bad
                .cells
                .iter_mut()
                .find(|land| land.cell_id == 11)
                .unwrap();
            if colors {
                land.vertex_colors.pop();
            } else {
                land.normals.pop();
            }
            std::fs::write(&terrain, rkyv::to_bytes::<Error>(&bad).unwrap()).unwrap();
            let report =
                write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next).unwrap();
            assert_eq!(report.skipped_cells, 1);
            assert_eq!(report.first_invalid_cell, Some(11));
            assert!(!report.warnings().is_empty());
            assert_eq!(
                read(&next)
                    .cells
                    .iter()
                    .map(|cell| cell.cell_id)
                    .collect::<Vec<_>>(),
                [10, 12]
            );
        }
    }

    /// Overlapping paint stays raw; a sixth layer does not suppress base residual.
    #[test]
    fn raw_opacity_base_first_and_first_five_painted_layers() {
        let mut land = CachedLand {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![],
            normals: vec![],
            vertex_colors: vec![],
            water_height: None,
            water_type_form_id: None,
            layers: (0..6)
                .map(|slot| shared::TerrainLayer {
                    texture_form_id: 31 + u32::from(slot),
                    quadrant: 0,
                    layer: slot,
                    is_base: false,
                    weights: vec![shared::TerrainWeight {
                        vertex: 117,
                        opacity: if slot < 2 { 0.7 } else { 0.1 },
                    }],
                })
                .collect(),
        };
        land.layers.push(shared::TerrainLayer {
            texture_form_id: 30,
            quadrant: 0,
            layer: 0,
            is_base: true,
            weights: vec![],
        });
        let links = (30..=36).map(|id| (id, vec![id + 100])).collect();
        let layers = blend_layers(&land, 0, &links);
        assert_eq!(layers.len(), 6);
        assert!(layers[0].is_base);
        assert_eq!(layers[0].weights[117], 0.0);
        assert_eq!(layers[1].weights[117], 178.0 / 255.0);
        assert_eq!(layers[2].weights[117], 178.0 / 255.0);
        assert!(layers.iter().all(|layer| layer.texture_form_id != 36));
        for layer in &mut land.layers {
            for weight in &mut layer.weights {
                weight.opacity = 0.1;
            }
        }
        let layers = blend_layers(&land, 0, &links);
        assert!((layers[0].weights[117] - 130.0 / 255.0).abs() < 1e-6);
        for layer in &mut land.layers {
            if !layer.is_base {
                layer.weights = (0..289)
                    .map(|vertex| shared::TerrainWeight {
                        vertex,
                        opacity: 1.0,
                    })
                    .collect();
            }
        }
        let layers = blend_layers(&land, 0, &links);
        assert!(layers[0].is_base);
        assert!(layers[0].weights.iter().all(|weight| *weight == 0.0));
    }

    /// Omit malformed terrain heights while publishing healthy neighboring cells.
    #[test]
    fn malformed_terrain_heights_skip_only_the_bad_cell() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let output = dir.path().join("grass.rkyv");
        let map = super::super::cell_cache::validate_cell_cache(&terrain).unwrap();
        let mut cache: shared::CellCache = rkyv::deserialize::<_, Error>(
            rkyv::access::<shared::ArchivedCellCache, Error>(&map).unwrap(),
        )
        .unwrap();
        drop(map);
        cache
            .cells
            .iter_mut()
            .find(|cell| cell.cell_id == 11)
            .unwrap()
            .heights
            .clear();
        std::fs::write(&terrain, rkyv::to_bytes::<Error>(&cache).unwrap()).unwrap();
        let report = write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        assert_eq!(report.skipped_cells, 1);
        assert_eq!(report.first_invalid_cell, Some(11));
        assert_eq!(
            read(&output)
                .cells
                .iter()
                .map(|cell| cell.cell_id)
                .collect::<Vec<_>>(),
            vec![10, 12]
        );
    }

    /// Write a triangulated box with independently selected axis bounds.
    fn write_box(root: &Path, min: [f32; 3], max: [f32; 3]) {
        let vertices: [[f32; 3]; 8] = std::array::from_fn(|bits| {
            std::array::from_fn(|axis| {
                if bits & (1 << axis) == 0 {
                    min[axis]
                } else {
                    max[axis]
                }
            })
        });
        let indices = [
            [0, 1, 3],
            [0, 3, 2],
            [4, 6, 7],
            [4, 7, 5],
            [0, 4, 5],
            [0, 5, 1],
            [2, 3, 7],
            [2, 7, 6],
            [0, 2, 6],
            [0, 6, 4],
            [1, 5, 7],
            [1, 7, 3],
        ];
        write_mesh(
            root,
            &indices.map(|triangle| triangle.map(|i| vertices[i])),
            "OPAQUE",
        );
    }

    /// Compose nested transforms correctly and account for retained geometry allocation capacity.
    #[test]
    fn indexed_geometry_composes_nested_node_transforms_and_cache_capacity() {
        let dir = tempfile::tempdir().unwrap();
        write_mesh_options(
            dir.path(),
            &[[[1.0, 2.0, 3.0], [4.0, 6.0, 8.0], [10.0, 12.0, 14.0]]],
            "MASK",
            true,
        );
        let mut geometry = GeometryCache::new(dir.path()).unwrap();
        let mesh = geometry.get("meshes/blocker.glb").unwrap();
        assert_eq!(
            mesh[0],
            [
                DVec3::new(23.0, 29.0, 21.0),
                DVec3::new(11.0, 17.0, 9.0),
                DVec3::new(5.0, 9.0, -1.0)
            ]
        );
        assert_eq!(
            geometry.bytes,
            mesh.capacity() * std::mem::size_of::<Triangle>()
        );
        assert!(Arc::ptr_eq(
            &mesh,
            &geometry.get("meshes/blocker.glb").unwrap()
        ));
    }

    /// Return solid wall triangles around a hollow model opening.
    fn hollow_triangles() -> Vec<[[f32; 3]; 3]> {
        [
            (-64.0, -32.0, -48.0, 48.0),
            (32.0, 64.0, -48.0, 48.0),
            (-32.0, 32.0, -48.0, -32.0),
            (-32.0, 32.0, 32.0, 48.0),
        ]
        .into_iter()
        .flat_map(|(x0, x1, z0, z1)| {
            [
                [[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1]],
                [[x0, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]],
            ]
        })
        .collect()
    }

    /// Preserve hollow openings across cells and invalidate masks when model bytes change.
    #[test]
    fn hollow_mesh_keeps_open_space_across_cells_and_model_bytes_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute("UPDATE \"references\" SET scale=1 WHERE id=50", [])
            .unwrap();
        let terrain = dir.path().join("terrain.rkyv");
        let first = dir.path().join("first.rkyv");
        let next = dir.path().join("next.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &first).unwrap();
        assert_ne!(
            read(&first).cells[1].quadrants[0].exclusion[16] & 1,
            0,
            "solid box covers neighbor tile center"
        );
        // Keep DB bounds and records unchanged; only opaque triangles acquire an opening.
        write_mesh(dir.path(), &hollow_triangles(), "OPAQUE");
        let report = write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (2, 1));
        let cache = read(&next);
        assert_eq!(
            cache.cells[0].quadrants[1].exclusion[23] & 128,
            0,
            "opening at world x4080/y80 remains clear"
        );
        assert_eq!(
            cache.cells[1].quadrants[0].exclusion[16] & 1,
            0,
            "opening continues into neighboring cell"
        );
        assert_ne!(
            cache.cells[1].quadrants[0].exclusion[16] & 2,
            0,
            "solid side still blocks x4144/y80"
        );
    }

    /// Excluded or unavailable geometry must never fall back to filled bounding boxes.
    #[test]
    fn vegetation_blended_effects_and_missing_geometry_do_not_blanket_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let output = dir.path().join("grass.rkyv");
        for kind in ["TREE", "FLOR"] {
            conn.execute("INSERT OR REPLACE INTO records(form_id,record_type,load_order,data) VALUES(40,?1,0,x'00')", [kind]).unwrap();
            write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
            assert!(
                read(&output)
                    .cells
                    .iter()
                    .flat_map(|c| &c.quadrants)
                    .all(|q| q.exclusion.iter().all(|&b| b == 0))
            );
        }
        conn.execute("UPDATE records SET record_type='STAT' WHERE form_id=40", [])
            .unwrap();
        write_mesh(dir.path(), &hollow_triangles(), "BLEND");
        write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        assert!(
            read(&output)
                .cells
                .iter()
                .flat_map(|c| &c.quadrants)
                .all(|q| q.exclusion.iter().all(|&b| b == 0))
        );
        for path in ["meshes/effects/test.glb", "meshes/markers/test.glb"] {
            conn.execute("UPDATE statics SET model_path=?1 WHERE id=40", [path])
                .unwrap();
            assert_eq!(
                write_grass_cache(&conn, dir.path(), &terrain, None, &output)
                    .unwrap()
                    .skipped_obstruction_models,
                0
            );
        }
        conn.execute(
            "UPDATE statics SET model_path='meshes/blocker.glb' WHERE id=40",
            [],
        )
        .unwrap();
        std::fs::remove_file(dir.path().join("meshes/blocker.glb")).unwrap();
        let report = write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        assert_eq!(report.skipped_obstruction_models, 1);
        assert_eq!(report.obstruction_diagnostics.len(), 1);
        assert!(
            read(&output)
                .cells
                .iter()
                .flat_map(|c| &c.quadrants)
                .all(|q| q.exclusion.iter().all(|&b| b == 0))
        );
        let previous = dir.path().join("missing.rkyv");
        std::fs::copy(&output, &previous).unwrap();
        write_box(dir.path(), [-64.0, -16.0, -48.0], [64.0, 32.0, 48.0]);
        assert_eq!(
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &output)
                .unwrap()
                .baked_cells,
            2
        );
    }

    /// Build grass-bearing neighboring cells and a separate worldspace with a converted blocker.
    pub(super) fn fixture(root: &Path) -> Connection {
        write_box(root, [-64.0, -16.0, -48.0], [64.0, 32.0, 48.0]);
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch("INSERT INTO cells(id,worldspace_id,grid_x,grid_y,flags) VALUES(10,1,0,0,0),(11,1,1,0,0),(12,2,1,0,0);
            INSERT INTO grass_types(id,model_path,density,min_slope,max_slope,units_from_water,water_comparison,position_range,height_range,color_range,wave_period,flags,load_order) VALUES(21,'meshes/grass.glb',50,0,60,0,0,4,0.2,0.1,1.2,1,0),(22,'meshes/grass2.glb',80,0,80,0,0,2,0.1,0.3,2.1,0,0);
            INSERT INTO texture_sets(id) VALUES(2); INSERT INTO landscape_textures(id,texture_set_id) VALUES(30,2);
            INSERT INTO landscape_texture_grasses(ltex_id,gras_id) VALUES(30,21),(30,22),(30,999);
            INSERT INTO records(form_id,record_type,load_order,data) VALUES(21,'GRAS',0,x'0102'),(22,'GRAS',0,x'0304'),(30,'LTEX',0,x'0506'),(40,'STAT',0,x'00');
            INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid) VALUES(40,'meshes/blocker.glb',0,-64,-16,-48,64,32,48,1);
            INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale) VALUES(50,10,1,40,1,4090,64,0,0,0,0,2);") .unwrap();
        let records: HashMap<_, _> = [(100, 10), (101, 11), (102, 12)]
            .into_iter()
            .map(|(id, cell)| {
                let mut btxt = 30u32.to_le_bytes().to_vec();
                btxt.extend([0, 0, 0, 0]);
                (
                    id,
                    RawRecord {
                        form_id: id,
                        record_type: *b"LAND",
                        flags: 0,
                        subrecords: vec![
                            (b"VHGT".to_vec(), super::super::cell_cache::flat_test_vhgt()),
                            (b"BTXT".to_vec(), btxt),
                        ],
                        cell_form_id: Some(cell),
                        worldspace_form_id: Some(if cell == 12 { 2 } else { 1 }),
                        load_order: 0,
                    },
                )
            })
            .collect();
        super::super::cell_cache::write_cell_cache(&records, &root.join("terrain.rkyv")).unwrap();
        conn
    }

    /// Deserialize a grass cache only after structural and semantic validation.
    pub(super) fn read(path: &Path) -> GrassCache {
        let map = validate_grass_cache(path).unwrap();
        rkyv::deserialize::<GrassCache, Error>(
            rkyv::access::<ArchivedGrassCache, Error>(&map).unwrap(),
        )
        .unwrap()
    }

    /// Newly tagged Havok models invalidate affected warm masks through model identity.
    #[test]
    fn havok_geometry_tags_rebuild_only_affected_warm_grass_masks() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let nif = dummy_content::nif::static_shape_with_bodies_and_material_flags(
            &dummy_content::nif::StaticShape {
                name: "GrassBlocker",
                positions: &[
                    [-64.0, -64.0, 0.0],
                    [64.0, -64.0, 0.0],
                    [64.0, 64.0, 0.0],
                    [-64.0, 64.0, 0.0],
                ],
                normals: &[[0.0, 0.0, 1.0]; 4],
                uvs: &[[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
                indices: &[[0, 1, 2], [0, 2, 3]],
                diffuse: "textures/generated_color.dds",
                normal_texture: "textures/generated_normal.dds",
            },
            &[dummy_content::nif::fixed_box_body("Collision")],
            1 << 31,
            1,
        )
        .unwrap();
        let input = dir.path().join("blocker.nif");
        let model = dir.path().join("meshes/blocker.glb");
        std::fs::write(&input, nif).unwrap();
        let diagnostics = crate::mesh::MeshConverter::inspect_nif(&input).unwrap();
        assert_eq!(
            diagnostics
                .fallback_blocks
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["bhkBoxShape", "bhkCollisionObject", "bhkRigidBody"]
        );
        crate::mesh::MeshConverter::convert_nif_to_glb(&input, &model).unwrap();
        let tagged = std::fs::read(&model).unwrap();
        let glb = gltf::binary::Glb::from_slice(&tagged).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&glb.json).unwrap();
        assert_eq!(
            document["meshes"][0]["primitives"][0]["extras"]["mudcrabGeometryIndex"],
            0
        );
        assert_eq!(
            document["scenes"][0]["extras"]["mudcrabCollision"]["bodies"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        conn.execute_batch(
            "UPDATE statics SET bounds_min_x=-64,bounds_min_y=-64,bounds_min_z=0,
                bounds_max_x=64,bounds_max_y=64,bounds_max_z=0 WHERE id=40;
             UPDATE \"references\" SET pos_x=128,pos_y=128,scale=1 WHERE id=50;
             INSERT INTO base_texture_swaps(base_id,position,shape_name,shape_index,texture_set_id,disposition)
             VALUES(40,0,'GrassBlocker',0,40,'null_texture_set');",
        )
        .unwrap();
        // The previous mesh contract left Havok models untagged while preserving their geometry.
        edit_mesh(dir.path(), |json, _| {
            json["meshes"][0]["primitives"][0]["extras"]
                .as_object_mut()
                .unwrap()
                .remove("mudcrabGeometryIndex");
        });
        let terrain = dir.path().join("terrain.rkyv");
        let previous = dir.path().join("previous.rkyv");
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &previous).unwrap();
        let old = read(&previous);
        assert_eq!(old.version, shared::grass::GRASS_CACHE_VERSION);
        assert!(
            old.cells[0].quadrants[0]
                .exclusion
                .iter()
                .any(|&byte| byte != 0),
            "the previous untagged shape contributes obstruction samples"
        );
        std::fs::write(&model, &tagged).unwrap();
        let rebuilt =
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &output).unwrap();
        assert_eq!((rebuilt.baked_cells, rebuilt.reused_cells), (1, 2));
        assert_eq!(rebuilt.skipped_obstruction_models, 0);
        let current = read(&output);
        assert_eq!(current.version, shared::grass::GRASS_CACHE_VERSION);
        assert_ne!(current.cells[0].input_hash, old.cells[0].input_hash);
        assert!(
            current.cells[0]
                .quadrants
                .iter()
                .all(|quadrant| { quadrant.exclusion.iter().all(|&byte| byte == 0) }),
            "the tagged NullTextureSet shape leaves its grass samples clear"
        );
        assert_eq!(&current.cells[1..], &old.cells[1..]);
        let warm = dir.path().join("warm.rkyv");
        let reused = write_grass_cache(&conn, dir.path(), &terrain, Some(&output), &warm).unwrap();
        assert_eq!(
            (
                reused.baked_cells,
                reused.reused_cells,
                reused.geometry_decodes
            ),
            (0, 3, 0)
        );
        assert_eq!(read(&warm), current);
        assert_eq!(std::fs::read(model).unwrap(), tagged);
    }

    /// Neighbor masks preserve worldspace identity, terrain cell selection and all linked species.
    #[test]
    fn cross_cell_masks_keep_worldspaces_separate_and_many_species() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        // Persistent CELL records can share a coordinate with the actual LAND-bearing cell.
        conn.execute(
            "INSERT INTO cells(id,worldspace_id,grid_x,grid_y,flags) VALUES(13,1,1,0,0)",
            [],
        )
        .unwrap();
        let path = dir.path().join("grass.rkyv");
        let report = write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &path,
        )
        .unwrap();
        assert_eq!(report.baked_cells, 3);
        let cache = read(&path);
        assert_eq!(cache.cells[0].quadrants[0].layers[0].grass_ids, [21, 22]);
        assert_eq!(
            cache.cells[0].quadrants[0].layers[0].weights,
            vec![1.0; 289]
        );
        assert!(
            cache.cells[0].quadrants[1]
                .exclusion
                .iter()
                .any(|&byte| byte != 0)
        );
        assert!(
            cache.cells[1].quadrants[0]
                .exclusion
                .iter()
                .any(|&byte| byte != 0)
        );
        assert!(
            cache.cells[2]
                .quadrants
                .iter()
                .all(|q| q.exclusion.iter().all(|&byte| byte == 0))
        );
        // At x=64/y=64 in cell 11, the scaled box still overlaps. Unscaled x=64 would miss it.
        let bit = 2 * 64 + 2;
        assert_ne!(
            cache.cells[1].quadrants[0].exclusion[bit / 8] & (1 << (bit % 8)),
            0
        );
    }

    /// Header markers on ordinary opaque models contribute no local or neighboring masks.
    #[test]
    fn marker_header_omits_cross_cell_masks_and_keeps_ordinary_blocker() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute("UPDATE statics SET flags=?1 WHERE id=40", [0x00800000u32])
            .unwrap();
        conn.execute_batch(
            "INSERT INTO records(form_id,record_type,load_order,data) VALUES(41,'STAT',0,x'00');
             INSERT INTO statics(id,model_path,flags,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid)
             SELECT 41,model_path,0,bounds_min_x,bounds_min_y,bounds_min_z,bounds_max_x,bounds_max_y,bounds_max_z,bounds_valid FROM statics WHERE id=40;
             INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale)
             VALUES(51,10,1,41,1,512,512,0,0,0,0,2);",
        )
        .unwrap();
        let path = dir.path().join("grass.rkyv");
        write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &path,
        )
        .unwrap();
        let cache = read(&path);
        let local = cache.cells.iter().find(|cell| cell.cell_id == 10).unwrap();
        let neighbor = cache.cells.iter().find(|cell| cell.cell_id == 11).unwrap();
        assert!(
            local.quadrants[0].exclusion.iter().any(|&byte| byte != 0),
            "ordinary nearby blocker must still contribute"
        );
        assert!(
            local.quadrants[1].exclusion.iter().all(|&byte| byte == 0),
            "marker header must remove local contributions from meshes/blocker.glb"
        );
        assert!(
            neighbor
                .quadrants
                .iter()
                .all(|q| q.exclusion.iter().all(|&byte| byte == 0)),
            "marker header must remove neighboring contributions"
        );
    }

    /// Missing bounds on a header marker are not an omitted obstruction candidate.
    #[test]
    fn marker_header_is_not_counted_as_unbounded_obstruction() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute_batch(
            "INSERT INTO records(form_id,record_type,load_order,data) VALUES(41,'STAT',0,x'00'),(42,'STAT',0,x'00');
             INSERT INTO statics(id,model_path,flags) VALUES
             (41,'meshes/arbitrary.glb',0),(42,'meshes/ordinary.glb',0);
             INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale)
             VALUES(51,10,1,41,1,0,0,0,0,0,0,1),(52,10,1,42,1,0,0,0,0,0,0,1);",
        )
        .unwrap();
        conn.execute("UPDATE statics SET flags=?1 WHERE id=41", [0x00800000u32])
            .unwrap();
        let (references, unbounded) = read_reference_bounds(&conn).unwrap();
        assert_eq!(
            references.len(),
            1,
            "healthy bounded blocker remains eligible"
        );
        assert_eq!(
            unbounded, 1,
            "header marker must not count as an unbounded obstruction"
        );
    }

    /// Full-layer coverage uses the current version and rebuilds caches made from capped terrain.
    #[test]
    fn full_layer_grass_cache_uses_current_version_and_rebuilds_version_one() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let previous = dir.path().join("previous.rkyv");
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &previous).unwrap();
        let mut cache = read(&previous);
        assert_eq!(
            cache.version, GRASS_CACHE_VERSION,
            "full-layer coverage invalidates capped terrain masks"
        );
        cache.version = 1;
        std::fs::write(&previous, rkyv::to_bytes::<Error>(&cache).unwrap()).unwrap();
        assert!(validate_grass_cache(&previous).is_err());
        let report =
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &output).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (3, 0));
        assert_eq!(read(&output).version, GRASS_CACHE_VERSION);
    }

    /// A warm v6 archive must rebuild rather than retain masks from a now-marker base.
    #[test]
    fn marker_header_policy_rebuilds_warm_v6_masks() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let previous = dir.path().join("previous.rkyv");
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &previous).unwrap();
        let mut legacy = read(&previous);
        assert!(
            legacy.cells[0].quadrants[1]
                .exclusion
                .iter()
                .any(|&b| b != 0)
        );
        legacy.version = 6;
        std::fs::write(&previous, rkyv::to_bytes::<Error>(&legacy).unwrap()).unwrap();
        conn.execute("UPDATE statics SET flags=8388608 WHERE id=40", [])
            .unwrap();
        let report =
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &output).unwrap();
        assert_eq!(
            (report.baked_cells, report.reused_cells),
            (3, 0),
            "v6 marker masks must never be reused"
        );
        let cache = read(&output);
        assert_eq!(cache.version, shared::grass::GRASS_CACHE_VERSION);
        assert!(cache.cells.iter().all(|cell| {
            cell.quadrants
                .iter()
                .all(|q| q.exclusion.iter().all(|&byte| byte == 0))
        }));
    }

    /// Reuse unchanged cells and invalidate cells affected by rule or reference changes.
    #[test]
    fn incremental_keys_cover_relevant_rules_and_cross_cell_reference_changes() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let first = dir.path().join("first.rkyv");
        let next = dir.path().join("next.rkyv");
        let terrain = dir.path().join("terrain.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &first).unwrap();
        let report = write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (0, 3));
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&next).unwrap()
        );
        conn.execute(
            "INSERT INTO records(form_id,record_type,load_order,data) VALUES(99,'GRAS',0,x'FF')",
            [],
        )
        .unwrap();
        assert_eq!(
            write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next)
                .unwrap()
                .reused_cells,
            3
        );
        conn.execute("UPDATE records SET data=x'AA' WHERE form_id=21", [])
            .unwrap();
        assert_eq!(
            write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next)
                .unwrap()
                .baked_cells,
            3
        );
        // Use the changed-rule cache as baseline; moving a reference changes both overlapping cells only.
        std::fs::copy(&next, &first).unwrap();
        conn.execute("UPDATE \"references\" SET pos_x=1000 WHERE id=50", [])
            .unwrap();
        let report = write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (2, 1));
        assert!(
            read(&next).cells[1]
                .quadrants
                .iter()
                .all(|q| q.exclusion.iter().all(|&byte| byte == 0))
        );
    }

    /// Rebuild an invalid previous archive and replace stale cells with an empty cache.
    #[test]
    fn invalid_previous_cache_rebuilds_and_empty_output_replaces_old_cells() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let previous = dir.path().join("old.rkyv");
        let output = dir.path().join("new.rkyv");
        let terrain = dir.path().join("terrain.rkyv");
        std::fs::write(&previous, b"torn cache").unwrap();
        assert_eq!(
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &output)
                .unwrap()
                .baked_cells,
            3
        );
        conn.execute("DELETE FROM cells", []).unwrap();
        write_grass_cache(&conn, dir.path(), &terrain, Some(&output), &previous).unwrap();
        assert!(read(&previous).cells.is_empty());
    }

    /// Keep incomplete definitions as non-emitting slots beside valid neighboring rules.
    #[test]
    fn partial_grass_rules_retain_slots_without_aborting_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute("INSERT INTO grass_types(id,model_path,density,load_order) VALUES(23,'meshes/partial.glb',40,0)",[]).unwrap();
        conn.execute(
            "INSERT INTO landscape_texture_grasses(ltex_id,gras_id) VALUES(30,23)",
            [],
        )
        .unwrap();
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &output,
        )
        .unwrap();
        assert_eq!(read(&output).types.len(), 3);
    }

    /// A model-less grass type is a notice; a modeled type with incomplete data still warns.
    #[test]
    fn modelless_grass_types_are_notices_and_incomplete_modeled_rules_warn() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute(
            "INSERT INTO grass_types(id,editor_id,load_order) VALUES(24,'NoModel',0)",
            [],
        )
        .unwrap();
        let output = dir.path().join("grass.rkyv");
        let report = write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &output,
        )
        .unwrap();
        assert_eq!((report.skipped_rules, report.modelless_rules), (0, 1));
        assert!(report.warnings().is_empty(), "{:?}", report.warnings());
        assert!(
            report.notices().contains(
                &"grass cache retained 1 non-emitting grass types without a model".to_owned()
            ),
            "{:?}",
            report.notices()
        );
        conn.execute(
            "INSERT INTO grass_types(id,model_path,density,load_order) VALUES(25,'meshes/partial.glb',40,0)",
            [],
        )
        .unwrap();
        let report = write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &output,
        )
        .unwrap();
        assert_eq!((report.skipped_rules, report.modelless_rules), (1, 1));
        assert_eq!(report.first_invalid_rule, Some(25));
        assert_eq!(
            report.warnings(),
            ["grass cache skipped 1 incomplete or invalid grass rules; first GRAS 00000019"]
        );
        assert_eq!(read(&output).types.len(), 4);
    }

    /// Count only eligible modeled obstructions that lack validated bounds.
    #[test]
    fn unbounded_count_omits_nonmodels_and_excluded_obstruction_categories() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute_batch(
            "INSERT INTO records(form_id,record_type,load_order,data) VALUES
            (41,'NPC_',0,x'00'),(42,'STAT',0,x'00'),(43,'TREE',0,x'00'),
            (44,'STAT',0,x'00'),(45,'STAT',0,x'00');
            INSERT INTO statics(id,model_path,flags) VALUES
            (42,NULL,0),(43,'plants/tree.nif',0),(44,'effects/glow.nif',0),
            (45,'architecture/missing.nif',0);",
        )
        .unwrap();
        for base in 41..=45 {
            conn.execute("INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale)
                VALUES(?1,10,1,?2,1,0,0,0,0,0,0,1)", [base + 100, base]).unwrap();
        }
        let (references, unbounded) = read_reference_bounds(&conn).unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(
            unbounded, 1,
            "only the missing architecture model is an obstruction candidate"
        );
    }

    /// Match the shared coordinate contract for asymmetric rotations and scales.
    #[test]
    fn rotated_scaled_bounds_follow_creation_inverse_xyz_composition() {
        let local = shared::Bounds3 {
            min: [-12.0, -3.0, -7.0],
            max: [31.0, 19.0, 11.0],
        };
        let angles: [f32; 3] = [0.31, -0.57, 0.83];
        let position = [140.0, -53.0, 28.0];
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for bits in 0..8 {
            let runtime = std::array::from_fn(|axis| {
                if bits & (1 << axis) == 0 {
                    local.min[axis] * 1.7
                } else {
                    local.max[axis] * 1.7
                }
            });
            let [mut x, mut y, mut z] = shared::coordinates::runtime_to_creation_vector(runtime);
            let (s, c) = (-angles[2]).sin_cos();
            (x, y) = (c * x - s * y, s * x + c * y);
            let (s, c) = (-angles[1]).sin_cos();
            (x, z) = (c * x + s * z, -s * x + c * z);
            let (s, c) = (-angles[0]).sin_cos();
            (y, z) = (c * y - s * z, s * y + c * z);
            for (axis, value) in [x, y, z].into_iter().enumerate() {
                let value = value + position[axis];
                min[axis] = min[axis].min(value);
                max[axis] = max[axis].max(value);
            }
        }
        let actual = transform_bounds(local, position, angles, 1.7);
        for axis in 0..3 {
            assert!((actual.0[axis] - f64::from(min[axis])).abs() < 0.0001);
            assert!((actual.1[axis] - f64::from(max[axis])).abs() < 0.0001);
        }
    }

    /// Preserve painted opacity while keeping multiple species and grassless attenuation.
    #[test]
    fn overlays_preserve_raw_opacity_instead_of_collapsing_species() {
        let base = shared::TerrainLayer {
            texture_form_id: 30,
            quadrant: 0,
            layer: 0,
            is_base: true,
            weights: vec![],
        };
        let overlay = shared::TerrainLayer {
            texture_form_id: 31,
            quadrant: 0,
            layer: 1,
            is_base: false,
            weights: vec![shared::TerrainWeight {
                vertex: 117,
                opacity: 0.65,
            }],
        };
        let land = CachedLand {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![],
            normals: vec![],
            vertex_colors: vec![],
            layers: vec![base, overlay],
            water_height: None,
            water_type_form_id: None,
        };
        let layers = blend_layers(
            &land,
            0,
            &BTreeMap::from([(30, vec![21, 22]), (31, vec![23])]),
        );
        assert!((layers[0].weights[117] - 90.0 / 255.0).abs() < 0.0001);
        assert!((layers[1].weights[117] - 165.0 / 255.0).abs() < 0.0001);
        assert_eq!(layers[0].grass_ids, [21, 22]);
        assert_eq!(layers[1].grass_ids, [23]);
        assert_eq!(layers[0].weights[118], 1.0);
    }

    /// Reject malformed grid lengths and unsupported archive versions.
    #[test]
    fn semantic_validation_rejects_wrong_grid_lengths_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let path = dir.path().join("grass.rkyv");
        write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &path,
        )
        .unwrap();
        let original = read(&path);
        for colors in [false, true] {
            let mut bad = original.clone();
            if colors {
                bad.cells[0].vertex_colors = vec![1];
            } else {
                bad.cells[0].normals = vec![1];
            }
            std::fs::write(&path, rkyv::to_bytes::<Error>(&bad).unwrap()).unwrap();
            assert!(validate_grass_cache(&path).is_err());
        }
        let mut bad = original.clone();
        bad.cells[0].quadrants[0].layers[0].weights.pop();
        std::fs::write(&path, rkyv::to_bytes::<Error>(&bad).unwrap()).unwrap();
        assert!(validate_grass_cache(&path).is_err());
        let mut bad = original;
        bad.version += 1;
        std::fs::write(&path, rkyv::to_bytes::<Error>(&bad).unwrap()).unwrap();
        assert!(validate_grass_cache(&path).is_err());
    }

    /// Removing a reference releases masks and invalidates affected neighboring cells.
    #[test]
    fn reference_removal_releases_exclusion_and_changes_neighbor_keys() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let previous = dir.path().join("previous.rkyv");
        let next = dir.path().join("next.rkyv");
        let terrain = dir.path().join("terrain.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &previous).unwrap();
        conn.execute("DELETE FROM \"references\" WHERE id=50", [])
            .unwrap();
        let report =
            write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &next).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (2, 1));
        assert!(
            read(&next)
                .cells
                .iter()
                .flat_map(|cell| &cell.quadrants)
                .all(|quadrant| quadrant.exclusion.iter().all(|&byte| byte == 0))
        );
    }

    /// Apply the upward-ray height window consistently across neighboring cells.
    #[test]
    fn upward_ray_window_applies_across_cell_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let previous = dir.path().join("previous.rkyv");
        let next = dir.path().join("next.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &previous).unwrap();
        for z in [9000.0, -1000.0, 0.0] {
            conn.execute("UPDATE \"references\" SET pos_z=?1 WHERE id=50", [z])
                .unwrap();
            let report =
                write_grass_cache(&conn, dir.path(), &terrain, Some(&previous), &next).unwrap();
            assert_eq!((report.baked_cells, report.reused_cells), (2, 1));
            for cell in &read(&next).cells[..2] {
                let excluded = cell
                    .quadrants
                    .iter()
                    .any(|q| q.exclusion.iter().any(|&b| b != 0));
                assert_eq!(excluded, z == 0.0, "cell {} object Z {z}", cell.cell_id);
            }
            std::fs::copy(&next, &previous).unwrap();
        }
    }

    /// Neighbor heights and negative scale affect masks, and mismatched versions require rebuilding.
    #[test]
    fn exclusions_use_each_neighbor_terrain_height_and_support_negative_scale() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let terrain = dir.path().join("terrain.rkyv");
        let first = dir.path().join("first.rkyv");
        let next = dir.path().join("next.rkyv");
        conn.execute("UPDATE \"references\" SET scale=-2 WHERE id=50", [])
            .unwrap();
        write_grass_cache(&conn, dir.path(), &terrain, None, &first).unwrap();
        assert!(
            read(&first).cells[1].quadrants[0]
                .exclusion
                .iter()
                .any(|&b| b != 0)
        );

        let map = super::super::cell_cache::validate_cell_cache(&terrain).unwrap();
        let mut changed: shared::CellCache = rkyv::deserialize::<_, Error>(
            rkyv::access::<shared::ArchivedCellCache, Error>(&map).unwrap(),
        )
        .unwrap();
        drop(map);
        changed
            .cells
            .iter_mut()
            .find(|cell| cell.cell_id == 11)
            .unwrap()
            .heights
            .fill(128.0);
        std::fs::write(&terrain, rkyv::to_bytes::<Error>(&changed).unwrap()).unwrap();
        let report = write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next).unwrap();
        assert_eq!((report.baked_cells, report.reused_cells), (1, 2));
        let cache = read(&next);
        assert!(
            cache.cells[0].quadrants[1]
                .exclusion
                .iter()
                .any(|&b| b != 0)
        );
        assert!(
            cache.cells[1]
                .quadrants
                .iter()
                .all(|q| q.exclusion.iter().all(|&b| b == 0)),
            "the same ref remains below the neighboring cell's elevated terrain"
        );

        let mut obsolete = read(&first);
        obsolete.version = 8;
        std::fs::write(&first, rkyv::to_bytes::<Error>(&obsolete).unwrap()).unwrap();
        assert!(validate_grass_cache(&first).is_err());
        let report = write_grass_cache(&conn, dir.path(), &terrain, Some(&first), &next).unwrap();
        assert_eq!(
            (report.baked_cells, report.reused_cells),
            (3, 0),
            "an unsupported cache version cannot be reused"
        );
    }

    /// Use rendered terrain triangle heights when bilinear interpolation would produce a false hit.
    #[test]
    fn exclusions_follow_rendered_triangle_height_instead_of_bilinear_ground() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute_batch("UPDATE statics SET bounds_min_x=40,bounds_min_y=85,bounds_min_z=-56,bounds_max_x=56,bounds_max_y=87,bounds_max_z=-40 WHERE id=40;
            UPDATE \"references\" SET pos_x=0,pos_y=0,pos_z=0,scale=1 WHERE id=50;").unwrap();
        let terrain = dir.path().join("terrain.rkyv");
        let map = super::super::cell_cache::validate_cell_cache(&terrain).unwrap();
        let mut changed: shared::CellCache = rkyv::deserialize::<_, Error>(
            rkyv::access::<shared::ArchivedCellCache, Error>(&map).unwrap(),
        )
        .unwrap();
        drop(map);
        let heights = &mut changed
            .cells
            .iter_mut()
            .find(|cell| cell.cell_id == 10)
            .unwrap()
            .heights;
        write_box(dir.path(), [40.0, 85.0, -56.0], [56.0, 87.0, -40.0]);
        heights[1] = 128.0;
        heights[33] = 128.0;
        // At center48,48 the rendered triangle is Z96; the box top87 is below Z-8.
        // Bilinear Z=x+y-xy/64 is Z60 and would incorrectly hit the box above it.
        std::fs::write(&terrain, rkyv::to_bytes::<Error>(&changed).unwrap()).unwrap();
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &output).unwrap();
        let bit = 64 + 1;
        assert_eq!(
            read(&output).cells[0].quadrants[0].exclusion[bit / 8] & (1 << (bit % 8)),
            0,
            "rendered Z96 lies above the blocker, while bilinear Z60 would be blocked"
        );
    }

    /// Preserve small obstruction footprints at far world coordinates.
    #[test]
    fn far_world_bounds_keep_sub_float_ulp_mask_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let grid = 131_072;
        write_box(dir.path(), [8.0, -1.0, -24.0], [24.0, 1.0, -8.0]);
        conn.execute_batch("UPDATE statics SET bounds_min_x=8,bounds_min_y=-1,bounds_min_z=-24,bounds_max_x=24,bounds_max_y=1,bounds_max_z=-8 WHERE id=40;
            UPDATE \"references\" SET pos_y=0,pos_z=0,scale=1 WHERE id=50;").unwrap();
        conn.execute("UPDATE cells SET grid_x=?1 WHERE id=10", [grid])
            .unwrap();
        conn.execute("UPDATE cells SET grid_x=?1 WHERE id=11", [grid + 1])
            .unwrap();
        conn.execute(
            "UPDATE \"references\" SET pos_x=?1 WHERE id=50",
            [grid as f32 * GRASS_CELL_SIZE],
        )
        .unwrap();
        let output = dir.path().join("grass.rkyv");
        write_grass_cache(
            &conn,
            dir.path(),
            &dir.path().join("terrain.rkyv"),
            None,
            &output,
        )
        .unwrap();
        assert_ne!(
            read(&output).cells[0].quadrants[0].exclusion[0] & 1,
            0,
            "8..24unitbounds must notcollapse when added to a far world position with64unit f32ULP"
        );
    }

    /// Omit grassless layers after computing the base residual.
    #[test]
    fn grassless_cells_and_layers_are_omitted_after_base_residual() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        let path = dir.path().join("grass.rkyv");
        let terrain = dir.path().join("terrain.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, None, &path).unwrap();
        conn.execute("DELETE FROM landscape_texture_grasses", [])
            .unwrap();
        let empty = dir.path().join("empty.rkyv");
        write_grass_cache(&conn, dir.path(), &terrain, Some(&path), &empty).unwrap();
        assert!(read(&empty).cells.is_empty());

        let land = CachedLand {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![],
            normals: vec![],
            vertex_colors: vec![],
            water_height: None,
            water_type_form_id: None,
            layers: vec![
                shared::TerrainLayer {
                    texture_form_id: 30,
                    quadrant: 0,
                    layer: 0,
                    is_base: true,
                    weights: vec![],
                },
                shared::TerrainLayer {
                    texture_form_id: 31,
                    quadrant: 0,
                    layer: 1,
                    is_base: false,
                    weights: vec![shared::TerrainWeight {
                        vertex: 117,
                        opacity: 0.75,
                    }],
                },
                shared::TerrainLayer {
                    texture_form_id: 32,
                    quadrant: 0,
                    layer: 2,
                    is_base: false,
                    weights: vec![],
                },
            ],
        };
        let layers = blend_layers(&land, 0, &BTreeMap::from([(30, vec![21]), (32, vec![22])]));
        assert_eq!(
            layers.len(),
            1,
            "grassless overlay and zero-weight grassy overlay are omitted"
        );
        assert_eq!(layers[0].texture_form_id, 30);
        assert_eq!(
            layers[0].weights[117],
            64.0 / 255.0,
            "bare overlay still attenuates grassy base"
        );
    }
}

#[cfg(test)]
mod selection_transform_tests {
    use super::*;

    /// A null modeled rule is omitted with the same bounded warning as malformed fields.
    #[test]
    fn null_rule_identity_keeps_valid_neighbors() {
        let directory = tempfile::tempdir().unwrap();
        let connection = tests::fixture(directory.path());
        connection.execute("INSERT INTO grass_types SELECT 0,editor_id,model_path,density,min_slope,max_slope,units_from_water,water_comparison,position_range,height_range,color_range,wave_period,flags,load_order FROM grass_types WHERE id=21",[]).unwrap();
        let (rules, skipped, modelless, first) = read_types(&connection).unwrap();
        assert_eq!(
            rules.iter().map(|rule| rule.form_id).collect::<Vec<_>>(),
            [21, 22]
        );
        assert_eq!((skipped, modelless, first), (1, 0, Some(0)));
    }

    /// Missing TXST suppresses bindings and duplicate merging without erasing raw paint coverage.
    #[test]
    fn texture_set_availability_controls_binding_and_merge() {
        let land = CachedLand {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![],
            normals: vec![],
            vertex_colors: vec![],
            water_height: None,
            water_type_form_id: None,
            layers: vec![
                shared::TerrainLayer {
                    texture_form_id: 30,
                    quadrant: 0,
                    layer: 0,
                    is_base: true,
                    weights: vec![],
                },
                shared::TerrainLayer {
                    texture_form_id: 31,
                    quadrant: 0,
                    layer: 0,
                    is_base: false,
                    weights: vec![shared::TerrainWeight {
                        vertex: 0,
                        opacity: 200.0 / 255.0,
                    }],
                },
                shared::TerrainLayer {
                    texture_form_id: 31,
                    quadrant: 0,
                    layer: 1,
                    is_base: false,
                    weights: vec![shared::TerrainWeight {
                        vertex: 0,
                        opacity: 200.0 / 255.0,
                    }],
                },
            ],
        };
        let links = BTreeMap::from([(30, vec![7]), (31, vec![11])]);
        let all = selection_layers(&land, 0, &links, &|_| true);
        assert_eq!(all[0].weights[0], 111.0 / 255.0);
        let missing = selection_layers(&land, 0, &links, &|id| id == 30);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].weights[0], 0.0);
        let mut cleared = land.clone();
        cleared.layers.truncate(2);
        cleared.layers[1].texture_form_id = 0;
        cleared.layers[1].weights[0].opacity = 0.75;
        let recovered = selection_layers(&cleared, 0, &links, &|id| id == 30);
        assert_eq!(
            recovered[0].weights[0],
            64.0 / 255.0,
            "cleared optional references still carry paint coverage"
        );
        let directory = tempfile::tempdir().unwrap();
        let connection = tests::fixture(directory.path());
        connection.execute("DELETE FROM texture_sets", []).unwrap();
        let report = write_grass_cache(
            &connection,
            directory.path(),
            &directory.path().join("terrain.rkyv"),
            None,
            &directory.path().join("grass.rkyv"),
        )
        .unwrap();
        assert_eq!(report.skipped_texture_lists, 1);
        assert_eq!(report.first_invalid_texture, Some(30));
        assert_eq!(
            tests::read(&directory.path().join("grass.rkyv"))
                .cells
                .len(),
            0
        );
    }

    /// Duplicate texture bytes wrap on a copy, while nomination bindings retain closed order.
    #[test]
    fn duplicate_and_base_equal_layers_transform_weights_only() {
        let mut land = CachedLand {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![],
            normals: vec![],
            vertex_colors: vec![],
            water_height: None,
            water_type_form_id: None,
            layers: vec![shared::TerrainLayer {
                texture_form_id: 30,
                quadrant: 0,
                layer: 0,
                is_base: true,
                weights: vec![],
            }],
        };
        for (slot, texture, byte) in [(0, 31, 20u8), (2, 31, 20), (3, 32, 240), (4, 32, 32)] {
            land.layers.push(shared::TerrainLayer {
                texture_form_id: texture,
                quadrant: 0,
                layer: slot,
                is_base: false,
                weights: vec![shared::TerrainWeight {
                    vertex: 0,
                    opacity: f32::from(byte) / 255.0,
                }],
            });
        }
        let links = BTreeMap::from([(30, vec![7]), (31, vec![11]), (32, vec![13])]);
        let layers = blend_layers(&land, 0, &links);
        assert_eq!(
            layers
                .iter()
                .map(|layer| layer.texture_form_id)
                .collect::<Vec<_>>(),
            [30, 31, 31]
        );
        assert_eq!(layers[1].weights[0], 40.0 / 255.0);
        assert_eq!(
            layers[2].weights[0],
            16.0 / 255.0,
            "240+32 wraps as a byte and stays on the second original binding"
        );
        assert_eq!(layers[0].weights[0], 199.0 / 255.0);
        land.layers[1].texture_form_id = 30;
        let layers = blend_layers(&land, 0, &links);
        assert_eq!(
            layers[1].texture_form_id, 30,
            "base-equal binding remains but its weights shift away"
        );
        assert_eq!(layers[1].weights[0], 20.0 / 255.0);
    }
}
