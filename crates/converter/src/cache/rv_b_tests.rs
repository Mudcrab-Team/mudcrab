//! Asset settings and explicit producer compatibility controls.

use super::*;
use crate::config::{PipelineConfig, RecordReader, TextureEncoder};

/// Producer-24 compatibility is explicitly bound to the producer-26 contract.
#[test]
fn producer_24_mesh_compatibility_does_not_follow_a_future_producer() {
    assert!(can_reuse_meshes_for_producer(24, 26));
    assert!(can_reuse_meshes_for_producer(26, 26));
    assert!(!can_reuse_meshes_for_producer(25, 26));
    assert!(!can_reuse_meshes_for_producer(27, 26));
    assert!(!can_reuse_meshes_for_producer(24, 27));
    assert!(!can_reuse_meshes_for_producer(26, 27));
    assert!(can_reuse_meshes_for_producer(27, 27));
}

/// Every asset kind ignores the full reader identity while metadata retains it.
#[test]
fn asset_settings_are_independent_of_reader_metadata() {
    let mut legacy = PipelineConfig::new("synthetic-data", "synthetic-output");
    legacy.record_reader = RecordReader::Legacy;
    let mut inhouse = legacy.clone();
    inhouse.record_reader = RecordReader::Inhouse;
    assert_ne!(
        configuration_hash(&legacy).unwrap(),
        configuration_hash(&inhouse).unwrap()
    );
    for kind in ["nif", "dds", "pex"] {
        assert_eq!(
            asset_configuration_hash_for_schema(&legacy, 26, kind).unwrap(),
            asset_configuration_hash_for_schema(&inhouse, 26, kind).unwrap(),
        );
    }
}

/// MO2 source selection binds metadata without widening per-kind asset settings.
#[test]
fn mo2_profile_and_reader_bind_metadata_without_changing_asset_settings() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    let instance = root.path().join("MO2");
    fs::create_dir_all(&data).unwrap();
    for path in ["mods", "overwrite", "profiles/Default", "profiles/Second"] {
        fs::create_dir_all(instance.join(path)).unwrap();
    }
    fs::write(
        instance.join("ModOrganizer.ini"),
        "[General]\ngameName=Skyrim Special Edition\n",
    )
    .unwrap();
    let mut original = PipelineConfig::new(&data, root.path().join("output"));
    original.record_reader = RecordReader::Legacy;
    let mut mo2_config = original.clone();
    mo2_config.mo2 = Some(mo2::Selection {
        instance_path: instance,
        profile: "Default".into(),
    });
    let mut other_profile = mo2_config.clone();
    other_profile.mo2.as_mut().unwrap().profile = "Second".into();
    let mut other_reader = mo2_config.clone();
    other_reader.record_reader = RecordReader::Inhouse;
    let hash = configuration_hash(&mo2_config).unwrap();
    for config in [&original, &other_profile, &other_reader] {
        assert_ne!(hash, configuration_hash(config).unwrap());
        for kind in ["nif", "dds", "pex"] {
            assert_eq!(
                asset_configuration_hash_for_schema(&mo2_config, 26, kind).unwrap(),
                asset_configuration_hash_for_schema(config, 26, kind).unwrap(),
            );
        }
    }
}

/// New legacy projections have distinct metadata proof without changing asset settings.
#[test]
fn legacy_projection_identity_separates_current_and_historical_full_hashes() {
    let mut legacy = PipelineConfig::new("synthetic-data", "synthetic-output");
    legacy.record_reader = RecordReader::Legacy;
    let historical = configuration_hash_for_schema(&legacy, 26).unwrap();
    let identity = crate::esm::inhouse::reader_identity(RecordReader::Legacy);
    assert_eq!(
        identity,
        serde_json::json!({"mode": "legacy", "projection_version": crate::esm::inhouse::LEGACY_PROJECTION_VERSION})
    );
    assert_ne!(configuration_hash(&legacy).unwrap(), historical);
    assert_eq!(
        legacy_projection_v1_configuration_hash(&legacy, 26).unwrap(),
        "81deb6e0dccb63cc67289e38d9eeba2b6a312db62e09db8dbd35fa834ec022b2"
    );
    assert_eq!(
        legacy_projection_v2_configuration_hash(&legacy, 26).unwrap(),
        "52045b4ce9519e636d5948e24185871df559dfe1f40b1edbe6195b844d71d59f"
    );
    assert_eq!(
        configuration_hash(&legacy).unwrap(),
        "4482b6966e40954faa2655b13e909ba75a107d58ebbf6478b977dcf45c92b912"
    );
    assert_eq!(
        configuration_hash_for_schema(&legacy, 15).unwrap(),
        "9a58fda00b27d0f2a8e46afb9334ea869602556393a35bffd7fcb39582a08a4f"
    );
    let mut changed_identity = identity.clone();
    changed_identity["projection_version"] =
        serde_json::json!(crate::esm::inhouse::LEGACY_PROJECTION_VERSION + 1);
    assert_ne!(
        configuration_hash(&legacy).unwrap(),
        configuration_hash_with_identity(&legacy, 26, Some(&changed_identity)).unwrap()
    );
    for kind in ["nif", "dds", "pex"] {
        let mut inhouse = legacy.clone();
        inhouse.record_reader = RecordReader::Inhouse;
        assert_eq!(
            asset_configuration_hash_for_schema(&legacy, 26, kind).unwrap(),
            asset_configuration_hash_for_schema(&inhouse, 26, kind).unwrap()
        );
    }
}

/// A recorded projection contract must match its own full settings proof.
#[test]
fn retained_legacy_hash_validation_does_not_upgrade_old_reader_provenance() {
    let mut config = PipelineConfig::new("synthetic-data", "synthetic-output");
    config.record_reader = RecordReader::Legacy;
    let old_identity = serde_json::json!({"mode": "legacy"});
    let current_identity = crate::esm::inhouse::reader_identity(RecordReader::Legacy);
    let v1_identity = serde_json::json!({"mode": "legacy", "projection_version": 1});
    let v2_identity = serde_json::json!({"mode": "legacy", "projection_version": 2});
    let historical = configuration_hash_for_schema(&config, 26).unwrap();
    let current = configuration_hash(&config).unwrap();
    let v1 = legacy_projection_v1_configuration_hash(&config, 26).unwrap();
    let v2 = legacy_projection_v2_configuration_hash(&config, 26).unwrap();
    let contracts = [
        (&old_identity, historical.as_str()),
        (&v1_identity, v1.as_str()),
        (&v2_identity, v2.as_str()),
        (&current_identity, current.as_str()),
    ];
    for (identity, matching) in contracts {
        assert!(
            retained_configuration_matches_for_reader(&config, 26, matching, identity).unwrap()
        );
        for (other_identity, other_hash) in contracts {
            if identity != other_identity {
                assert_ne!(matching, other_hash);
                assert!(
                    !retained_configuration_matches_for_reader(&config, 26, other_hash, identity)
                        .unwrap()
                );
            }
        }
        let mut changed = config.clone();
        changed.script_abi_version += 1;
        assert!(
            !retained_configuration_matches_for_reader(&changed, 26, matching, identity).unwrap()
        );
    }
    assert!(
        retained_configuration_matches_for_reader(
            &config,
            15,
            "b23881a864cdcfe1609779a9a27fefab2de629b50ec339101d4e0ad592e7e435",
            &old_identity,
        )
        .unwrap()
    );
    for identity in [&v1_identity, &v2_identity, &current_identity] {
        assert!(
            !retained_configuration_matches_for_reader(
                &config,
                15,
                "b23881a864cdcfe1609779a9a27fefab2de629b50ec339101d4e0ad592e7e435",
                identity,
            )
            .unwrap()
        );
    }
}

/// Changes to texture encoding settings do not discard unrelated mesh/script proof.
#[test]
fn texture_settings_invalidate_only_the_texture_kind() {
    let original = PipelineConfig::new("synthetic-data", "synthetic-output");
    let mut variants = Vec::new();
    let mut quality = original.clone();
    quality.texture_fallback_quality -= 1;
    variants.push(quality);
    let mut uastc = original.clone();
    uastc.texture_uastc_level += 1;
    variants.push(uastc);
    let mut zstd = original.clone();
    zstd.texture_zstd_level = 0;
    variants.push(zstd);
    let mut gpu = original.clone();
    gpu.texture_encoder = TextureEncoder::Gpu {
        quality: 2,
        batch_mb: 4,
    };
    variants.push(gpu);
    for changed in variants {
        assert_ne!(
            asset_configuration_hash_for_schema(&original, 26, "dds").unwrap(),
            asset_configuration_hash_for_schema(&changed, 26, "dds").unwrap(),
        );
        for kind in ["nif", "pex"] {
            assert_eq!(
                asset_configuration_hash_for_schema(&original, 26, kind).unwrap(),
                asset_configuration_hash_for_schema(&changed, 26, kind).unwrap(),
            );
        }
    }
}

/// ABI changes discard script proof while leaving mesh/texture settings intact.
#[test]
fn script_abi_invalidates_only_the_script_kind() {
    let original = PipelineConfig::new("synthetic-data", "synthetic-output");
    let mut changed = original.clone();
    changed.script_abi_version += 1;
    assert_ne!(
        asset_configuration_hash_for_schema(&original, 26, "pex").unwrap(),
        asset_configuration_hash_for_schema(&changed, 26, "pex").unwrap(),
    );
    for kind in ["nif", "dds"] {
        assert_eq!(
            asset_configuration_hash_for_schema(&original, 26, kind).unwrap(),
            asset_configuration_hash_for_schema(&changed, 26, kind).unwrap(),
        );
    }
}

/// Script settings and GPU scheduling stay independent of unrelated output producers.
#[test]
fn script_settings_and_gpu_batch_size_keep_their_existing_contracts() {
    let mut first = PipelineConfig::new("synthetic-data", "synthetic-output");
    assert_eq!(
        asset_configuration_hash_for_schema(&first, 16, "pex").unwrap(),
        asset_configuration_hash_for_schema(&first, 26, "pex").unwrap(),
    );
    first.texture_encoder = TextureEncoder::Gpu {
        quality: 2,
        batch_mb: 4,
    };
    let mut second = first.clone();
    second.texture_encoder = TextureEncoder::Gpu {
        quality: 2,
        batch_mb: 8,
    };
    assert_eq!(
        asset_configuration_hash_for_schema(&first, 26, "dds").unwrap(),
        asset_configuration_hash_for_schema(&second, 26, "dds").unwrap(),
    );
}
