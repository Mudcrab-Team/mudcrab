//! Earlier base-schema digests whose packs a metadata rebuild may still retain assets from.
//!
//! A pack records the digest of the schema its database was decoded with. A metadata rebuild
//! decodes the plugins again with the compiled schema, so that digest is provenance for the
//! retained asset bytes, not an input to them. When a schema edit touches only fields that no
//! mesh, texture or script stage reads, packs built before the edit keep their assets.
//!
//! Every entry is pinned twice, and the two pins live in two files:
//! - `schema-lineage.json` records the compiled digest the entry was reviewed against.
//!   An entry applies only while that digest is the compiled one.
//! - [`PINNED_ENTRIES`] below records a hash of the whole entry (digest, producer, stated
//!   difference and reviewed digest). An entry whose text differs from its pin is ignored.
//!
//! A schema edit therefore invalidates every entry. Keeping an old pack usable takes a
//! reviewed edit to both files, and `.github/CODEOWNERS` requires approval for both. When the
//! edit changes any asset byte, delete the entry instead.

use serde::Deserialize;
use std::sync::OnceLock;

/// One earlier digest accepted for retained assets.
#[derive(Debug, Deserialize)]
pub(crate) struct LineageEntry {
    pub(crate) sha256: String,
    pub(crate) produced_by: String,
    pub(crate) difference: String,
    /// The compiled schema digest a maintainer reviewed this entry against.
    pub(crate) reviewed_against: String,
}

impl LineageEntry {
    /// The hash a pin must equal: SHA-256 of the four fields, one per line.
    fn pin(&self) -> String {
        crate::cache::hash_bytes(
            [
                self.sha256.as_str(),
                &self.produced_by,
                &self.difference,
                &self.reviewed_against,
            ]
            .join("\n")
            .as_bytes(),
        )
    }
}

#[derive(Debug, Deserialize)]
struct Lineage {
    entries: Vec<LineageEntry>,
}

/// `(lineage digest, entry pin)` for every reviewed entry. Change it only with the JSON entry.
const PINNED_ENTRIES: &[(&str, &str)] = &[(
    "ee7e85d996711c656abcc427fb76496452c7d0e89c1b383abf64a8acd1392d30",
    "e67cedb9c9ae2a97df368be57a29f5abf81ed27f78d787a6a833ef1d3a2bc840",
)];

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

/// The entry for `digest` among `entries`, when it was reviewed against `current` and still
/// matches its pin in `pins`. Any other entry matches nothing.
fn entry_in<'a>(
    entries: &'a [LineageEntry],
    pins: &[(&str, &str)],
    current: &str,
    digest: &str,
) -> Option<&'a LineageEntry> {
    entries.iter().find(|entry| {
        entry.sha256 == digest
            && entry.reviewed_against == current
            && pins
                .iter()
                .any(|(pinned, pin)| *pinned == digest && *pin == entry.pin())
    })
}

/// The lineage entry for `digest` under the compiled schema.
pub(crate) fn entry_for(digest: &str) -> Option<&'static LineageEntry> {
    entry_in(
        &lineage().entries,
        PINNED_ENTRIES,
        &current_digest(),
        digest,
    )
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

    /// A schema edit must be reviewed against the lineage in the same change. Update the
    /// entry's `reviewed_against` and its pin only when the edit leaves every asset byte
    /// unchanged. Otherwise delete the entry, and older packs refuse a metadata rebuild.
    #[test]
    fn every_entry_is_pinned_and_reviewed_against_the_compiled_schema() {
        let mut seen = std::collections::BTreeSet::new();
        assert_eq!(lineage().entries.len(), PINNED_ENTRIES.len());
        for entry in &lineage().entries {
            assert_eq!(entry.sha256.len(), 64);
            assert_ne!(
                entry.sha256,
                current_digest(),
                "the current digest is not lineage"
            );
            assert!(!entry.produced_by.is_empty() && !entry.difference.is_empty());
            assert!(seen.insert(&entry.sha256), "duplicate lineage digest");
            assert_eq!(
                entry.reviewed_against,
                current_digest(),
                "the base schema changed; review schema-lineage.json (see this test)"
            );
            assert!(
                entry_for(&entry.sha256).is_some(),
                "entry {} does not match its pin in PINNED_ENTRIES",
                entry.sha256
            );
        }
    }

    fn entry(reviewed_against: &str) -> LineageEntry {
        LineageEntry {
            sha256: "a".repeat(64),
            produced_by: "abc".into(),
            difference: "database-only field".into(),
            reviewed_against: reviewed_against.into(),
        }
    }

    /// A schema bump that keeps the old entry, an edited entry text, an unpinned entry and a
    /// different digest all match nothing. Only an untouched, pinned entry reviewed against
    /// the compiled digest applies.
    #[test]
    fn a_stale_edited_or_unpinned_entry_matches_nothing() {
        let current = "c".repeat(64);
        let reviewed = entry(&current);
        let pins = [("a".repeat(64), reviewed.pin())];
        let pins: Vec<(&str, &str)> = pins.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let digest = "a".repeat(64);
        assert!(entry_in(&[entry(&current)], &pins, &current, &digest).is_some());
        // The compiled schema moved on and the maintainer left the old review in place.
        let moved = "d".repeat(64);
        assert!(entry_in(&[entry(&current)], &pins, &moved, &digest).is_none());
        // The maintainer moved the review forward without updating the pin.
        assert!(entry_in(&[entry(&moved)], &pins, &moved, &digest).is_none());
        // The stated difference was edited.
        let mut edited = entry(&current);
        edited.difference = "mesh vertex format".into();
        assert!(entry_in(&[edited], &pins, &current, &digest).is_none());
        // No pin at all, and a different digest.
        assert!(entry_in(&[entry(&current)], &[], &current, &digest).is_none());
        assert!(entry_in(&[entry(&current)], &pins, &current, &"b".repeat(64)).is_none());
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
