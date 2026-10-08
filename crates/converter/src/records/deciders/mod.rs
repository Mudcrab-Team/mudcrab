//! Small independently authored family policies, shared by build validation and decoding.
use super::schema_format::FieldSchema;
pub mod actors;
pub mod audio_extras;
pub mod items;
pub mod magic;
pub mod visual_extras;
pub mod world_extras;

/// Binary context available to a family selector without reference-tool dependencies.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub record_type: [u8; 4],
    /// Native header version, used only by documented version-specific layouts.
    pub form_version: u16,
    pub editor_id: &'a str,
    pub preceding_subrecords: &'a [([u8; 4], &'a [u8])],
    pub parent_payload: Option<&'a [u8]>,
    /// Resolves a file-relative link to its accepted winning record kind.
    pub link_record_type: Option<&'a dyn Fn(u32) -> Option<[u8; 4]>>,
}

/// A selected alternative, or a dependency on the not-yet-built winner catalogue.
pub enum Selection {
    Alternative(usize),
    Deferred,
}

/// Refuse names absent from each family's explicit registry at build time.
pub fn known(name: &str) -> bool {
    name == "inventory_owner_condition"
        || items::NAMES.contains(&name)
        || actors::NAMES.contains(&name)
        || magic::NAMES.contains(&name)
        || world_extras::NAMES.contains(&name)
        || visual_extras::NAMES.contains(&name)
        || audio_extras::NAMES.contains(&name)
}

/// Describe family-owned variable payloads using the existing typed schema contract.
/// Counts and boundaries are checked before the generic decoder reads any members.
pub fn layout(field: &FieldSchema, bytes: &[u8]) -> Result<Option<FieldSchema>, String> {
    let Some(name) = field.decider.as_deref() else {
        return Ok(None);
    };
    if world_extras::NAMES.contains(&name) {
        world_extras::layout(name, bytes)
    } else if audio_extras::NAMES.contains(&name) {
        audio_extras::layout(name, bytes)
    } else {
        Ok(None)
    }
}

/// Dispatch only to the family that explicitly owns a named selector.
pub fn select(
    name: &str,
    field: &FieldSchema,
    bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    if name == "inventory_owner_condition" {
        let parent = context
            .parent_payload
            .ok_or("ownership selector needs its parent struct")?;
        let owner = parent.get(..4).ok_or("ownership owner is truncated")?;
        let owner = u32::from_le_bytes(owner.try_into().expect("checked owner"));
        let Some(kind) = context.link_record_type else {
            return Ok(Selection::Deferred);
        };
        let selected_kind = match kind(owner) {
            Some(signature) if signature == *b"NPC_" => "form_id",
            Some(signature) if signature == *b"FACT" => "i32",
            _ => "bytes",
        };
        let index = field
            .fields
            .iter()
            .position(|alternative| alternative.kind == selected_kind)
            .ok_or_else(|| format!("ownership lacks {selected_kind} alternative"))?;
        if bytes.len() != 4 {
            return Err("ownership condition is not four bytes".into());
        }
        Ok(Selection::Alternative(index))
    } else if items::NAMES.contains(&name) {
        items::select(name, field, bytes, context)
    } else if actors::NAMES.contains(&name) {
        actors::select(name, field, bytes, context)
    } else if magic::NAMES.contains(&name) {
        magic::select(name, field, bytes, context)
    } else if visual_extras::NAMES.contains(&name) {
        visual_extras::select(name, field, bytes, context)
    } else {
        Err(format!("unknown family decider {name}"))
    }
}
