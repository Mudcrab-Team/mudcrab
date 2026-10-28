//! Serialized refusal reasons shared by the producer and certified consumers.

use std::fmt;

pub const STATE_CHANGE_WRITER_UNAVAILABLE: &str = "state_change_writer_unavailable";
pub const SCRIPTED: &str = "scripted";
pub const ENABLE_PARENT: &str = "enable_parent";
pub const INITIALLY_DISABLED: &str = "initially_disabled";

/// Why a reference or primitive is not eligible for the cluster path. The `as_str` code is what
/// `cluster_rejects.reason` stores and what the report counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RejectReason {
    BelowTriangleThreshold,
    NotOpaque,
    EffectShader,
    UnsupportedMaterialFeature,
    Skinned,
    NonRendering,
    NotIndexedTriangles,
    MissingAttributes,
    TooManyClusters,
    BuilderFailed,
    NonMonotonicError,
    NonFiniteGeometry,
}

impl RejectReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BelowTriangleThreshold => "below_triangle_threshold",
            Self::NotOpaque => "not_opaque",
            Self::EffectShader => "effect_shader",
            Self::UnsupportedMaterialFeature => "unsupported_material_feature",
            Self::Skinned => "skinned",
            Self::NonRendering => "non_rendering",
            Self::NotIndexedTriangles => "not_indexed_triangles",
            Self::MissingAttributes => "missing_attributes",
            Self::TooManyClusters => "too_many_clusters",
            Self::BuilderFailed => "builder_failed",
            Self::NonMonotonicError => "non_monotonic_error",
            Self::NonFiniteGeometry => "non_finite_geometry",
        }
    }
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reach class of authoritative coverage refusals. Ordering is explicit policy:
/// runtime dependency wins over unknown/other reasons, which win over look-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum StockRefusalClass {
    #[default]
    None,
    Look,
    Other,
    Runtime,
}

/// Interpret producer reason codes without granting reach to unknown reasons.
pub fn stock_refusal_class(status: &str, reason: Option<&str>) -> StockRefusalClass {
    let reason = reason.unwrap_or_default();
    if [
        STATE_CHANGE_WRITER_UNAVAILABLE,
        SCRIPTED,
        ENABLE_PARENT,
        INITIALLY_DISABLED,
    ]
    .contains(&reason)
    {
        StockRefusalClass::Runtime
    } else if [
        RejectReason::NotOpaque,
        RejectReason::UnsupportedMaterialFeature,
        RejectReason::MissingAttributes,
        RejectReason::EffectShader,
        RejectReason::NonRendering,
    ]
    .iter()
    .any(|kind| kind.as_str() == reason)
        || reason
            .strip_prefix("unresolved_reference_override:")
            .is_some_and(|tag| matches!(tag, "XEMI" | "XALP"))
    {
        StockRefusalClass::Look
    } else if status == "expanded" && reason.is_empty() {
        StockRefusalClass::None
    } else {
        StockRefusalClass::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_reasons_have_one_conservative_stock_reach_policy() {
        for reason in [
            SCRIPTED,
            ENABLE_PARENT,
            INITIALLY_DISABLED,
            STATE_CHANGE_WRITER_UNAVAILABLE,
        ] {
            assert_eq!(
                stock_refusal_class("not_eligible", Some(reason)),
                StockRefusalClass::Runtime
            );
        }
        for reason in [
            RejectReason::NotOpaque,
            RejectReason::UnsupportedMaterialFeature,
            RejectReason::EffectShader,
            RejectReason::NonRendering,
            RejectReason::MissingAttributes,
        ] {
            assert_eq!(
                stock_refusal_class("rejected", Some(reason.as_str())),
                StockRefusalClass::Look
            );
        }
        for tag in ["XEMI", "XALP"] {
            assert_eq!(
                stock_refusal_class(
                    "unknown",
                    Some(&format!("unresolved_reference_override:{tag}"))
                ),
                StockRefusalClass::Look
            );
        }
        for reason in [
            None,
            Some(""),
            Some("new_reason"),
            Some("unresolved_reference_override:XXXX"),
        ] {
            assert_eq!(
                stock_refusal_class("unknown", reason),
                StockRefusalClass::Other
            );
        }
        assert_eq!(
            stock_refusal_class("expanded", None),
            StockRefusalClass::None
        );
        assert_eq!(
            stock_refusal_class("expanded", Some("")),
            StockRefusalClass::None
        );
        assert!(StockRefusalClass::Runtime > StockRefusalClass::Other);
        assert!(StockRefusalClass::Other > StockRefusalClass::Look);
        assert!(StockRefusalClass::Look > StockRefusalClass::None);
    }
}
