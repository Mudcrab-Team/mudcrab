//! What a converted GLB says about itself without loading its geometry: how many primitives it
//! has and which textures its images name. The primitive count is the exact set of primitive
//! indices the converter's `glb_input::load_model` numbers (one per (node, primitive) pair reached from
//! the default scene, in depth-first order), so the expected primitive set of a reference's model
//! comes from the source GLB and never from the cluster tables. Only the GLB header and its JSON
//! chunk are read.

use crate::cluster::{canonical_json, hex};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

const GLB_MAGIC: &[u8; 4] = b"glTF";
const GLB_JSON_CHUNK: &[u8; 4] = b"JSON";
/// A JSON chunk larger than this is not a converted model.
const MAX_JSON_BYTES: u64 = 256 * 1024 * 1024;
/// Node visits allowed while counting, so a cyclic scene graph fails instead of looping.
const MAX_VISITS: usize = 4_000_000;

/// What the GLB's JSON chunk says.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelFacts {
    /// Every primitive index `load_model` would number, `0..count`.
    pub primitives: BTreeSet<u32>,
    /// The resolved asset path of every image that has a URI (lower case, like the material keys).
    pub texture_paths: BTreeSet<String>,
    /// What `load_model` gives each primitive for its material (see [`MaterialDescriptor`]).
    pub materials: BTreeMap<u32, MaterialDescriptor>,
}

/// The material facts of one primitive, as the stage turns them into a mesh row. The key is the
/// hash of the material with every texture reference resolved against the GLB's path, so
/// same-byte GLBs in other folders can have other keys; the rest comes from the bytes alone.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialDescriptor {
    pub material_index: Option<u32>,
    pub material_key: String,
    /// `alphaMode` MASK (a blended primitive is never a mesh, so its flag is not used).
    pub alpha_mask: bool,
    pub alpha_cutoff: Option<f32>,
    pub double_sided: bool,
}

/// Reads the facts of `assets_root/glb_path`, reading only the header and the JSON chunk.
pub fn read_model_facts(assets_root: &Path, glb_path: &str) -> Result<ModelFacts> {
    let path = assets_root.join(glb_path);
    let mut file =
        fs::File::open(&path).wrap_err_with(|| format!("cannot open {}", path.display()))?;
    let mut header = [0u8; 20];
    file.read_exact(&mut header)
        .wrap_err_with(|| format!("{} is too short to be a GLB", path.display()))?;
    ensure!(
        &header[0..4] == GLB_MAGIC && &header[16..20] == GLB_JSON_CHUNK,
        "{} is not a GLB with a JSON first chunk",
        path.display()
    );
    let json_length = u64::from(u32::from_le_bytes(header[12..16].try_into().unwrap()));
    ensure!(
        json_length <= MAX_JSON_BYTES,
        "{} has an oversized JSON chunk",
        path.display()
    );
    let mut json = vec![0u8; json_length as usize];
    file.read_exact(&mut json)
        .wrap_err_with(|| format!("{} ends inside its JSON chunk", path.display()))?;
    model_facts_from_json(&json, glb_path)
}

/// The JSON of a GLB's first chunk, as raw JSON. Everything that decides a material key or a
/// primitive number is read from this document and never from a typed glTF model, so the answer
/// does not depend on which crate features the build that links this code happens to enable.
pub fn parse_glb_json(bytes: &[u8]) -> Result<Value> {
    ensure!(
        bytes.len() >= 20 && &bytes[0..4] == GLB_MAGIC && &bytes[16..20] == GLB_JSON_CHUNK,
        "not a GLB with a JSON first chunk"
    );
    let length = u64::from(u32::from_le_bytes(bytes[12..16].try_into().unwrap()));
    ensure!(length <= MAX_JSON_BYTES, "oversized JSON chunk");
    let end = 20 + length as usize;
    ensure!(bytes.len() >= end, "the GLB ends inside its JSON chunk");
    parse_json(&bytes[20..end])
}

fn parse_json(json: &[u8]) -> Result<Value> {
    let document: Value =
        serde_json::from_slice(json).map_err(|error| eyre!("invalid glTF JSON: {error}"))?;
    check_document(&document)?;
    Ok(document)
}

/// The shapes this module consumes: the arrays the facts and material keys read must be arrays
/// when present (omitted means empty), the images and textures in them objects, an image uri a
/// string. It is not a glTF validator: only what the projection reads is checked, so a malformed
/// document is unknown and never an expanded empty or fallback one.
fn check_document(document: &Value) -> Result<()> {
    ensure!(document.is_object(), "the glTF JSON is not an object");
    for name in [
        "scenes",
        "nodes",
        "meshes",
        "materials",
        "textures",
        "images",
    ] {
        list(document, name)?;
    }
    for image in list(document, "images")? {
        ensure!(image.is_object(), "an image is not an object");
        if let Some(uri) = image.get("uri") {
            ensure!(uri.is_string(), "an image uri is not a string");
        }
    }
    for texture in list(document, "textures")? {
        ensure!(texture.is_object(), "a texture is not an object");
    }
    Ok(())
}

fn index_of(value: &Value, what: &str) -> Result<usize> {
    value
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
        .map(|index| index as usize)
        .ok_or_else(|| eyre!("{what} is not an index"))
}

/// The array `document[name]` names: empty when the field is omitted, an error when it is there
/// and is not an array, so a malformed structure is unknown and never an expanded empty one.
fn list<'a>(document: &'a Value, name: &str) -> Result<&'a [Value]> {
    match document.get(name) {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(eyre!("{name} is not an array")),
    }
}

/// The indices an optional array field of `object` lists (empty when omitted).
fn indices(object: &Value, name: &str, what: &str) -> Result<Vec<usize>> {
    match object.get(name) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items.iter().map(|item| index_of(item, what)).collect(),
        Some(_) => Err(eyre!("{what} list is not an array")),
    }
}

/// The raw JSON of the material in slot `slot` of the document: null for a primitive with no
/// material, an error for an explicit slot the document does not have or that is not an object.
pub fn raw_material(document: &Value, slot: Option<usize>) -> Result<Value> {
    let Some(slot) = slot else {
        return Ok(Value::Null);
    };
    let material = list(document, "materials")?
        .get(slot)
        .ok_or_else(|| eyre!("material slot {slot} is out of range"))?;
    ensure!(
        material.is_object(),
        "material slot {slot} is not an object"
    );
    Ok(material.clone())
}

/// The facts of a GLB JSON chunk.
pub fn model_facts_from_json(json: &[u8], glb_path: &str) -> Result<ModelFacts> {
    let document = parse_json(json)?;
    let scenes = list(&document, "scenes")?;
    let scene = match document.get("scene") {
        Some(index) => Some(index_of(index, "the default scene")?),
        None => (!scenes.is_empty()).then_some(0),
    }
    .and_then(|index| scenes.get(index))
    .ok_or_else(|| eyre!("GLB has no scene"))?;
    ensure!(scene.is_object(), "the selected scene is not an object");
    let nodes = list(&document, "nodes")?;
    let meshes = list(&document, "meshes")?;
    // The depth-first order of `load_model`, so a primitive has the same index here. A scene with
    // no `nodes` field is a valid empty scene; anything present must be well formed.
    let mut materials = BTreeMap::new();
    let mut stack = indices(scene, "nodes", "a scene node")?;
    stack.reverse();
    let mut visits = 0usize;
    while let Some(node) = stack.pop() {
        visits += 1;
        ensure!(
            visits <= MAX_VISITS,
            "the scene graph is cyclic or too large"
        );
        let node = nodes
            .get(node)
            .ok_or_else(|| eyre!("a scene node index is out of range"))?;
        ensure!(node.is_object(), "a node is not an object");
        if let Some(mesh) = node.get("mesh") {
            let mesh = meshes
                .get(index_of(mesh, "a node's mesh")?)
                .ok_or_else(|| eyre!("a node's mesh index is out of range"))?;
            let primitives = mesh
                .get("primitives")
                .and_then(Value::as_array)
                .ok_or_else(|| eyre!("a mesh has no primitives array"))?;
            for primitive in primitives {
                ensure!(primitive.is_object(), "a primitive is not an object");
                let index =
                    u32::try_from(materials.len()).map_err(|_| eyre!("too many primitives"))?;
                let slot = primitive
                    .get("material")
                    .map(|slot| index_of(slot, "a primitive's material"))
                    .transpose()?;
                let material = raw_material(&document, slot)?;
                let alpha_mask = material.get("alphaMode").and_then(Value::as_str) == Some("MASK");
                materials.insert(
                    index,
                    MaterialDescriptor {
                        material_index: slot.map(|slot| slot as u32),
                        material_key: material_key(&material, &document, glb_path)?,
                        alpha_mask,
                        // As `read_primitive` reads it: 0.5 when a mask names no cutoff.
                        alpha_cutoff: alpha_mask.then(|| {
                            material
                                .get("alphaCutoff")
                                .and_then(Value::as_f64)
                                .map_or(0.5, |cutoff| cutoff as f32)
                        }),
                        double_sided: material
                            .get("doubleSided")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    },
                );
            }
        }
        let mut children = indices(node, "children", "a node's child")?;
        children.reverse();
        stack.extend(children);
    }
    let mut texture_paths = BTreeSet::new();
    for image in list(&document, "images")? {
        if let Some(uri) = image.get("uri").and_then(Value::as_str) {
            texture_paths.insert(resolve_uri(glb_path, uri).to_lowercase());
        }
    }
    Ok(ModelFacts {
        primitives: materials.keys().copied().collect(),
        texture_paths,
        materials,
    })
}

/// Strip converter-owned sampler/transfer aliases back to the converted source.
/// Alias ordering is transfer first, sampler second; only known suffixes are reserved.
pub fn runtime_texture_source(runtime_key: &str) -> Option<String> {
    let mut stem = runtime_key.strip_suffix(".ktx2")?;
    for suffix in [".opensky-wrap0", ".opensky-wrap1", ".opensky-wrap2"] {
        if let Some(source) = stem.strip_suffix(suffix) {
            stem = source;
            break;
        }
    }
    stem = stem.strip_suffix(".opensky-srgb").unwrap_or(stem);
    Some(format!("{stem}.ktx2"))
}

/// SHA-256 (hex) over the sorted (path, content hash) pairs of the textures a model names. A
/// texture the conversion did not publish is listed with a null hash, so it is not forgotten and
/// a later conversion that adds it changes the hash. `textures` maps a published texture path (lower
/// case) to its content hash.
pub fn texture_set_hash<'a>(
    facts: &ModelFacts,
    texture_hash: impl Fn(&str) -> Option<&'a str>,
) -> String {
    let pairs: Vec<_> = facts
        .texture_paths
        .iter()
        .map(|path| json!([path, texture_hash(path)]))
        .collect();
    hex(&Sha256::digest(canonical_json(&json!(pairs))))
}

/// SHA-256 of the material as canonical JSON, every texture reference replaced by the converted
/// texture's asset key (ADR-0013 section 3), so identical materials in different GLBs share a key.
pub fn material_key(material: &Value, document: &Value, glb_path: &str) -> Result<String> {
    check_document(document)?;
    let mut value = material.clone();
    resolve_textures(&mut value, document, glb_path, None)?;
    Ok(crate::cluster::hex(&Sha256::digest(
        crate::cluster::canonical_json(&value),
    )))
}

fn resolve_textures(
    value: &mut Value,
    document: &Value,
    glb_path: &str,
    parent_key: Option<&str>,
) -> Result<()> {
    match value {
        Value::Object(map) => {
            let is_texture_info = parent_key.is_some_and(|key| key.ends_with("Texture"));
            if is_texture_info && let Some(index) = map.get("index") {
                let key = texture_key(document, glb_path, index_of(index, "a texture index")?)?;
                map.insert("index".into(), key);
            }
            for (key, child) in map.iter_mut() {
                // `extras` is application data, hashed as it is: a key there that looks like a
                // texture reference (`texture`, `fooTexture`) is not one.
                if key == "extras" {
                    continue;
                }
                if key == "texture" {
                    *child = texture_key(document, glb_path, index_of(child, "a texture index")?)?;
                } else {
                    resolve_textures(child, document, glb_path, Some(key))?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                resolve_textures(item, document, glb_path, parent_key)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The asset key of texture `texture`: its image's resolved uri, or `texture#N` for a texture
/// whose image has no uri (embedded) or that names no source. A texture or image the document does
/// not have is an error.
fn texture_key(document: &Value, glb_path: &str, texture: usize) -> Result<Value> {
    let entry = list(document, "textures")?
        .get(texture)
        .ok_or_else(|| eyre!("texture {texture} is out of range"))?;
    let uri = match entry.get("source") {
        None => None,
        Some(source) => {
            let source = index_of(source, "a texture source")?;
            list(document, "images")?
                .get(source)
                .ok_or_else(|| eyre!("image {source} is out of range"))?
                .get("uri")
                .and_then(Value::as_str)
        }
    };
    Ok(Value::String(match uri {
        Some(uri) => resolve_uri(glb_path, uri),
        None => format!("texture#{texture}"),
    }))
}

/// Resolves a relative texture URI against the GLB's folder, without touching the file system.
pub fn resolve_uri(glb_path: &str, uri: &str) -> String {
    let mut parts: Vec<&str> = glb_path.split('/').collect();
    parts.pop();
    let uri = uri.replace('\\', "/");
    for part in uri.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn uris_resolve_against_the_glb_folder() {
        assert_eq!(
            resolve_uri(
                "meshes/architecture/whiterun/x.glb",
                "../../../textures/a/B.ktx2"
            ),
            "textures/a/b.ktx2"
        );
    }

    #[test]
    fn a_missing_scene_or_a_cycle_is_an_error() {
        let no_scene = br#"{"asset":{"version":"2.0"}}"#;
        assert!(model_facts_from_json(no_scene, "x.glb").is_err());
        let cycle = br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
            "nodes":[{"children":[0]}]}"#;
        assert!(model_facts_from_json(cycle, "x.glb").is_err());
    }

    #[test]
    fn the_texture_set_hash_follows_paths_and_content_hashes() {
        let facts = ModelFacts {
            primitives: BTreeSet::new(),
            texture_paths: ["textures/a.ktx2".to_owned(), "textures/b.ktx2".to_owned()].into(),
            materials: BTreeMap::new(),
        };
        let mut textures = HashMap::from([("textures/a.ktx2".to_owned(), "11".repeat(32))]);
        let missing_b = texture_set_hash(&facts, |path| textures.get(path).map(String::as_str));
        textures.insert("textures/b.ktx2".to_owned(), "22".repeat(32));
        let with_b = texture_set_hash(&facts, |path| textures.get(path).map(String::as_str));
        assert_ne!(missing_b, with_b);
        textures.insert("textures/b.ktx2".to_owned(), "33".repeat(32));
        assert_ne!(
            with_b,
            texture_set_hash(&facts, |path| textures.get(path).map(String::as_str))
        );
        let mut same = textures.clone();
        same.insert("textures/b.ktx2".to_owned(), "22".repeat(32));
        assert_eq!(
            with_b,
            texture_set_hash(&facts, |path| same.get(path).map(String::as_str))
        );
    }
}

#[cfg(test)]
mod raw_projection_tests {
    use super::*;
    use serde_json::json;

    fn document(material: Value) -> Value {
        json!({
            "textures": [{"source": 0}],
            "images": [{"uri": "../textures/A.ktx2"}],
            "materials": [material],
        })
    }

    #[test]
    fn the_key_is_the_raw_material_with_textures_resolved_and_nothing_else() {
        let material = json!({
            "name": "wall",
            "pbrMetallicRoughness": {"baseColorTexture": {"index": 0, "extensions": {
                "KHR_texture_transform": {"scale": [2.0, 2.0]}}}},
            "extensions": {"KHR_materials_emissive_strength": {"emissiveStrength": 3.0}},
        });
        let doc = document(material.clone());
        let key = material_key(&material, &doc, "meshes/x/y.glb").unwrap();
        // The same key from a document that spells the texture another way.
        let other = json!({
            "textures": [{"source": 0}],
            "images": [{"uri": "..\\textures\\a.ktx2"}],
            "materials": [material.clone()],
        });
        assert_eq!(
            key,
            material_key(&material, &other, "meshes/x/y.glb").unwrap()
        );
        // Raw fields count: a name, an extension value or a default spelled out each change it.
        let mut named = material.clone();
        named["name"] = json!("floor");
        assert_ne!(key, material_key(&named, &doc, "meshes/x/y.glb").unwrap());
        let mut scaled = material.clone();
        scaled["pbrMetallicRoughness"]["baseColorTexture"]["extensions"]["KHR_texture_transform"]
            ["scale"] = json!([3.0, 2.0]);
        assert_ne!(key, material_key(&scaled, &doc, "meshes/x/y.glb").unwrap());
    }

    #[test]
    fn only_an_omitted_material_is_null() {
        let doc = document(json!({}));
        assert_eq!(raw_material(&doc, None).unwrap(), Value::Null);
        assert_eq!(raw_material(&doc, Some(0)).unwrap(), json!({}));
        // An explicit slot the document does not have is unknown, not the default material.
        assert!(raw_material(&doc, Some(9)).is_err());
    }
}

#[cfg(test)]
mod malformed_tests {
    use super::*;
    use serde_json::json;

    fn facts(document: Value) -> Result<ModelFacts> {
        model_facts_from_json(document.to_string().as_bytes(), "meshes/x.glb")
    }

    fn base() -> Value {
        json!({
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "children": [1]}, {}],
            "meshes": [{"primitives": [{"material": 0}, {}]}],
            "materials": [{"alphaMode": "MASK"}],
            "images": [{"uri": "a.ktx2"}, {"bufferView": 0}],
        })
    }

    #[test]
    fn omitted_optional_fields_and_empty_scenes_are_known_empty() {
        let known = facts(base()).unwrap();
        assert_eq!(known.primitives, BTreeSet::from([0, 1]));
        let empty_scene = facts(json!({"scenes": [{}]})).unwrap();
        assert!(empty_scene.primitives.is_empty());
        let no_nodes = facts(json!({"scenes": [{"nodes": []}], "nodes": []})).unwrap();
        assert!(no_nodes.primitives.is_empty() && no_nodes.texture_paths.is_empty());
    }

    #[test]
    fn malformed_structures_are_unknown_never_an_empty_expected_set() {
        let edits: [(&str, Value); 14] = [
            ("scenes", json!("x")),
            ("scene", json!("x")),
            ("scene", json!(5)),
            ("nodes", json!({})),
            ("meshes", json!(3)),
            ("materials", json!("x")),
            ("images", json!(null)),
            ("nodes", json!([5, 6])),
            ("meshes", json!([{"primitives": 3}])),
            ("meshes", json!([{}])),
            ("meshes", json!([{"primitives": [3]}])),
            ("meshes", json!([{"primitives": [{"material": 7}]}])),
            (
                "meshes",
                json!([{"primitives": [{"material": 4294967296u64}]}]),
            ),
            ("images", json!([{"uri": 5}])),
        ];
        for (field, bad) in edits {
            let mut document = base();
            document[field] = bad.clone();
            assert!(facts(document).is_err(), "{field} = {bad}");
        }
        for children in [json!(3), json!(["x"]), json!([9])] {
            let mut document = base();
            document["nodes"][0]["children"] = children.clone();
            assert!(facts(document).is_err(), "children = {children}");
        }
        let mut document = base();
        document["scenes"][0]["nodes"] = json!("x");
        assert!(facts(document).is_err());
        let mut document = base();
        document["materials"] = json!([3]);
        assert!(facts(document).is_err());
        let mut document = base();
        document["nodes"][0]["mesh"] = json!(-1);
        assert!(facts(document).is_err());
    }

    #[test]
    fn every_consumed_projection_shape_is_checked_even_when_unused() {
        // The selected scene must be an object, not a number.
        assert!(facts(json!({"scenes": [5]})).is_err());
        assert!(facts(json!({"scene": 1, "scenes": [{}, "x"]})).is_err());
        // An image that is not an object is not "no uri".
        let mut document = base();
        document["images"] = json!([5]);
        assert!(facts(document).is_err());
        // A bad materials field is refused although every primitive omits its material.
        let mut document = base();
        document["materials"] = json!("bad");
        document["meshes"] = json!([{"primitives": [{}]}]);
        assert!(facts(document).is_err());
        let mut document = base();
        document["textures"] = json!("bad");
        document["meshes"] = json!([{"primitives": [{}]}]);
        assert!(facts(document).is_err());
        // A texture the material names must exist, with a usable source, in a well formed field.
        let material = json!({"normalTexture": {"index": 0}});
        let mut document = base();
        document["textures"] = json!("bad");
        assert!(material_key(&material, &document, "meshes/x.glb").is_err());
        let mut document = base();
        document["textures"] = json!([]);
        assert!(material_key(&material, &document, "meshes/x.glb").is_err());
        for source in [json!("x"), json!(9)] {
            let mut document = base();
            document["textures"] = json!([{"source": source.clone()}]);
            assert!(
                material_key(&material, &document, "meshes/x.glb").is_err(),
                "source = {source}"
            );
        }
        assert!(
            material_key(
                &json!({"normalTexture": {"index": "x"}}),
                &base(),
                "meshes/x.glb"
            )
            .is_err()
        );
    }

    #[test]
    fn custom_extras_are_hashed_as_data_not_read_as_texture_references() {
        let mut doc = base();
        doc["textures"] = json!([{"source": 0}]);
        let plain = json!({"extras": {"custom": {"texture": "artist-note",
            "fooTexture": {"index": "n/a"}, "list": [{"texture": null}]}}});
        let key = material_key(&plain, &doc, "meshes/x.glb").unwrap();
        // The extras change the key (they are data), and an integer there is not resolved.
        let integer = json!({"extras": {"texture": 0}});
        let other = json!({"extras": {"texture": 1}});
        assert_ne!(
            material_key(&integer, &doc, "meshes/x.glb").unwrap(),
            material_key(&other, &doc, "meshes/x.glb").unwrap()
        );
        let mut changed = plain.clone();
        changed["extras"]["custom"]["texture"] = json!("other-note");
        assert_ne!(key, material_key(&changed, &doc, "meshes/x.glb").unwrap());
        // Extras inside a texture info are data too; the info itself is still strict.
        let info = json!({"normalTexture": {"index": 0, "extras": {"texture": "x"}}});
        assert!(material_key(&info, &doc, "meshes/x.glb").is_ok());
        let bad = json!({"normalTexture": {"index": "x", "extras": {}}});
        assert!(material_key(&bad, &doc, "meshes/x.glb").is_err());
    }

    #[test]
    fn a_valid_embedded_or_sourceless_texture_keeps_its_fallback_key() {
        let mut document = base();
        document["textures"] = json!([{"source": 1}, {}]);
        let embedded = json!({"normalTexture": {"index": 0}});
        let sourceless = json!({"normalTexture": {"index": 1}});
        let first = material_key(&embedded, &document, "meshes/x.glb").unwrap();
        let second = material_key(&sourceless, &document, "meshes/x.glb").unwrap();
        // texture#0 and texture#1 differ, and both are known.
        assert_ne!(first, second);
    }
}
