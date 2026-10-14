//! Original owner/full-local identity is independent of runtime slot width.
use crate::esm::load_order::LoadOrder;
use serde::Serialize;

/// Provenance state under the supplied original MAST context, independent of runtime width.
/// GMST convention and unproven owner states are never promoted to original owner proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceIdentityState {
    Resolved,
    NullRawWord,
    MasterIndexOutOfRange,
    OwnerUnavailable,
    /// Retain the documented GMST header convention without claiming native owner proof.
    GmstEditorIdConvention,
}

impl SourceIdentityState {
    /// Stable tooling state independent of runtime slot availability or field interpretation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::NullRawWord => "null_raw_word",
            Self::MasterIndexOutOfRange => "master_index_out_of_range",
            Self::OwnerUnavailable => "owner_unavailable",
            Self::GmstEditorIdConvention => "gmst_editor_id_convention",
        }
    }
}

/// Original owner and unmasked 24-bit local from source-authenticated master metadata.
/// Signature does not create a new identity namespace; type conflicts remain conflicts.
/// When loaded from SQL, callers must authenticate the producing bundle before trusting context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceIdentity {
    pub original_owner: Option<String>,
    pub full_local: Option<u32>,
    pub state: SourceIdentityState,
}

/// Resolve the original owner only from the supplied original MAST context.
pub(crate) fn resolve(
    signature: &[u8; 4],
    raw: u32,
    supplier: usize,
    order: &LoadOrder,
) -> SourceIdentity {
    if raw == 0 {
        return SourceIdentity {
            original_owner: None,
            full_local: None,
            state: SourceIdentityState::NullRawWord,
        };
    }
    let local = raw & 0x00ff_ffff; // Full native local word, never a light-width mask.
    let masters = &order.metadata[supplier].masters;
    let index = (raw >> 24) as usize;
    if index > masters.len() {
        return SourceIdentity {
            original_owner: None,
            full_local: Some(local),
            state: if signature == b"GMST" {
                SourceIdentityState::GmstEditorIdConvention
            } else {
                SourceIdentityState::MasterIndexOutOfRange
            },
        };
    }
    let owner = if index == masters.len() {
        crate::esm::load_order::normalize_plugin_name(&order.names[supplier])
    } else {
        crate::esm::load_order::normalize_plugin_name(&masters[index])
    };
    let available = order.normal.contains_key(&owner) || order.light.contains_key(&owner);
    SourceIdentity {
        original_owner: Some(owner),
        full_local: Some(local),
        state: if available {
            SourceIdentityState::Resolved
        } else {
            SourceIdentityState::OwnerUnavailable
        },
    }
}

/// Preserve the existing runtime convention without manufacturing wide-ID handles.
pub(crate) fn header_runtime_id(
    signature: &[u8; 4],
    raw: u32,
    supplier: usize,
    order: &LoadOrder,
) -> Option<u32> {
    if signature == b"TES4" {
        return None;
    }
    if raw == 0 {
        return Some(0);
    }
    let source = resolve(signature, raw, supplier, order);
    let owner = match source.state {
        SourceIdentityState::Resolved => source.original_owner.as_deref()?,
        SourceIdentityState::GmstEditorIdConvention => &order.names[supplier],
        _ => return None,
    };
    let local = source.full_local?;
    if let Some(slot) = order.light.get(owner) {
        // No OR spill into an adjacent owner and no truncated alias.
        (local <= 0xfff).then_some(0xfe00_0000 | slot << 12 | local)
    } else {
        order.normal.get(owner).map(|slot| slot << 24 | local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::binary::PluginMetadata;

    #[test]
    /// A wide local cannot carry into the next light slot or alias that slot's narrow local.
    fn wide_light_and_adjacent_owner_never_alias() {
        let order = LoadOrder::from_metadata(
            vec!["Base.esm".into(), "A.esl".into(), "B.esl".into()],
            vec![
                PluginMetadata::default(),
                PluginMetadata {
                    flags: 0x200,
                    masters: vec!["Base.esm".into()],
                },
                PluginMetadata {
                    flags: 0x200,
                    masters: vec!["Base.esm".into()],
                },
            ],
        )
        .unwrap();
        let wide = resolve(b"STAT", 0x0100_1234, 1, &order);
        let narrow = resolve(b"STAT", 0x0100_0234, 2, &order);
        assert_eq!(wide.original_owner.as_deref(), Some("a.esl"));
        assert_eq!(wide.full_local, Some(0x1234));
        assert_eq!(wide.state, SourceIdentityState::Resolved);
        assert_ne!(wide.original_owner, narrow.original_owner);
        assert_eq!(header_runtime_id(b"STAT", 0x0100_1234, 1, &order), None);
        assert_eq!(
            header_runtime_id(b"STAT", 0x0100_0234, 2, &order),
            Some(0xfe00_1234)
        );
    }

    #[test]
    /// The signature-specific GMST fallback is explicit; generic invalid owners remain unknown.
    fn invalid_master_index_is_not_self_but_gmst_convention_is_explicit() {
        let order = LoadOrder::from_metadata(
            vec!["Base.esm".into(), "Patch.esp".into()],
            vec![
                PluginMetadata::default(),
                PluginMetadata {
                    flags: 0,
                    masters: vec!["Base.esm".into()],
                },
            ],
        )
        .unwrap();
        assert_eq!(
            resolve(b"STAT", 0x0100_0700, 1, &order)
                .original_owner
                .as_deref(),
            Some("patch.esp")
        );
        let bad = resolve(b"STAT", 0x0200_0700, 1, &order);
        assert_eq!(bad.state, SourceIdentityState::MasterIndexOutOfRange);
        assert_eq!(bad.original_owner, None);
        assert_eq!(header_runtime_id(b"STAT", 0x0200_0700, 1, &order), None);
        assert_eq!(
            resolve(b"GMST", 0x0200_0700, 1, &order).state,
            SourceIdentityState::GmstEditorIdConvention
        );
        assert_eq!(
            header_runtime_id(b"GMST", 0x0200_0700, 1, &order),
            Some(0x0100_0700)
        );
    }
}
