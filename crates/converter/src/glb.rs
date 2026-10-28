//! Converter-owned GLB JSON/BIN codec for authored mesh rewrites.
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use serde_json::Value;

pub(crate) fn json_end(glb: &[u8]) -> Result<usize> {
    ensure!(
        glb.len() >= 20 && &glb[..4] == b"glTF",
        "invalid GLB container"
    );
    ensure!(
        u32::from_le_bytes(glb[4..8].try_into().unwrap()) == 2,
        "unsupported GLB version"
    );
    ensure!(
        u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize == glb.len(),
        "GLB length is inconsistent"
    );
    ensure!(&glb[16..20] == b"JSON", "GLB JSON chunk is missing");
    let length = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let end = 20usize
        .checked_add(length)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB JSON range overflow"))?;
    ensure!(end <= glb.len(), "truncated GLB JSON chunk");
    Ok(end)
}

pub(crate) fn split_glb(glb: &[u8]) -> Result<(Value, Vec<u8>)> {
    let end = json_end(glb)?;
    let document = serde_json::from_slice(&glb[20..end]).wrap_err("invalid glTF JSON")?;
    let tail = &glb[end..];
    let bin = if tail.len() >= 8 && &tail[4..8] == b"BIN\0" {
        let length = u32::from_le_bytes(tail[..4].try_into().unwrap()) as usize;
        let end = 8usize
            .checked_add(length)
            .ok_or_else(|| color_eyre::eyre::eyre!("GLB binary range overflow"))?;
        tail.get(8..end)
            .ok_or_else(|| color_eyre::eyre::eyre!("truncated GLB binary chunk"))?
            .to_vec()
    } else {
        Vec::new()
    };
    Ok((document, bin))
}

/// Rewrites JSON while retaining every binary/unknown chunk byte without cloning geometry first.
pub(crate) fn replace_document(original: &[u8], document: &Value) -> Result<Vec<u8>> {
    let end = json_end(original)?;
    encode(document, &original[end..], false)
}

/// Rebuilds animation output with its extended binary buffer.
pub(crate) fn rebuild_glb(document: &Value, bin: &[u8]) -> Result<Vec<u8>> {
    encode(document, bin, !bin.is_empty())
}

fn encode(document: &Value, tail: &[u8], binary: bool) -> Result<Vec<u8>> {
    let mut json = serde_json::to_vec(document).wrap_err("failed to serialize glTF JSON")?;
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let padding = if binary { (4 - tail.len() % 4) % 4 } else { 0 };
    let tail_length = tail
        .len()
        .checked_add(padding)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB size overflow"))?;
    let total = 20usize
        .checked_add(json.len())
        .and_then(|length| length.checked_add(tail_length))
        .and_then(|length| length.checked_add(if binary { 8 } else { 0 }))
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB size overflow"))?;
    let total = u32::try_from(total).wrap_err("GLB exceeds 4 GiB")?;
    let json_length = u32::try_from(json.len()).wrap_err("GLB JSON exceeds 4 GiB")?;
    let mut output = Vec::with_capacity(total as usize);
    output.extend_from_slice(b"glTF");
    output.extend_from_slice(&2u32.to_le_bytes());
    output.extend_from_slice(&total.to_le_bytes());
    output.extend_from_slice(&json_length.to_le_bytes());
    output.extend_from_slice(b"JSON");
    output.extend_from_slice(&json);
    if binary {
        output.extend_from_slice(
            &u32::try_from(tail_length)
                .wrap_err("GLB binary chunk exceeds 4 GiB")?
                .to_le_bytes(),
        );
        output.extend_from_slice(b"BIN\0");
    }
    output.extend_from_slice(tail);
    output.resize(output.len() + padding, 0);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_rewrite_retains_binary_and_unknown_chunks() {
        let input =
            rebuild_glb(&serde_json::json!({"asset":{"version":"2.0"}}), &[1, 2, 3]).unwrap();
        let mut with_unknown = input;
        with_unknown.extend_from_slice(&4u32.to_le_bytes());
        with_unknown.extend_from_slice(b"TEST");
        with_unknown.extend_from_slice(&[9, 8, 7, 6]);
        let length = with_unknown.len() as u32;
        with_unknown[8..12].copy_from_slice(&length.to_le_bytes());
        let end = json_end(&with_unknown).unwrap();
        let output = replace_document(
            &with_unknown,
            &serde_json::json!({"asset":{"version":"2.0"},"extras":{"notice":"longer"}}),
        )
        .unwrap();
        assert_eq!(&output[json_end(&output).unwrap()..], &with_unknown[end..]);
    }
}
