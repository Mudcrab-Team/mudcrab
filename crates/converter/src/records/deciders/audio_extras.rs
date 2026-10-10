//! Independently authored bounded layouts for SSE audio and miscellaneous data.
use super::FieldSchema;

/// Hooks requiring a payload-specific descriptor rather than fixed member offsets.
pub const NAMES: &[&str] = &["audio_debris_data"];

/// Describe DEBR's percentage, terminated filename and trailing collision flags.
///
/// This descriptor leaves primitive decoding and malformed-field recovery with
/// the generic reader. xEdit's Common definition identifies the variable string
/// between two single-byte members; no reference implementation is translated.
pub fn layout(name: &str, bytes: &[u8]) -> Result<Option<FieldSchema>, String> {
    if name != "audio_debris_data" {
        return Ok(None);
    }
    let tail = bytes.get(1..).ok_or("debris percentage is missing")?;
    let terminator = tail
        .iter()
        .position(|&byte| byte == 0)
        .ok_or("debris filename lacks its terminator")?
        + 1;
    let flags_offset = terminator + 1;
    if bytes.len() != flags_offset + 1 {
        return Err("debris flags must immediately follow the filename terminator".into());
    }
    Ok(Some(FieldSchema {
        name: "debris_model".into(),
        kind: "struct".into(),
        size: Some(bytes.len()),
        members: vec![
            FieldSchema {
                name: "percentage".into(),
                kind: "u8".into(),
                ..FieldSchema::default()
            },
            FieldSchema {
                name: "model_filename".into(),
                kind: "zstring".into(),
                offset: 1,
                size: Some(terminator),
                ..FieldSchema::default()
            },
            FieldSchema {
                name: "flags".into(),
                kind: "u8".into(),
                offset: flags_offset,
                flags: serde_json::json!({"1": "has_collision_data"}),
                ..FieldSchema::default()
            },
        ],
        ..FieldSchema::default()
    }))
}
