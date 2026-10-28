//! Builders for the synthetic GLBs and datasets the cluster tests use.

use serde_json::Value;
#[cfg(test)]
use serde_json::json;

/// A GLB whose meshes have `primitives_per_mesh[i]` primitives (each only a POSITION accessor of one
/// triangle, so it loads as a structural reject but is still numbered), nodes
/// `(mesh, children)`, default-scene roots `roots`, and one image per entry of `image_uris`.
#[cfg(test)]
pub fn glb_with_nodes(
    primitives_per_mesh: &[usize],
    nodes: &[(Option<usize>, Vec<usize>)],
    roots: &[usize],
    image_uris: &[&str],
) -> Vec<u8> {
    let meshes: Vec<Value> = primitives_per_mesh
        .iter()
        .map(|&count| {
            json!({ "primitives": (0..count)
                .map(|_| json!({ "attributes": { "POSITION": 0 } }))
                .collect::<Vec<_>>() })
        })
        .collect();
    let nodes: Vec<Value> = nodes
        .iter()
        .map(|(mesh, children)| {
            let mut node = json!({});
            if let Some(mesh) = mesh {
                node["mesh"] = json!(mesh);
            }
            if !children.is_empty() {
                node["children"] = json!(children);
            }
            node
        })
        .collect();
    let mut document = json!({
        "asset": { "version": "2.0" },
        "buffers": [{ "byteLength": 36 }],
        "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]
        }],
        "meshes": meshes,
        "nodes": nodes,
        "scene": 0,
        "scenes": [{ "nodes": roots }],
    });
    if !image_uris.is_empty() {
        document["images"] = json!(
            image_uris
                .iter()
                .map(|uri| json!({ "uri": uri }))
                .collect::<Vec<_>>()
        );
    }
    let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    glb(&document, &positions)
}

/// Wraps a JSON document and a binary chunk in the GLB container.
pub fn glb(document: &Value, binary: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = binary.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}
