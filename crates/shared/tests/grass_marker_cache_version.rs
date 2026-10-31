//! Marker-header policy must not accept masks produced before header filtering.

use rkyv::rancor::Error;
use shared::grass::{ArchivedGrassCache, GrassCache, validate_archived_cache};

/// A structurally valid v6 archive can still contain invisible marker obstruction bits.
#[test]
fn header_marker_policy_rejects_v6_masks() {
    let legacy = GrassCache {
        version: 6,
        types: vec![],
        cells: vec![],
    };
    let bytes = rkyv::to_bytes::<Error>(&legacy).unwrap();
    let archived = rkyv::access::<ArchivedGrassCache, Error>(&bytes).unwrap();
    assert!(
        validate_archived_cache(archived).is_err(),
        "v6 masks predate authoritative marker-header filtering and must rebuild"
    );
}
