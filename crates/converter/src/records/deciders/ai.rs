//! Independently authored package selectors from pinned native format facts.
use super::{Context, FieldSchema, Selection};

/// Names implemented by this family, shared with schema validation.
pub const NAMES: &[&str] = &["ai_package_value", "ai_location", "ai_target", "ai_topic"];

/// Read the discriminator from an enclosing fixed native structure.
fn discriminator(context: &Context<'_>, parent_size: usize) -> Result<i32, String> {
    let parent = context
        .parent_payload
        .ok_or("package union lacks its enclosing payload")?;
    if parent.len() != parent_size {
        return Err(format!(
            "package union requires a {parent_size}-byte enclosing payload"
        ));
    }
    Ok(i32::from_le_bytes(
        parent[..4].try_into().expect("checked parent width"),
    ))
}

/// Pick one explicit union alternative; malformed selectors remain local field errors.
pub fn select(
    name: &str,
    field: &FieldSchema,
    bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    if context.record_type != *b"PACK" {
        return Err("AI package selector requires a PACK owner".into());
    }
    let index = match name {
        "ai_package_value" => {
            // ANAM belongs to the current repeated input. Do not borrow a type across
            // a data-input index, procedure marker, or package event boundary.
            let mut type_name = None;
            for &(signature, payload) in context.preceding_subrecords.iter().rev() {
                if matches!(
                    &signature,
                    b"UNAM" | b"XNAM" | b"POBA" | b"POEA" | b"POCA" | b"PKCU"
                ) {
                    break;
                }
                if signature == *b"ANAM" {
                    // Match the generic zstring contract; selector normalization must
                    // not rewrite the canonical ANAM payload or its trailing padding.
                    let end = payload
                        .iter()
                        .position(|&byte| byte == 0)
                        .unwrap_or(payload.len());
                    type_name = Some(&payload[..end]);
                    break;
                }
            }
            match type_name.ok_or("package value lacks its current input type")? {
                b"Bool" => 1,
                b"Int" => 2,
                b"Float" | b"ObjectList" => 3,
                // The permitted source explicitly defines this union branch as unknown.
                // Preserve it only here; other package fields remain fully typed.
                _ => 0,
            }
        }
        "ai_location" => {
            if bytes.len() != 4 {
                return Err("package location value is not four bytes".into());
            }
            let kind = discriminator(context, 12)?;
            if !(0..=12).contains(&kind) {
                return Err(format!("unknown package location kind {kind}"));
            }
            kind as usize
        }
        "ai_target" => {
            if bytes.len() != 4 {
                return Err("package target value is not four bytes".into());
            }
            let kind = discriminator(context, 12)?;
            if !(0..=6).contains(&kind) {
                return Err(format!("unknown package target kind {kind}"));
            }
            kind as usize
        }
        "ai_topic" => {
            if bytes.len() != 4 {
                return Err("package topic value is not four bytes".into());
            }
            let kind = discriminator(context, 8)?;
            if !(0..=1).contains(&kind) {
                return Err(format!("unknown package topic kind {kind}"));
            }
            kind as usize
        }
        _ => return Err(format!("unknown AI selector {name}")),
    };
    if index >= field.fields.len() {
        return Err(format!("AI schema lacks {name} alternative {index}"));
    }
    Ok(Selection::Alternative(index))
}
