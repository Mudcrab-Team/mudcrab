//! Canonical record payloads shared by the converter and runtime.
//!
//! These field types and their order are the existing `records.data` rkyv wire
//! contract. Keep them unchanged when adding typed readers for individual records.

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Debug, Archive, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchivedSubRecord {
    pub tag: [u8; 4],
    pub data: Vec<u8>,
}

#[derive(Debug, Archive, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchivedRecordData {
    pub subrecords: Vec<ArchivedSubRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(subrecords: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let data = ArchivedRecordData {
            subrecords: subrecords
                .iter()
                .map(|(tag, data)| ArchivedSubRecord {
                    tag: **tag,
                    data: data.to_vec(),
                })
                .collect(),
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&data)
            .unwrap()
            .to_vec()
    }

    #[test]
    fn the_archive_bytes_are_pinned() {
        // The exporter's `serialize_subrecords` must give exactly these bytes: they are the
        // stored blobs, and the digest hashes them.
        let sample = bytes(&[(b"XALP", &[1, 0]), (b"MODS", &[1, 0, 0, 0])]);
        let hex: String = sample.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, PINNED_SAMPLE_HEX);
        let back = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&sample).unwrap();
        assert_eq!(back.subrecords.len(), 2);
        assert_eq!(&back.subrecords[1].tag, b"MODS");
    }

    #[test]
    fn bytes_that_are_not_an_archive_do_not_decode() {
        for junk in [&b"junk"[..], &[], &[0xff; 64]] {
            assert!(rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(junk).is_err());
        }
    }

    const PINNED_SAMPLE_HEX: &str =
        "010001000000000058414c50f4ffffff020000004d4f4453eaffffff04000000e8ffffff02000000";
    // Independently reproduce the converter's old definitions to detect a wire
    // change when the shared contract is refactored.
    #[derive(Archive, Serialize, Deserialize)]
    struct OldSubRecord {
        tag: [u8; 4],
        data: Vec<u8>,
    }

    #[derive(Archive, Serialize, Deserialize)]
    struct OldRecordData {
        subrecords: Vec<OldSubRecord>,
    }

    #[test]
    fn canonical_record_archive_preserves_previous_bytes() {
        let fields = [
            (*b"EDID", b"asymmetric\0".to_vec()),
            (*b"CNAM", vec![3, 7, 11, 19]),
        ];
        let previous = OldRecordData {
            subrecords: fields
                .iter()
                .map(|(tag, data)| OldSubRecord {
                    tag: *tag,
                    data: data.clone(),
                })
                .collect(),
        };
        let shared = ArchivedRecordData {
            subrecords: fields
                .iter()
                .map(|(tag, data)| ArchivedSubRecord {
                    tag: *tag,
                    data: data.clone(),
                })
                .collect(),
        };
        let previous_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&previous).unwrap();
        let shared_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&shared).unwrap();
        assert_eq!(previous_bytes.as_slice(), shared_bytes.as_slice());
        let restored =
            rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&previous_bytes).unwrap();
        assert_eq!(restored, shared);
    }
}
