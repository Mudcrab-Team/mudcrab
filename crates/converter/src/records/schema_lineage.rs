//! Earlier base-schema digests whose packs a metadata rebuild may still retain assets from.
//!
//! A pack records the digest of the schema its database was decoded with. A metadata rebuild
//! decodes the plugins again with the compiled schema, so that digest is provenance for the
//! retained asset bytes, not an input to them. When a schema edit touches only fields that no
//! mesh, texture or script stage reads, packs built before the edit keep their assets. The
//! maintainer who makes such an edit adds the previous digest to `schema-lineage.json` and
//! states the difference. The file records the compiled digest it was reviewed against, and a
//! test fails when the schema changes without that review.

use serde::Deserialize;
use std::sync::OnceLock;

/// One earlier digest accepted for retained assets.
#[derive(Debug, Deserialize)]
pub(crate) struct LineageEntry {
    pub(crate) sha256: String,
    pub(crate) produced_by: String,
    pub(crate) difference: String,
}

#[derive(Debug, Deserialize)]
struct Lineage {
    /// The compiled schema digest this list was reviewed against.
    asset_equivalent_to: String,
    entries: Vec<LineageEntry>,
}

fn lineage() -> &'static Lineage {
    static LINEAGE: OnceLock<Lineage> = OnceLock::new();
    LINEAGE.get_or_init(|| {
        serde_json::from_str(include_str!("schema-lineage.json"))
            .expect("schema-lineage.json is valid")
    })
}

fn current_digest() -> String {
    crate::cache::hash_bytes(super::SCHEMA_BYTES)
}

/// The lineage entry for `digest`, when the list was reviewed against the compiled schema.
/// A stale list matches nothing, so an unreviewed schema edit refuses older packs.
pub(crate) fn entry_for(digest: &str) -> Option<&'static LineageEntry> {
    let lineage = lineage();
    (lineage.asset_equivalent_to == current_digest())
        .then(|| lineage.entries.iter().find(|entry| entry.sha256 == digest))
        .flatten()
}

/// The earlier base-schema digest an in-house reader identity records, when the lineage
/// accepts it. `None` for the current schema, for add-on schemas and for unknown digests.
pub(crate) fn lineage_digest(identity: &serde_json::Value) -> Option<String> {
    if identity.get("mode")?.as_str()? != "inhouse" || identity.get("schema_addons").is_some() {
        return None;
    }
    let digest = identity.get("schema_sha256")?.as_str()?;
    (digest != current_digest()).then_some(())?;
    entry_for(digest).map(|entry| entry.sha256.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A schema edit must be reviewed against the lineage in the same change. Update
    /// `asset_equivalent_to` and add the previous digest only when the edit leaves every asset
    /// byte unchanged. Otherwise clear `entries`, and older packs refuse a metadata rebuild.
    #[test]
    fn lineage_was_reviewed_against_the_compiled_schema() {
        assert_eq!(
            lineage().asset_equivalent_to,
            current_digest(),
            "the base schema changed; review schema-lineage.json (see this test)"
        );
        let mut seen = std::collections::BTreeSet::new();
        for entry in &lineage().entries {
            assert_eq!(entry.sha256.len(), 64);
            assert_ne!(
                entry.sha256,
                current_digest(),
                "the current digest is not lineage"
            );
            assert!(!entry.produced_by.is_empty() && !entry.difference.is_empty());
            assert!(seen.insert(&entry.sha256), "duplicate lineage digest");
        }
    }

    #[test]
    fn only_listed_inhouse_digests_are_accepted() {
        let listed = &lineage().entries[0].sha256;
        let identity = |digest: &str| serde_json::json!({"mode": "inhouse", "adapter_version": 12, "schema_sha256": digest});
        assert_eq!(lineage_digest(&identity(listed)), Some(listed.clone()));
        assert_eq!(lineage_digest(&identity(&"0".repeat(64))), None);
        assert_eq!(lineage_digest(&identity(&current_digest())), None);
        let mut addons = identity(listed);
        addons["schema_addons"] = serde_json::json!([]);
        assert_eq!(lineage_digest(&addons), None);
        let mut legacy = identity(listed);
        legacy["mode"] = "legacy".into();
        assert_eq!(lineage_digest(&legacy), None);
    }
}
