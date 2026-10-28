//! Minimum guards against subrecords the cluster path does not resolve.
//!
//! A reference can carry subrecords that change how its object is drawn (a material swap, an alpha
//! or emittance override), and its base record can carry alternate textures. The cluster path
//! draws the converted model as stock does. Reference overrides whose stock result is not
//! represented remain refused. MODS is ignored by both current material paths, so its presence
//! does not refuse an otherwise valid archived base record. The guards here read the
//! archived subrecord list the way the exporter wrote it (`serialize_subrecords`), so only a real
//! subrecord tag counts, never bytes that happen to spell one inside another subrecord's payload.
//!
//! The tag lists are a MINIMUM, not a certified exhaustive list of what changes behaviour: other
//! overrides (`XTNM`, the layout caveats of the object-layer notes) are not guessed at here.
//! Data that cannot be parsed is never "clear": it is reported as unparsed.

use crate::cluster_mapping::{
    Eligibility, REASON_UNPARSED_BASE_RECORD, REASON_UNPARSED_REFERENCE_DATA,
    REASON_UNRESOLVED_REFERENCE_OVERRIDE,
};
use crate::records::ArchivedRecordData;

pub use crate::cluster_mapping::MINIMUM_REFERENCE_OVERRIDE_TAGS;

/// What a guard found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Parsed, and none of the guarded subrecords is present.
    Clear,
    /// Parsed, and this guarded subrecord is present.
    Unresolved(&'static str),
    /// The bytes are not a subrecord list: nothing is known about them.
    Unparsed,
}

impl Guard {
    /// The `cluster_reference_coverage.reason` for a reference this guard refuses, `None` when
    /// the reference is clear.
    pub fn reference_reason(self) -> Option<String> {
        match self {
            Self::Clear => None,
            Self::Unresolved(tag) => Some(format!("{REASON_UNRESOLVED_REFERENCE_OVERRIDE}{tag}")),
            Self::Unparsed => Some(REASON_UNPARSED_REFERENCE_DATA.to_owned()),
        }
    }

    /// The reason for a reference whose base record this guard refuses.
    pub fn base_record_reason(self) -> Option<String> {
        match self {
            Self::Clear | Self::Unresolved(_) => None,
            Self::Unparsed => Some(REASON_UNPARSED_BASE_RECORD.to_owned()),
        }
    }
}

fn parse(blob: &[u8]) -> Option<ArchivedRecordData> {
    rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(blob).ok()
}

fn guard(blob: &[u8], tags: &[&'static str]) -> Guard {
    let Some(record) = parse(blob) else {
        return Guard::Unparsed;
    };
    tags.iter()
        .copied()
        .find(|tag| {
            record
                .subrecords
                .iter()
                .any(|sub| sub.tag == *tag.as_bytes())
        })
        .map_or(Guard::Clear, Guard::Unresolved)
}

/// Guards a reference's `references.data` blob against the minimum override tags.
pub fn check_reference(blob: &[u8]) -> Guard {
    guard(blob, &MINIMUM_REFERENCE_OVERRIDE_TAGS)
}

/// Validates the archived base record; alternate textures use the same unresolved stock material.
pub fn check_base_record(blob: &[u8]) -> Guard {
    // Stock loads the converted GLB material unchanged; MODS does not alter either path.
    guard(blob, &[])
}

/// The guards of a reference's two blobs; an absent blob is `Unparsed`.
pub fn guards(reference_blob: Option<&[u8]>, base_blob: Option<&[u8]>) -> (Guard, Guard) {
    (
        reference_blob.map_or(Guard::Unparsed, check_reference),
        base_blob.map_or(Guard::Unparsed, check_base_record),
    )
}

/// The authority's verdict for one reference: the classifier's own refusal comes first, then the
/// guards of the reference and of its base record, then the model. A guarded subrecord makes the
/// reference `NotEligible`; data that could not be parsed, or a model that is missing from the
/// conversion or cannot be read (`model_problem`, a reason), makes it `Unknown`. Only a reference
/// with every input resolved is `Eligible`. This is the function an `AuthoritativeInputs`
/// implementation and the stage both call, so the guards are not test-only.
pub fn eligibility(
    classifier_refusal: Option<&str>,
    reference: Guard,
    base: Guard,
    model_problem: Option<&str>,
) -> Eligibility {
    if let Some(reason) = classifier_refusal {
        return Eligibility::NotEligible(reason.to_owned());
    }
    for (guard, reason) in [
        (reference, reference.reference_reason()),
        (base, base.base_record_reason()),
    ] {
        match (guard, reason) {
            (Guard::Unresolved(_), Some(reason)) => return Eligibility::NotEligible(reason),
            (Guard::Unparsed, Some(reason)) => return Eligibility::Unknown(reason),
            _ => {}
        }
    }
    match model_problem {
        None => Eligibility::Eligible,
        Some(reason) => Eligibility::Unknown(reason.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exporter's `serialize_subrecords`, which the converter pins against the same bytes.
    fn serialize_subrecords(subs: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let data = crate::records::ArchivedRecordData {
            subrecords: subs
                .iter()
                .map(|(tag, data)| crate::records::ArchivedSubRecord {
                    tag: tag[..4].try_into().unwrap(),
                    data: data.clone(),
                })
                .collect(),
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&data)
            .unwrap()
            .to_vec()
    }

    fn blob(subrecords: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let subs: Vec<(Vec<u8>, Vec<u8>)> = subrecords
            .iter()
            .map(|(tag, data)| (tag.to_vec(), data.to_vec()))
            .collect();
        serialize_subrecords(&subs)
    }

    #[test]
    fn a_real_guarded_tag_is_found() {
        let data = blob(&[(b"NAME", &[1, 0, 0, 0]), (b"XALP", &[0, 1])]);
        assert_eq!(check_reference(&data), Guard::Unresolved("XALP"));
        assert_eq!(
            Guard::Unresolved("XALP").reference_reason().as_deref(),
            Some("unresolved_reference_override:XALP")
        );
        assert_eq!(
            check_reference(&blob(&[(b"XMSP", &[0; 4])])),
            Guard::Unresolved("XMSP")
        );
        assert_eq!(
            check_reference(&blob(&[(b"XEMI", &[0; 4])])),
            Guard::Unresolved("XEMI")
        );
    }

    #[test]
    fn the_bytes_of_a_tag_inside_another_payload_are_not_a_tag() {
        let data = blob(&[
            (b"DATA", b"..XALP..XEMI..XMSP.."),
            (b"XSCL", &[0, 0, 128, 63]),
        ]);
        assert_eq!(check_reference(&data), Guard::Clear);
        assert_eq!(Guard::Clear.reference_reason(), None);
    }

    #[test]
    fn data_that_does_not_parse_is_never_clear() {
        assert_eq!(check_reference(b"not a subrecord list"), Guard::Unparsed);
        assert_eq!(check_reference(&[]), Guard::Unparsed);
        assert_eq!(check_base_record(b"XXXX"), Guard::Unparsed);
        assert_eq!(
            Guard::Unparsed.reference_reason().as_deref(),
            Some("unparsed_reference_data")
        );
        assert_eq!(
            Guard::Unparsed.base_record_reason().as_deref(),
            Some("unparsed_base_record")
        );
    }

    #[test]
    fn base_alternate_textures_are_found_as_a_tag_only() {
        let with = blob(&[(b"MODL", b"a.nif"), (b"MODS", &[1, 0, 0, 0])]);
        assert_eq!(check_base_record(&with), Guard::Clear);
        assert_eq!(
            check_base_record(&with).base_record_reason().as_deref(),
            None
        );
        let without = blob(&[(b"MODL", b"MODS.nif"), (b"MODT", &[0; 8])]);
        assert_eq!(check_base_record(&without), Guard::Clear);
    }

    #[test]
    fn the_verdict_follows_the_guards_in_order() {
        let clear = blob(&[(b"EDID", b"x")]);
        let alpha = blob(&[(b"XALP", &[1, 0])]);
        let alternate = blob(&[(b"MODS", &[1, 0, 0, 0])]);
        let verdict = |classifier, reference: Option<&[u8]>, base: Option<&[u8]>, problem| {
            let (reference, base) = guards(reference, base);
            eligibility(classifier, reference, base, problem)
        };
        assert_eq!(
            verdict(None, Some(&clear), Some(&clear), None),
            Eligibility::Eligible
        );
        assert_eq!(
            verdict(Some("scripted"), Some(&alpha), Some(&clear), None),
            Eligibility::NotEligible("scripted".to_owned())
        );
        assert_eq!(
            verdict(None, Some(&alpha), Some(&clear), None),
            Eligibility::NotEligible("unresolved_reference_override:XALP".to_owned())
        );
        assert_eq!(
            verdict(None, Some(&clear), Some(&alternate), None),
            Eligibility::Eligible
        );
        // Missing or unparseable data is unknown, and so is an unresolved model: never eligible.
        assert_eq!(
            verdict(None, None, Some(&clear), None),
            Eligibility::Unknown("unparsed_reference_data".to_owned())
        );
        assert_eq!(
            verdict(None, Some(&clear), Some(b"junk"), None),
            Eligibility::Unknown("unparsed_base_record".to_owned())
        );
        assert_eq!(
            verdict(
                None,
                Some(&clear),
                Some(&clear),
                Some(crate::cluster_mapping::REASON_MODEL_NOT_LOADED)
            ),
            Eligibility::Unknown("model_not_loaded".to_owned())
        );
    }
}
