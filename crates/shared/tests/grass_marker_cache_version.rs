//! Marker-header policy must not accept masks produced before header filtering.

use rkyv::rancor::Error;
use shared::grass::{ArchivedGrassCache, GRASS_CACHE_VERSION, GrassCache, validate_archived_cache};

/// Restored model-less slots rebuild earlier masks and keep the current identity explicit.
#[test]
fn authored_slots_reject_prior_producers_and_accept_current() {
    for version in [10, 11, GRASS_CACHE_VERSION] {
        let cache = GrassCache {
            version,
            types: vec![],
            cells: vec![],
        };
        let bytes = rkyv::to_bytes::<Error>(&cache).unwrap();
        let archived = rkyv::access::<ArchivedGrassCache, Error>(&bytes).unwrap();
        assert_eq!(
            validate_archived_cache(archived).is_ok(),
            version == GRASS_CACHE_VERSION
        );
    }
}

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

/// Producer 14 changes what zero and negative Position Range rules mean, so producer-13 caches
/// must rebuild instead of keeping placeholder slots for negative ranges.
#[test]
fn retail_range_policy_rejects_v13_and_accepts_v14() {
    for (version, accepted) in [(13, false), (14, true)] {
        let cache = GrassCache {
            version,
            types: vec![],
            cells: vec![],
        };
        let bytes = rkyv::to_bytes::<Error>(&cache).unwrap();
        let archived = rkyv::access::<ArchivedGrassCache, Error>(&bytes).unwrap();
        assert_eq!(validate_archived_cache(archived).is_ok(), accepted);
    }
    assert_eq!(GRASS_CACHE_VERSION, 14);
}
