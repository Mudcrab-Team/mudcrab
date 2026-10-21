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
