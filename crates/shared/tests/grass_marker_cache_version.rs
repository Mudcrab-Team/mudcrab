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

/// Version-nine masks predate effective NullTextureSet hiding in the supported MODS path.
#[test]
fn mods_visibility_policy_rejects_v9_masks() {
    let legacy = GrassCache {
        version: 9,
        types: vec![],
        cells: vec![],
    };
    let bytes = rkyv::to_bytes::<Error>(&legacy).unwrap();
    let archived = rkyv::access::<ArchivedGrassCache, Error>(&bytes).unwrap();
    assert!(validate_archived_cache(archived).is_err());
}

/// The current producer rebuilds producer-ten masks for reference-order placement and LAND recovery.
#[test]
fn reference_placement_policy_rejects_v10_masks() {
    let legacy = GrassCache {
        version: 10,
        types: vec![],
        cells: vec![],
    };
    let bytes = rkyv::to_bytes::<Error>(&legacy).unwrap();
    let archived = rkyv::access::<ArchivedGrassCache, Error>(&bytes).unwrap();
    assert!(validate_archived_cache(archived).is_err());
}

/// G7 keeps its LAND layout distinct from both earlier G7 and the restored G2 slots.
#[test]
fn g7_layout_rejects_prior_and_lower_layer_producers() {
    for version in [11, 12] {
        let legacy = GrassCache {
            version,
            types: vec![],
            cells: vec![],
        };
        let bytes = rkyv::to_bytes::<Error>(&legacy).unwrap();
        let archived = rkyv::access::<ArchivedGrassCache, Error>(&bytes).unwrap();
        assert!(validate_archived_cache(archived).is_err());
    }
}
