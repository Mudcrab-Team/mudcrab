//! Offline cost estimates consumed by runtime streaming admission.
//!
//! Bytes are estimates of combined CPU and GPU allocations, including copies on
//! unified-memory machines. They are not file sizes or certified memory bounds.
//! Transient bytes are additional headroom beyond the retained resident bytes.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

pub const STREAMING_COST_SCHEMA_VERSION: u32 = 1;
pub const STREAMING_COST_ESTIMATOR_VERSION: u32 = 1;
pub const STREAMING_COST_FILE_NAME: &str = "streaming-costs.json";
pub const MAX_STREAMING_COST_CATALOG_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimateQuality {
    Conservative,
    /// The format, dependency, or generated allocation cannot be estimated.
    /// The runtime must apply its configured fallback before admitting it.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteEstimate {
    pub resident_bytes: u64,
    pub peak_transient_bytes: u64,
    pub quality: EstimateQuality,
    pub notes: Vec<String>,
}

impl ByteEstimate {
    pub fn conservative(resident_bytes: u64, peak_transient_bytes: u64, note: &str) -> Self {
        Self {
            resident_bytes,
            peak_transient_bytes,
            quality: EstimateQuality::Conservative,
            notes: vec![note.to_owned()],
        }
    }

    pub fn unknown(note: &str) -> Self {
        Self {
            resident_bytes: 0,
            peak_transient_bytes: 0,
            quality: EstimateQuality::Unknown,
            notes: vec![note.to_owned()],
        }
    }

    pub fn with_fallback(&self, fallback_bytes: u64) -> Self {
        let mut resolved = self.clone();
        if resolved.quality == EstimateQuality::Unknown {
            resolved.resident_bytes = resolved.resident_bytes.max(fallback_bytes);
            resolved.peak_transient_bytes = resolved.peak_transient_bytes.max(fallback_bytes);
        }
        resolved
    }

    pub fn total_peak_bytes(&self) -> Option<u64> {
        self.resident_bytes.checked_add(self.peak_transient_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    SceneGeometry,
    Texture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceCost {
    pub kind: ResourceKind,
    pub resident_bytes: u64,
    pub peak_transient_bytes: u64,
    pub quality: EstimateQuality,
    pub notes: Vec<String>,
}

impl ResourceCost {
    pub fn new(kind: ResourceKind, estimate: ByteEstimate) -> Self {
        Self {
            kind,
            resident_bytes: estimate.resident_bytes,
            peak_transient_bytes: estimate.peak_transient_bytes,
            quality: estimate.quality,
            notes: estimate.notes,
        }
    }

    pub fn estimate(&self) -> ByteEstimate {
        ByteEstimate {
            resident_bytes: self.resident_bytes,
            peak_transient_bytes: self.peak_transient_bytes,
            quality: self.quality,
            notes: self.notes.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneCost {
    /// Root-relative resource keys. Repeated placements share these allocations.
    pub resource_keys: Vec<String>,
    /// Collider and body allocations are charged for each placement separately.
    pub per_placement_collision: ByteEstimate,
    pub per_placement_ecs: ByteEstimate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedCostDefaults {
    /// Four full-detail terrain quadrants, weight images and cell bookkeeping.
    /// External terrain textures still require separate resource reservations.
    pub full_cell_terrain: ByteEstimate,
    pub full_cell_collision: ByteEstimate,
    pub full_cell_water: ByteEstimate,
    pub model_placement: ByteEstimate,
    /// Grass density, particles and dynamically generated geometry vary at runtime.
    pub other_generated: ByteEstimate,
}

impl Default for GeneratedCostDefaults {
    fn default() -> Self {
        Self {
            full_cell_terrain: ByteEstimate::conservative(
                2 * 1024 * 1024,
                2 * 1024 * 1024,
                "Four 17x17 meshes and 17x17 RGBA weight images; includes CPU/GPU copies, materials and bookkeeping; excludes external textures",
            ),
            full_cell_collision: ByteEstimate::conservative(
                1024 * 1024,
                1024 * 1024,
                "33x33 terrain samples, 2048 triangles and conservative collider/BVH/build overhead",
            ),
            full_cell_water: ByteEstimate::conservative(
                256 * 1024,
                256 * 1024,
                "Cell water mesh and material bookkeeping; shared water textures and reflection targets are excluded",
            ),
            model_placement: ByteEstimate::conservative(
                16 * 1024,
                16 * 1024,
                "Generic scene root, hierarchy, transforms, instance data and scheduling bookkeeping",
            ),
            other_generated: ByteEstimate::unknown(
                "Grass, particles, procedural models and reflection targets require runtime-specific reservations",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamingCostCatalog {
    pub schema_version: u32,
    pub estimator_version: u32,
    /// SHA-256 of the exact conversion-manifest.json bytes for this immutable pack.
    pub pack_fingerprint_sha256: String,
    pub resources: BTreeMap<String, ResourceCost>,
    pub scenes: BTreeMap<String, SceneCost>,
    pub generated: GeneratedCostDefaults,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSceneCost {
    pub resources: BTreeMap<String, ResourceCost>,
    pub per_placement_collision: ByteEstimate,
    pub per_placement_ecs: ByteEstimate,
    pub used_unknown_fallback: bool,
}

impl StreamingCostCatalog {
    pub fn empty(pack_fingerprint_sha256: String) -> Self {
        Self { schema_version: STREAMING_COST_SCHEMA_VERSION,
            estimator_version: STREAMING_COST_ESTIMATOR_VERSION,
            pack_fingerprint_sha256, resources: BTreeMap::new(), scenes: BTreeMap::new(),
            generated: GeneratedCostDefaults::default(), notes: vec![
                "Estimates include CPU and GPU copies; allocator, driver and platform variation require runtime pressure feedback".to_owned(),
                "Manifest fingerprint assumes immutable converted assets; editing a file without updating its manifest invalidates this contract".to_owned(),
            ] }
    }

    /// Reads one bounded metadata file. Does not open or inspect asset payloads.
    pub fn load(path: &Path, expected_sha256: &str) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        ensure!(
            file.metadata()?.len() <= MAX_STREAMING_COST_CATALOG_BYTES,
            "streaming cost catalog exceeds metadata size limit"
        );
        let mut bytes = Vec::new();
        file.take(MAX_STREAMING_COST_CATALOG_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_STREAMING_COST_CATALOG_BYTES,
            "streaming cost catalog grew beyond metadata size limit"
        );
        let catalog: Self =
            serde_json::from_slice(&bytes).wrap_err("invalid streaming cost catalog")?;
        catalog.validate(expected_sha256)?;
        Ok(catalog)
    }

    pub fn validate(&self, expected_sha256: &str) -> Result<()> {
        ensure!(
            self.schema_version == STREAMING_COST_SCHEMA_VERSION,
            "unsupported streaming cost catalog schema {}",
            self.schema_version
        );
        ensure!(
            self.estimator_version == STREAMING_COST_ESTIMATOR_VERSION,
            "unsupported streaming cost estimator {}",
            self.estimator_version
        );
        ensure!(
            is_sha256(expected_sha256) && is_sha256(&self.pack_fingerprint_sha256),
            "streaming cost pack fingerprint is not a canonical SHA-256"
        );
        ensure!(
            self.pack_fingerprint_sha256 == expected_sha256,
            "streaming cost catalog belongs to a different converted pack"
        );
        for (key, cost) in &self.resources {
            ensure!(
                canonical_resource_key(key)? == *key,
                "noncanonical resource key {key}"
            );
            ensure!(
                match cost.kind {
                    ResourceKind::SceneGeometry => key.ends_with(".glb"),
                    ResourceKind::Texture => key.ends_with(".ktx2"),
                },
                "resource type does not match key {key}"
            );
            validate_estimate(&cost.estimate())?;
        }
        for (key, scene) in &self.scenes {
            ensure!(
                canonical_resource_key(key)? == *key && key.ends_with(".glb"),
                "noncanonical scene key {key}"
            );
            let mut seen = BTreeSet::new();
            for resource in &scene.resource_keys {
                ensure!(
                    canonical_resource_key(resource)? == *resource,
                    "noncanonical dependency key {resource}"
                );
                ensure!(
                    seen.insert(resource),
                    "duplicate dependency {resource} in {key}"
                );
                ensure!(
                    self.resources.contains_key(resource),
                    "missing resource metadata {resource} in {key}"
                );
            }
            ensure!(
                scene.resource_keys.contains(key),
                "scene {key} has no geometry resource"
            );
            ensure!(
                self.resources[key].kind == ResourceKind::SceneGeometry,
                "scene {key} names a nongeometry root resource"
            );
            validate_estimate(&scene.per_placement_collision)?;
            validate_estimate(&scene.per_placement_ecs)?;
        }
        for estimate in [
            &self.generated.full_cell_terrain,
            &self.generated.full_cell_collision,
            &self.generated.full_cell_water,
            &self.generated.model_placement,
            &self.generated.other_generated,
        ] {
            validate_estimate(estimate)?;
        }
        Ok(())
    }

    /// Resolves metadata only. Unknown estimates remain labelled unknown after
    /// their configured conservative fallback reservation has been applied.
    pub fn resolve_scene(&self, path: &str, fallback_bytes: u64) -> Result<ResolvedSceneCost> {
        ensure!(
            fallback_bytes > 0,
            "unknown resource fallback must be nonzero"
        );
        let key = canonical_resource_key(path)?;
        ensure!(
            key.ends_with(".glb"),
            "scene resource must name a converted GLB"
        );
        let missing_scene;
        let scene = match self.scenes.get(&key) {
            Some(scene) => scene,
            None => {
                missing_scene = SceneCost {
                    resource_keys: vec![key.clone()],
                    per_placement_collision: ByteEstimate::unknown(
                        "Scene collision metadata is absent",
                    ),
                    per_placement_ecs: self.generated.model_placement.clone(),
                };
                &missing_scene
            }
        };
        let mut resources = BTreeMap::new();
        let mut used_unknown_fallback = false;
        for resource_key in &scene.resource_keys {
            let original = self
                .resources
                .get(resource_key)
                .cloned()
                .unwrap_or_else(|| {
                    ResourceCost::new(
                        ResourceKind::SceneGeometry,
                        ByteEstimate::unknown("Scene resource metadata is absent"),
                    )
                });
            used_unknown_fallback |= original.quality == EstimateQuality::Unknown;
            let resolved = ResourceCost::new(
                original.kind,
                original.estimate().with_fallback(fallback_bytes),
            );
            ensure!(
                resolved
                    .resident_bytes
                    .checked_add(resolved.peak_transient_bytes)
                    .is_some(),
                "resolved resource cost overflows for {resource_key}"
            );
            resources.insert(resource_key.clone(), resolved);
        }
        used_unknown_fallback |= scene.per_placement_collision.quality == EstimateQuality::Unknown
            || scene.per_placement_ecs.quality == EstimateQuality::Unknown;
        let collision = scene.per_placement_collision.with_fallback(fallback_bytes);
        let ecs = scene.per_placement_ecs.with_fallback(fallback_bytes);
        validate_estimate(&collision)?;
        validate_estimate(&ecs)?;
        Ok(ResolvedSceneCost {
            resources,
            per_placement_collision: collision,
            per_placement_ecs: ecs,
            used_unknown_fallback,
        })
    }
}

fn validate_estimate(estimate: &ByteEstimate) -> Result<()> {
    ensure!(
        estimate.total_peak_bytes().is_some(),
        "streaming byte estimate overflows"
    );
    ensure!(
        !estimate.notes.is_empty(),
        "streaming estimate has no assumptions"
    );
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Converts runtime asset paths to lowercase root-relative resource identities.
/// URI decoding belongs to the offline analyzer; catalog keys are filesystem paths.
pub fn canonical_resource_key(input: &str) -> Result<String> {
    ensure!(
        !input.is_empty() && !input.chars().any(char::is_control),
        "invalid resource key"
    );
    let normalized = input.replace('\\', "/");
    ensure!(
        !normalized.starts_with('/') && !normalized.contains([':', '?', '#', '%']),
        "resource key is absolute or contains URI syntax: {input}"
    );
    let components: Vec<_> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    ensure!(
        !components.is_empty() && !components.contains(&".."),
        "resource key escapes asset root: {input}"
    );
    Ok(components.join("/").to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> StreamingCostCatalog {
        StreamingCostCatalog::empty("a".repeat(64))
    }

    #[test]
    fn shared_dependencies_have_one_identity_across_scenes() {
        let mut catalog = catalog();
        let texture = "textures/shared.ktx2".to_owned();
        catalog.resources.insert(
            texture.clone(),
            ResourceCost::new(
                ResourceKind::Texture,
                ByteEstimate::conservative(1024, 2048, "fixture"),
            ),
        );
        for scene_key in ["meshes/a.glb", "meshes/b.glb"] {
            catalog.resources.insert(
                scene_key.to_owned(),
                ResourceCost::new(
                    ResourceKind::SceneGeometry,
                    ByteEstimate::conservative(100, 200, "fixture"),
                ),
            );
            catalog.scenes.insert(
                scene_key.to_owned(),
                SceneCost {
                    resource_keys: vec![scene_key.to_owned(), texture.clone()],
                    per_placement_collision: ByteEstimate::conservative(10, 20, "fixture"),
                    per_placement_ecs: ByteEstimate::conservative(30, 40, "fixture"),
                },
            );
        }
        catalog.validate(&"a".repeat(64)).unwrap();
        let mut unique = BTreeMap::new();
        for scene in ["meshes/a.glb", "meshes/b.glb", "meshes/a.glb"] {
            unique.extend(catalog.resolve_scene(scene, 4096).unwrap().resources);
        }
        assert_eq!(unique.len(), 3);
        assert_eq!(
            unique.values().map(|cost| cost.resident_bytes).sum::<u64>(),
            1224
        );
    }

    #[test]
    fn unknown_resources_are_explicit_and_never_free() {
        let resolved = catalog()
            .resolve_scene("Meshes\\Unlisted.glb", 4096)
            .unwrap();
        assert!(resolved.used_unknown_fallback);
        assert_eq!(
            resolved.resources["meshes/unlisted.glb"].resident_bytes,
            4096
        );
        assert_eq!(resolved.per_placement_collision.resident_bytes, 4096);
        assert_eq!(
            resolved.resources["meshes/unlisted.glb"].quality,
            EstimateQuality::Unknown
        );
        assert!(catalog().resolve_scene("meshes/a.glb", 0).is_err());
    }

    #[test]
    fn fingerprint_schema_and_paths_are_checked() {
        assert!(catalog().validate(&"b".repeat(64)).is_err());
        for invalid in [
            "../x.glb",
            "/x.glb",
            "C:\\x.glb",
            "meshes/%2e%2e/x.glb",
            "meshes/x.glb#Scene0",
        ] {
            assert!(canonical_resource_key(invalid).is_err(), "{invalid}");
        }
        let mut catalog = catalog();
        catalog.schema_version += 1;
        assert!(catalog.validate(&"a".repeat(64)).is_err());
    }

    #[test]
    fn arithmetic_overflow_and_dangling_dependencies_are_rejected() {
        let mut invalid_catalog = catalog();
        invalid_catalog.resources.insert(
            "meshes/a.glb".to_owned(),
            ResourceCost::new(
                ResourceKind::SceneGeometry,
                ByteEstimate::conservative(u64::MAX, 1, "overflow"),
            ),
        );
        assert!(invalid_catalog.validate(&"a".repeat(64)).is_err());
        invalid_catalog.resources.clear();
        invalid_catalog.scenes.insert(
            "meshes/a.glb".to_owned(),
            SceneCost {
                resource_keys: vec!["textures/missing.ktx2".to_owned()],
                per_placement_collision: ByteEstimate::unknown("fixture"),
                per_placement_ecs: ByteEstimate::unknown("fixture"),
            },
        );
        assert!(invalid_catalog.validate(&"a".repeat(64)).is_err());
        assert!(
            catalog()
                .resolve_scene("meshes/unknown.glb", u64::MAX)
                .is_err()
        );
    }
}
