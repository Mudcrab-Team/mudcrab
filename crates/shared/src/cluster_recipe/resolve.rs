//! The one recipe for what the converter's inputs say about a reference in the declared scope.
//!
//! The stage uses it to write the mapping tables and the authority uses it, later and from the
//! dataset itself, to recompute the same facts, so a table row is only ever compared with the
//! output of this module. It turns a [`ScopedReference`] (one row of the core database) and a
//! [`ModelCatalog`] (the published model hashes and the GLB headers) into a
//! [`ReferenceSnapshot`]: the typed fields, the input digest, the resolved model, the transform
//! and the eligibility verdict.

use super::{
    classify::ScopedReference,
    model_facts::{ModelFacts, read_model_facts, runtime_texture_source, texture_set_hash},
    overrides::eligibility,
};
use crate::{
    cluster::hex,
    cluster_mapping::{
        MaterialSource, REASON_MODEL_CONTENT_CHANGED, REASON_MODEL_MATERIAL_PATH_CONFLICT,
        REASON_MODEL_NOT_CONVERTED, REASON_MODEL_NOT_LOADED, REASON_TEXTURE_CONTENT_CHANGED,
        REASON_TEXTURE_NOT_CONVERTED, REASON_TEXTURE_NOT_LOADED, ReferenceInputs,
        ReferenceSnapshot, input_digest,
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs, io,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

/// SHA-256 (hex) of a completed file hash, streamed.
fn hash_file(path: &Path, #[cfg(test)] reads: Option<&HashReads>) -> io::Result<String> {
    #[cfg(test)]
    if let Some(reads) = reads {
        reads.attempts.fetch_add(1, Ordering::Relaxed);
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 256 * 1024];
    #[cfg(test)]
    let mut bytes = 0;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        #[cfg(test)]
        {
            bytes += read as u64;
        }
    }
    #[cfg(test)]
    if let Some(reads) = reads {
        reads.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
    Ok(hex(&hasher.finalize()))
}

/// Test evidence for completed full-file verification and short-circuiting.
#[cfg(test)]
#[derive(Default)]
struct HashReads {
    attempts: AtomicU64,
    bytes: AtomicU64,
}

/// What the catalog knows about one converted model.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// The content hash the conversion manifest recorded for the model's GLB.
    pub hash: Option<String>,
    /// The header facts of the GLB (primitive set, texture paths).
    pub facts: Option<ModelFacts>,
    /// [`texture_set_hash`] of the facts against the published textures.
    pub texture_hash: Option<String>,
    /// Why the model is not usable (a coverage reason), `None` when it is.
    pub problem: Option<&'static str>,
}

type TextureVerification = Arc<OnceLock<Option<&'static str>>>;

/// The published models and textures of a conversion, resolved lazily and cached. A model counts
/// only when the bytes of its GLB, as they are on disk now, hash to the manifest's content hash,
/// and every named texture is published or a byte-identical reserved runtime alias
/// of a published source. Both alias and source bytes must match the manifest;
/// each unique model and texture is cached, however many references place it.
pub struct ModelCatalog {
    assets_root: PathBuf,
    hashes: HashMap<String, String>,
    /// The first (sorted) published GLB path of each content hash.
    paths_by_hash: HashMap<String, String>,
    /// Lower-case texture path to (published path, content hash).
    textures: HashMap<String, (String, String)>,
    cache: Mutex<HashMap<String, Arc<ModelInfo>>>,
    /// Per-launch, single-initialization verification of each lower-case runtime texture path.
    verified_textures: Mutex<HashMap<String, TextureVerification>>,
    #[cfg(test)]
    texture_computations: AtomicU64,
    #[cfg(test)]
    texture_reads: HashReads,
}

impl ModelCatalog {
    /// `assets_root` is the folder the published paths are relative to; `published` are the
    /// (path, content hash) pairs of the published files, as `conversion-manifest.json` lists them
    /// (`entries[*].output` and `output_hash`), in any order.
    ///
    /// The answer must not depend on that order, so a published identity that is ambiguous is an
    /// error: the same path with two content hashes, or two paths that differ only in case (a model
    /// or texture name is matched case-insensitively, so they would be one file).
    pub fn new(
        assets_root: &Path,
        published: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, String> {
        let mut hashes: HashMap<String, String> = HashMap::new();
        let mut textures: HashMap<String, (String, String)> = HashMap::new();
        let mut folded: HashMap<String, String> = HashMap::new();
        for (output, output_hash) in published {
            // Relevance is decided on the folded name, so `MESHES/a.GLB` is checked for aliases too.
            let lower = output.to_lowercase();
            let is_glb = lower.starts_with("meshes/") && lower.ends_with(".glb");
            if !is_glb && !lower.ends_with(".ktx2") {
                continue;
            }
            if let Some(other) = folded.insert(lower, output.clone())
                && other != output
            {
                return Err(format!(
                    "published paths {other} and {output} differ only in case"
                ));
            }
            let known = if is_glb {
                hashes.insert(output.clone(), output_hash.clone())
            } else {
                textures
                    .insert(output.to_lowercase(), (output.clone(), output_hash.clone()))
                    .map(|(_, hash)| hash)
            };
            if known.is_some_and(|known| known != output_hash) {
                return Err(format!("{output} is published with two content hashes"));
            }
        }
        let mut paths: Vec<&String> = hashes.keys().collect();
        paths.sort();
        let mut paths_by_hash = HashMap::new();
        for path in paths {
            paths_by_hash
                .entry(hashes[path].clone())
                .or_insert_with(|| path.clone());
        }
        Ok(Self {
            assets_root: assets_root.to_owned(),
            hashes,
            paths_by_hash,
            textures,
            cache: Mutex::new(HashMap::new()),
            verified_textures: Mutex::new(HashMap::new()),
            #[cfg(test)]
            texture_computations: AtomicU64::new(0),
            #[cfg(test)]
            texture_reads: HashReads::default(),
        })
    }

    /// The model at the converted GLB path `glb_path`.
    pub fn info(&self, glb_path: &str) -> Arc<ModelInfo> {
        if let Some(known) = self.cache.lock().expect("catalog lock").get(glb_path) {
            return known.clone();
        }
        // Computed outside the lock: two threads may both read the same header, which is harmless.
        let info = Arc::new(self.read(glb_path));
        self.cache
            .lock()
            .expect("catalog lock")
            .entry(glb_path.to_owned())
            .or_insert(info)
            .clone()
    }

    /// The first published GLB path (sorted) of this content: the path whose texture resolution
    /// the shared mesh payload and its material keys are built under.
    pub fn canonical_path(&self, hash: &str) -> Option<&str> {
        self.paths_by_hash.get(hash).map(String::as_str)
    }

    /// The material source of every primitive of the model with this content hash, at its
    /// canonical path: the GLB and slot the reader loads, the key and the render flags.
    pub fn materials_by_hash(&self, hash: &str) -> Option<BTreeMap<u32, MaterialSource>> {
        let path = self.paths_by_hash.get(hash)?;
        let info = self.info(path);
        let facts = info.facts.as_ref()?;
        Some(
            facts
                .materials
                .iter()
                .map(|(index, material)| {
                    (
                        *index,
                        MaterialSource {
                            model_path: path.clone(),
                            material_index: material.material_index,
                            material_key: material.material_key.clone(),
                            alpha_mask: material.alpha_mask,
                            alpha_cutoff: material.alpha_cutoff,
                            double_sided: material.double_sided,
                        },
                    )
                })
                .collect(),
        )
    }

    /// The primitive set of the model with this content hash, read from its GLB header.
    pub fn facts_by_hash(&self, hash: &str) -> Option<BTreeSet<u32>> {
        let path = self.paths_by_hash.get(hash)?;
        self.info(path)
            .facts
            .as_ref()
            .map(|facts| facts.primitives.clone())
    }

    /// Manifest expectation for a runtime image. Explicit published outputs win;
    /// only converter-reserved suffixes may borrow a canonical source hash.
    /// The caller keeps the original runtime path in the digest recipe.
    fn texture_hash(&self, path: &str) -> Option<&str> {
        if let Some((_, hash)) = self.textures.get(path) {
            return Some(hash.as_str());
        }
        let source = runtime_texture_source(path)?;
        if source == path {
            return None;
        }
        self.textures.get(&source).map(|(_, hash)| hash.as_str())
    }

    /// Verify a lower-case runtime texture against its explicit manifest entry,
    /// or a reserved alias against BOTH canonical-source and runtime bytes.
    fn texture_problem(&self, path: &str) -> Option<&'static str> {
        let known = {
            let mut textures = self.verified_textures.lock().expect("texture lock");
            if let Some(known) = textures.get(path) {
                Arc::clone(known)
            } else {
                let known = Arc::new(OnceLock::new());
                textures.insert(path.to_owned(), Arc::clone(&known));
                known
            }
        };
        // Release the map lock before initialization: aliases recurse to their explicitly
        // published source, which has a distinct key and must retain its own full-byte check.
        *known.get_or_init(|| self.compute_texture_problem(path))
    }

    fn compute_texture_problem(&self, path: &str) -> Option<&'static str> {
        #[cfg(test)]
        self.texture_computations.fetch_add(1, Ordering::Relaxed);
        if let Some((published, expected)) = self.textures.get(path) {
            // An explicit runtime output has its own authoritative bytes, even
            // when its name happens to use a reserved alias suffix.
            match hash_file(
                &self.assets_root.join(published),
                #[cfg(test)]
                Some(&self.texture_reads),
            ) {
                Err(_) => Some(REASON_TEXTURE_NOT_LOADED),
                Ok(found) if found != *expected => Some(REASON_TEXTURE_CONTENT_CHANGED),
                Ok(_) => None,
            }
        } else if let Some(source) = runtime_texture_source(path).filter(|source| source != path) {
            if let Some((_, expected)) = self.textures.get(&source) {
                // The source must itself be explicitly published and verified.
                // A copied alias is not authority for an absent/changed source.
                self.texture_problem(&source).or_else(|| {
                    match hash_file(
                        &self.assets_root.join(path),
                        #[cfg(test)]
                        Some(&self.texture_reads),
                    ) {
                        Err(_) => Some(REASON_TEXTURE_NOT_LOADED),
                        Ok(found) if found != *expected => Some(REASON_TEXTURE_CONTENT_CHANGED),
                        Ok(_) => None,
                    }
                })
            } else {
                Some(REASON_TEXTURE_NOT_CONVERTED)
            }
        } else {
            Some(REASON_TEXTURE_NOT_CONVERTED)
        }
    }

    fn read(&self, glb_path: &str) -> ModelInfo {
        let Some(hash) = self.hashes.get(glb_path).cloned() else {
            return ModelInfo {
                hash: None,
                facts: None,
                texture_hash: None,
                problem: Some(REASON_MODEL_NOT_CONVERTED),
            };
        };
        let unusable = |problem| ModelInfo {
            hash: Some(hash.clone()),
            facts: None,
            texture_hash: None,
            problem: Some(problem),
        };
        // The bytes on disk must be the bytes the manifest hashed: geometry, materials and
        // everything else the header does not show.
        match hash_file(
            &self.assets_root.join(glb_path),
            #[cfg(test)]
            None,
        ) {
            Err(_) => return unusable(REASON_MODEL_NOT_LOADED),
            Ok(found) if found != hash => return unusable(REASON_MODEL_CONTENT_CHANGED),
            Ok(_) => {}
        }
        let facts = match read_model_facts(&self.assets_root, glb_path) {
            Ok(facts) => facts,
            Err(_) => return unusable(REASON_MODEL_NOT_LOADED),
        };
        let mut problem = facts
            .texture_paths
            .iter()
            .find_map(|path| self.texture_problem(path));
        // Same-byte GLBs share one mesh payload, built under the canonical path. A reference whose
        // GLB is at another path may use it only when its materials resolve to the same keys.
        if problem.is_none()
            && let Some(canonical) = self.paths_by_hash.get(&hash)
            && canonical != glb_path
        {
            let winner = self.info(canonical);
            problem = match (winner.problem, &winner.facts) {
                (Some(problem), _) => Some(problem),
                (None, Some(winner)) if winner.materials != facts.materials => {
                    Some(REASON_MODEL_MATERIAL_PATH_CONFLICT)
                }
                _ => None,
            };
        }
        ModelInfo {
            // Only this model's own texture paths are looked up in the dataset's map.
            texture_hash: Some(texture_set_hash(&facts, |path| self.texture_hash(path))),
            hash: Some(hash),
            facts: Some(facts),
            problem,
        }
    }
}

/// The snapshot of one scoped reference. `include_trees` is part of the run's scope and already
/// reflected in `reference.refusal`.
pub fn snapshot(reference: &ScopedReference, catalog: &ModelCatalog) -> ReferenceSnapshot {
    // A candidate names its converted GLB; a refused reference is not resolved against a model.
    let model = reference
        .candidate
        .as_ref()
        .map(|candidate| catalog.info(&candidate.glb_path));
    let model_problem = model.as_ref().and_then(|info| info.problem);
    let eligibility = eligibility(
        reference.refusal.as_deref(),
        reference.reference_guard,
        reference.base_guard,
        model_problem,
    );
    let inputs = ReferenceInputs {
        reference_id: reference.reference_id,
        base_form_id: reference.base_form_id,
        record_type: &reference.record_type,
        placed_worldspace_id: reference.placed_worldspace_id,
        source_cell_id: reference.source_cell_id,
        model_path: reference.model_path.as_deref(),
        model_hash: model.as_ref().and_then(|info| info.hash.as_deref()),
        position: reference.position,
        rotation: reference.rotation,
        scale: reference.scale,
        header_flags: reference.header_flags,
        enable_parent_id: reference.enable_parent,
        snow_material: reference.snow_material,
        data_hash: reference.data_hash.as_deref(),
        base_record_hash: reference.base_record_hash.as_deref(),
        texture_set_hash: model.as_ref().and_then(|info| info.texture_hash.as_deref()),
    };
    let eligible = matches!(eligibility, crate::cluster_mapping::Eligibility::Eligible);
    ReferenceSnapshot {
        base_form_id: reference.base_form_id,
        snow_material: reference.snow_material,
        placed_worldspace_id: reference.placed_worldspace_id,
        source_cell_id: reference.source_cell_id,
        cell: reference.cell,
        block: (
            reference.block_worldspace_id,
            crate::cluster::BLOCK_SIDE_CELLS,
            reference.block_anchor.0,
            reference.block_anchor.1,
        ),
        digest: input_digest(&inputs),
        model: if eligible {
            model.as_ref().and_then(|info| info.hash.clone())
        } else {
            None
        },
        transform: reference
            .candidate
            .as_ref()
            .map(|candidate| candidate.transform),
        eligibility,
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(path, hash)| ((*path).to_owned(), (*hash).to_owned()))
            .collect()
    }

    #[test]
    fn the_catalog_does_not_depend_on_the_order_of_the_published_files() {
        let list = [
            ("meshes/b/x.glb", "11"),
            ("meshes/a/x.glb", "11"),
            ("textures/t.ktx2", "22"),
            ("textures/t.ktx2", "22"),
        ];
        let mut reversed = list;
        reversed.reverse();
        let forward = ModelCatalog::new(Path::new("."), pairs(&list)).unwrap();
        let backward = ModelCatalog::new(Path::new("."), pairs(&reversed)).unwrap();
        assert_eq!(forward.canonical_path("11"), Some("meshes/a/x.glb"));
        assert_eq!(backward.canonical_path("11"), Some("meshes/a/x.glb"));
    }

    #[test]
    fn ambiguous_published_identities_are_refused() {
        for mut list in [
            // Textures that differ only in case.
            vec![("textures/T.ktx2", "11"), ("textures/t.ktx2", "11")],
            vec![("textures/T.ktx2", "11"), ("textures/t.ktx2", "22")],
            // A model path with two hashes.
            vec![("meshes/a.glb", "11"), ("meshes/a.glb", "22")],
            // A model path that differs only in case.
            vec![("meshes/a.glb", "11"), ("meshes/A.glb", "11")],
            vec![("meshes/a.glb", "11"), ("MESHES/a.GLB", "11")],
            vec![("textures/t.ktx2", "11"), ("TEXTURES/T.KTX2", "11")],
        ] {
            for _ in 0..2 {
                assert!(
                    ModelCatalog::new(Path::new("."), pairs(&list)).is_err(),
                    "{list:?}"
                );
                list.reverse();
            }
        }
    }
}

#[cfg(test)]
mod runtime_alias_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SOURCE: &str = "textures/a.ktx2";
    const BYTES: &[u8] = b"invented canonical KTX2 bytes for hashing, not image decode";
    const ALIAS: &str = "textures/a.opensky-srgb.opensky-wrap1.ktx2";
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Files(PathBuf);
    impl Files {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mudcrab-runtime-alias-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::write(
                path.join("OWNER.txt"),
                b"owner: shared runtime texture alias tests\n",
            )
            .unwrap();
            Self(path)
        }
        fn write(&self, path: &str, bytes: &[u8]) -> String {
            let target = self.0.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, bytes).unwrap();
            hash_file(&target, None).unwrap()
        }
        fn catalog(&self, published: Vec<(String, String)>) -> ModelCatalog {
            ModelCatalog::new(&self.0, published).unwrap()
        }
    }
    impl Drop for Files {
        fn drop(&mut self) {
            // Remove only this create_dir-owned test root, never another path.
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
            assert!(
                self.0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mudcrab-runtime-alias-")
            );
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn one_source(hash: &str) -> Vec<(String, String)> {
        vec![(SOURCE.to_owned(), hash.to_owned())]
    }

    #[test]
    fn concurrent_texture_requests_hash_the_same_full_file_once() {
        let files = Files::new();
        let bytes = vec![0x5a; 2 * 1024 * 1024];
        let hash = files.write(SOURCE, &bytes);
        let catalog = files.catalog(one_source(&hash));
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let catalog = &catalog;
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    assert_eq!(catalog.texture_problem(SOURCE), None);
                });
            }
        });
        assert_eq!(catalog.texture_computations.load(Ordering::Relaxed), 1);
        assert_eq!(catalog.verified_textures.lock().unwrap().len(), 1);
        assert_eq!(catalog.texture_reads.attempts.load(Ordering::Relaxed), 1);
        assert_eq!(
            catalog.texture_reads.bytes.load(Ordering::Relaxed),
            bytes.len() as u64
        );
    }

    #[test]
    fn concurrent_aliases_and_source_hash_each_required_full_file_once() {
        let files = Files::new();
        let bytes = vec![0x37; 2 * 1024 * 1024];
        let hash = files.write(SOURCE, &bytes);
        let other_alias = "textures/a.opensky-wrap2.ktx2";
        files.write(ALIAS, &bytes);
        files.write(other_alias, &bytes);
        let catalog = files.catalog(one_source(&hash));
        let paths = [SOURCE, ALIAS, other_alias];
        let start = std::sync::Barrier::new(12);
        std::thread::scope(|scope| {
            for index in 0..12 {
                let catalog = &catalog;
                let start = &start;
                let path = paths[index % paths.len()];
                scope.spawn(move || {
                    start.wait();
                    assert_eq!(catalog.texture_problem(path), None);
                });
            }
        });
        assert_eq!(catalog.texture_computations.load(Ordering::Relaxed), 3);
        assert_eq!(catalog.verified_textures.lock().unwrap().len(), 3);
        assert_eq!(catalog.texture_reads.attempts.load(Ordering::Relaxed), 3);
        assert_eq!(
            catalog.texture_reads.bytes.load(Ordering::Relaxed),
            3 * bytes.len() as u64
        );
    }

    #[test]
    fn a_fresh_catalog_rechecks_changed_alias_or_source_bytes() {
        for changed in [SOURCE, ALIAS] {
            let files = Files::new();
            let hash = files.write(SOURCE, BYTES);
            files.write(ALIAS, BYTES);
            let catalog = files.catalog(one_source(&hash));
            assert_eq!(catalog.texture_problem(ALIAS), None);
            files.write(changed, b"changed after the first launch's verification");
            // Preserve the existing within-launch cache, never carry it into a fresh catalog.
            assert_eq!(catalog.texture_problem(SOURCE), None);
            assert_eq!(catalog.texture_problem(ALIAS), None);
            assert_eq!(catalog.texture_reads.attempts.load(Ordering::Relaxed), 2);
            let fresh = files.catalog(one_source(&hash));
            assert_eq!(
                fresh.texture_problem(ALIAS),
                Some(REASON_TEXTURE_CONTENT_CHANGED)
            );
            // Source failure still short-circuits the alias read; an alias never vouches for it.
            assert_eq!(
                fresh.texture_reads.attempts.load(Ordering::Relaxed),
                if changed == SOURCE { 1 } else { 2 }
            );
        }
    }

    #[test]
    fn every_reserved_transfer_sampler_combination_verifies_both_files() {
        let files = Files::new();
        let hash = files.write(SOURCE, BYTES);
        let catalog = files.catalog(one_source(&hash));
        for transfer in ["", ".opensky-srgb"] {
            for sampler in ["", ".opensky-wrap0", ".opensky-wrap1", ".opensky-wrap2"] {
                let path = format!("textures/a{transfer}{sampler}.ktx2");
                files.write(&path, BYTES);
                assert_eq!(
                    runtime_texture_source(&path).as_deref(),
                    Some(SOURCE),
                    "{path}"
                );
                assert_eq!(catalog.texture_hash(&path), Some(hash.as_str()), "{path}");
                assert_eq!(catalog.texture_problem(&path), None, "{path}");
            }
        }
    }

    #[test]
    fn aliases_require_manifest_source_and_both_current_files() {
        for failure in [
            "alias_missing",
            "source_missing",
            "unpublished_source",
            "alias_changed",
            "source_changed",
        ] {
            let files = Files::new();
            let hash = files.write(SOURCE, BYTES);
            files.write(ALIAS, BYTES);
            let expected = match failure {
                "alias_missing" => {
                    fs::remove_file(files.0.join(ALIAS)).unwrap();
                    REASON_TEXTURE_NOT_LOADED
                }
                "source_missing" => {
                    fs::remove_file(files.0.join(SOURCE)).unwrap();
                    REASON_TEXTURE_NOT_LOADED
                }
                "unpublished_source" => REASON_TEXTURE_NOT_CONVERTED,
                "alias_changed" => {
                    files.write(ALIAS, b"changed only the runtime alias");
                    REASON_TEXTURE_CONTENT_CHANGED
                }
                "source_changed" => {
                    files.write(SOURCE, b"changed only the canonical source");
                    REASON_TEXTURE_CONTENT_CHANGED
                }
                _ => unreachable!(),
            };
            let published = if failure == "unpublished_source" {
                Vec::new()
            } else {
                one_source(&hash)
            };
            let catalog = files.catalog(published);
            assert_eq!(catalog.texture_problem(ALIAS), Some(expected), "{failure}");
        }
    }

    #[test]
    fn unknown_suffix_and_wrong_suffix_order_do_not_borrow_plain_source_hash() {
        let files = Files::new();
        let hash = files.write(SOURCE, BYTES);
        for path in [
            "textures/a.opensky-wrap3.ktx2",
            "textures/a.opensky-linear.ktx2",
            "textures/a.opensky-wrap1.opensky-srgb.ktx2",
            "textures/a.opensky-srgb.opensky-wrap1.opensky-wrap2.ktx2",
            "textures/a.opensky-srgb.KTX2",
        ] {
            files.write(path, BYTES);
            let catalog = files.catalog(one_source(&hash));
            assert_eq!(catalog.texture_hash(path), None, "{path}");
            assert_eq!(
                catalog.texture_problem(path),
                Some(REASON_TEXTURE_NOT_CONVERTED),
                "{path}"
            );
        }
    }

    #[test]
    fn explicitly_published_alias_hash_has_precedence_over_canonical_fallback() {
        let files = Files::new();
        let canonical = files.write(SOURCE, BYTES);
        let explicit = files.write(ALIAS, b"independently published runtime output bytes");
        files.write(
            SOURCE,
            b"canonical source changed after its manifest was written",
        );
        let catalog = files.catalog(vec![
            (SOURCE.into(), canonical),
            (ALIAS.into(), explicit.clone()),
        ]);
        assert_eq!(catalog.texture_hash(ALIAS), Some(explicit.as_str()));
        assert_eq!(catalog.texture_problem(ALIAS), None);
        assert_eq!(
            catalog.texture_problem(SOURCE),
            Some(REASON_TEXTURE_CONTENT_CHANGED)
        );
    }

    #[test]
    fn texture_digest_binds_original_runtime_path_and_manifest_expected_hash() {
        let files = Files::new();
        let first = files.catalog(one_source(&"11".repeat(32)));
        let changed = files.catalog(one_source(&"22".repeat(32)));
        let facts = ModelFacts {
            primitives: BTreeSet::new(),
            texture_paths: BTreeSet::from([ALIAS.to_owned()]),
            materials: BTreeMap::new(),
        };
        let digest = texture_set_hash(&facts, |path| first.texture_hash(path));
        assert_ne!(
            digest,
            texture_set_hash(&facts, |path| changed.texture_hash(path))
        );
        let canonical = ModelFacts {
            texture_paths: BTreeSet::from([SOURCE.to_owned()]),
            ..facts.clone()
        };
        assert_ne!(
            digest,
            texture_set_hash(&canonical, |path| first.texture_hash(path))
        );
        let unpublished = files.catalog(Vec::new());
        assert_ne!(
            digest,
            texture_set_hash(&facts, |path| unpublished.texture_hash(path))
        );
    }

    #[test]
    fn case_aliases_in_published_runtime_or_source_paths_remain_ambiguous() {
        for (first, second) in [
            (SOURCE, "textures/A.ktx2"),
            (ALIAS, "Textures/a.opensky-srgb.opensky-wrap1.ktx2"),
        ] {
            assert!(
                ModelCatalog::new(
                    Path::new("."),
                    vec![
                        (first.to_owned(), "11".repeat(32)),
                        (second.to_owned(), "11".repeat(32)),
                    ]
                )
                .is_err()
            );
        }
    }

    #[test]
    fn real_catalog_read_accepts_verified_alias_and_retains_runtime_digest_path() {
        let files = Files::new();
        let texture_hash = files.write(SOURCE, BYTES);
        files.write(ALIAS, BYTES);
        let document = serde_json::json!({
            "asset": {"version":"2.0"}, "scene":0,
            "scenes":[{"nodes":[0]}], "nodes":[{"mesh":0}],
            "meshes":[{"primitives":[{"material":0}]}],
            "materials":[{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}],
            "textures":[{"source":0}],
            "images":[{"uri":"../textures/a.opensky-srgb.opensky-wrap1.ktx2"}],
        });
        let mut json = serde_json::to_vec(&document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(20u32 + json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        let model_hash = files.write("meshes/alias.glb", &glb);
        let catalog = files.catalog(vec![
            (SOURCE.into(), texture_hash),
            ("meshes/alias.glb".into(), model_hash),
        ]);
        let info = catalog.info("meshes/alias.glb");
        assert_eq!(info.problem, None);
        let facts = info.facts.as_ref().unwrap();
        assert_eq!(facts.primitives, BTreeSet::from([0]));
        assert_eq!(facts.texture_paths, BTreeSet::from([ALIAS.to_owned()]));
        assert_eq!(
            info.texture_hash.as_ref(),
            Some(&texture_set_hash(facts, |path| catalog.texture_hash(path)))
        );
    }
}
