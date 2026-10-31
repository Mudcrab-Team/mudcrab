//! Real producer/consumer proof for a derived cluster overlay. The converter's own
//! `derive_extensions::run` writes the overlay from a small synthetic complete base; the engine's
//! `envelope::open`, `CertifiedPackage::open` and `load_cluster_package` then read it. No overlay
//! writer, hash or manifest is reimplemented here: whatever the producer serializes (extension
//! hashes, `owns` lists, the terrain `requires` dependency, `overlay.json`) the consumer verifies.
//!
//! No GPU is needed. The derivation is forced into cluster mode, so its capability probe only fills
//! the run report: with no adapter the probe fails and the derivation proceeds, and the overlay's
//! bytes do not depend on it. Free disk is checked by the real admission, on the tempfile folder.
use super::*;
use crate::cluster::{envelope, terrain::ClusterTerrainMaterial};
use bevy::ecs::system::RunSystemOnce;
use converter::{
    derive_extensions::{self, DeriveReport},
    test_support::SyntheticBase,
};
use shared::{
    cluster_overlay::read_metadata,
    extensions::{CLUSTER_TERRAIN, CLUSTERS},
};
use std::path::Path;

fn derive(base: &SyntheticBase, terrain: bool) -> DeriveReport {
    derive_extensions::run(&base.derive_options(terrain)).expect("real derivation of the base")
}

/// Runs the real loader system over a certified package and returns the app holding its result.
fn load(base: &Path, overlay: &Path, terrain: bool, package: Arc<CertifiedPackage>) -> App {
    let mut app = App::new();
    app.insert_resource(PackageSource {
        base: base.to_owned(),
        overlay: overlay.to_owned(),
        terrain,
        priority_point: None,
    })
    .insert_resource(ValidatedPackage(package))
    .insert_resource(PackageValidationStatus::Ready)
    .init_resource::<ClusterBatchHandles>()
    .init_resource::<Assets<Image>>()
    .init_resource::<Assets<ClusterTerrainMaterial>>()
    .add_message::<AppExit>();
    app.world_mut()
        .run_system_once(load_cluster_package)
        .unwrap();
    assert!(
        !app.world()
            .resource::<Messages<AppExit>>()
            .iter_current_update_messages()
            .any(AppExit::is_error),
        "the loader refused a package the producer just derived"
    );
    app
}

#[test]
fn derived_object_and_terrain_overlay_certifies_and_loads() {
    let base = SyntheticBase::complete();
    let derived = derive(&base, true);
    assert!(!derived.incomplete_core);
    let produced = |id: &str| {
        derived
            .extensions
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("the derivation produced no {id} extension"))
    };
    let (produced_base, produced_terrain) = (produced(CLUSTERS), produced(CLUSTER_TERRAIN));

    // The consumer's envelope recomputes both extension hashes from the overlay's tables and files and
    // checks the `owns` lists and the terrain manifest's `requires` hash against what it read.
    let opened = envelope::open(base.path(), &derived.output, true).expect("envelope::open");
    assert_eq!(&opened.base, produced_base);
    assert_eq!(opened.terrain.as_ref(), Some(produced_terrain));
    assert_eq!(opened.builder_identity, derived.stage.builder_identity);
    let metadata = read_metadata(&derived.output).unwrap();
    assert_eq!(metadata.base_failure_count, 0);
    assert_eq!(opened.metadata.settings_id, metadata.settings_id);
    drop(opened);
    // An object-only open of a terrain overlay is also valid: the terrain entry is simply unread.
    drop(envelope::open(base.path(), &derived.output, false).expect("object-only open"));

    let package = Arc::new(
        CertifiedPackage::open(base.path(), &derived.output, true).expect("CertifiedPackage::open"),
    );
    assert!(
        package.records().count() > 0,
        "the object reference is certified"
    );

    let app = load(base.path(), &derived.output, true, package.clone());
    let scene = app.world().resource::<ClusterScene>();
    assert!(Arc::ptr_eq(scene.certificate.as_ref().unwrap(), &package));
    assert!(!scene.meshes.is_empty());
    assert_eq!(scene.terrain_blocks.len(), 1, "one terrain block");
    assert!(
        scene
            .instances
            .iter()
            .any(|instance| matches!(instance.identity, ClusterInstanceIdentity::Object(_)))
    );
    assert!(
        scene
            .instances
            .iter()
            .any(|instance| matches!(instance.identity, ClusterInstanceIdentity::Terrain { .. }))
    );
    assert!(app.world().contains_resource::<LazyBatches>());
    assert!(app.world().contains_resource::<TerrainBatchIds>());

    drop(app);
    drop(package);
}

#[test]
fn overlay_derived_over_a_base_with_a_recorded_failure_opens_and_loads() {
    let base = SyntheticBase::with_failed_asset();
    let derived = derive(&base, false);
    let mut config = crate::config::EngineConfig {
        assets_dir: base.path().to_owned(),
        ..Default::default()
    };
    let refusal = format!(
        "{:#}",
        crate::app::validate_runtime_assets(&config).unwrap_err()
    );
    assert!(refusal.contains("--allow-incomplete-assets"), "{refusal}");
    config.allow_incomplete_assets = true;
    crate::app::validate_runtime_assets(&config)
        .expect("existing incomplete-assets admission flag");
    assert!(derived.incomplete_core);
    let metadata = read_metadata(&derived.output).unwrap();
    assert_eq!(
        metadata.base_failure_count, 1,
        "the overlay records the base's failure count and does not hide it"
    );
    let package = Arc::new(
        CertifiedPackage::open(base.path(), &derived.output, false)
            .expect("CertifiedPackage::open"),
    );
    assert!(package.records().count() > 0);
    let app = load(base.path(), &derived.output, false, package.clone());
    let scene = app.world().resource::<ClusterScene>();
    assert!(!scene.instances.is_empty());
    assert!(scene.terrain_blocks.is_empty());
    drop(app);
    drop(package);
}
