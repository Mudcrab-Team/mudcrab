//! Read-only analysis of published runtime assets for streaming reservations.
//! The analyzer reads GLB JSON and KTX2 headers/level tables, never decodes images
//! or meshes and never writes into the pack. Unknown estimates remain explicit.

use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use serde_json::Value;
use shared::streaming_costs::{
    ByteEstimate, EstimateQuality, ResourceCost, ResourceKind, SceneCost, StreamingCostCatalog,
    canonical_resource_key,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

const MAX_GLB_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;

/// Analyze an immutable pack without modifying it. A malformed asset receives
/// unknown metadata rather than a zero-cost estimate or a guessed file-size cost.
pub fn analyze_pack(assets_root: &Path) -> Result<StreamingCostCatalog> {
    let root = assets_root
        .canonicalize()
        .wrap_err("converted assets root does not exist")?;
    ensure!(root.is_dir(), "converted assets root is not a directory");
    let manifest_path = root.join("conversion-manifest.json");
    ensure!(
        manifest_path.canonicalize()?.starts_with(&root),
        "conversion manifest symlink escapes converted pack"
    );
    ensure!(
        fs::metadata(&manifest_path)?.len() <= MAX_MANIFEST_BYTES,
        "conversion manifest exceeds size limit"
    );
    let manifest =
        fs::read(&manifest_path).wrap_err("converted pack has no conversion-manifest.json")?;
    let parsed: Value =
        serde_json::from_slice(&manifest).wrap_err("invalid conversion manifest")?;
    ensure!(parsed.is_object(), "conversion manifest must be an object");
    let fingerprint = crate::cache::hash_bytes(&manifest);
    let mut catalog = StreamingCostCatalog::empty(fingerprint.clone());
    catalog.notes.push("KTX2 estimates reserve expanded texels for format fallback and CPU/GPU copies, even when native block compression would use less memory".to_owned());
    catalog.notes.push("Collider/BVH and ECS costs use conservative allocation factors; these are estimates and require runtime memory-pressure feedback".to_owned());
    if parsed.get("complete").and_then(Value::as_bool) != Some(true) {
        catalog.notes.push("The conversion manifest is incomplete; absent or failed assets require unknown-resource fallback".to_owned());
    }
    let mut files = BTreeMap::<String, PathBuf>::new();
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !(entry.file_type().is_dir()
                    && (entry.file_name().to_string_lossy().starts_with('.')
                        || entry.file_name() == "vfs"))
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if !path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("glb") || extension.eq_ignore_ascii_case("ktx2")
        }) {
            continue;
        }
        let key = key_for_path(&root, &path)?;
        ensure!(
            files.insert(key.clone(), path).is_none(),
            "canonical asset path collision for {key}"
        );
    }
    // Analyze each shared image once, including images used directly by terrain/water.
    for (key, path) in &files {
        if key.ends_with(".ktx2") {
            let estimate = inspect_texture(path).unwrap_or_else(|error| {
                ByteEstimate::unknown(&format!("KTX2 metadata unavailable: {error}"))
            });
            catalog.resources.insert(
                key.clone(),
                ResourceCost::new(ResourceKind::Texture, estimate),
            );
        }
    }
    for (key, path) in &files {
        if !key.ends_with(".glb") {
            continue;
        }
        let (geometry, scene) = match inspect_scene(&root, path, key, &mut catalog.resources) {
            Ok(cost) => cost,
            Err(error) => (
                ByteEstimate::unknown(&format!("GLB metadata unavailable: {error}")),
                SceneCost {
                    resource_keys: vec![key.clone()],
                    per_placement_collision: ByteEstimate::unknown(
                        "Collision cost is unavailable because GLB metadata could not be read",
                    ),
                    per_placement_ecs: catalog.generated.model_placement.clone(),
                },
            ),
        };
        catalog.resources.insert(
            key.clone(),
            ResourceCost::new(ResourceKind::SceneGeometry, geometry),
        );
        catalog.scenes.insert(key.clone(), scene);
    }
    // Reject publication races instead of attaching estimates to a different pack.
    ensure!(
        crate::cache::hash_bytes(&fs::read(&manifest_path)?) == fingerprint,
        "conversion manifest changed during cost analysis; analyze an immutable published pack"
    );
    catalog.validate(&fingerprint)?;
    Ok(catalog)
}

/// Export to an explicitly chosen, new sidecar outside the immutable asset tree.
/// Existing files are never truncated, including hard links and symlink targets.
pub fn export_pack(assets_root: &Path, output_path: &Path) -> Result<StreamingCostCatalog> {
    let root = assets_root.canonicalize()?;
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .wrap_err("catalog output parent must already exist")?;
    ensure!(
        !parent.starts_with(&root),
        "catalog export must be outside the read-only asset pack"
    );
    let name = output_path
        .file_name()
        .ok_or_else(|| color_eyre::eyre::eyre!("catalog output needs a file name"))?;
    let output = parent.join(name);
    let catalog = analyze_pack(&root)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .wrap_err_with(|| format!("catalog output must be a new file: {}", output.display()))?;
    let bytes = serde_json::to_vec_pretty(&catalog)?;
    if let Err(error) = file
        .write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&output);
        return Err(error.into());
    }
    Ok(catalog)
}

fn key_for_path(root: &Path, path: &Path) -> Result<String> {
    canonical_resource_key(
        &path
            .strip_prefix(root)
            .wrap_err("asset is outside converted pack")?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn checked_add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| color_eyre::eyre::eyre!("asset byte estimate overflows"))
}
fn checked_mul(left: u64, right: u64) -> Result<u64> {
    left.checked_mul(right)
        .ok_or_else(|| color_eyre::eyre::eyre!("asset byte estimate overflows"))
}
fn field_u64(value: &Value, name: &str) -> Result<u64> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid or missing {name}"))
}

struct GlbStorage {
    file_bytes: u64,
    binary_bytes: u64,
    binary_offset: u64,
    json_bytes: u64,
}

fn read_glb_json(path: &Path) -> Result<(Value, GlbStorage)> {
    let mut file = File::open(path)?;
    let file_bytes = file.metadata()?.len();
    let mut header = [0_u8; 20];
    file.read_exact(&mut header)
        .wrap_err("truncated GLB header")?;
    ensure!(
        &header[..4] == b"glTF" && u32::from_le_bytes(header[4..8].try_into().unwrap()) == 2,
        "unsupported GLB container"
    );
    ensure!(
        u64::from(u32::from_le_bytes(header[8..12].try_into().unwrap())) == file_bytes,
        "GLB declared length does not match file"
    );
    let json_bytes = u64::from(u32::from_le_bytes(header[12..16].try_into().unwrap()));
    ensure!(
        &header[16..20] == b"JSON"
            && json_bytes.is_multiple_of(4)
            && json_bytes <= MAX_GLB_JSON_BYTES,
        "invalid or oversized GLB JSON chunk"
    );
    let mut offset = checked_add(20, json_bytes)?;
    ensure!(offset <= file_bytes, "truncated GLB JSON chunk");
    let mut json = vec![0; usize::try_from(json_bytes)?];
    file.read_exact(&mut json)?;
    let document: Value = serde_json::from_slice(&json).wrap_err("invalid GLB JSON")?;
    ensure!(
        document["asset"]["version"] == "2.0",
        "unsupported glTF JSON version"
    );
    let mut binary_bytes = 0_u64;
    let mut binary_offset = None;
    while offset < file_bytes {
        ensure!(
            checked_add(offset, 8)? <= file_bytes,
            "truncated GLB chunk header"
        );
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk_header = [0; 8];
        file.read_exact(&mut chunk_header)?;
        let length = u64::from(u32::from_le_bytes(chunk_header[..4].try_into().unwrap()));
        ensure!(length.is_multiple_of(4), "unaligned GLB chunk");
        let data_offset = checked_add(offset, 8)?;
        offset = checked_add(data_offset, length)?;
        ensure!(offset <= file_bytes, "truncated GLB binary chunk");
        if &chunk_header[4..] == b"BIN\0" {
            ensure!(binary_offset.is_none(), "multiple GLB binary chunks");
            binary_bytes = length;
            binary_offset = Some(data_offset);
        }
    }
    Ok((
        document,
        GlbStorage {
            file_bytes,
            binary_bytes,
            binary_offset: binary_offset.unwrap_or(0),
            json_bytes,
        },
    ))
}

fn contained_dependency(root: &Path, document: &Path, uri: &str) -> Result<(String, PathBuf)> {
    let lexical = crate::asset_path::resolve_asset_uri(root, document, uri)?;
    let key = key_for_path(root, &lexical)?;
    if lexical.exists() {
        ensure!(
            lexical.canonicalize()?.starts_with(root),
            "asset dependency symlink escapes converted pack"
        );
    }
    Ok((key, lexical))
}

fn inspect_scene(
    root: &Path,
    path: &Path,
    key: &str,
    resources: &mut BTreeMap<String, ResourceCost>,
) -> Result<(ByteEstimate, SceneCost)> {
    let (document, storage) = read_glb_json(path)?;
    let GlbStorage {
        file_bytes,
        binary_bytes,
        binary_offset,
        json_bytes,
    } = storage;
    let buffers = document
        .get("buffers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut buffer_lengths = Vec::with_capacity(buffers.len());
    let mut input_buffer_bytes = 0_u64;
    for (index, buffer) in buffers.iter().enumerate() {
        let declared = field_u64(buffer, "byteLength")?;
        if let Some(uri) = buffer.get("uri").and_then(Value::as_str) {
            let (_, dependency) = contained_dependency(root, path, uri)?;
            ensure!(
                declared <= fs::metadata(dependency)?.len(),
                "external GLB buffer is truncated"
            );
        } else {
            ensure!(
                index == 0 && declared <= binary_bytes,
                "GLB buffer exceeds binary chunk"
            );
        }
        buffer_lengths.push(declared);
        input_buffer_bytes = checked_add(input_buffer_bytes, declared)?;
    }
    let views = document
        .get("bufferViews")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for view in views {
        let buffer = usize::try_from(field_u64(view, "buffer")?)?;
        let length = field_u64(view, "byteLength")?;
        let offset = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0);
        ensure!(
            checked_add(offset, length)?
                <= *buffer_lengths
                    .get(buffer)
                    .ok_or_else(|| color_eyre::eyre::eyre!(
                        "GLB buffer view names absent buffer"
                    ))?,
            "GLB buffer view exceeds buffer"
        );
    }
    let accessors = document
        .get("accessors")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut decoded_accessor_bytes = Vec::with_capacity(accessors.len());
    for accessor in accessors {
        let count = field_u64(accessor, "count")?;
        let components = match accessor.get("type").and_then(Value::as_str) {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4" | "MAT2") => 4,
            Some("MAT3") => 9,
            Some("MAT4") => 16,
            _ => bail!("unsupported GLB accessor type"),
        };
        let component_bytes = match field_u64(accessor, "componentType")? {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => bail!("unsupported GLB accessor component type"),
        };
        let stored_element = checked_mul(components, component_bytes)?;
        let decoded_element = checked_mul(components, 4)?;
        decoded_accessor_bytes.push(checked_mul(count, decoded_element)?);
        if let Some(view_index) = accessor.get("bufferView").and_then(Value::as_u64) {
            let view = views
                .get(usize::try_from(view_index)?)
                .ok_or_else(|| color_eyre::eyre::eyre!("GLB accessor names absent buffer view"))?;
            let stride = view
                .get("byteStride")
                .and_then(Value::as_u64)
                .unwrap_or(stored_element);
            ensure!(stride >= stored_element, "GLB accessor stride is too short");
            let offset = accessor
                .get("byteOffset")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let used = if count == 0 {
                0
            } else {
                checked_add(checked_mul(count - 1, stride)?, stored_element)?
            };
            ensure!(
                checked_add(offset, used)? <= field_u64(view, "byteLength")?,
                "GLB accessor exceeds buffer view"
            );
        }
    }
    let mut expanded_geometry = 0_u64;
    if let Some(meshes) = document.get("meshes").and_then(Value::as_array) {
        for mesh in meshes {
            for primitive in mesh
                .get("primitives")
                .and_then(Value::as_array)
                .ok_or_else(|| color_eyre::eyre::eyre!("GLB mesh has no primitives"))?
            {
                let attributes = primitive
                    .get("attributes")
                    .and_then(Value::as_object)
                    .ok_or_else(|| color_eyre::eyre::eyre!("GLB primitive has no attributes"))?;
                for accessor in attributes.values().chain(primitive.get("indices")) {
                    let index = accessor
                        .as_u64()
                        .ok_or_else(|| color_eyre::eyre::eyre!("invalid GLB accessor index"))?;
                    expanded_geometry = checked_add(
                        expanded_geometry,
                        *decoded_accessor_bytes
                            .get(usize::try_from(index)?)
                            .ok_or_else(|| {
                                color_eyre::eyre::eyre!("GLB primitive names absent accessor")
                            })?,
                    )?;
                }
                if let Some(position) = attributes.get("POSITION").and_then(Value::as_u64) {
                    let vertices = field_u64(&accessors[usize::try_from(position)?], "count")?;
                    // Generated normals/tangents, preprocessing copies and GPU packing.
                    expanded_geometry = checked_add(expanded_geometry, checked_mul(vertices, 40)?)?;
                }
                if let Some(targets) = primitive.get("targets").and_then(Value::as_array) {
                    for target in targets {
                        for accessor in target
                            .as_object()
                            .ok_or_else(|| color_eyre::eyre::eyre!("invalid morph target"))?
                            .values()
                        {
                            let index = usize::try_from(accessor.as_u64().ok_or_else(|| {
                                color_eyre::eyre::eyre!("invalid morph accessor")
                            })?)?;
                            expanded_geometry = checked_add(
                                expanded_geometry,
                                *decoded_accessor_bytes.get(index).ok_or_else(|| {
                                    color_eyre::eyre::eyre!("absent morph accessor")
                                })?,
                            )?;
                        }
                    }
                }
            }
        }
    }
    let nodes = document
        .get("nodes")
        .and_then(Value::as_array)
        .map_or(0, Vec::len) as u64;
    let materials = document
        .get("materials")
        .and_then(Value::as_array)
        .map_or(0, Vec::len) as u64;
    let json_heap = checked_mul(json_bytes, 4)?;
    let bookkeeping = checked_add(
        checked_mul(checked_add(nodes, materials)?, 4096)?,
        16 * 1024,
    )?;
    let resident = checked_add(
        checked_mul(expanded_geometry.max(input_buffer_bytes), 2)?,
        checked_add(json_heap, bookkeeping)?,
    )?;
    let transient = checked_add(
        file_bytes,
        checked_add(checked_mul(expanded_geometry, 2)?, json_heap)?,
    )?;
    let mut geometry = ByteEstimate::conservative(
        resident,
        transient,
        "Expanded 32-bit accessor data plus generated normal/tangent capacity, CPU/GPU copies, parsed JSON, scene/material bookkeeping and decode/upload headroom",
    );
    if document
        .get("extensionsRequired")
        .and_then(Value::as_array)
        .is_some_and(|extensions| {
            extensions.iter().any(|extension| {
                matches!(
                    extension.as_str(),
                    Some("KHR_draco_mesh_compression" | "EXT_meshopt_compression")
                )
            })
        })
    {
        geometry.quality = EstimateQuality::Unknown;
        geometry
            .notes
            .push("Compressed geometry expansion requires format-specific estimation".to_owned());
    }
    let mut dependency_keys = BTreeSet::from([key.to_owned()]);
    if let Some(images) = document.get("images").and_then(Value::as_array) {
        for image in images {
            if let Some(uri) = image.get("uri").and_then(Value::as_str) {
                let (texture_key, texture_path) = contained_dependency(root, path, uri)?;
                ensure!(
                    texture_key.ends_with(".ktx2"),
                    "external image is not a converted KTX2 texture"
                );
                dependency_keys.insert(texture_key.clone());
                resources.entry(texture_key).or_insert_with(|| {
                    ResourceCost::new(
                        ResourceKind::Texture,
                        inspect_texture(&texture_path).unwrap_or_else(|error| {
                            ByteEstimate::unknown(&format!(
                                "External texture metadata unavailable: {error}"
                            ))
                        }),
                    )
                });
            } else if image.get("mimeType").and_then(Value::as_str) == Some("image/ktx2") {
                let image_view = views
                    .get(usize::try_from(field_u64(image, "bufferView")?)?)
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!("embedded image names absent buffer view")
                    })?;
                ensure!(
                    field_u64(image_view, "buffer")? == 0
                        && buffers
                            .first()
                            .is_some_and(|buffer| buffer.get("uri").is_none()),
                    "embedded KTX2 image is not in the GLB binary chunk"
                );
                let offset = checked_add(
                    binary_offset,
                    image_view
                        .get("byteOffset")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                )?;
                let image_cost =
                    inspect_texture_region(path, offset, field_u64(image_view, "byteLength")?)?;
                geometry.resident_bytes =
                    checked_add(geometry.resident_bytes, image_cost.resident_bytes)?;
                geometry.peak_transient_bytes = checked_add(
                    geometry.peak_transient_bytes,
                    image_cost.peak_transient_bytes,
                )?;
                if image_cost.quality == EstimateQuality::Unknown {
                    geometry.quality = EstimateQuality::Unknown;
                }
                geometry.notes.push("Embedded KTX2 image allocations are included in this scene resource; repeated scene placements share them".to_owned());
            } else {
                geometry.quality = EstimateQuality::Unknown;
                geometry.notes.push("Embedded GLB image dimensions are not catalogued; apply unknown-resource fallback".to_owned());
            }
        }
    }
    let collision = collision_estimate(&document)?;
    let ecs = ByteEstimate::conservative(
        checked_add(16 * 1024, checked_mul(nodes, 4096)?)?,
        checked_add(16 * 1024, checked_mul(nodes, 4096)?)?,
        "Scene root and per-node ECS, hierarchy, instance and scheduling allocations; charged for every placement",
    );
    Ok((
        geometry,
        SceneCost {
            resource_keys: dependency_keys.into_iter().collect(),
            per_placement_collision: collision,
            per_placement_ecs: ecs,
        },
    ))
}

fn collision_estimate(document: &Value) -> Result<ByteEstimate> {
    let collision = document
        .get("scenes")
        .and_then(Value::as_array)
        .and_then(|scenes| scenes.first())
        .and_then(|scene| scene.get("extras"))
        .and_then(|extras| {
            extras
                .get("mudcrabCollision")
                .or_else(|| extras.get("openSkyrimCollision"))
        });
    let Some(collision) = collision else {
        return Ok(ByteEstimate::unknown(
            "No authored collision metadata; runtime may construct collision from render meshes",
        ));
    };
    if collision.get("authored").and_then(Value::as_bool) != Some(true)
        || !collision
            .get("version")
            .and_then(Value::as_u64)
            .is_some_and(|version| {
                version > 0 && version <= u64::from(shared::collision::COLLISION_ASSET_VERSION)
            })
    {
        return Ok(ByteEstimate::unknown(
            "Authored collision contract is unsupported",
        ));
    }
    let shapes = collision
        .get("shapes")
        .and_then(Value::as_array)
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid authored collision shapes"))?;
    let mut resident = 0_u64;
    for shape in shapes {
        let cost = match shape.get("kind").and_then(Value::as_str) {
            Some("mesh") => {
                let vertices = shape
                    .get("vertices")
                    .and_then(Value::as_array)
                    .ok_or_else(|| color_eyre::eyre::eyre!("invalid collision mesh vertices"))?
                    .len() as u64;
                let triangles = shape
                    .get("triangles")
                    .and_then(Value::as_array)
                    .ok_or_else(|| color_eyre::eyre::eyre!("invalid collision mesh triangles"))?
                    .len() as u64;
                checked_add(checked_mul(vertices, 48)?, checked_mul(triangles, 256)?)?
            }
            Some("hull") => checked_mul(
                shape
                    .get("points")
                    .and_then(Value::as_array)
                    .ok_or_else(|| color_eyre::eyre::eyre!("invalid collision hull"))?
                    .len() as u64,
                1024,
            )?,
            Some("box" | "capsule") => 4096,
            _ => {
                return Ok(ByteEstimate::unknown(
                    "Unsupported authored collision shape",
                ));
            }
        };
        resident = checked_add(resident, checked_add(cost, 4096)?)?;
    }
    let bodies = collision
        .get("bodies")
        .and_then(Value::as_array)
        .map_or(0, Vec::len) as u64;
    resident = checked_add(resident, checked_mul(bodies, 8192)?)?;
    let mut estimate = ByteEstimate::conservative(
        resident,
        checked_mul(resident, 2)?,
        "Per-placement collision vertices, triangle/BVH capacity, hull capacity, rigid bodies and build copies; authored empty collision costs zero",
    );
    if collision
        .get("skipped")
        .and_then(Value::as_array)
        .is_some_and(|skipped| !skipped.is_empty())
    {
        estimate.notes.push("Authored unsupported/skipped shapes remain absent at runtime; this estimates only represented shapes".to_owned());
    }
    Ok(estimate)
}

fn inspect_texture(path: &Path) -> Result<ByteEstimate> {
    let mut file = File::open(path)?;
    let file_bytes = file.metadata()?.len();
    inspect_texture_header(&mut file, file_bytes)
}

fn inspect_texture_region(path: &Path, offset: u64, length: u64) -> Result<ByteEstimate> {
    let mut file = File::open(path)?;
    ensure!(
        checked_add(offset, length)? <= file.metadata()?.len(),
        "embedded KTX2 region exceeds GLB file"
    );
    file.seek(SeekFrom::Start(offset))?;
    inspect_texture_header(&mut file, length)
}

fn inspect_texture_header(file: &mut File, file_bytes: u64) -> Result<ByteEstimate> {
    let region_offset = file.stream_position()?;
    ensure!(
        file_bytes >= ktx2::Header::LENGTH as u64,
        "truncated KTX2 header region"
    );
    let mut bytes = [0; ktx2::Header::LENGTH];
    file.read_exact(&mut bytes)
        .wrap_err("truncated KTX2 header")?;
    let header = ktx2::Header::from_bytes(&bytes)
        .map_err(|error| color_eyre::eyre::eyre!("invalid KTX2 header: {error:?}"))?;
    ensure!(
        matches!(header.face_count, 1 | 6) && header.type_size > 0,
        "invalid KTX2 face count or type size"
    );
    ensure!(
        header.pixel_depth == 0 || header.pixel_height > 0,
        "invalid KTX2 dimensions"
    );
    ensure!(
        header.face_count != 6
            || (header.pixel_height == header.pixel_width && header.pixel_depth == 0),
        "invalid KTX2 cubemap dimensions"
    );
    let max_dimension = header
        .pixel_width
        .max(header.pixel_height)
        .max(header.pixel_depth);
    let complete_levels = u32::BITS - max_dimension.leading_zeros();
    ensure!(
        header.level_count <= complete_levels,
        "KTX2 has more mip levels than its dimensions permit"
    );
    let stored_levels = header.level_count.max(1);
    let index_end = checked_add(
        ktx2::Header::LENGTH as u64,
        checked_mul(u64::from(stored_levels), ktx2::LevelIndex::LENGTH as u64)?,
    )?;
    ensure!(index_end <= file_bytes, "truncated KTX2 mip index");
    let mut max_decompressed_level = 0_u64;
    let mut ranges = Vec::with_capacity(stored_levels as usize);
    for _ in 0..stored_levels {
        let mut level_bytes = [0; ktx2::LevelIndex::LENGTH];
        file.read_exact(&mut level_bytes)?;
        let level = ktx2::LevelIndex::from_bytes(&level_bytes);
        let end = checked_add(level.byte_offset, level.byte_length)?;
        ensure!(
            level.byte_length > 0 && level.byte_offset >= index_end && end <= file_bytes,
            "KTX2 mip data range is invalid"
        );
        ranges.push((level.byte_offset, end));
        max_decompressed_level = max_decompressed_level.max(level.uncompressed_byte_length);
    }
    ranges.sort_unstable();
    ensure!(
        ranges.windows(2).all(|pair| pair[0].1 <= pair[1].0),
        "KTX2 mip data ranges overlap"
    );
    for (offset, length) in [
        (
            u64::from(header.index.dfd_byte_offset),
            u64::from(header.index.dfd_byte_length),
        ),
        (
            u64::from(header.index.kvd_byte_offset),
            u64::from(header.index.kvd_byte_length),
        ),
        (header.index.sgd_byte_offset, header.index.sgd_byte_length),
    ] {
        ensure!(
            length == 0 || (offset >= index_end && checked_add(offset, length)? <= file_bytes),
            "KTX2 metadata range is invalid"
        );
    }
    let raw_format = header.format.map_or(0, |format| format.value());
    let (texel_bytes, known_format) = match raw_format {
        0 => universal_texture_texel_bytes(file, &header, region_offset)?,
        143 | 144 => (8, true), // BC6H may expand to RGBA16F on format fallback.
        1..=70 | 122..=127 | 131..=184 => (4, true),
        71..=97 | 128..=130 => (8, true),
        98..=109 => (16, true),
        110..=121 => (32, true),
        _ => (32, false),
    };
    let cost_levels = if header.level_count == 0 {
        complete_levels
    } else {
        header.level_count
    };
    let mut expanded_texels = 0_u64;
    for level in 0..cost_levels {
        let width = u64::from((header.pixel_width >> level).max(1));
        let height = u64::from((header.pixel_height.max(1) >> level).max(1));
        let depth = u64::from((header.pixel_depth.max(1) >> level).max(1));
        let bytes = checked_mul(
            checked_mul(checked_mul(width, height)?, depth)?,
            checked_mul(
                checked_mul(
                    u64::from(header.layer_count.max(1)),
                    u64::from(header.face_count),
                )?,
                texel_bytes,
            )?,
        )?;
        expanded_texels = checked_add(expanded_texels, bytes)?;
    }
    let resident = checked_add(checked_mul(expanded_texels, 2)?, 4096)?;
    let transient = checked_add(
        file_bytes,
        checked_add(checked_mul(expanded_texels, 2)?, max_decompressed_level)?,
    )?;
    let mut estimate = ByteEstimate::conservative(
        resident,
        transient,
        "Dimensions, arrays/faces/depth and all mip levels expanded to at least RGBA8 (higher precision when needed), CPU/GPU copies and decode/upload buffers; compressed file bytes are transient only",
    );
    if !known_format {
        estimate.quality = EstimateQuality::Unknown;
        estimate.notes.push(if raw_format == 0 {
            "Universal KTX2 color model or precision is unsupported; dimensional estimate uses 32 bytes per texel and requires fallback".to_owned()
        } else {
            format!("Unsupported Vulkan texture format {raw_format}; dimensional estimate uses 32 bytes per texel and requires fallback")
        });
    }
    if header
        .supercompression_scheme
        .is_some_and(|scheme| !matches!(scheme.value(), 1..=3))
    {
        estimate.quality = EstimateQuality::Unknown;
        estimate
            .notes
            .push("Unsupported KTX2 supercompression scheme".to_owned());
    }
    if header.level_count == 0 {
        estimate.notes.push(
            "Header requests generated mipmaps; reservation includes the complete mip chain"
                .to_owned(),
        );
    }
    Ok(estimate)
}

fn universal_texture_texel_bytes(
    file: &mut File,
    header: &ktx2::Header,
    region_offset: u64,
) -> Result<(u64, bool)> {
    let length = u64::from(header.index.dfd_byte_length);
    if !(28..=4096).contains(&length) {
        return Ok((32, false));
    }
    file.seek(SeekFrom::Start(checked_add(
        region_offset,
        u64::from(header.index.dfd_byte_offset),
    )?))?;
    let mut descriptor = vec![0; length as usize];
    file.read_exact(&mut descriptor)?;
    ensure!(
        u64::from(u32::from_le_bytes(descriptor[..4].try_into().unwrap())) == length,
        "KTX2 data-format descriptor length is inconsistent"
    );
    if descriptor[4..8] != [0; 4] || u16::from_le_bytes(descriptor[8..10].try_into().unwrap()) != 2
    {
        return Ok((32, false));
    }
    let block_length = usize::from(u16::from_le_bytes(descriptor[10..12].try_into().unwrap()));
    ensure!(
        block_length >= 24 && block_length + 4 <= descriptor.len(),
        "KTX2 basic data-format descriptor is truncated"
    );
    let basic_bytes = &descriptor[12..4 + block_length];
    ensure!(
        basic_bytes[4..8]
            .iter()
            .all(|dimension| *dimension < u8::MAX),
        "KTX2 data-format descriptor block dimensions overflow"
    );
    let basic = ktx2::dfd::Basic::parse(basic_bytes)
        .map_err(|error| color_eyre::eyre::eyre!("invalid KTX2 basic descriptor: {error:?}"))?;
    let ldr = matches!(
        basic.color_model,
        Some(ktx2::ColorModel::UASTC | ktx2::ColorModel::ETC1S)
    ) && !basic.sample_information.is_empty()
        && basic.sample_information.iter().all(|sample| {
            !sample.channel_type_qualifiers.intersects(
                ktx2::dfd::ChannelTypeQualifiers::FLOAT
                    | ktx2::dfd::ChannelTypeQualifiers::EXPONENT,
            )
        });
    Ok(if ldr { (4, true) } else { (32, false) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> tempfile::TempDir {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("meshes")).unwrap();
        fs::create_dir(fixture.path().join("textures")).unwrap();
        fs::write(
            fixture.path().join("conversion-manifest.json"),
            br#"{"complete":true,"entries":{}}"#,
        )
        .unwrap();
        fixture
    }

    fn write_glb(path: &Path, document: &Value, binary: &[u8]) {
        let mut json = serde_json::to_vec(document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bytes = b"glTF".to_vec();
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&((20 + json.len() + 8 + binary.len()) as u32).to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"JSON");
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(&(binary.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"BIN\0");
        bytes.extend_from_slice(binary);
        fs::write(path, bytes).unwrap();
    }

    fn write_ktx2(
        path: &Path,
        width: u32,
        height: u32,
        faces: u32,
        layers: u32,
        depth: u32,
        levels: u32,
    ) {
        let header = ktx2::Header {
            format: Some(ktx2::Format::BC7_UNORM_BLOCK),
            type_size: 1,
            pixel_width: width,
            pixel_height: height,
            pixel_depth: depth,
            layer_count: layers,
            face_count: faces,
            level_count: levels,
            supercompression_scheme: None,
            index: ktx2::Index {
                dfd_byte_offset: 0,
                dfd_byte_length: 0,
                kvd_byte_offset: 0,
                kvd_byte_length: 0,
                sgd_byte_offset: 0,
                sgd_byte_length: 0,
            },
        };
        let mut bytes = header.as_bytes().to_vec();
        for level in 0..levels.max(1) {
            bytes.extend_from_slice(
                &ktx2::LevelIndex {
                    byte_offset: 80 + u64::from(levels.max(1)) * 24 + u64::from(level) * 16,
                    byte_length: 16,
                    uncompressed_byte_length: 16,
                }
                .as_bytes(),
            );
        }
        bytes.resize(bytes.len() + levels.max(1) as usize * 16, 0);
        fs::write(path, bytes).unwrap();
    }

    fn document(uri: &str) -> Value {
        json!({"asset":{"version":"2.0"}, "buffers":[{"byteLength":36}],
            "bufferViews":[{"buffer":0,"byteLength":36}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "images":[{"uri":uri}], "nodes":[{"mesh":0}], "scenes":[{"nodes":[0],
                "extras":{"mudcrabCollision":{"version":2,"authored":true,"shapes":[],"skipped":[],"bodies":[]}}}]})
    }

    #[test]
    fn two_glbs_share_one_texture_and_zero_authored_collision_is_preserved() {
        let fixture = fixture();
        write_ktx2(
            &fixture.path().join("textures/shared.ktx2"),
            1024,
            1024,
            1,
            0,
            0,
            1,
        );
        for name in ["a.glb", "b.glb"] {
            write_glb(
                &fixture.path().join("meshes").join(name),
                &document("../textures/shared.ktx2"),
                &[0; 36],
            );
        }
        let catalog = analyze_pack(fixture.path()).unwrap();
        assert_eq!(catalog.resources.len(), 3);
        assert_eq!(catalog.scenes.len(), 2);
        assert_eq!(
            catalog.scenes["meshes/a.glb"].resource_keys,
            vec!["meshes/a.glb", "textures/shared.ktx2"]
        );
        assert_eq!(
            catalog.scenes["meshes/a.glb"]
                .per_placement_collision
                .resident_bytes,
            0
        );
        let texture = &catalog.resources["textures/shared.ktx2"];
        assert_eq!(texture.resident_bytes, 1024 * 1024 * 4 * 2 + 4096);
        assert!(
            texture.resident_bytes
                > fs::metadata(fixture.path().join("textures/shared.ktx2"))
                    .unwrap()
                    .len()
                    * 1000
        );
    }

    #[test]
    fn texture_cost_includes_mips_array_layers_faces_and_volume_depth() {
        let fixture = fixture();
        let cube = fixture.path().join("textures/cube.ktx2");
        write_ktx2(&cube, 8, 8, 6, 2, 0, 2);
        assert_eq!(
            inspect_texture(&cube).unwrap().resident_bytes,
            (64 + 16) * 6 * 2 * 4 * 2 + 4096
        );
        let volume = fixture.path().join("textures/volume.ktx2");
        write_ktx2(&volume, 8, 8, 1, 0, 8, 2);
        assert_eq!(
            inspect_texture(&volume).unwrap().resident_bytes,
            (8 * 8 * 8 + 4 * 4 * 4) * 4 * 2 + 4096
        );
    }

    #[test]
    fn universal_texture_precision_is_read_from_bounded_dfd_metadata() {
        let fixture = fixture();
        let path = fixture.path().join("textures/universal.ktx2");
        let (mut basic, _) = ktx2::dfd::Basic::from_format(ktx2::Format::BC7_UNORM_BLOCK).unwrap();
        basic.color_model = Some(ktx2::ColorModel::UASTC);
        for hdr in [false, true] {
            if hdr {
                basic.sample_information[0].channel_type_qualifiers |=
                    ktx2::dfd::ChannelTypeQualifiers::FLOAT;
            }
            write_ktx2(&path, 1024, 1024, 1, 0, 0, 1);
            let mut bytes = fs::read(&path).unwrap();
            let block = ktx2::dfd::Block::Basic(basic.clone()).to_vec();
            let mut descriptor = ((block.len() + 4) as u32).to_le_bytes().to_vec();
            descriptor.extend_from_slice(&block);
            bytes[12..16].copy_from_slice(&0_u32.to_le_bytes());
            bytes[48..52].copy_from_slice(&104_u32.to_le_bytes());
            bytes[52..56].copy_from_slice(&(descriptor.len() as u32).to_le_bytes());
            bytes[80..88].copy_from_slice(&(104 + descriptor.len() as u64).to_le_bytes());
            bytes.splice(104..104, descriptor);
            fs::write(&path, &bytes).unwrap();
            let estimate = inspect_texture(&path).unwrap();
            assert_eq!(
                estimate.resident_bytes,
                1024 * 1024 * if hdr { 32 } else { 4 } * 2 + 4096
            );
            assert_eq!(
                estimate.quality,
                if hdr {
                    EstimateQuality::Unknown
                } else {
                    EstimateQuality::Conservative
                }
            );
        }
    }

    #[test]
    fn embedded_ktx2_allocation_is_shared_with_its_scene_without_decoding() {
        let fixture = fixture();
        let image_path = fixture.path().join("textures/embedded.ktx2");
        write_ktx2(&image_path, 1024, 1024, 1, 0, 0, 1);
        let image = fs::read(&image_path).unwrap();
        fs::remove_file(&image_path).unwrap();
        let mut binary = vec![0; 36];
        binary.extend_from_slice(&image);
        let mut document = document("unused");
        document["buffers"][0]["byteLength"] = json!(binary.len());
        document["bufferViews"].as_array_mut().unwrap().push(json!({
            "buffer":0,"byteOffset":36,"byteLength":image.len()}));
        document["images"] = json!([{"bufferView":1,"mimeType":"image/ktx2"}]);
        write_glb(
            &fixture.path().join("meshes/embedded.glb"),
            &document,
            &binary,
        );
        let catalog = analyze_pack(fixture.path()).unwrap();
        assert_eq!(catalog.resources.len(), 1);
        let geometry = &catalog.resources["meshes/embedded.glb"];
        assert_eq!(geometry.quality, EstimateQuality::Conservative);
        assert!(geometry.resident_bytes >= 1024 * 1024 * 4 * 2);
        let first = catalog.resolve_scene("meshes/embedded.glb", 4096).unwrap();
        let second = catalog.resolve_scene("meshes/embedded.glb", 4096).unwrap();
        let unique: BTreeMap<_, _> = first
            .resources
            .into_iter()
            .chain(second.resources)
            .collect();
        assert_eq!(unique.len(), 1);
        assert_eq!(
            unique["meshes/embedded.glb"].resident_bytes,
            geometry.resident_bytes
        );
        document["bufferViews"][1]["byteLength"] = json!(4);
        write_glb(
            &fixture.path().join("meshes/embedded.glb"),
            &document,
            &binary,
        );
        assert_eq!(
            analyze_pack(fixture.path()).unwrap().resources["meshes/embedded.glb"].quality,
            EstimateQuality::Unknown
        );
    }

    #[test]
    fn corrupt_headers_bounds_and_estimate_overflow_become_explicit_unknowns() {
        let fixture = fixture();
        fs::write(fixture.path().join("textures/bad.ktx2"), b"bad").unwrap();
        fs::write(fixture.path().join("meshes/bad.glb"), b"bad").unwrap();
        let catalog = analyze_pack(fixture.path()).unwrap();
        assert_eq!(
            catalog.resources["textures/bad.ktx2"].quality,
            EstimateQuality::Unknown
        );
        assert_eq!(
            catalog.resources["meshes/bad.glb"].quality,
            EstimateQuality::Unknown
        );
        let path = fixture.path().join("textures/overflow.ktx2");
        write_ktx2(&path, u32::MAX, u32::MAX, 1, u32::MAX, u32::MAX, 1);
        assert!(inspect_texture(&path).is_err());
        let mut invalid = document("../textures/bad.ktx2");
        invalid["accessors"][0]["count"] = json!(u64::MAX);
        write_glb(
            &fixture.path().join("meshes/overflow.glb"),
            &invalid,
            &[0; 36],
        );
        assert!(read_glb_json(&fixture.path().join("meshes/overflow.glb")).is_ok());
        assert_eq!(
            analyze_pack(fixture.path()).unwrap().resources["meshes/overflow.glb"].quality,
            EstimateQuality::Unknown
        );
    }

    #[test]
    fn external_uri_traversal_and_symlink_escape_cannot_read_other_files() {
        let fixture = fixture();
        write_glb(
            &fixture.path().join("meshes/escape.glb"),
            &document("../../secret.ktx2"),
            &[0; 36],
        );
        let catalog = analyze_pack(fixture.path()).unwrap();
        assert_eq!(
            catalog.resources["meshes/escape.glb"].quality,
            EstimateQuality::Unknown
        );
        assert!(!catalog.resources.contains_key("secret.ktx2"));
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("secret.ktx2");
            fs::write(&target, b"must not read").unwrap();
            std::os::unix::fs::symlink(&target, fixture.path().join("textures/escaped.ktx2"))
                .unwrap();
            write_glb(
                &fixture.path().join("meshes/symlink.glb"),
                &document("../textures/escaped.ktx2"),
                &[0; 36],
            );
            assert_eq!(
                analyze_pack(fixture.path()).unwrap().resources["meshes/symlink.glb"].quality,
                EstimateQuality::Unknown
            );
        }
    }

    #[test]
    fn export_is_read_only_and_refuses_existing_or_in_pack_destinations() {
        let fixture = fixture();
        let outside = tempfile::tempdir().unwrap();
        let original = fs::read(fixture.path().join("conversion-manifest.json")).unwrap();
        assert!(export_pack(fixture.path(), &fixture.path().join("costs.json")).is_err());
        let output = outside.path().join("costs.json");
        let catalog = export_pack(fixture.path(), &output).unwrap();
        assert!(export_pack(fixture.path(), &output).is_err());
        assert_eq!(
            fs::read(fixture.path().join("conversion-manifest.json")).unwrap(),
            original
        );
        assert_eq!(
            StreamingCostCatalog::load(&output, &catalog.pack_fingerprint_sha256).unwrap(),
            catalog
        );
    }

    #[test]
    fn ktx2_rejects_overlap_and_out_of_file_levels() {
        let fixture = fixture();
        let path = fixture.path().join("textures/ranges.ktx2");
        write_ktx2(&path, 8, 8, 1, 0, 0, 2);
        let mut bytes = fs::read(&path).unwrap();
        bytes[104..112].copy_from_slice(&128_u64.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        assert!(inspect_texture(&path).is_err());
        bytes[104..112].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        assert!(inspect_texture(&path).is_err());
    }
}
