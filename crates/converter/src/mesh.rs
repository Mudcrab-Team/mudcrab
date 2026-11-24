use crate::collision;
use crate::material::{
    NifMaterialDisposition, NifShapeMaterial, build_nif_material_contract, publish_gltf_materials,
};
use crate::texture::TextureSemantic;
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use log::warn;
use project_wormhole_esm::structs::strings::{SizedString8, SizedString32, StringN};
use project_wormhole_nif::{
    nif_block::{NiObjectNETTraits, NifBlock},
    nif_file::{NifFile, nif_to_model, nif_to_model_with_skin_fallback, nif_to_static_model},
    nif_header::{Endianess, NifFileVersion, NifHeader},
};
use serde::{Deserialize, Serialize};
use shared::collision::CollisionAsset;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{Read, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use walkdir::WalkDir;

pub(crate) mod room_bounds;

pub struct MeshConverter;

pub(crate) struct NifSnapshot {
    path: PathBuf,
    bytes: Vec<u8>,
    hash: OnceLock<String>,
}

impl NifSnapshot {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let bytes =
            fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            bytes,
            hash: OnceLock::new(),
        })
    }

    pub(crate) fn hash(&self) -> &str {
        self.hash
            .get_or_init(|| crate::cache::hash_bytes(&self.bytes))
    }

    pub(crate) fn is_geometry_template(&self) -> Result<bool> {
        self.parse().map(|(nif, _, _)| is_geometry_template(&nif))
    }

    fn parse(&self) -> Result<(NifFile, NifParseDiagnostics, Vec<NifShapeMaterial>)> {
        parse_nif_for_conversion(&self.path, &self.bytes)
    }
}

/// Distinguishes rigid unresolved-skin output from a self-contained rig's skins.
pub(crate) const UNRESOLVED_SKIN_HASH_SUFFIX: &str = ":skin-resolution:unresolved";

#[derive(Debug, Clone, Default, Serialize)]
pub struct NifParseDiagnostics {
    pub block_count: usize,
    pub parsed_block_count: usize,
    pub geometry_block_count: usize,
    pub scene_node_count: usize,
    /// Longest parsed scene-node chain; zero for a cycle (see conversion refusals).
    /// Geometry and other non-node blocks contribute zero.
    pub max_scene_depth: usize,
    /// Bounded reasons conversion refuses this file while inspection retains its measurements.
    pub conversion_refusals: Vec<String>,
    /// Bounded warnings about recoverable input during conversion.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conversion_warnings: Vec<String>,
    pub block_types: BTreeMap<String, usize>,
    pub fallback_blocks: BTreeMap<String, usize>,
    pub fallback_offsets: BTreeMap<String, Vec<usize>>,
    /// Inspected material shapes; zero when material inspection is unavailable (see refusals).
    pub material_shape_count: usize,
    pub validated_material_shape_count: usize,
    pub excluded_material_shape_count: usize,
    pub material_exclusions: BTreeMap<String, usize>,
    pub unparsed_regions: usize,
    pub unparsed_bytes: usize,
    pub unparsed_by_reason: BTreeMap<String, usize>,
    /// At most eight regions, in physical file order. Counts above remain exact.
    pub unparsed_samples: Vec<NifUnparsedRegion>,
}

/// Exact source coordinates make ignored tails and fallback blocks recoverable from the
/// separately hashed input. Opaque bytes are not evidence of decoded or rendered support.
#[derive(Debug, Clone, Serialize)]
pub struct NifUnparsedRegion {
    pub reason: &'static str,
    pub block_index: Option<usize>,
    pub block_type: Option<String>,
    pub file_offset: usize,
    pub byte_length: usize,
    pub sha256: String,
}

impl NifParseDiagnostics {
    fn record_unparsed(
        &mut self,
        reason: &'static str,
        block: Option<(usize, &str)>,
        file_offset: usize,
        bytes: &[u8],
    ) {
        self.unparsed_regions += 1;
        self.unparsed_bytes += bytes.len();
        *self.unparsed_by_reason.entry(reason.into()).or_default() += 1;
        if self.unparsed_samples.len() < 8 {
            self.unparsed_samples.push(NifUnparsedRegion {
                reason,
                block_index: block.map(|(index, _)| index),
                block_type: block.map(|(_, name)| name.to_owned()),
                file_offset,
                byte_length: bytes.len(),
                sha256: crate::cache::hash_bytes(bytes),
            });
        }
    }
}

impl MeshConverter {
    /// Paths a normalized source tree must provide for the on-disk skeleton resolver.
    /// Actor rigs can search other actors' skeletons and use sibling rigs to break ties.
    pub fn skeleton_search_inputs(models: &BTreeSet<PathBuf>) -> impl Fn(&Path) -> bool {
        let candidates: BTreeSet<_> = models
            .iter()
            .flat_map(|path| skeleton_candidates(path))
            .collect();
        let mut actor_roots = BTreeSet::new();
        let mut rig_dirs = BTreeSet::new();
        for model in models {
            if let Some((root, _)) = actor_root(model) {
                actor_roots.insert(root);
                rig_dirs.extend(model.parent().map(Path::to_owned));
            }
        }
        move |path| {
            candidates.contains(path)
                || (path.extension().is_some_and(|ext| ext == "nif")
                    && (path.parent().is_some_and(|dir| rig_dirs.contains(dir))
                        || (actor_roots.iter().any(|root| path.starts_with(root))
                            && is_skeleton_key(&normalize_path_key(path)))))
        }
    }

    pub fn dependency_paths(nif_path: &Path) -> Vec<PathBuf> {
        match find_skeleton(nif_path) {
            SkeletonResolution::Skeleton(path) => vec![path],
            SkeletonResolution::SelfContained | SkeletonResolution::Unresolved { .. } => Vec::new(),
        }
    }

    /// Converts a NIF into a GLB with collision and material metadata, using static geometry as a fallback.
    pub fn convert_nif_to_glb<P: AsRef<Path>>(nif_path: P, glb_output_path: P) -> Result<()> {
        let source = NifSnapshot::read(nif_path.as_ref())?;
        Self::convert_nif_with_local_resolution(&source, glb_output_path.as_ref(), None)
    }

    /// Historical prune replay supplies its pre-repair source proof. Verify it
    /// before exporting from the same captured mesh and selected dependency.
    pub(crate) fn convert_nif_with_local_resolution(
        source: &NifSnapshot,
        glb_output_path: &Path,
        expected_source_hash: Option<&str>,
    ) -> Result<()> {
        let parsed = source.parse();
        let mut index = SkeletonIndex::default();
        let resolution = match &parsed {
            Ok((nif, diagnostics, _)) => index.resolve_local(&source.path, nif, diagnostics),
            Err(_) => SkeletonResolution::SelfContained,
        };
        if let Some(expected) = expected_source_hash {
            let mut hash = source.hash().to_owned();
            match &resolution {
                SkeletonResolution::Skeleton(entry) => {
                    hash.push(':');
                    hash.push_str(entry.snapshot()?.hash());
                }
                SkeletonResolution::Unresolved { .. } => hash.push_str(UNRESOLVED_SKIN_HASH_SUFFIX),
                SkeletonResolution::SelfContained => {}
            }
            ensure!(
                hash == expected,
                "pruned GLB source checksum mismatch: {}",
                glb_output_path.display()
            );
        }
        Self::export_nif_with_resolution(source, parsed?, glb_output_path, resolution)
    }

    /// Converts a staged model with the same logical rig search as batch conversion.
    pub fn convert_nif_to_glb_from_index(
        relative: &Path,
        nif_path: &Path,
        glb_output_path: &Path,
        index: &SkeletonIndex,
    ) -> Result<()> {
        let source = index.input_snapshot(relative, nif_path)?;
        let parsed = source.parse()?;
        let resolution = resolve_indexed_skeleton(relative, &source, &parsed.0, &parsed.1, index);
        Self::export_nif_with_resolution(&source, parsed, glb_output_path, resolution)
    }

    fn export_nif_with_resolution(
        source: &NifSnapshot,
        parsed: (NifFile, NifParseDiagnostics, Vec<NifShapeMaterial>),
        glb_output_path: &Path,
        resolution: SkeletonResolution<&SkeletonEntry>,
    ) -> Result<()> {
        let (skeleton, unresolved_skin) = match resolution {
            SkeletonResolution::Skeleton(entry) if declares_skin_type(&parsed.1) => {
                (Some(entry.snapshot()?), None)
            }
            SkeletonResolution::Unresolved { best_coverage } => (
                None,
                Some(unresolved_skeleton_diagnostic(
                    format_args!("skinned NIF {}", source.path.display()),
                    best_coverage,
                )),
            ),
            _ => (None, None),
        };
        Self::export_nif(
            source,
            parsed,
            glb_output_path,
            skeleton.as_deref(),
            unresolved_skin.as_deref(),
        )
    }

    /// Converts using an explicitly resolved winning skeleton, without physical-neighbor fallback.
    ///
    /// `None` means the mesh is a self-contained rig: the vendor builds its bone
    /// tree from the mesh's own joints. Parsed skins with unusable bindings
    /// retain rigid geometry; valid sibling skins stay skinned. An undecodable
    /// declared skin instance refuses the model before either export path.
    pub fn convert_nif_to_glb_with_skeleton(
        nif_path: &Path,
        glb_output_path: &Path,
        skeleton_path: Option<&Path>,
    ) -> Result<()> {
        let source = NifSnapshot::read(nif_path)?;
        let parsed = source.parse()?;
        let skeleton = if declares_skin_type(&parsed.1) {
            skeleton_path.map(NifSnapshot::read).transpose()?
        } else {
            None
        };
        Self::export_nif(&source, parsed, glb_output_path, skeleton.as_ref(), None)
    }

    /// Carries an unresolved resolver result to each affected shape without guessing a rig.
    pub(crate) fn convert_nif_snapshot(
        source: &NifSnapshot,
        glb_output_path: &Path,
        skeleton: Option<&NifSnapshot>,
        unresolved_skin: Option<&str>,
    ) -> Result<()> {
        Self::export_nif(
            source,
            source.parse()?,
            glb_output_path,
            skeleton,
            unresolved_skin,
        )
    }

    fn export_nif(
        source: &NifSnapshot,
        (nif, diagnostics, material_contract): (
            NifFile,
            NifParseDiagnostics,
            Vec<NifShapeMaterial>,
        ),
        glb_output_path: &Path,
        skeleton: Option<&NifSnapshot>,
        unresolved_skin: Option<&str>,
    ) -> Result<()> {
        let nif_path = &source.path;
        for warning in &diagnostics.conversion_warnings {
            eprintln!("warning: NIF {} {warning}", nif_path.display());
        }
        let mut collision = collision::from_nif_bytes(nif_path, &nif, &source.bytes)?;
        let declares_skin = declares_skin_type(&diagnostics);
        let room_sphere = if !declares_skin
            || (skeleton.is_none()
                && unresolved_skin.is_none()
                && collision::skin_pose_is_fixed(nif_path, &source.bytes, &nif.blocks)?)
        {
            room_bounds::source_sphere(&nif)
        } else {
            None
        };
        let skeleton = if declares_skin {
            match skeleton {
                Some(skeleton) => Some(skeleton.parse()?.0),
                None => None,
            }
        } else {
            None
        };
        let primary = catch_unwind(AssertUnwindSafe(|| {
            nif_to_model_with_skin_fallback(&nif, skeleton.as_ref(), unresolved_skin)
        }));
        let (mut model, mut used_static_fallback) = match primary {
            Ok(Ok((model, skin_diagnostics))) => {
                for (block, cause) in skin_diagnostics {
                    let name = material_contract
                        .iter()
                        .find(|shape| shape.shape_block == block)
                        .and_then(|shape| shape.shape_name.as_deref())
                        .unwrap_or("<unnamed>");
                    let bounded_cause: String = cause.chars().take(512).collect();
                    // The standalone CLI has no log subscriber. Keep recovery
                    // diagnostics visible in normal runs as well as library hosts.
                    eprintln!(
                        "warning: NIF {} shape block {block} ({name:?}): {bounded_cause}",
                        nif_path.display()
                    );
                }
                (
                    finish_skinned_model(&nif, nif_path, declares_skin, model, &material_contract)?,
                    false,
                )
            }
            primary => {
                let primary_error = match primary {
                    Ok(Err(error)) => format!("NIF model conversion failed: {error}"),
                    Err(_) => format!("NIF model conversion panicked for {}", nif_path.display()),
                    Ok(Ok(_)) => unreachable!(),
                };
                // Skin recovery is per shape above. A failure here is outside
                // that boundary; do not hide a broken geometry/scene export.
                ensure!(
                    !declares_skin,
                    "NIF {} declares a skin, so the static fallback is not allowed: {primary_error}",
                    nif_path.display()
                );
                let static_model = catch_unwind(AssertUnwindSafe(|| nif_to_static_model(&nif)))
                    .map_err(|_| color_eyre::eyre::eyre!("static NIF fallback panicked"))?
                    .map_err(|error| color_eyre::eyre::eyre!("static NIF fallback failed: {error}"))
                    .wrap_err(primary_error)?;
                (
                    finish_skinned_model(
                        &nif,
                        nif_path,
                        declares_skin,
                        static_model,
                        &material_contract,
                    )?,
                    true,
                )
            }
        };
        let name = nif_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if model.static_meshes.is_empty() && model.skeletal_meshes.is_empty() {
            ensure!(
                !declares_skin
                    && (is_deferred_dynamic_mesh(nif_path)
                        || is_geometry_template(&nif)
                        || !diagnostics
                            .block_types
                            .keys()
                            .any(|block_type| is_declared_geometry_block(block_type))),
                "NIF declares mesh geometry, but no supported geometry was converted"
            );
            let output = glb_output_path;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            // An empty scene has no glTF nodes, so no body can be attached to one.
            retain_bodies_with_nodes(&mut collision, &[], false);
            return write_glb_atomic(
                output,
                &embed_scene_metadata(empty_scene_glb(&name), &collision, None, None, None)?,
            );
        }
        let output = glb_output_path;
        let mut glb = catch_unwind(AssertUnwindSafe(|| model.to_glb(name.clone())))
            .map_err(|_| color_eyre::eyre::eyre!("NIF GLB export panicked"))?
            .map_err(|error| {
                color_eyre::eyre::eyre!("NIF {} GLB export failed: {error}", nif_path.display())
            })?;
        if glb_bounds_from_bytes(&glb).is_err() && !used_static_fallback {
            ensure!(
                !declares_skin,
                "NIF {} declares a skin but the exported GLB has invalid bounds; refusing the static fallback",
                nif_path.display()
            );
            let mut static_model = nif_to_static_model(&nif)
                .map_err(|error| color_eyre::eyre::eyre!("static NIF fallback failed: {error}"))?;
            static_model.scene_root_rotation =
                Some(shared::coordinates::CREATION_TO_RUNTIME_ROTATION);
            clear_root_transform(&mut static_model);
            apply_vertex_color_contract(&mut static_model, &nif, &material_contract)?;
            ensure!(
                !static_model.static_meshes.is_empty(),
                "NIF contains no supported mesh geometry"
            );
            glb = catch_unwind(AssertUnwindSafe(|| static_model.to_glb(name)))
                .map_err(|_| color_eyre::eyre::eyre!("static NIF GLB export panicked"))?
                .map_err(|error| {
                    color_eyre::eyre::eyre!(
                        "NIF {} static GLB export failed: {error}",
                        nif_path.display()
                    )
                })?;
            model = static_model;
            // The recovered mesh is a static fallback, so it carries no geometry index.
            used_static_fallback = true;
        }
        let shape_blocks = exported_shape_blocks(&nif, &model, &material_contract)?;
        let exported_material_contract = shape_blocks
            .iter()
            .map(|block| {
                material_contract
                    .iter()
                    .find(|shape| shape.shape_block == *block)
                    .cloned()
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!(
                            "exported mesh references shape block {block} without a material contract"
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let glb = rewrite_materials_and_texture_uris(
            glb,
            &exported_material_contract,
            &shape_blocks,
            output,
        )?;
        // A static fallback or a skin export does not prove the exported primitive for each source
        // shape, and an unparsed block other than Havok collision data makes the geometry count
        // unreliable. Those meshes keep their untagged form, so the engine leaves their materials
        // alone.
        let source_shapes_known =
            !declares_skin && !used_static_fallback && !has_unparsed_non_havok_block(&diagnostics);
        let glb = if source_shapes_known {
            tag_geometry_indices(glb, &exported_material_contract, &shape_blocks, &nif)?
        } else {
            glb
        };
        ensure!(
            glb.len() >= 12 && &glb[..4] == b"glTF",
            "NIF exporter produced an invalid GLB header"
        );
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        // Check the bodies against the GLB that is actually written, not the source
        // model: skeletal and effect NIFs lay their nodes out differently from the
        // static scene, so the two orders can disagree. Vanilla meshes reuse names (a
        // root and a child both called "Potato"); such a name is trusted at its index
        // only when the GLB keeps the static scene's node order, which is what the
        // index was predicted from.
        let source_order: Vec<String> = model
            .static_nodes
            .iter()
            .map(|node| node.name.clone().unwrap_or_default())
            .collect();
        retain_bodies_for_written_glb(&mut collision, &glb, Some(&source_order))?;
        let switch_geometry = nif
            .blocks
            .iter()
            .any(|block| matches!(block, NifBlock::NiSwitchNode(_)))
            .then_some((&nif, shape_blocks.as_slice()));
        let drawable_sources = source_shapes_known.then_some((
            &nif,
            model.static_meshes.as_slice(),
            shape_blocks.as_slice(),
        ));
        let glb = embed_scene_metadata(
            glb,
            &collision,
            room_sphere,
            switch_geometry,
            drawable_sources,
        )?;
        if !model.skeletal_meshes.is_empty() {
            validate_declared_skin_glb(&glb, nif_path)?;
        }
        write_glb_atomic(output, &glb)
    }

    /// Add NIF-authored collision to an already converted GLB without changing its render data.
    /// This supports upgrading a packaged world without repeating texture conversion.
    pub fn annotate_glb_collision(nif_path: &Path, glb_path: &Path) -> Result<CollisionAsset> {
        let (nif, _, _) = open_nif_for_conversion(nif_path)?;
        let mut collision = collision::from_nif(nif_path, &nif)?;
        let glb = fs::read(glb_path)?;
        if collision::gltf_node_index(&nif, NIF_ROOT_BLOCK as usize).is_ok_and(|node| node == 0) {
            ensure_untransformed_root_node(&glb)
                .wrap_err_with(|| format!("cannot annotate {}", glb_path.display()))?;
        }
        retain_bodies_for_written_glb(&mut collision, &glb, None)?;
        write_glb_atomic(
            glb_path,
            &embed_scene_metadata(glb, &collision, None, None, None)?,
        )?;
        Ok(collision)
    }

    /// The collision a NIF authors, without writing a GLB (used by the physics census example).
    pub fn extract_collision(nif_path: &Path) -> Result<CollisionAsset> {
        let (nif, _, _) = open_nif_for_conversion(nif_path)?;
        collision::from_nif(nif_path, &nif)
    }

    /// Inspects parsed blocks and topology even when conversion refuses the file.
    pub fn inspect_nif(path: &Path) -> Result<NifParseDiagnostics> {
        open_nif_resilient(path).map(|(_, diagnostics, _)| diagnostics)
    }

    /// Parses NIF bytes for inspection and returns the blocks with their diagnostics. Unknown
    /// block types stay [`NifBlock::Unhandled`] and are counted in the diagnostics; `label` only
    /// names the source in errors. Used by the retail LOD reader, which reads archive bytes.
    pub fn parse_nif_bytes(bytes: &[u8], label: &Path) -> Result<(NifFile, NifParseDiagnostics)> {
        open_nif_resilient_bytes(bytes, label).map(|(nif, diagnostics, _)| (nif, diagnostics))
    }

    /// Checks source eligibility before reusing an existing converted artifact.
    pub fn validate_nif_for_conversion(path: &Path) -> Result<()> {
        open_nif_for_conversion(path).map(|_| ())
    }

    /// Loads a NIF and reports whether its declared geometry consists only of empty overlay templates.
    pub fn is_geometry_template(path: &Path) -> Result<bool> {
        NifSnapshot::read(path)?.is_geometry_template()
    }

    /// Extracts the validated per-shape NIF material contract without
    /// publishing glTF/PBR decisions that belong to the next pipeline stage.
    pub fn inspect_nif_materials(path: &Path) -> Result<Vec<NifShapeMaterial>> {
        open_nif_for_conversion(path).map(|(_, _, contract)| contract)
    }

    /// Reads the accessor bounds written to a GLB and applies the complete glTF
    /// node hierarchy. This avoids loading vertex buffers merely to build the
    /// runtime spatial index.
    pub fn glb_bounds(path: &Path) -> Result<shared::Bounds3> {
        Self::drawable_bounds(path)?
            .ok_or_else(|| color_eyre::eyre::eyre!("GLB contains no bounded POSITION accessor"))
    }

    /// A switch with no active geometry is a valid asset without drawable bounds.
    pub fn drawable_bounds(path: &Path) -> Result<Option<shared::Bounds3>> {
        let bytes =
            fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
        drawable_bounds(&glb_json_from_bytes(&bytes)?)
            .wrap_err_with(|| format!("failed to extract bounds from {}", path.display()))
    }

    /// Returns every external image URI referenced by a GLB. Embedded images
    /// have no URI and are intentionally omitted.
    pub fn glb_texture_uris(path: &Path) -> Result<Vec<String>> {
        let bytes =
            fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let document = glb_json_from_bytes(&bytes)
            .wrap_err_with(|| format!("failed to inspect textures in {}", path.display()))?;
        let mut uris = document
            .get("images")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|image| image.get("uri").and_then(serde_json::Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        uris.sort_by_key(|uri| uri.to_ascii_lowercase());
        uris.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        Ok(uris)
    }

    /// Returns material-aware external texture dependencies. Base-color
    /// textures are mandatory; auxiliary maps remain explicitly optional until
    /// the shader contract requires them.
    pub fn glb_texture_dependencies(path: &Path) -> Result<Vec<TextureDependency>> {
        let mut file = fs::File::open(path)?;
        let mut header = [0u8; 20];
        file.read_exact(&mut header)?;
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let json_length = u32::from_le_bytes(header[12..16].try_into().unwrap());
        ensure!(
            &header[..4] == b"glTF"
                && &header[16..20] == b"JSON"
                && u32::from_le_bytes(header[4..8].try_into().unwrap()) == 2
                && u64::from(length) == file.metadata()?.len()
                && u64::from(json_length) + 20 <= u64::from(length),
            "invalid GLB JSON chunk in {}",
            path.display()
        );
        // Dependency scans need JSON only, not potentially gigabytes of geometry.
        let document =
            serde_json::from_reader(std::io::BufReader::new(file.take(u64::from(json_length))))
                .wrap_err_with(|| format!("failed to inspect textures in {}", path.display()))?;
        Ok(texture_dependencies(&document))
    }

    /// Repairs the scoped volcanic normal path, then removes image URIs with no file on disk.
    ///
    /// NIF sources occasionally reference textures Bethesda never shipped.
    /// Those references survive material publishing as dangling URIs, which
    /// fail strict runtime validation. Pruning drops the missing images along
    /// with every texture and core or OPEN_SKYRIM material slot that points at
    /// them, so the mesh renders with its remaining maps instead of failing to
    /// load. Files without dangling URIs are left untouched. Returns one
    /// report per rewritten file, ordered by path. The two shipped volcanic grass models may
    /// retain their normal through the published DLC02 replacement before pruning.
    pub fn prune_dangling_texture_uris(root: &Path) -> Result<Vec<PrunedGlb>> {
        Self::prune_dangling_texture_uris_with_sources(root, &BTreeSet::new())
    }

    /// [`Self::prune_dangling_texture_uris`], with the canonical source texture keys under
    /// `root/vfs` supplied.
    ///
    /// A missing converted artifact whose source key is in `source_textures` is not pruned: the
    /// game data contains the texture and only its publication failed, so dropping the reference
    /// would misrecord a conversion failure as absent source data. `source_textures` holds keys
    /// as [`crate::asset_path::canonical_asset_path`] produces them, for example
    /// `textures/rock01.dds`.
    pub fn prune_dangling_texture_uris_with_sources(
        root: &Path,
        source_textures: &BTreeSet<String>,
    ) -> Result<Vec<PrunedGlb>> {
        let checkpoint = root.join(crate::lod::retail_terrain_import::CHECKPOINT_DIR);
        let mut glbs: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.path() != checkpoint)
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("glb"))
            })
            .map(|entry| entry.into_path())
            .collect();
        glbs.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
        let mut pruned = Vec::new();
        for glb_path in glbs {
            if let Some(report) = Self::prune_glb_texture_uris(root, &glb_path, source_textures)? {
                pruned.push(report);
            }
        }
        Ok(pruned)
    }

    /// Apply conversion texture repair and pruning to a single regenerated GLB.
    pub fn prune_glb_texture_uris(
        root: &Path,
        glb: &Path,
        sources: &BTreeSet<String>,
    ) -> Result<Option<PrunedGlb>> {
        prune_dangling_uris_in_glb(root, glb, sources, false)
    }

    /// Repairs the two shipped volcanic grass normals after DDS conversion, before publishing
    /// sampler aliases. A source at the original path always wins, even if its conversion failed.
    pub(crate) fn repair_volcanic_grass_normals(
        root: &Path,
        source_textures: &BTreeSet<String>,
    ) -> Result<Vec<PrunedGlb>> {
        let mut repaired = Vec::new();
        for model in VOLCANIC_GRASS_MODELS {
            let glb = root.join(format!("{model}.glb"));
            if glb.is_file()
                && let Some(report) = prune_dangling_uris_in_glb(root, &glb, source_textures, true)?
            {
                repaired.push(report);
            }
        }
        Ok(repaired)
    }
}

const VOLCANIC_GRASS_MODELS: [&str; 2] = [
    "meshes/landscape/grass/dlc02volcanicashgrass01",
    "meshes/landscape/grass/dlc02volcanicashgrass02",
];
const VOLCANIC_NORMAL: &str = "textures/landscape/grass/volcanicashgrass02_n.ktx2";
const VOLCANIC_NORMAL_TARGET: &str = "textures/dlc02/landscape/grass/volcanicashgrass02_n.ktx2";
const VOLCANIC_NORMAL_SOURCE: &str = "textures/landscape/grass/volcanicashgrass02_n.dds";
const VOLCANIC_NORMAL_TARGET_SOURCE: &str =
    "textures/dlc02/landscape/grass/volcanicashgrass02_n.dds";

/// Whether a canonical mesh key belongs to the two shipped models affected by issue #86.
fn volcanic_grass_model(model: &str) -> bool {
    model
        .strip_suffix(".glb")
        .or_else(|| model.strip_suffix(".nif"))
        .is_some_and(|stem| VOLCANIC_GRASS_MODELS.contains(&stem))
}

/// Replaces only the authored missing normal path, retaining sampler and transfer suffixes.
pub(crate) fn volcanic_normal_replacement(
    model: &str,
    runtime_key: &str,
    sources: &BTreeSet<String>,
) -> Option<String> {
    if !volcanic_grass_model(model)
        || sources.contains(VOLCANIC_NORMAL_SOURCE)
        || crate::asset_path::runtime_texture_source(runtime_key).as_deref()
            != Some(VOLCANIC_NORMAL)
    {
        return None;
    }
    Some(format!(
        "textures/dlc02/{}",
        runtime_key.strip_prefix("textures/")?
    ))
}

/// Invalidates only the two affected meshes when this repair or either source presence changes.
pub(crate) fn volcanic_normal_cache_suffix(
    model: &str,
    sources: &BTreeSet<String>,
) -> Option<String> {
    volcanic_grass_model(model).then(|| {
        format!(
            ":volcanic-normal-repair:1:original={}:target={}",
            sources.contains(VOLCANIC_NORMAL_SOURCE),
            sources.contains(VOLCANIC_NORMAL_TARGET_SOURCE)
        )
    })
}

/// One GLB rewritten by scoped texture-path repair or dangling-reference pruning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrunedGlb {
    /// GLB path relative to the pruned root, with `/` separators.
    pub glb: String,
    /// Removed image URIs exactly as they appeared in the document.
    pub removed_uris: Vec<String>,
    /// Original image URIs mapped to repaired URIs; all texture uses retain their indices.
    pub repaired_uris: BTreeMap<String, String>,
}

/// Repairs scoped texture paths in the pipeline wrapper, then optionally prunes missing images.
/// The bytes-level pruning helpers remain repair-free for legacy GLB replay during verified reuse.
fn prune_dangling_uris_in_glb(
    root: &Path,
    glb_path: &Path,
    source_textures: &BTreeSet<String>,
    repair_only: bool,
) -> Result<Option<PrunedGlb>> {
    let bytes =
        fs::read(glb_path).wrap_err_with(|| format!("failed to read {}", glb_path.display()))?;
    let relative = glb_path
        .strip_prefix(root)
        .unwrap_or(glb_path)
        .to_string_lossy()
        .replace('\\', "/");
    // The repair needs JSON inspection only for the two allowlisted models. Other meshes
    // retain the bytes helper's single parse, including full-install directory scans.
    let (repaired, repaired_uris) = if volcanic_grass_model(&relative) {
        let mut document = glb_json_from_bytes(&bytes)
            .wrap_err_with(|| format!("failed to inspect textures in {}", glb_path.display()))?;
        let repairs = repair_volcanic_grass_normal(root, glb_path, &mut document, source_textures);
        let repaired = if repairs.is_empty() {
            bytes
        } else {
            rebuild_glb_with_document(&bytes, &document)
                .wrap_err_with(|| format!("failed to rebuild {}", glb_path.display()))?
        };
        (repaired, repairs)
    } else {
        (bytes, BTreeMap::new())
    };
    // A repair-only pass preserves every image, texture use and sampler. In particular, it
    // never remaps indices through prune_document_images when no references were removed.
    let pruned = if repair_only {
        None
    } else {
        prune_glb_texture_bytes_with_sources(root, glb_path, &repaired, source_textures)?
    };
    let (rewritten, removed_uris) = match pruned {
        Some(pruned) => pruned,
        None if !repaired_uris.is_empty() => (repaired, Vec::new()),
        None => return Ok(None),
    };
    write_glb_atomic(glb_path, &rewritten)?;
    Ok(Some(PrunedGlb {
        glb: relative,
        removed_uris,
        repaired_uris,
    }))
}

pub(crate) fn prune_glb_texture_bytes(
    root: &Path,
    glb_path: &Path,
    bytes: &[u8],
) -> Result<Option<(Vec<u8>, Vec<String>)>> {
    prune_glb_texture_bytes_with_sources(root, glb_path, bytes, &BTreeSet::new())
}

fn prune_glb_texture_bytes_with_sources(
    root: &Path,
    glb_path: &Path,
    bytes: &[u8],
    source_textures: &BTreeSet<String>,
) -> Result<Option<(Vec<u8>, Vec<String>)>> {
    let mut document = glb_json_from_bytes(bytes)
        .wrap_err_with(|| format!("failed to inspect textures in {}", glb_path.display()))?;
    let missing: Vec<(usize, String)> = document
        .get("images")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, image)| {
            image
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .map(|uri| (index, uri.to_owned()))
        })
        .filter(|(_, uri)| {
            !texture_uri_resolves(root, glb_path, uri)
                && !texture_source_exists(root, glb_path, uri, source_textures)
        })
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    let removed: HashSet<usize> = missing.iter().map(|(index, _)| *index).collect();
    prune_document_images(&mut document, &removed);
    let pruned = rebuild_glb_with_document(bytes, &document)
        .wrap_err_with(|| format!("failed to rebuild {}", glb_path.display()))?;
    Ok(Some((
        pruned,
        missing.into_iter().map(|(_, uri)| uri).collect(),
    )))
}

/// Repairs normal images only when the authored source is absent and the target published.
fn repair_volcanic_grass_normal(
    root: &Path,
    glb: &Path,
    document: &mut serde_json::Value,
    sources: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let mut repairs = BTreeMap::new();
    let Ok(relative) = glb.strip_prefix(root) else {
        return repairs;
    };
    let model = relative.to_string_lossy().replace('\\', "/");
    if !volcanic_grass_model(&model)
        || sources.contains(VOLCANIC_NORMAL_SOURCE)
        || root.join(VOLCANIC_NORMAL).is_file()
        || !root.join(VOLCANIC_NORMAL_TARGET).is_file()
    {
        return repairs;
    }
    let normal_images: HashSet<usize> = document["materials"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|material| material["normalTexture"]["index"].as_u64())
        .filter_map(|index| usize::try_from(index).ok())
        .filter_map(|index| document["textures"].get(index)?["source"].as_u64())
        .filter_map(|index| usize::try_from(index).ok())
        .collect();
    if let Some(images) = document["images"].as_array_mut() {
        for (index, image) in images.iter_mut().enumerate() {
            let Some(uri) = image["uri"].as_str() else {
                continue;
            };
            if !normal_images.contains(&index) || texture_uri_resolves(root, glb, uri) {
                continue;
            }
            let Ok(resolved) = crate::asset_path::resolve_asset_uri(root, glb, uri) else {
                continue;
            };
            let Ok(relative) = resolved.strip_prefix(root) else {
                continue;
            };
            let key = relative.to_string_lossy().replace('\\', "/");
            if let Some(replacement) = volcanic_normal_replacement(&model, &key, sources) {
                let replacement = format!("../../../{replacement}");
                repairs.insert(uri.to_owned(), replacement.clone());
                image["uri"] = serde_json::Value::String(replacement);
            }
        }
    }
    if !repairs.is_empty() {
        document["extras"]["mudcrab"]["texturePathRepairs"] = serde_json::json!(repairs);
    }
    repairs
}

fn texture_uri_resolves(root: &Path, glb_path: &Path, uri: &str) -> bool {
    // Embedded and remote content needs no file; only prune what claims to be
    // a local path but has nothing on disk.
    if uri.is_empty() || uri.starts_with("data:") || uri.contains("://") {
        return uri.starts_with("data:") || uri.contains("://");
    }
    if crate::asset_path::resolve_asset_uri(root, glb_path, uri)
        .is_ok_and(|candidate| candidate.is_file())
    {
        return true;
    }
    // Canonical URIs without `../` segments resolve against the tree root.
    crate::asset_path::resolve_asset_uri(root, &root.join("meshes"), uri)
        .is_ok_and(|candidate| candidate.is_file())
}

/// True when `uri`'s converted artifact is missing but its original source is present under
/// `root/vfs`, so the reference is a failed publication rather than absent game data. Only
/// converted KTX2 texture URIs can have a source here; embedded, remote and non-texture URIs
/// return false.
fn texture_source_exists(
    root: &Path,
    glb_path: &Path,
    uri: &str,
    source_textures: &BTreeSet<String>,
) -> bool {
    if source_textures.is_empty() {
        return false;
    }
    let Ok(resolved) = crate::asset_path::resolve_asset_uri(root, glb_path, uri) else {
        return false;
    };
    let Ok(relative) = resolved.strip_prefix(root) else {
        return false;
    };
    let relative = relative.to_string_lossy().replace('\\', "/");
    let Some(source) = crate::asset_path::runtime_texture_source(&relative) else {
        return false;
    };
    crate::asset_path::canonical_asset_path(&source, crate::asset_path::AssetKind::Texture, "dds")
        .is_ok_and(|key| source_textures.contains(&key))
}

fn prune_document_images(document: &mut serde_json::Value, removed: &HashSet<usize>) {
    let Some(images) = document
        .get_mut("images")
        .and_then(|images| images.as_array_mut())
    else {
        return;
    };
    let mut image_remap = vec![None; images.len()];
    let mut kept = Vec::with_capacity(images.len());
    for (index, image) in images.drain(..).enumerate() {
        if removed.contains(&index) {
            continue;
        }
        image_remap[index] = Some(kept.len());
        kept.push(image);
    }
    *images = kept;
    let mut texture_remap = Vec::new();
    if let Some(textures) = document
        .get_mut("textures")
        .and_then(|textures| textures.as_array_mut())
    {
        let mut kept = Vec::with_capacity(textures.len());
        for mut texture in textures.drain(..) {
            let remapped = texture
                .get("source")
                .and_then(serde_json::Value::as_u64)
                .and_then(|source| usize::try_from(source).ok())
                .and_then(|source| image_remap.get(source).copied().flatten());
            match remapped {
                Some(source) => {
                    texture["source"] = serde_json::Value::from(source as u64);
                    texture_remap.push(Some(kept.len()));
                    kept.push(texture);
                }
                None => {
                    texture_remap.push(None);
                }
            }
        }
        *textures = kept;
    }
    if texture_remap.iter().all(|entry| entry.is_some()) {
        return;
    }
    let Some(materials) = document
        .get_mut("materials")
        .and_then(|materials| materials.as_array_mut())
    else {
        return;
    };
    for material in materials.iter_mut() {
        let Some(object) = material.as_object_mut() else {
            continue;
        };
        if let Some(pbr) = object
            .get_mut("pbrMetallicRoughness")
            .and_then(|pbr| pbr.as_object_mut())
        {
            for slot in ["baseColorTexture", "metallicRoughnessTexture"] {
                remap_texture_info(pbr, slot, "index", &texture_remap);
            }
        }
        for slot in ["normalTexture", "occlusionTexture", "emissiveTexture"] {
            remap_texture_info(object, slot, "index", &texture_remap);
        }
        if let Some(extension) = object
            .get_mut("extensions")
            .and_then(|extensions| extensions.get_mut("KHR_materials_specular"))
            .and_then(serde_json::Value::as_object_mut)
        {
            for slot in ["specularTexture", "specularColorTexture"] {
                remap_texture_info(extension, slot, "index", &texture_remap);
            }
        }
        if let Some(slots) = object
            .get_mut("extensions")
            .and_then(|extensions| extensions.get_mut("OPEN_SKYRIM_material"))
            .and_then(|extension| extension.get_mut("textureSlots"))
            .and_then(|slots| slots.as_array_mut())
        {
            let mut kept = Vec::with_capacity(slots.len());
            for mut slot in slots.drain(..) {
                let remapped = slot
                    .get("texture")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| texture_remap.get(index).copied().flatten());
                if let Some(texture) = remapped {
                    slot["texture"] = serde_json::Value::from(texture as u64);
                    kept.push(slot);
                }
            }
            *slots = kept;
        }
    }
}

fn remap_texture_info(
    object: &mut serde_json::Map<String, serde_json::Value>,
    slot: &str,
    index_key: &str,
    texture_remap: &[Option<usize>],
) {
    let remapped = object
        .get(slot)
        .and_then(|info| info.get(index_key))
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| texture_remap.get(index).copied().flatten());
    match remapped {
        Some(index) => {
            object[slot][index_key] = serde_json::Value::from(index as u64);
        }
        None => {
            object.remove(slot);
        }
    }
}

fn rebuild_glb_with_document(original: &[u8], document: &serde_json::Value) -> Result<Vec<u8>> {
    let json_length = u32::from_le_bytes(
        original
            .get(12..16)
            .ok_or_else(|| color_eyre::eyre::eyre!("truncated GLB header"))?
            .try_into()
            .unwrap(),
    ) as usize;
    let json_end = 20usize
        .checked_add(json_length)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB JSON range overflow"))?;
    let binary = original
        .get(json_end..)
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated GLB JSON chunk"))?;
    let mut json = serde_json::to_vec(document)?;
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut glb = Vec::with_capacity(20 + json.len() + binary.len());
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&((20 + json.len() + binary.len()) as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb.extend_from_slice(binary);
    Ok(glb)
}

/// The index of the one node named `target`, or `None` when no node or more than one node has
/// that name. A repeated name cannot say which node the body was authored against.
fn unique_node_index(node_names: &[String], target: &str) -> Option<usize> {
    let mut matches = node_names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.as_str() == target);
    let (index, _) = matches.next()?;
    matches.next().is_none().then_some(index)
}

/// Keeps only the bodies whose `node` index names, in the exported GLB, the NIF node the body
/// targets. The collision reader derives the index from the NIF alone; this checks it against
/// what the exporter really wrote, and moves a body it cannot place to `skipped`. Its shapes
/// remain as fixed collision.
///
/// A body is accepted only when its target name is non-empty and the written GLB can place it:
/// at the predicted index when that node carries the name, otherwise at the index of the name
/// that appears exactly once, so a reordered or shifted export cannot attach a body to the
/// wrong node. With `require_unique_name` (annotate, or a GLB whose node order differs from
/// the NIF's static scene) the predicted index is not trusted even when it matches, and the
/// name must appear exactly once; its index is then the node's real index, so the body is
/// rebound to it instead of being skipped. The shapes already carry the baked node and body
/// transforms, so this index only binds the body to its node name.
fn retain_bodies_with_nodes(
    collision: &mut CollisionAsset,
    node_names: &[String],
    require_unique_name: bool,
) {
    let bodies = std::mem::take(&mut collision.bodies);
    for mut body in bodies {
        let index = if body.target.is_empty() {
            None
        } else if require_unique_name {
            unique_node_index(node_names, &body.target)
        } else if node_names.get(body.node as usize) == Some(&body.target) {
            Some(body.node as usize)
        } else {
            unique_node_index(node_names, &body.target)
        };
        let Some(index) = index else {
            collision.skipped.push(format!(
                "rigid body targeting {:?}: no matching glTF node at index {}",
                body.target, body.node
            ));
            continue;
        };
        let Ok(node) = u32::try_from(index) else {
            collision.skipped.push(format!(
                "rigid body targeting {:?}: glTF node index {} is out of range",
                body.target, index
            ));
            continue;
        };
        body.node = node;
        collision.bodies.push(body);
    }
}

fn embed_scene_metadata(
    glb: Vec<u8>,
    collision: &CollisionAsset,
    sphere: Option<shared::room_assignment::ModelSphere>,
    switch_geometry: Option<(&NifFile, &[u32])>,
    drawable_sources: Option<(
        &NifFile,
        &[project_wormhole_nif::model::all::StaticMesh],
        &[u32],
    )>,
) -> Result<Vec<u8>> {
    let mut document = glb_json_from_bytes(&glb)?;
    let scene = document
        .get_mut("scenes")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|scenes| scenes.first_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB has no scene for collision metadata"))?;
    scene["extras"]["mudcrabCollision"] = serde_json::to_value(collision)?;
    if let Some((nif, blocks)) = switch_geometry {
        room_bounds::select_switch_geometry(nif, blocks, &mut document)?;
    }
    if let Some((nif, meshes, blocks)) = drawable_sources {
        room_bounds::tag_drawable_spheres(nif, meshes, blocks, &mut document)?;
    }
    if let Some(sphere) = sphere
        && (document["extras"][INACTIVE_SWITCH] == true
            || room_bounds::encloses_scene(&glb, &document, sphere))
    {
        document["extras"]["mudcrabRoomSphere"] = serde_json::json!({
            "version": 1, "center": sphere.center, "radius": sphere.radius
        });
    }
    rebuild_glb_with_document(&glb, &document)
}

fn is_declared_geometry_block(block_type: &str) -> bool {
    matches!(
        block_type,
        "BSTriShape"
            | "BSDynamicTriShape"
            | "BSSubIndexTriShape"
            | "BSMeshLODTriShape"
            | "BSLODTriShape"
            | "NiTriShape"
            | "NiTriStrips"
    )
}

/// The NIF root block. Skyrim places a model by replacing this node's local transform with the
/// reference's, so the root's own translation, rotation and scale never reach the world.
/// `clutter/counterset/countercornerin01.nif` authors a 90 degree root rotation and only lines up
/// with its neighbouring counter pieces without it.
pub(crate) const NIF_ROOT_BLOCK: u32 = 0;

pub(crate) fn clear_root_transform(model: &mut project_wormhole_nif::model::all::Model) {
    for node in &mut model.static_nodes {
        if node.block_index == NIF_ROOT_BLOCK {
            node.translation = project_wormhole_shared::glam::Vec3::ZERO;
            node.rotation = project_wormhole_shared::glam::Mat3::IDENTITY;
            node.scale = 1.0;
        }
    }
}

/// Block types that bind a mesh to a skeleton.
///
/// Whether a NIF is skinned is decided from these declared header block types,
/// never from the parsed blocks: a skin block that fails to parse is replaced by
/// `NifBlock::Unhandled`, which would otherwise make a skinned mesh
/// indistinguishable from a static one.
const SKIN_INSTANCE_BLOCKS: [&str; 3] = [
    "BSSkin::Instance",
    "NiSkinInstance",
    "BSDismemberSkinInstance",
];

/// Identifies header type names whose instances carry mesh-to-skeleton bindings.
fn is_declared_skin_block(block_type: &str) -> bool {
    SKIN_INSTANCE_BLOCKS.contains(&block_type)
}

/// Whether a header type name is a Havok physics block. nif.xml names every Havok block `bhk*`,
/// except `hkPackedNiTriStripsData`, and none of them derives from `NiAVObject`: collision hangs off
/// a scene object's collision reference, never off a node's child list. Such a block is neither a
/// scene node nor a geometry leaf, so leaving it unparsed cannot change the retail 3D Index count.
fn is_havok_block(block_type: &str) -> bool {
    block_type.starts_with("bhk") || block_type == "hkPackedNiTriStripsData"
}

/// Whether any block other than Havok collision data was left unparsed. The converter does not
/// parse Havok blocks for rendering (collision decodes their raw bytes itself), so they are
/// expected fallbacks. Any other fallback may be scene data, so geometry tagging refuses it.
/// [`geometry_preorder_indices`] separately refuses any unparsed block in a child list.
fn has_unparsed_non_havok_block(diagnostics: &NifParseDiagnostics) -> bool {
    diagnostics
        .fallback_blocks
        .keys()
        .any(|block_type| !is_havok_block(block_type))
}

/// Whether the NIF header declares a skin instance block, parsed or not.
fn declares_skin_type(diagnostics: &NifParseDiagnostics) -> bool {
    diagnostics
        .block_types
        .keys()
        .any(|block_type| is_declared_skin_block(block_type))
}

/// Offsets named in a skin-parse refusal before the rest are only counted.
const MAX_REPORTED_SKIN_OFFSETS: usize = 8;

/// Characters retained when an already refused file also has an invalid material contract.
const MAX_REPORTED_MATERIAL_REFUSAL_CHARS: usize = 512;

/// Refuses a NIF whose declared skin instance block did not parse.
///
/// Rescuing an unparsable block keeps the rest of the file readable, but for a
/// skin instance the binding references are unavailable. This policy refuses
/// the model, including any usable sibling shapes, rather than silently making
/// its skin rigid. The ordinary pipeline records that asset's failure and keeps
/// converting other assets. Parsed skins still use per-shape binding recovery.
fn ensure_declared_skins_parsed(diagnostics: &NifParseDiagnostics, path: &Path) -> Result<()> {
    for (block_type, count) in &diagnostics.fallback_blocks {
        if !is_declared_skin_block(block_type) {
            continue;
        }
        let all_offsets = diagnostics
            .fallback_offsets
            .get(block_type)
            .map(Vec::as_slice)
            .unwrap_or_default();
        // A crafted file can declare up to the header's block limit, so the
        // message names the first few and counts the rest.
        let mut offsets = all_offsets
            .iter()
            .take(MAX_REPORTED_SKIN_OFFSETS)
            .map(|offset| {
                if *offset == usize::MAX {
                    "parser panicked".to_owned()
                } else {
                    format!("offset {offset}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let more = all_offsets.len().saturating_sub(MAX_REPORTED_SKIN_OFFSETS);
        if more > 0 {
            offsets.push_str(&format!(", and {more} more"));
        }
        bail!(
            "NIF {} has {count} {block_type} block(s) that could not be parsed ({offsets}); \
             refusing to convert a skinned mesh as static",
            path.display()
        );
    }
    Ok(())
}

/// Returns whether all declared geometry blocks are NiTriShape templates without geometry data.
fn is_geometry_template(nif: &NifFile) -> bool {
    let mut templates = 0;
    for (index, block) in nif.blocks.iter().enumerate() {
        if !nif
            .header
            .get_block_type(index)
            .is_ok_and(is_declared_geometry_block)
        {
            continue;
        }
        match block {
            NifBlock::NiTriShape(shape) if shape.data == u32::MAX => templates += 1,
            _ => return false,
        }
    }
    templates > 0
}

fn is_deferred_dynamic_mesh(path: &Path) -> bool {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    // Creation Club layouts nest the same dynamic categories under an extra
    // `creationclub/<mod>/` infix, so match the category segment anywhere
    // below the meshes root instead of only directly below it.
    ["/actors/", "/magic/", "/effects/"]
        .iter()
        .any(|category| normalized.contains(category))
}

fn empty_scene_glb(name: &str) -> Vec<u8> {
    let mut json = serde_json::to_vec(&serde_json::json!({
        "asset": { "version": "2.0", "generator": "Mudcrab converter" },
        "scene": 0,
        "scenes": [{ "name": name, "nodes": [] }]
    }))
    .expect("static empty-scene glTF JSON is serializable");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total_length = 20 + json.len();
    let mut glb = Vec::with_capacity(total_length);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_length as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb
}

/// Fails when an actual skinned export produces a GLB the renderer cannot bind:
/// no `skins`, a skinned node with no mesh-bound skin, or a primitive missing the
/// joint or weight attribute. This inspects the bytes actually written, not the
/// in-memory model.
fn validate_declared_skin_glb(glb: &[u8], path: &Path) -> Result<()> {
    let document = glb_json_from_bytes(glb)
        .wrap_err_with(|| format!("failed to inspect declared-skin GLB for {}", path.display()))?;
    let skins = document
        .get("skins")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    ensure!(
        !skins.is_empty(),
        "NIF {} declares a skin, but the exported GLB has no skins",
        path.display()
    );
    let nodes = document
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let meshes = document
        .get("meshes")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut skinned_meshes = BTreeSet::new();
    for node in nodes {
        if node.get("skin").is_none() {
            continue;
        }
        ensure!(
            node.get("skin")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|index| (index as usize) < skins.len()),
            "NIF {} exported a mesh with an invalid skin index",
            path.display()
        );
        let Some(mesh_index) = node.get("mesh").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        skinned_meshes.insert(mesh_index);
        let mesh = meshes.get(mesh_index as usize).ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "NIF {} declares a skin, but the exported GLB references missing mesh {mesh_index}",
                path.display()
            )
        })?;
        let primitives = mesh
            .get("primitives")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        ensure!(
            !primitives.is_empty(),
            "NIF {} declares a skin, but exported mesh {mesh_index} has no primitives",
            path.display()
        );
        for (primitive_index, primitive) in primitives.iter().enumerate() {
            let attributes = primitive
                .get("attributes")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "NIF {} declares a skin, but exported mesh {mesh_index} primitive {primitive_index} has no attributes",
                        path.display()
                    )
                })?;
            for semantic in ["JOINTS_0", "WEIGHTS_0"] {
                ensure!(
                    attributes.contains_key(semantic),
                    "NIF {} declares a skin, but exported mesh {mesh_index} primitive {primitive_index} is missing {semantic}",
                    path.display()
                );
            }
        }
    }
    ensure!(
        !skinned_meshes.is_empty(),
        "NIF {} declares a skin, but no exported mesh is bound to a skin",
        path.display()
    );
    Ok(())
}

pub(crate) fn glb_json_from_bytes(bytes: &[u8]) -> Result<serde_json::Value> {
    ensure!(
        bytes.len() >= 20 && &bytes[..4] == b"glTF",
        "invalid GLB container"
    );
    ensure!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()) == 2,
        "unsupported GLB version"
    );
    let declared_length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    ensure!(declared_length == bytes.len(), "GLB length is inconsistent");
    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    ensure!(&bytes[16..20] == b"JSON", "GLB JSON chunk is missing");
    let json_end = 20usize
        .checked_add(json_length)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB JSON range overflow"))?;
    let json = bytes
        .get(20..json_end)
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated GLB JSON chunk"))?;
    serde_json::from_slice(json).wrap_err("invalid glTF JSON")
}

/// Checks the bodies against the node names of `glb`, the GLB that is written with them.
/// `source_order` is the static-scene node order the body indices were predicted from, when
/// the GLB was built from the same NIF; a reused node name is trusted at its index only when
/// the GLB starts with exactly that order. Otherwise every target name must be unique.
fn retain_bodies_for_written_glb(
    collision: &mut CollisionAsset,
    glb: &[u8],
    source_order: Option<&[String]>,
) -> Result<()> {
    let node_names = glb_node_names(glb)?;
    let same_order =
        source_order.is_some_and(|order| !order.is_empty() && node_names.starts_with(order));
    retain_bodies_with_nodes(collision, &node_names, !same_order);
    Ok(())
}

/// A GLB written before the root-transform rule keeps the NIF root's transform on glTF node 0,
/// the root block, while collision now starts from an identity root. Annotating such a GLB
/// would part its collision from its render nodes, so it has to be reconverted instead.
fn ensure_untransformed_root_node(glb: &[u8]) -> Result<()> {
    const IDENTITY_MATRIX: [f64; 16] = [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let document = glb_json_from_bytes(glb)?;
    let Some(root) = document.get("nodes").and_then(|nodes| nodes.get(0)) else {
        return Ok(());
    };
    let is_identity = |field: &str, identity: &[f64]| {
        root.get(field).is_none_or(|value| {
            value.as_array().is_some_and(|values| {
                values.len() == identity.len()
                    && values.iter().zip(identity).all(|(value, expected)| {
                        value
                            .as_f64()
                            .is_some_and(|value| (value - expected).abs() <= 1.0e-6)
                    })
            })
        })
    };
    ensure!(
        is_identity("translation", &[0.0; 3])
            && is_identity("rotation", &[0.0, 0.0, 0.0, 1.0])
            && is_identity("scale", &[1.0; 3])
            && is_identity("matrix", &IDENTITY_MATRIX),
        "its root node keeps the NIF root transform, which collision no longer applies; \
         reconvert the model instead"
    );
    Ok(())
}

/// The `name` of every node in a GLB, in node-index order, so a body's predicted index can be
/// checked against the scene the exporter actually wrote. A node with no name contributes `""`.
fn glb_node_names(glb: &[u8]) -> Result<Vec<String>> {
    Ok(glb_json_from_bytes(glb)?
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .map(|node| node["name"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default())
}

/// Names the skin instances no exported shape claimed, together with the
/// geometry blocks that point at them. Built from the parsed blocks so a block
/// type the exporter does not handle is named instead of hidden behind a
/// generic "no skeletal mesh" message.
fn unclaimed_skin_instances(
    nif: &NifFile,
    model: &project_wormhole_nif::model::all::Model,
) -> Vec<String> {
    let claimed: BTreeSet<u32> = model
        .skeletal_meshes
        .iter()
        .filter_map(|mesh| mesh.source_block)
        .collect();

    let mut referencing: BTreeMap<u32, Vec<(u32, &'static str)>> = BTreeMap::new();
    for (index, block) in nif.blocks.iter().enumerate() {
        let (skin, block_type) = match block {
            NifBlock::BSTriShape(shape) => (shape.skin, "BSTriShape"),
            NifBlock::BSDynamicTriShape(shape) => (shape.bs_tri_shape.skin, "BSDynamicTriShape"),
            NifBlock::BSSubIndexTriShape(shape) => (shape.bs_tri_shape.skin, "BSSubIndexTriShape"),
            NifBlock::BSLODTriShape(shape) => (shape.bs_tri_shape.skin, "BSLODTriShape"),
            _ => continue,
        };
        referencing
            .entry(skin)
            .or_default()
            .push((index as u32, block_type));
    }

    let mut unclaimed = Vec::new();
    for (index, block) in nif.blocks.iter().enumerate() {
        let instance_type = match block {
            NifBlock::BSSkinInstance(_) => "BSSkin::Instance",
            NifBlock::NiSkinInstance(_) => "NiSkinInstance",
            NifBlock::BsDismemberSkinInstance(_) => "BSDismemberSkinInstance",
            _ => continue,
        };
        let index = index as u32;
        if claimed.contains(&index) {
            continue;
        }
        match referencing.get(&index) {
            Some(shapes) if !shapes.is_empty() => {
                let shapes = shapes
                    .iter()
                    .map(|(block, block_type)| format!("{block_type} {block}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                unclaimed.push(format!(
                    "skin instance {index} ({instance_type}) is referenced by {shapes}, which the legacy exporter does not handle"
                ));
            }
            _ => unclaimed.push(format!(
                "skin instance {index} ({instance_type}) is not referenced by any shape block"
            )),
        }
    }
    unclaimed
}

/// Associates rigid and skinned meshes with their original material/colour contracts.
fn exported_shape_blocks(
    nif: &NifFile,
    model: &project_wormhole_nif::model::all::Model,
    contract: &[NifShapeMaterial],
) -> Result<Vec<u32>> {
    let mut blocks = Vec::with_capacity(model.static_meshes.len() + model.skeletal_meshes.len());
    for mesh_index in 0..model.static_meshes.len() {
        let mut matches = model
            .static_nodes
            .iter()
            .filter(|node| node.mesh == Some(mesh_index));
        let node = matches.next().ok_or_else(|| {
            color_eyre::eyre::eyre!("static mesh {mesh_index} has no source shape block")
        })?;
        ensure!(
            matches.next().is_none(),
            "static mesh {mesh_index} is associated with multiple source shape blocks"
        );
        blocks.push(node.block_index);
    }
    if !model.skeletal_meshes.is_empty() {
        let source_blocks = model
            .skeletal_meshes
            .iter()
            .map(|mesh| mesh.source_block)
            .collect::<Option<Vec<_>>>();
        let skeletal_blocks = match source_blocks {
            Some(source_blocks) => {
                let mut seen = BTreeSet::new();
                for block in &source_blocks {
                    ensure!(
                        matches!(
                            nif.blocks.get(*block as usize),
                            Some(
                                NifBlock::BSTriShape(_)
                                    | NifBlock::BSDynamicTriShape(_)
                                    | NifBlock::BSSubIndexTriShape(_)
                            )
                        ),
                        "skeletal mesh source block {block} is not a skinned geometry block"
                    );
                    ensure!(
                        seen.insert(*block),
                        "two skeletal meshes claim source block {block}"
                    );
                }
                source_blocks
            }
            None => {
                let mut skeletal_blocks = Vec::new();
                let predicates: [fn(&NifBlock) -> bool; 3] = [
                    |block: &NifBlock| matches!(block, NifBlock::BSTriShape(_)),
                    |block: &NifBlock| matches!(block, NifBlock::BSDynamicTriShape(_)),
                    |block: &NifBlock| matches!(block, NifBlock::BSSubIndexTriShape(_)),
                ];
                for predicate in predicates {
                    skeletal_blocks.extend(
                        nif.blocks
                            .iter()
                            .enumerate()
                            .filter(|(_, block)| predicate(block))
                            .map(|(index, _)| u32::try_from(index))
                            .collect::<std::result::Result<Vec<_>, _>>()?,
                    );
                }
                ensure!(
                    skeletal_blocks.len() == model.skeletal_meshes.len(),
                    "skeletal mesh/material association is incomplete: {} meshes, {} source shapes",
                    model.skeletal_meshes.len(),
                    skeletal_blocks.len()
                );
                skeletal_blocks
            }
        };
        blocks.extend(skeletal_blocks);
    }
    ensure!(
        blocks.len() <= contract.len(),
        "mesh/material contract is incomplete: {} exported meshes, {} source shapes",
        blocks.len(),
        contract.len()
    );
    Ok(blocks)
}

/// Preserve only the vertex channels read by the source shader. Tree/LOD
/// lighting uses alpha for animation/fade data rather than ordinary cutout alpha.
fn vertex_color_channels(material: &crate::material::ValidatedNifMaterial) -> (bool, bool) {
    use crate::material::LightingShaderType;
    let tree_or_lod = material.shader_flags_2 & (1 << 29) != 0
        || matches!(
            material.lighting_shader_type,
            Some(
                LightingShaderType::TreeAnimation
                    | LightingShaderType::LodObjects
                    | LightingShaderType::LodObjectsHd
            )
        );
    (
        material.shader_flags_2 & (1 << 5) != 0,
        material.shader_flags_1 & (1 << 3) != 0 && !tree_or_lod,
    )
}

fn apply_vertex_color_contract(
    model: &mut project_wormhole_nif::model::all::Model,
    nif: &NifFile,
    contract: &[NifShapeMaterial],
) -> Result<()> {
    let blocks = exported_shape_blocks(nif, model, contract)?;
    let static_count = model.static_meshes.len();
    for (mesh_index, block) in blocks.iter().enumerate() {
        let shape = contract
            .iter()
            .find(|shape| shape.shape_block == *block)
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "exported mesh references shape block {block} without a material contract"
                )
            })?;
        let NifMaterialDisposition::Validated { material } = &shape.disposition else {
            continue;
        };
        let (use_rgb, use_alpha) = vertex_color_channels(material);
        let apply = |color: &mut project_wormhole_shared::prelude::BSVec4| {
            if !use_rgb {
                color.0.x = 1.0;
                color.0.y = 1.0;
                color.0.z = 1.0;
            }
            if !use_alpha {
                color.0.w = 1.0;
            }
        };
        if mesh_index < static_count {
            for color in &mut model.static_meshes[mesh_index].colors {
                apply(color);
            }
        } else if let Some(inner) = model
            .skeletal_meshes
            .get_mut(mesh_index - static_count)
            .and_then(|mesh| mesh.mesh.as_mut())
        {
            for color in &mut inner.colors {
                apply(color);
            }
        }
    }
    Ok(())
}

/// Enforces conversion eligibility before mesh export, skeleton use or collision extraction.
/// Ordinary batch callers record this model's refusal and continue with usable assets.
fn open_nif_for_conversion(
    path: &Path,
) -> Result<(NifFile, NifParseDiagnostics, Vec<NifShapeMaterial>)> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    parse_nif_for_conversion(path, &bytes)
}

fn parse_nif_for_conversion(
    path: &Path,
    bytes: &[u8],
) -> Result<(NifFile, NifParseDiagnostics, Vec<NifShapeMaterial>)> {
    let opened = open_nif_resilient_bytes(bytes, path)?;
    ensure!(
        opened.1.conversion_refusals.is_empty(),
        "{}",
        opened.1.conversion_refusals.join("; ")
    );
    Ok(opened)
}

/// Loads Skyrim NIF blocks within their declared sizes, recording parse failures as unhandled blocks.
///
/// Returns parsed blocks, diagnostics and material contracts. Conversion refusals
/// remain in diagnostics, so inspection can measure files beyond the export limit.
/// Material errors on already refused inputs become bounded reasons with zero
/// material counters. Material-only inspection failures retain their error path.
fn open_nif_resilient(
    path: &Path,
) -> Result<(NifFile, NifParseDiagnostics, Vec<NifShapeMaterial>)> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    open_nif_resilient_bytes(&bytes, path)
}

/// [`open_nif_resilient`] over bytes already in memory; `path` only labels errors.
fn open_nif_resilient_bytes(
    bytes: &[u8],
    path: &Path,
) -> Result<(NifFile, NifParseDiagnostics, Vec<NifShapeMaterial>)> {
    let (mut data, header) = parse_skyrim_header(bytes, path)?;
    let block_count = usize::try_from(header.block_count)
        .wrap_err_with(|| format!("NIF block count is out of range in {}", path.display()))?;
    ensure!(
        header.block_type_index.len() == block_count
            && header.block_size_index.len() == block_count,
        "NIF header block tables are inconsistent in {}",
        path.display()
    );
    let mut diagnostics = NifParseDiagnostics {
        block_count,
        ..Default::default()
    };
    let mut blocks = Vec::with_capacity(block_count);
    for index in 0..block_count {
        let size = usize::try_from(header.block_size_index[index])
            .wrap_err("NIF block size is out of range")?;
        ensure!(
            data.len() >= size,
            "NIF block {index} is truncated in {}",
            path.display()
        );
        let block_offset = bytes.len() - data.len();
        let (raw, remaining) = data.split_at(size);
        data = remaining;
        let block_type = header
            .get_block_type(index)
            .map_err(|_| {
                color_eyre::eyre::eyre!(
                    "NIF block {index} has an invalid type index in {}",
                    path.display()
                )
            })?
            .to_owned();
        *diagnostics
            .block_types
            .entry(block_type.clone())
            .or_default() += 1;
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            NifBlock::parse_with_version(raw, block_type.clone(), header.bethesda_version)
        }));
        let block = match parsed {
            Ok(Ok((_, NifBlock::Unhandled))) => {
                diagnostics.record_unparsed(
                    "unhandled_block",
                    Some((index, &block_type)),
                    block_offset,
                    raw,
                );
                diagnostics
                    .fallback_offsets
                    .entry(block_type.clone())
                    .or_default()
                    .push(0);
                *diagnostics.fallback_blocks.entry(block_type).or_default() += 1;
                NifBlock::Unhandled
            }
            Ok(Ok((tail, block))) => {
                if !tail.is_empty() {
                    diagnostics.record_unparsed(
                        "unparsed_block_tail",
                        Some((index, &block_type)),
                        block_offset + raw.len() - tail.len(),
                        tail,
                    );
                }
                diagnostics.parsed_block_count += 1;
                if matches!(
                    &block,
                    NifBlock::BSTriShape(_)
                        | NifBlock::BSDynamicTriShape(_)
                        | NifBlock::BSSubIndexTriShape(_)
                        | NifBlock::BSLODTriShape(_)
                        | NifBlock::NiTriShape(_)
                ) {
                    diagnostics.geometry_block_count += 1;
                }
                block
            }
            Ok(Err(error)) => {
                diagnostics.record_unparsed(
                    "block_parse_failure",
                    Some((index, &block_type)),
                    block_offset,
                    raw,
                );
                let offset = match error {
                    nom_derive::nom::Err::Error(error) | nom_derive::nom::Err::Failure(error) => {
                        raw.len().saturating_sub(error.input.len())
                    }
                    nom_derive::nom::Err::Incomplete(_) => raw.len(),
                };
                diagnostics
                    .fallback_offsets
                    .entry(block_type.clone())
                    .or_default()
                    .push(offset);
                *diagnostics.fallback_blocks.entry(block_type).or_default() += 1;
                NifBlock::Unhandled
            }
            Err(_) => {
                diagnostics.record_unparsed(
                    "block_parser_panic",
                    Some((index, &block_type)),
                    block_offset,
                    raw,
                );
                diagnostics
                    .fallback_offsets
                    .entry(block_type.clone())
                    .or_default()
                    .push(usize::MAX);
                *diagnostics.fallback_blocks.entry(block_type).or_default() += 1;
                NifBlock::Unhandled
            }
        };
        blocks.push(block);
    }
    if !data.is_empty() {
        diagnostics.record_unparsed(
            "unparsed_file_trailer",
            None,
            bytes.len() - data.len(),
            data,
        );
    }
    let mut invalid_switches = blocks.iter().enumerate().filter_map(|(index, block)| {
        let NifBlock::NiSwitchNode(switch) = block else {
            return None;
        };
        let selector = switch.active_child;
        let child_count = switch.node.children.len();
        // Negative selectors mean no active child.
        usize::try_from(selector)
            .is_ok_and(|slot| slot >= child_count)
            .then_some((index, selector, child_count))
    });
    if let Some((index, selector, child_count)) = invalid_switches.next() {
        let count = 1 + invalid_switches.count();
        diagnostics.conversion_warnings.push(format!(
            "NiSwitchNode block {index} has selector {selector} outside {child_count} child slots ({count} out-of-range selectors in this file)"
        ));
    }
    diagnostics.scene_node_count = blocks
        .iter()
        .filter(|block| {
            block.as_node().is_ok()
                || matches!(
                    block,
                    NifBlock::BSTriShape(_)
                        | NifBlock::BSDynamicTriShape(_)
                        | NifBlock::BSSubIndexTriShape(_)
                        | NifBlock::BSLODTriShape(_)
                        | NifBlock::NiTriShape(_)
                )
        })
        .count();
    if let Err(error) = ensure_declared_skins_parsed(&diagnostics, path) {
        diagnostics.conversion_refusals.push(format!("{error:#}"));
    }
    match nif_scene_depth(&blocks) {
        Ok(depth) => {
            diagnostics.max_scene_depth = depth;
            if depth > MAX_SCENE_DEPTH {
                diagnostics.conversion_refusals.push(format!(
                    "NIF {} scene graph nests {depth} scene nodes, exceeding the {MAX_SCENE_DEPTH}-node limit",
                    path.display()
                ));
            }
        }
        Err(error) => diagnostics
            .conversion_refusals
            .push(format!("invalid scene in {}: {error:#}", path.display())),
    }
    let nif = NifFile { header, blocks };
    let material_contract = match build_nif_material_contract(&nif, path) {
        Ok(contract) => contract,
        Err(error) if !diagnostics.conversion_refusals.is_empty() => {
            diagnostics.conversion_refusals.push(
                format!("material inspection unavailable: {error:#}")
                    .chars()
                    .take(MAX_REPORTED_MATERIAL_REFUSAL_CHARS)
                    .collect(),
            );
            Vec::new()
        }
        Err(error) => return Err(error),
    };
    diagnostics.material_shape_count = material_contract.len();
    for shape in &material_contract {
        match &shape.disposition {
            NifMaterialDisposition::Validated { .. } => {
                diagnostics.validated_material_shape_count += 1;
            }
            NifMaterialDisposition::Excluded { reason } => {
                diagnostics.excluded_material_shape_count += 1;
                *diagnostics
                    .material_exclusions
                    .entry(reason.clone())
                    .or_default() += 1;
            }
        }
    }
    Ok((nif, diagnostics, material_contract))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextureDependency {
    pub uri: String,
    pub semantic: TextureSemantic,
    pub required: bool,
}

/// A skinned export together with the material contract needed to publish it.
pub(crate) struct SkinnedModelBuild {
    pub model: project_wormhole_nif::model::all::Model,
    pub material_contract: Vec<NifShapeMaterial>,
}

/// [`build_skinned_model_with_materials`] for callers that need only the geometry and bones.
pub(crate) fn build_skinned_model(
    nif_path: &Path,
    skeleton_path: &Path,
) -> Result<project_wormhole_nif::model::all::Model> {
    Ok(build_skinned_model_with_materials(nif_path, skeleton_path)?.model)
}

/// Builds one part of a composed actor through the skinned export path, without
/// the static fallback that a standalone conversion may take. The material contract comes back
/// in exported mesh order, so callers that compose several NIFs can publish materials over the
/// merged mesh list.
pub(crate) fn build_skinned_model_with_materials(
    nif_path: &Path,
    skeleton_path: &Path,
) -> Result<SkinnedModelBuild> {
    let (nif, diagnostics, material_contract) = open_nif_for_conversion(nif_path)?;
    let declares_skin = declares_skin_type(&diagnostics);
    let skeleton = open_nif_for_conversion(skeleton_path)
        .wrap_err_with(|| format!("failed to open skeleton {}", skeleton_path.display()))?
        .0;
    let model = catch_unwind(AssertUnwindSafe(|| nif_to_model(&nif, Some(&skeleton))))
        .map_err(|_| {
            color_eyre::eyre::eyre!("NIF model conversion panicked for {}", nif_path.display())
        })?
        .map_err(|error| {
            color_eyre::eyre::eyre!(
                "NIF {} model conversion failed: {error}",
                nif_path.display()
            )
        })?;
    let model = finish_skinned_model(&nif, nif_path, declares_skin, model, &material_contract)?;
    let shape_blocks = exported_shape_blocks(&nif, &model, &material_contract)?;
    let exported_contract = shape_blocks
        .iter()
        .map(|block| {
            material_contract
                .iter()
                .find(|shape| shape.shape_block == *block)
                .cloned()
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "exported mesh references shape block {block} without a material contract"
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SkinnedModelBuild {
        model,
        material_contract: exported_contract,
    })
}

/// Applies the shared export contract: a declared skin must yield usable skinned
/// or rigid geometry after its instance blocks have parsed. The model must
/// validate and carry the runtime coordinate rotation and vertex-colour policy.
/// Strict composition builds its skins before reaching this shared finishing step.
fn finish_skinned_model(
    nif: &NifFile,
    nif_path: &Path,
    declares_skin: bool,
    mut model: project_wormhole_nif::model::all::Model,
    material_contract: &[NifShapeMaterial],
) -> Result<project_wormhole_nif::model::all::Model> {
    if declares_skin && model.skeletal_meshes.is_empty() && model.static_meshes.is_empty() {
        let unclaimed = unclaimed_skin_instances(nif, &model);
        let detail = if unclaimed.is_empty() {
            "no skin instance blocks are present".to_owned()
        } else {
            unclaimed.join("; ")
        };
        return Err(color_eyre::eyre::eyre!(
            "NIF {} declares a skin but produced no skeletal mesh or usable static geometry: {detail}",
            nif_path.display()
        ));
    }
    model.scene_root_rotation = Some(shared::coordinates::CREATION_TO_RUNTIME_ROTATION);
    clear_root_transform(&mut model);
    model
        .validate()
        .map_err(|error| color_eyre::eyre::eyre!("invalid converted NIF model: {error}"))?;
    apply_vertex_color_contract(&mut model, nif, material_contract)?;
    Ok(model)
}

pub(crate) fn write_glb_atomic(output: &Path, bytes: &[u8]) -> Result<()> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".mudcrab-glb-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut temporary = builder
        .tempfile_in(parent)
        .wrap_err_with(|| format!("failed to stage {}", output.display()))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    crate::cache::persist_cache_file(temporary, output)
        .wrap_err_with(|| format!("failed to publish {}", output.display()))?;
    Ok(())
}

/// Deepest parsed scene-node chain eligible for conversion.
///
/// This engineering bound protects recursive model validation, GLB bounds and
/// collision parent transforms. A 256-node chain with geometry and the export
/// wrapper is exercised on default worker stacks; Windows main/std/Rayon stack
/// measurements also cover those walks. Geometry leaves do not count. This is
/// a conversion policy rather than a NIF format maximum, including for skeletons.
///
/// This bound and the unparsable-skin refusal add no `MESH_OUTPUT_CONTRACTS` name, so the `nif`
/// settings proof is unchanged. Vanilla and Creation Club output is unchanged: 0 of 25,388
/// shared NIFs nest deeper than 64 scene nodes (the deepest is 26) and 0 of 8,954 skinned NIFs
/// have an unparsable skin (see the pull request text for #445). The only exposure is an old
/// GLB of a third-party NIF deeper than 64 or with a bad skin. A normal conversion with an
/// unchanged source hash and settings reuses it from the cache, so it stays in its pack until a
/// forced rebuild or a change to the source or settings.
pub(crate) const MAX_SCENE_DEPTH: usize = 256;

/// Deepest chain of scene nodes in the NIF's block graph.
///
/// The walk is iterative and memoises each block's depth, so it costs one visit
/// per block and per child reference: a crafted file cannot overflow the stack
/// with a long chain or take exponential time through a child list that repeats
/// a shared subgraph. Cycles have no finite depth and return an error. Finite
/// depths are measured without an export limit; conversion enforces that limit.
fn nif_scene_depth(blocks: &[NifBlock]) -> Result<usize> {
    // 0 = unvisited, PENDING = on the current path, 2 = depth known.
    const PENDING: u8 = 1;
    let mut state = vec![0u8; blocks.len()];
    let mut depths = vec![0usize; blocks.len()];
    let mut max_depth = 0;
    for root in 0..blocks.len() {
        if state[root] != 0 {
            continue;
        }
        state[root] = PENDING;
        // (block index, next child position, deepest child seen so far).
        let mut stack = vec![(root, 0usize, 0usize)];
        while let Some(&(index, next, best)) = stack.last() {
            let node = match blocks.get(index).map(NifBlock::as_node) {
                Some(Ok(node)) => Some(node),
                _ => None,
            };
            let children = node.map_or(&[][..], |node| node.children.as_slice());
            let top = stack.len() - 1;
            if let Some(child) = children.get(next) {
                stack[top].1 += 1;
                let Ok(child) = usize::try_from(*child) else {
                    continue;
                };
                if child >= blocks.len() {
                    continue;
                }
                match state[child] {
                    0 => {
                        state[child] = PENDING;
                        stack.push((child, 0, 0));
                    }
                    PENDING => bail!("NIF scene graph loops through block {child}"),
                    _ => stack[top].2 = stack[top].2.max(depths[child]),
                }
            } else {
                // Count only scene nodes: geometry and unrelated blocks do not
                // consume a slot in the node-hierarchy budget.
                let depth = if node.is_some() { 1 + best } else { 0 };
                depths[index] = depth;
                state[index] = 2;
                max_depth = max_depth.max(depth);
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.2 = parent.2.max(depth);
                }
            }
        }
    }
    Ok(max_depth)
}

/// Parses a little-endian Skyrim NIF header for Bethesda versions 83 or 100.
///
/// Returns the remaining block bytes and header, rejecting unsupported versions and excessive counts.
pub(crate) fn parse_skyrim_header<'a>(
    bytes: &'a [u8],
    path: &Path,
) -> Result<(&'a [u8], NifHeader)> {
    let mut cursor = NifCursor::new(bytes, path);
    let file_desc = cursor.line()?;
    ensure!(
        file_desc.starts_with("Gamebryo File Format"),
        "unsupported NIF signature in {}",
        path.display()
    );
    let nif_version = cursor.u32()?;
    let endian_type = cursor.u8()?;
    ensure!(
        endian_type == 1,
        "big-endian NIF is not supported in {}",
        path.display()
    );
    let user_version = cursor.u32()?;
    ensure!(
        nif_version == 0x1402_0007 && user_version == 12,
        "unsupported NIF version {nif_version:#010x}, user version {user_version} in {}",
        path.display()
    );
    let block_count = cursor.u32()?;
    ensure!(
        block_count <= 1_000_000,
        "NIF block count exceeds the safety limit in {}",
        path.display()
    );
    let bethesda_version = cursor.u32()?;
    ensure!(
        matches!(bethesda_version, 83 | 100),
        "unsupported Bethesda NIF version {bethesda_version} in {}",
        path.display()
    );
    let author = cursor.sized_string8_optional()?;
    let process_script = cursor.sized_string8_optional()?;
    let export_script = cursor.sized_string8_optional()?;
    let block_type_count = usize::from(cursor.u16()?);
    let mut block_types = Vec::with_capacity(block_type_count);
    for _ in 0..block_type_count {
        block_types.push(SizedString32(cursor.sized_string32()?));
    }
    let block_count_usize = usize::try_from(block_count).wrap_err("NIF block count overflow")?;
    let mut block_type_index = Vec::with_capacity(block_count_usize);
    for _ in 0..block_count_usize {
        block_type_index.push(cursor.u16()?);
    }
    let mut block_size_index = Vec::with_capacity(block_count_usize);
    for _ in 0..block_count_usize {
        block_size_index.push(cursor.u32()?);
    }
    let string_count = cursor.u32()?;
    ensure!(
        string_count <= 1_000_000,
        "NIF string count exceeds the safety limit in {}",
        path.display()
    );
    let string_max_size = cursor.u32()?;
    let mut strings = Vec::with_capacity(usize::try_from(string_count)?);
    for _ in 0..string_count {
        strings.push(SizedString32(cursor.sized_string32()?));
    }
    let group_count = cursor.u32()?;
    ensure!(
        group_count <= 1_000_000,
        "NIF group count exceeds the safety limit in {}",
        path.display()
    );
    let mut groups = Vec::with_capacity(usize::try_from(group_count)?);
    for _ in 0..group_count {
        groups.push(cursor.u32()?);
    }
    let remaining = &bytes[cursor.position..];
    Ok((
        remaining,
        NifHeader {
            file_desc: StringN { value: file_desc },
            nif_version: NifFileVersion(nif_version),
            endian_type: Endianess::Little,
            user_version,
            block_count,
            bethesda_version,
            author,
            process_script,
            export_script,
            max_filepath: None,
            block_types,
            block_type_index,
            block_size_index,
            string_count,
            string_max_size,
            strings,
            groups,
        },
    ))
}

struct NifCursor<'a> {
    bytes: &'a [u8],
    position: usize,
    path: &'a Path,
}

impl<'a> NifCursor<'a> {
    fn new(bytes: &'a [u8], path: &'a Path) -> Self {
        Self {
            bytes,
            position: 0,
            path,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| color_eyre::eyre::eyre!("NIF offset overflow"))?;
        ensure!(
            end <= self.bytes.len(),
            "truncated NIF header at byte {} in {}",
            self.position,
            self.path.display()
        );
        let result = &self.bytes[self.position..end];
        self.position = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn line(&mut self) -> Result<String> {
        let Some(length) = self.bytes[self.position..]
            .iter()
            .position(|byte| *byte == b'\n')
        else {
            color_eyre::eyre::bail!(
                "NIF header line is not terminated in {}",
                self.path.display()
            );
        };
        let value = String::from_utf8_lossy(self.take(length)?).into_owned();
        self.take(1)?;
        Ok(value)
    }

    fn sized_string8_optional(&mut self) -> Result<Option<SizedString8>> {
        let length = usize::from(self.u8()?);
        let value = String::from_utf8_lossy(self.take(length)?)
            .trim_end_matches('\0')
            .to_owned();
        Ok((!value.is_empty()).then_some(SizedString8(value)))
    }

    fn sized_string32(&mut self) -> Result<String> {
        let length = usize::try_from(self.u32()?).wrap_err("NIF string length overflow")?;
        ensure!(
            length <= 16 * 1024 * 1024,
            "NIF string exceeds the safety limit in {}",
            self.path.display()
        );
        Ok(String::from_utf8_lossy(self.take(length)?)
            .trim_end_matches('\0')
            .to_owned())
    }
}

const INACTIVE_SWITCH: &str = "mudcrabInactiveSwitch";

pub(crate) fn glb_bounds_from_bytes(glb: &[u8]) -> Result<shared::Bounds3> {
    ensure!(
        glb.len() >= 20 && &glb[..4] == b"glTF",
        "invalid GLB container"
    );
    let json_length = u32::from_le_bytes([glb[12], glb[13], glb[14], glb[15]]) as usize;
    ensure!(&glb[16..20] == b"JSON", "GLB JSON chunk is missing");
    let document: serde_json::Value = serde_json::from_slice(
        glb.get(20..20 + json_length)
            .ok_or_else(|| color_eyre::eyre::eyre!("truncated GLB JSON chunk"))?,
    )?;
    drawable_bounds(&document)?
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB contains no bounded POSITION accessor"))
}

pub(crate) fn drawable_bounds(document: &serde_json::Value) -> Result<Option<shared::Bounds3>> {
    drawable_scene_bounds(document, None)
}

pub(crate) fn drawable_scene_bounds(
    document: &serde_json::Value,
    scene_index: Option<usize>,
) -> Result<Option<shared::Bounds3>> {
    let nodes = document
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let roots = scene_roots(document, nodes, scene_index)?;
    let mut bounds = BoundsAccumulator::default();
    let mut visited = vec![false; nodes.len()];
    let mut remaining = nodes.len();
    let mut pending: Vec<_> = roots
        .iter()
        .copied()
        .rev()
        .map(|id| (id, Mat4::IDENTITY))
        .collect();
    while let Some((index, parent)) = pending.pop() {
        ensure!(remaining > 0, "glTF node traversal exceeds the node count");
        remaining -= 1;
        let node = nodes
            .get(index)
            .ok_or_else(|| color_eyre::eyre::eyre!("glTF node {index} is out of range"))?;
        ensure!(
            !visited[index],
            "glTF node {index} occurs more than once in the scene"
        );
        visited[index] = true;
        let transform = parent.mul(Mat4::from_node(node));
        if let Some(value) = node.get("mesh") {
            let mesh = value
                .as_u64()
                .ok_or_else(|| color_eyre::eyre::eyre!("invalid mesh index at node {index}"))?;
            bounds.visited_mesh = true;
            let mesh = usize::try_from(mesh).wrap_err("glTF mesh index is out of range")?;
            accumulate_mesh(document, mesh, transform, &mut bounds)?;
        }
        if let Some(children) = node.get("children").and_then(serde_json::Value::as_array) {
            for child in children.iter().rev().filter_map(serde_json::Value::as_u64) {
                ensure!(
                    pending.len() < remaining,
                    "glTF node traversal exceeds the node count"
                );
                let child = usize::try_from(child).wrap_err("glTF child index is out of range")?;
                pending.push((child, transform));
            }
        }
    }
    if document["extras"][INACTIVE_SWITCH].as_bool() == Some(true) {
        ensure!(
            !bounds.visited_mesh
                && !roots.is_empty()
                && nodes.iter().any(|node| node["mesh"].is_u64()),
            "inactive switch scene must have a root and only detached meshes"
        );
        return Ok(None);
    }
    bounds.finish().map(Some)
}

fn scene_roots(
    document: &serde_json::Value,
    nodes: &[serde_json::Value],
    scene_index: Option<usize>,
) -> Result<Vec<usize>> {
    let index = match scene_index {
        Some(index) => index,
        None => usize::try_from(document["scene"].as_u64().unwrap_or(0))
            .wrap_err("glTF default scene index is out of range")?,
    };
    let scene = document["scenes"].get(index);
    if scene_index.is_some() {
        ensure!(scene.is_some(), "missing glTF scene {index}");
    }
    if let Some(roots) = scene.and_then(|scene| scene["nodes"].as_array()) {
        ensure!(
            roots.len() <= nodes.len(),
            "glTF node traversal exceeds the node count"
        );
        return roots
            .iter()
            .filter_map(serde_json::Value::as_u64)
            .map(usize::try_from)
            .collect::<Result<_, _>>()
            .wrap_err("glTF root node index is out of range");
    }
    if scene_index.is_some() {
        return Ok(Vec::new());
    }
    let mut children = vec![false; nodes.len()];
    for node in nodes {
        if let Some(indices) = node.get("children").and_then(serde_json::Value::as_array) {
            for index in indices.iter().filter_map(serde_json::Value::as_u64) {
                if let Ok(index) = usize::try_from(index)
                    && let Some(child) = children.get_mut(index)
                {
                    *child = true;
                }
            }
        }
    }
    Ok(children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| (!child).then_some(index))
        .collect())
}

fn accumulate_mesh(
    document: &serde_json::Value,
    mesh_index: usize,
    transform: Mat4,
    output: &mut BoundsAccumulator,
) -> Result<()> {
    let meshes = document
        .get("meshes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB has no meshes"))?;
    let accessors = document
        .get("accessors")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB has no accessors"))?;
    let primitives = meshes
        .get(mesh_index)
        .and_then(|mesh| mesh.get("primitives"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| color_eyre::eyre::eyre!("mesh {mesh_index} has no primitives"))?;
    for primitive in primitives {
        let Some(accessor_index) = primitive
            .pointer("/attributes/POSITION")
            .and_then(serde_json::Value::as_u64)
        else {
            continue;
        };
        let accessor = accessors
            .get(accessor_index as usize)
            .ok_or_else(|| color_eyre::eyre::eyre!("POSITION accessor is out of range"))?;
        let min = json_vec3(accessor.get("min"))?;
        let max = json_vec3(accessor.get("max"))?;
        for x in [min[0], max[0]] {
            for y in [min[1], max[1]] {
                for z in [min[2], max[2]] {
                    output.include(transform.transform([x, y, z]));
                }
            }
        }
    }
    Ok(())
}

fn json_vec3(value: Option<&serde_json::Value>) -> Result<[f64; 3]> {
    let values = value
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| color_eyre::eyre::eyre!("POSITION accessor has no min/max"))?;
    ensure!(values.len() >= 3, "POSITION accessor min/max is not a vec3");
    Ok([
        values[0]
            .as_f64()
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid bound"))?,
        values[1]
            .as_f64()
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid bound"))?,
        values[2]
            .as_f64()
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid bound"))?,
    ])
}

#[derive(Clone, Copy)]
struct Mat4([f64; 16]);

impl Mat4 {
    const IDENTITY: Self = Self([
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]);

    fn from_node(node: &serde_json::Value) -> Self {
        if let Some(matrix) = node.get("matrix").and_then(serde_json::Value::as_array)
            && matrix.len() == 16
        {
            let mut output = [0.0; 16];
            for (target, source) in output.iter_mut().zip(matrix) {
                *target = source.as_f64().unwrap_or(0.0);
            }
            return Self(output);
        }
        let t = array_or(node.get("translation"), [0.0, 0.0, 0.0]);
        let r = array_or(node.get("rotation"), [0.0, 0.0, 0.0, 1.0]);
        let s = array_or(node.get("scale"), [1.0, 1.0, 1.0]);
        let [x, y, z, w] = r;
        let mut matrix = [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + z * w),
            2.0 * (x * z - y * w),
            0.0,
            2.0 * (x * y - z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + x * w),
            0.0,
            2.0 * (x * z + y * w),
            2.0 * (y * z - x * w),
            1.0 - 2.0 * (x * x + y * y),
            0.0,
            t[0],
            t[1],
            t[2],
            1.0,
        ];
        for row in 0..4 {
            matrix[row] *= s[0];
            matrix[4 + row] *= s[1];
            matrix[8 + row] *= s[2];
        }
        Self(matrix)
    }

    fn mul(self, rhs: Self) -> Self {
        let mut output = [0.0; 16];
        for column in 0..4 {
            for row in 0..4 {
                output[column * 4 + row] = (0..4)
                    .map(|axis| self.0[axis * 4 + row] * rhs.0[column * 4 + axis])
                    .sum();
            }
        }
        Self(output)
    }

    fn transform(self, point: [f64; 3]) -> [f64; 3] {
        [
            self.0[0] * point[0] + self.0[4] * point[1] + self.0[8] * point[2] + self.0[12],
            self.0[1] * point[0] + self.0[5] * point[1] + self.0[9] * point[2] + self.0[13],
            self.0[2] * point[0] + self.0[6] * point[1] + self.0[10] * point[2] + self.0[14],
        ]
    }
}

fn array_or<const N: usize>(value: Option<&serde_json::Value>, fallback: [f64; N]) -> [f64; N] {
    let Some(values) = value.and_then(serde_json::Value::as_array) else {
        return fallback;
    };
    std::array::from_fn(|index| {
        values
            .get(index)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(fallback[index])
    })
}

#[derive(Default)]
struct BoundsAccumulator {
    visited_mesh: bool,
    min: [f64; 3],
    max: [f64; 3],
    populated: bool,
}

impl BoundsAccumulator {
    fn include(&mut self, point: [f64; 3]) {
        if !self.populated {
            self.min = point;
            self.max = point;
            self.populated = true;
        } else {
            for (axis, value) in point.into_iter().enumerate() {
                self.min[axis] = self.min[axis].min(value);
                self.max[axis] = self.max[axis].max(value);
            }
        }
    }

    fn finish(self) -> Result<shared::Bounds3> {
        ensure!(self.populated, "GLB contains no bounded POSITION accessor");
        let bounds = shared::Bounds3 {
            min: self.min.map(|value| value as f32),
            max: self.max.map(|value| value as f32),
        };
        ensure!(
            bounds.is_finite_and_ordered(),
            "GLB contains invalid bounds"
        );
        Ok(bounds)
    }
}

/// Whether a block is a geometry leaf for the retail 3D Index count: the five parsed geometry
/// block kinds. Any other geometry kind parses as an unhandled block, which stops tagging.
fn is_index_geometry(block: &NifBlock) -> bool {
    matches!(
        block,
        NifBlock::BSTriShape(_)
            | NifBlock::BSDynamicTriShape(_)
            | NifBlock::BSSubIndexTriShape(_)
            | NifBlock::BSLODTriShape(_)
            | NifBlock::NiTriShape(_)
    )
}

/// The retail 3D Index of every geometry block (RF-TEX-002): a pre-order, depth-first count of
/// geometry leaves in child-array order, starting at 0 under the single scene root. Every
/// geometry counts, including shapes the converter excludes or whose shader is not lighting.
///
/// Returns `None` when the count is not provable: no single root, a block reached twice (a shared
/// subgraph, which retail would count twice), or a child that is not a parsed block.
pub(crate) fn geometry_preorder_indices(blocks: &[NifBlock]) -> Option<BTreeMap<u32, u32>> {
    let is_scene = |block: &NifBlock| block.as_node().is_ok() || is_index_geometry(block);
    let mut is_child = vec![false; blocks.len()];
    for block in blocks {
        if let Ok(node) = block.as_node() {
            for child in &node.children {
                let child = usize::try_from(*child).ok()?;
                if let Some(slot) = is_child.get_mut(child) {
                    *slot = true;
                }
            }
        }
    }
    let mut roots = (0..blocks.len()).filter(|&index| !is_child[index] && is_scene(&blocks[index]));
    let root = roots.next()?;
    if roots.next().is_some() {
        return None;
    }
    let mut indices = BTreeMap::new();
    let mut visited = vec![false; blocks.len()];
    let mut stack = vec![(root, 0usize)];
    visited[root] = true;
    let mut next_index = 0u32;
    if is_index_geometry(&blocks[root]) {
        indices.insert(root as u32, next_index);
        next_index += 1;
    }
    while let Some((block, child_at)) = stack.pop() {
        let Ok(node) = blocks[block].as_node() else {
            continue;
        };
        let Some(&child) = node.children.get(child_at) else {
            continue;
        };
        stack.push((block, child_at + 1));
        let Ok(child) = usize::try_from(child) else {
            continue;
        };
        // A null child reference is not a block; any other missing child is unprovable.
        if child >= blocks.len() {
            if node.children[child_at] == u32::MAX {
                continue;
            }
            return None;
        }
        if !is_scene(&blocks[child]) {
            return None;
        }
        if std::mem::replace(&mut visited[child], true) {
            return None;
        }
        if is_index_geometry(&blocks[child]) {
            indices.insert(child as u32, next_index);
            next_index += 1;
        } else {
            stack.push((child, 0));
        }
    }
    Some(indices)
}

/// Record each exported lighting primitive's retail 3D Index as `extras.mudcrabGeometryIndex`.
/// RF-TEX-003: only lighting-shader shapes are swapped, so only those are tagged; effect shapes,
/// excluded shapes and untextured shapes still count in the index but carry no tag.
/// The index comes from the source NIF, not from the GLB order, so it is the same number the
/// game computes (RF-TEX-002) whatever order the exporter writes meshes in. The tag holds source
/// identity only: it never carries a record's substitution.
fn tag_geometry_indices(
    glb: Vec<u8>,
    contract: &[NifShapeMaterial],
    shape_blocks: &[u32],
    nif: &NifFile,
) -> Result<Vec<u8>> {
    use crate::material::NifShaderFamily;
    let Some(indices) = geometry_preorder_indices(&nif.blocks) else {
        return Ok(glb);
    };
    let mut document = glb_json_from_bytes(&glb)?;
    let Some(meshes) = document
        .get_mut("meshes")
        .and_then(serde_json::Value::as_array_mut)
        .filter(|meshes| meshes.len() == shape_blocks.len())
    else {
        return Ok(glb);
    };
    let mut tagged = false;
    for (mesh, block) in meshes.iter_mut().zip(shape_blocks) {
        let Some(NifMaterialDisposition::Validated { material }) = contract
            .iter()
            .find(|shape| shape.shape_block == *block)
            .map(|shape| &shape.disposition)
        else {
            continue;
        };
        let (Some(index), NifShaderFamily::Lighting) = (indices.get(block), material.shader_family)
        else {
            continue;
        };
        let Some(primitives) = mesh
            .get_mut("primitives")
            .and_then(serde_json::Value::as_array_mut)
            .filter(|primitives| primitives.len() == 1)
        else {
            continue;
        };
        let Some(primitive) = primitives[0].as_object_mut() else {
            continue;
        };
        let Some(extras) = primitive
            .entry("extras")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
        else {
            continue;
        };
        extras.insert("mudcrabGeometryIndex".to_owned(), (*index).into());
        tagged = true;
    }
    if !tagged {
        return Ok(glb);
    }
    rebuild_glb_with_document(&glb, &document)
}

pub(crate) fn rewrite_materials_and_texture_uris(
    glb: Vec<u8>,
    material_contract: &[NifShapeMaterial],
    shape_blocks: &[u32],
    glb_output_path: &Path,
) -> Result<Vec<u8>> {
    ensure!(
        glb.len() >= 20 && &glb[..4] == b"glTF",
        "invalid GLB container"
    );
    let json_length = u32::from_le_bytes([glb[12], glb[13], glb[14], glb[15]]) as usize;
    ensure!(&glb[16..20] == b"JSON", "GLB JSON chunk is missing");
    let json_end = 20usize
        .checked_add(json_length)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB JSON range overflow"))?;
    let json_bytes = glb
        .get(20..json_end)
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated GLB JSON chunk"))?;
    let mut document: serde_json::Value =
        serde_json::from_slice(json_bytes).wrap_err("NIF exporter produced invalid glTF JSON")?;
    publish_gltf_materials(
        &mut document,
        material_contract,
        shape_blocks,
        glb_output_path,
    )?;
    let mut json = serde_json::to_vec(&document)?;
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    let suffix = glb
        .get(json_end..)
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid GLB suffix"))?;
    let total_length = 20usize
        .checked_add(json.len())
        .and_then(|length| length.checked_add(suffix.len()))
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB size overflow"))?;
    let mut output = Vec::with_capacity(total_length);
    output.extend_from_slice(&glb[..8]);
    let total_length = u32::try_from(total_length).wrap_err("GLB exceeds 4 GiB")?;
    let json_length = u32::try_from(json.len()).wrap_err("GLB JSON exceeds 4 GiB")?;
    output.extend_from_slice(&total_length.to_le_bytes());
    output.extend_from_slice(&json_length.to_le_bytes());
    output.extend_from_slice(b"JSON");
    output.extend_from_slice(&json);
    output.extend_from_slice(suffix);
    Ok(output)
}

fn texture_dependencies(document: &serde_json::Value) -> Vec<TextureDependency> {
    let mut dependencies = BTreeMap::<(String, TextureSemantic), TextureDependency>::new();
    if let Some(materials) = document
        .get("materials")
        .and_then(serde_json::Value::as_array)
    {
        for material in materials {
            for (pointer, semantic, required) in [
                (
                    "/pbrMetallicRoughness/baseColorTexture/index",
                    TextureSemantic::BaseColor,
                    true,
                ),
                ("/normalTexture/index", TextureSemantic::Normal, false),
                ("/emissiveTexture/index", TextureSemantic::Emissive, false),
                (
                    "/pbrMetallicRoughness/metallicRoughnessTexture/index",
                    TextureSemantic::MetallicRoughness,
                    false,
                ),
                ("/occlusionTexture/index", TextureSemantic::Occlusion, false),
                (
                    "/extensions/KHR_materials_pbrSpecularGlossiness/diffuseTexture/index",
                    TextureSemantic::BaseColor,
                    true,
                ),
                (
                    "/extensions/KHR_materials_pbrSpecularGlossiness/specularGlossinessTexture/index",
                    TextureSemantic::SpecularGlossiness,
                    false,
                ),
                (
                    "/extensions/KHR_materials_specular/specularTexture/index",
                    TextureSemantic::SpecularGlossiness,
                    false,
                ),
                (
                    "/extensions/KHR_materials_specular/specularColorTexture/index",
                    TextureSemantic::SpecularGlossiness,
                    false,
                ),
            ] {
                if let Some(index) = material
                    .pointer(pointer)
                    .and_then(serde_json::Value::as_u64)
                    && let Some(uri) = texture_uri(document, index as usize)
                {
                    dependencies.insert(
                        (uri.to_ascii_lowercase(), semantic),
                        TextureDependency {
                            uri: uri.to_owned(),
                            semantic,
                            required,
                        },
                    );
                }
            }
            if let Some(slots) = material
                .pointer("/extensions/OPEN_SKYRIM_material/textureSlots")
                .and_then(serde_json::Value::as_array)
            {
                for slot in slots {
                    let Some(index) = slot.get("texture").and_then(serde_json::Value::as_u64)
                    else {
                        continue;
                    };
                    let Some(uri) = texture_uri(document, index as usize) else {
                        continue;
                    };
                    let semantic = match slot.get("semantic").and_then(serde_json::Value::as_str) {
                        Some("height") => TextureSemantic::Height,
                        Some("detail") => TextureSemantic::Detail,
                        Some("environment_cube") => TextureSemantic::EnvironmentCube,
                        Some("environment_mask") => TextureSemantic::EnvironmentMask,
                        Some("inner_layer") => TextureSemantic::InnerLayer,
                        Some("facegen_tint") => TextureSemantic::FacegenTint,
                        Some("greyscale") => TextureSemantic::Greyscale,
                        _ => TextureSemantic::Unclassified,
                    };
                    let required = slot
                        .get("required")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    dependencies.insert(
                        (uri.to_ascii_lowercase(), semantic),
                        TextureDependency {
                            uri: uri.to_owned(),
                            semantic,
                            required,
                        },
                    );
                }
            }
        }
    }
    if let Some(images) = document.get("images").and_then(serde_json::Value::as_array) {
        for uri in images
            .iter()
            .filter_map(|image| image.get("uri").and_then(serde_json::Value::as_str))
        {
            if !dependencies
                .keys()
                .any(|(known, _)| known.eq_ignore_ascii_case(uri))
            {
                dependencies.insert(
                    (uri.to_ascii_lowercase(), TextureSemantic::Unclassified),
                    TextureDependency {
                        uri: uri.to_owned(),
                        semantic: TextureSemantic::Unclassified,
                        required: false,
                    },
                );
            }
        }
    }
    dependencies.into_values().collect()
}

fn texture_uri(document: &serde_json::Value, texture_index: usize) -> Option<&str> {
    let image_index = document
        .get("textures")?
        .get(texture_index)?
        .get("source")?
        .as_u64()? as usize;
    document
        .get("images")?
        .get(image_index)?
        .get("uri")?
        .as_str()
}

/// Below half coverage no single skeleton is credible enough to attach.
const MIN_SKELETON_COVERAGE: f32 = 0.5;

/// Diagnostic for a rig that only partial skeletons cover. [`SkeletonResolution::Unresolved`]
/// is returned above [`MIN_SKELETON_COVERAGE`], so the best indexed match is always at or above
/// that threshold and the failure is missing full coverage, not missing coverage.
pub(crate) fn unresolved_skeleton_diagnostic(
    subject: impl std::fmt::Display,
    best_coverage: f32,
) -> String {
    format!(
        "{subject} declares skin joints that no skeleton covers fully and no path candidate covers at {MIN_SKELETON_COVERAGE:.1} or more; the best indexed skeleton covers {best_coverage:.2}, which is too partial to trust"
    )
}

/// Which skeleton a skinned mesh needs, or why it needs none.
#[derive(Debug, Clone, PartialEq)]
pub enum SkeletonResolution<T = PathBuf> {
    /// A skeleton file that covers the rig.
    Skeleton(T),
    /// No skeleton covers the rig; the mesh's own hierarchy is its bone tree.
    SelfContained,
    /// Only partial indexed matches exist; export the skin-bearing geometry as rigid nodes.
    Unresolved { best_coverage: f32 },
}

impl<T> SkeletonResolution<T> {
    fn map<U>(self, select: impl FnOnce(T) -> U) -> SkeletonResolution<U> {
        match self {
            Self::Skeleton(entry) => SkeletonResolution::Skeleton(select(entry)),
            Self::SelfContained => SkeletonResolution::SelfContained,
            Self::Unresolved { best_coverage } => SkeletonResolution::Unresolved { best_coverage },
        }
    }
}

pub(crate) struct SkeletonEntry {
    /// Exact source-set key (or physical path in a standalone search).
    pub(crate) path: PathBuf,
    /// Case-folded logical path used for ranking.
    key: String,
    snapshot: Result<Arc<NifSnapshot>>,
    nodes: Option<BTreeSet<String>>,
}

impl SkeletonEntry {
    fn load(path: &Path, source: &Path) -> Self {
        let report =
            |error: &color_eyre::Report| warn!("skipping skeleton {}: {error:#}", source.display());
        let snapshot = NifSnapshot::read(source).inspect_err(report);
        let nodes = snapshot
            .as_ref()
            .ok() // silent-ok: Read and parse failures are already reported and supply no node names.
            .and_then(|snapshot| snapshot.parse().inspect_err(report).ok())
            .map(|(nif, _, _)| {
                nif.get_nodes()
                    .iter()
                    .filter_map(|node| nif.header.get_string(node.name() as usize).ok())
                    .map(str::to_owned)
                    .collect()
            });
        Self {
            path: path.to_owned(),
            key: normalize_path_key(path),
            snapshot: snapshot.map(Arc::new),
            nodes,
        }
    }

    pub(crate) fn snapshot(&self) -> Result<Arc<NifSnapshot>> {
        self.snapshot
            .as_ref()
            .map(Arc::clone)
            .map_err(|error| color_eyre::eyre::eyre!("{error:#}"))
    }

    fn covers(&self, joints: &BTreeSet<String>) -> bool {
        self.nodes
            .as_ref()
            .is_some_and(|nodes| joints.is_subset(nodes))
    }
}

#[derive(Default)]
struct DirectoryMeshes {
    paths: Vec<PathBuf>,
    // Some(index) identifies the sole source of a joint. None means siblings share it.
    joint_sources: OnceLock<BTreeMap<String, Option<usize>>>,
}

/// Captures each winning rig's bytes and coverage together. Workers share this
/// index for one batch, including the joint union captured on each directory's
/// first tie-break. The next conversion or repair builds a fresh index.
#[derive(Default)]
pub struct SkeletonIndex {
    entries: BTreeMap<PathBuf, SkeletonEntry>,
    by_key: BTreeMap<String, PathBuf>,
    skeleton_union: BTreeSet<String>,
    dir_meshes: BTreeMap<String, DirectoryMeshes>,
}

impl SkeletonIndex {
    /// Indexes winning files by logical path. Full-cover ranking uses the NIFs
    /// in the model's logical directory and skeleton candidates from the whole set.
    pub fn from_sources(sources: &BTreeMap<PathBuf, PathBuf>) -> Self {
        let mut index = Self::default();
        for (path, source) in sources {
            let key = normalize_path_key(path);
            if is_skeleton_key(&key) {
                index.insert(path, source);
            } else if key.ends_with(".nif") {
                index
                    .dir_meshes
                    .entry(key_dirname(&key).to_owned())
                    .or_default()
                    .paths
                    .push(source.clone());
            }
        }
        for directory in index.dir_meshes.values_mut() {
            directory.paths.sort_unstable();
            directory.paths.dedup();
        }
        index
    }

    /// A skeleton can also be a conversion target. Use the same bytes when
    /// processing its own row as when other meshes select it as a dependency.
    pub(crate) fn input_snapshot(
        &self,
        relative: &Path,
        source: &Path,
    ) -> Result<Arc<NifSnapshot>> {
        match self.entries.get(relative) {
            Some(entry) => entry.snapshot(),
            None => NifSnapshot::read(source).map(Arc::new),
        }
    }

    fn insert(&mut self, path: &Path, source: &Path) {
        if self.entries.contains_key(path) {
            return;
        }
        let entry = SkeletonEntry::load(path, source);
        if let Some(nodes) = &entry.nodes {
            self.skeleton_union.extend(nodes.iter().cloned());
        }
        self.by_key.insert(entry.key.clone(), path.to_owned());
        self.entries.insert(path.to_owned(), entry);
    }

    fn add_actor_tree(&mut self, nif_path: &Path) {
        let Some((root, _)) = actor_root(nif_path) else {
            return;
        };
        let mut paths: Vec<PathBuf> = WalkDir::new(&root)
            .max_depth(6)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_skeleton_key(&normalize_path_key(path)))
            .collect();
        paths.sort();
        for path in paths {
            self.insert(&path, &path);
        }
    }

    fn candidate(&self, path: &Path) -> Option<&SkeletonEntry> {
        self.entries.get(path).or_else(|| {
            self.entries
                .get(self.by_key.get(&normalize_path_key(path))?)
        })
    }

    fn joint_union(&self, paths: impl IntoIterator<Item = impl AsRef<Path>>) -> BTreeSet<String> {
        let mut union = BTreeSet::new();
        for path in paths {
            if let Ok((nif, _, _)) = open_nif_for_conversion(path.as_ref()) {
                union.extend(
                    nif.legacy_skin_joint_names()
                        .intersection(&self.skeleton_union)
                        .cloned(),
                );
            }
        }
        union
    }

    fn directory_union(
        &self,
        mesh_dir: &str,
        mesh_source: &Path,
        mesh_joints: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let mut union: BTreeSet<_> = mesh_joints
            .intersection(&self.skeleton_union)
            .cloned()
            .collect();
        if let Some(directory) = self.dir_meshes.get(mesh_dir) {
            let sources = directory.joint_sources.get_or_init(|| {
                let mut sources = BTreeMap::new();
                for (index, path) in directory.paths.iter().enumerate() {
                    let joints = if path == mesh_source {
                        union.clone()
                    } else {
                        self.joint_union([path])
                    };
                    for joint in joints {
                        sources
                            .entry(joint)
                            .and_modify(|owner| *owner = None)
                            .or_insert(Some(index));
                    }
                }
                sources
            });
            // Another worker may have initialized the directory from an earlier version
            // of this file. Only genuine sibling contributions supplement its snapshot.
            union.extend(
                sources
                    .iter()
                    .filter(|(_, owner)| {
                        owner.is_none_or(|index| directory.paths[index] != mesh_source)
                    })
                    .map(|(joint, _)| joint.clone()),
            );
        }
        union
    }

    fn local_directory_union(
        &self,
        mesh_source: &Path,
        mesh_joints: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let paths = mesh_source
            .parent()
            .and_then(|directory| fs::read_dir(directory).ok())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name() != mesh_source.file_name()
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("nif"))
                    && !is_skeleton_path(path)
            });
        let mut union = self.joint_union(paths);
        union.extend(mesh_joints.intersection(&self.skeleton_union).cloned());
        union
    }
}

pub(crate) fn normalize_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn key_dirname(key: &str) -> &str {
    key.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn key_file_name(key: &str) -> &str {
    key.rsplit_once('/').map_or(key, |(_, name)| name)
}

fn is_skeleton_key(key: &str) -> bool {
    let name = key_file_name(key);
    name.starts_with("skeleton") && name.ends_with(".nif")
}

/// Whether this logical file is a skeleton candidate in the winning source set.
pub fn is_skeleton_path(path: &Path) -> bool {
    is_skeleton_key(&normalize_path_key(path))
}

#[derive(Debug, PartialEq, Eq)]
struct PathLocation {
    /// `dragon` or `dlc01/dragon`, without the leading `actors/`.
    actor: String,
    /// The base actor a DLC folder mirrors: `dragon` for `dlc01/dragon`.
    mirror: Option<String>,
    package: String,
}

fn path_location(key: &str) -> Option<PathLocation> {
    let components: Vec<&str> = key.split('/').collect();
    let actors = components
        .iter()
        .position(|component| *component == "actors")?;
    let first = *components.get(actors + 1)?;
    let package = match first {
        "dlc01" | "dlc02" => first,
        _ => "base",
    };
    if package == "base" {
        return Some(PathLocation {
            actor: first.to_owned(),
            mirror: None,
            package: package.to_owned(),
        });
    }
    let name = *components.get(actors + 2)?;
    Some(PathLocation {
        actor: format!("{first}/{name}"),
        mirror: Some(name.to_owned()),
        package: package.to_owned(),
    })
}

fn coverage(joints: &BTreeSet<String>, nodes: &BTreeSet<String>) -> f32 {
    if joints.is_empty() {
        return 1.0;
    }
    let covered = joints.intersection(nodes).count();
    covered as f32 / joints.len() as f32
}

type RankKey<'a> = (bool, bool, bool, bool, i64, bool, bool, &'a str);

/// The reference ranking: same directory, same actor, mirrored base actor, base
/// package, then the largest overlap with the directory's joints, a plain
/// `skeleton.nif` name, no first-person variant, and path
/// order. The sort is ascending, with overlap negated.
fn rank_key<'a>(
    entry: &'a SkeletonEntry,
    mesh_dir: &str,
    mesh_actor: Option<&str>,
    mesh_mirror: Option<&str>,
    directory_union: &BTreeSet<String>,
) -> RankKey<'a> {
    let location = path_location(&entry.key);
    let actor = location.as_ref().map(|location| location.actor.as_str());
    let in_base_package = location
        .as_ref()
        .is_none_or(|location| location.package == "base");
    let overlap = entry
        .nodes
        .as_ref()
        .map_or(0, |nodes| nodes.intersection(directory_union).count());
    (
        key_dirname(&entry.key) != mesh_dir,
        actor != mesh_actor,
        actor != mesh_mirror,
        !in_base_package,
        -(overlap as i64),
        key_file_name(&entry.key) != "skeleton.nif",
        entry.key.contains("_1stperson"),
        &entry.key,
    )
}

fn first_covering_candidate<'a>(
    candidates: &[PathBuf],
    index: &'a SkeletonIndex,
    joints: &BTreeSet<String>,
) -> Option<&'a SkeletonEntry> {
    candidates.iter().find_map(|candidate| {
        let entry = index.candidate(candidate)?;
        entry.covers(joints).then_some(entry)
    })
}

/// A mesh with no joints to rank by keeps the old candidate lookup: the first
/// path candidate that exists wins, and without one it has no skeleton.
fn resolve_without_joints<'a>(
    candidates: &[PathBuf],
    index: &'a SkeletonIndex,
) -> SkeletonResolution<&'a SkeletonEntry> {
    candidates
        .iter()
        .find_map(|candidate| index.candidate(candidate))
        .map_or(
            SkeletonResolution::SelfContained,
            SkeletonResolution::Skeleton,
        )
}

/// Applies the ranking of the reference implementation. Path candidates win
/// first, then a full-cover skeleton from the index, then a partly-covering
/// path candidate, and only then is the rig either self-contained or unresolved.
fn resolve_against<'a>(
    mesh_key: &str,
    mesh_source: &Path,
    candidates: &[PathBuf],
    index: &'a SkeletonIndex,
    joints: &BTreeSet<String>,
    directory_union: impl FnOnce(&str) -> BTreeSet<String>,
) -> SkeletonResolution<&'a SkeletonEntry> {
    if let Some(entry) = first_covering_candidate(candidates, index, joints) {
        return SkeletonResolution::Skeleton(entry);
    }

    let mut full: Vec<&SkeletonEntry> = index
        .entries
        .values()
        .filter(|entry| entry.covers(joints))
        .collect();
    if !full.is_empty() {
        let mesh_dir = key_dirname(mesh_key);
        let location = path_location(mesh_key);
        let mesh_actor = location.as_ref().map(|location| location.actor.as_str());
        let mesh_mirror = location
            .as_ref()
            .and_then(|location| location.mirror.as_deref());
        let union = if full.len() > 1 {
            directory_union(mesh_dir)
        } else {
            BTreeSet::new()
        };
        let rank = |entry| rank_key(entry, mesh_dir, mesh_actor, mesh_mirror, &union);
        full.sort_by_cached_key(|entry| rank(*entry));
        let chosen = full[0];
        if key_dirname(&chosen.key) != mesh_dir {
            warn!(
                "skeleton {} for {} was chosen outside the mesh's own directory",
                chosen.path.display(),
                mesh_source.display()
            );
        }
        if full.len() > 1 {
            let first = rank(chosen);
            let second = rank(full[1]);
            if first.0 == second.0
                && first.1 == second.1
                && first.2 == second.2
                && first.3 == second.3
                && first.4 == second.4
                && first.5 == second.5
                && first.6 == second.6
            {
                warn!(
                    "skeleton {} for {} ties with {} on every ranking rule; only the path order decided",
                    chosen.path.display(),
                    mesh_source.display(),
                    full[1].path.display()
                );
            }
        }
        return SkeletonResolution::Skeleton(chosen);
    }

    let mut best_path: Option<(&SkeletonEntry, f32)> = None;
    for candidate in candidates {
        let Some(entry) = index.candidate(candidate) else {
            continue;
        };
        let Some(nodes) = &entry.nodes else {
            continue;
        };
        let candidate_coverage = coverage(joints, nodes);
        if best_path.is_none_or(|(_, best)| candidate_coverage > best) {
            best_path = Some((entry, candidate_coverage));
        }
    }
    if let Some((entry, candidate_coverage)) = best_path
        && candidate_coverage >= MIN_SKELETON_COVERAGE
    {
        return SkeletonResolution::Skeleton(entry);
    }

    let best_coverage = index
        .entries
        .values()
        .filter_map(|entry| entry.nodes.as_ref())
        .map(|nodes| coverage(joints, nodes))
        .fold(0.0f32, f32::max);
    if best_coverage < MIN_SKELETON_COVERAGE {
        SkeletonResolution::SelfContained
    } else {
        SkeletonResolution::Unresolved { best_coverage }
    }
}

/// Resolves skeletons in a canonical, relative-path winning asset index.
/// Repair callers must include archive assets and overlay winning loose files before resolving.
pub fn resolve_skeleton(
    relative: &Path,
    sources: &BTreeMap<PathBuf, PathBuf>,
) -> SkeletonResolution {
    let index = SkeletonIndex::from_sources(sources);
    let key_path = PathBuf::from(normalize_path_key(relative));
    let mesh_source = sources
        .get(relative)
        .or_else(|| sources.get(&key_path))
        .or_else(|| {
            sources
                .iter()
                .find(|(path, _)| normalize_path_key(path) == key_path.to_string_lossy())
                .map(|(_, source)| source)
        });
    // silent-ok: An unreadable mesh selects the skeleton search that requires no joint evidence.
    match mesh_source.and_then(|source| NifSnapshot::read(source).ok()) {
        Some(source) => resolve_skeleton_from_snapshot(relative, &source, &index),
        None => resolve_without_joints(&skeleton_candidates(&key_path), &index),
    }
    .map(|entry| entry.path.clone())
}

/// Keeps the exact selected entry through provenance verification, hashing and export.
pub(crate) fn resolve_skeleton_from_snapshot<'a>(
    relative: &Path,
    source: &NifSnapshot,
    index: &'a SkeletonIndex,
) -> SkeletonResolution<&'a SkeletonEntry> {
    let Ok((nif, diagnostics, _)) = source.parse() else {
        let key = normalize_path_key(relative);
        return resolve_without_joints(&skeleton_candidates(Path::new(&key)), index);
    };
    resolve_indexed_skeleton(relative, source, &nif, &diagnostics, index)
}

fn resolve_indexed_skeleton<'a>(
    relative: &Path,
    source: &NifSnapshot,
    nif: &NifFile,
    diagnostics: &NifParseDiagnostics,
    index: &'a SkeletonIndex,
) -> SkeletonResolution<&'a SkeletonEntry> {
    let key = normalize_path_key(relative);
    let candidates = skeleton_candidates(Path::new(&key));
    let joints = nif.legacy_skin_joint_names();
    if !declares_skin_type(diagnostics) || joints.is_empty() {
        return resolve_without_joints(&candidates, index);
    }
    resolve_against(&key, &source.path, &candidates, index, &joints, |dir| {
        index.directory_union(dir, &source.path, &joints)
    })
}

fn skeleton_candidates(nif_path: &Path) -> Vec<PathBuf> {
    let Some(parent) = nif_path.parent() else {
        return Vec::new();
    };
    let mut candidates = vec![
        parent.join("skeleton.nif"),
        parent.join("skeleton_female.nif"),
        parent.join("character assets").join("skeleton.nif"),
    ];
    if let Some(grandparent) = parent.parent() {
        candidates.extend([
            grandparent.join("skeleton.nif"),
            grandparent.join("character assets").join("skeleton.nif"),
        ]);
    }
    if let Some((actors_root, actor_name)) = actor_root(nif_path) {
        let actor_dir = actors_root.join(actor_name);
        candidates.extend([
            actor_dir.join("character assets").join("skeleton.nif"),
            actor_dir
                .join("character assets female")
                .join("skeleton_female.nif"),
        ]);
    }
    candidates
}

/// Resolves a mesh on disk. The tree-wide skeleton index is built only when no
/// path candidate covers the rig, so ordinary meshes never pay for the walk.
pub fn find_skeleton(nif_path: &Path) -> SkeletonResolution {
    let Ok((nif, diagnostics, _)) = open_nif_for_conversion(nif_path) else {
        return SkeletonResolution::SelfContained;
    };
    SkeletonIndex::default()
        .resolve_local(nif_path, &nif, &diagnostics)
        .map(|entry| entry.path.clone())
}

impl SkeletonIndex {
    fn resolve_local(
        &mut self,
        nif_path: &Path,
        nif: &NifFile,
        diagnostics: &NifParseDiagnostics,
    ) -> SkeletonResolution<&SkeletonEntry> {
        let candidates = skeleton_candidates(nif_path);
        for path in candidates.iter().filter(|path| path.is_file()) {
            self.insert(path, path);
        }
        let joints = nif.legacy_skin_joint_names();
        if !declares_skin_type(diagnostics) || joints.is_empty() {
            return resolve_without_joints(&candidates, self);
        }
        if first_covering_candidate(&candidates, self, &joints).is_none() {
            self.add_actor_tree(nif_path);
        }
        resolve_against(
            &normalize_path_key(nif_path),
            nif_path,
            &candidates,
            self,
            &joints,
            |_| self.local_directory_union(nif_path, &joints),
        )
    }
}

fn actor_root(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let components: Vec<_> = path.components().collect();
    let index = components.iter().position(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("actors")
    })?;
    let first = components.get(index + 1)?.as_os_str();
    let mut root = PathBuf::new();
    for component in &components[..=index] {
        root.push(component.as_os_str());
    }
    let first_lossy = first.to_string_lossy();
    if first_lossy.eq_ignore_ascii_case("dlc01") || first_lossy.eq_ignore_ascii_case("dlc02") {
        let actor = components.get(index + 2)?.as_os_str();
        let mut actor_path = PathBuf::from(first);
        actor_path.push(actor);
        return Some((root, actor_path));
    }
    Some((root, PathBuf::from(first)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::NifAlphaMode;
    use crate::test_strategies::{arbitrary_bytes, config, corrupted};
    use proptest::prelude::*;

    #[test]
    fn glb_publication_failure_preserves_the_destination_and_cleans_the_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("mesh.glb");
        fs::create_dir(&output).unwrap();

        assert!(write_glb_atomic(&output, b"new GLB").is_err());
        assert!(output.is_dir());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    fn geometry_test_contract() -> Vec<NifShapeMaterial> {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("shape.nif");
        fs::write(&input, static_nif()).unwrap();
        let (_, _, contract) = open_nif_resilient(&input).unwrap();
        assert_eq!(contract.len(), 1);
        assert!(matches!(
            contract[0].disposition,
            NifMaterialDisposition::Validated { .. }
        ));
        contract
    }

    /// A NIF of `nodes.len()` `NiNode` blocks (block `i` lists `nodes[i]`) followed by
    /// `geometry` copies of the fixture's `BSTriShape`. Child references past the node count name
    /// the geometry blocks in order.
    fn tree_nif(nodes: &[Vec<u32>], geometry: usize) -> NifFile {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("tree.nif");
        // Parse empty nodes, then set the child lists: the parser need not see references to
        // blocks that come later in the file.
        fs::write(&input, node_graph_nif(&vec![Vec::new(); nodes.len()])).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        for (block, children) in nif.blocks.iter_mut().zip(nodes) {
            let NifBlock::NiNode(node) = block else {
                panic!("fixture block must be a node");
            };
            node.children = children.clone();
        }
        let fixture = directory.path().join("shape.nif");
        fs::write(&fixture, static_nif()).unwrap();
        // `NifBlock` is not `Clone`: parse the fixture again for each geometry block.
        for _ in 0..geometry {
            let (shapes, _, _) = open_nif_resilient(&fixture).unwrap();
            let shape = shapes
                .blocks
                .into_iter()
                .find(|block| matches!(block, NifBlock::BSTriShape(_)))
                .unwrap();
            nif.blocks.push(shape);
        }
        nif
    }

    /// RF-TEX-002: the 3D Index is a pre-order depth-first count of geometry leaves in child
    /// order. Nodes are 0 to 3 and geometry is 4 to 7. Root 0 lists node 1 then geometry 4; node 1
    /// lists geometry 5 then node 2 then geometry 6; node 2 lists node 3; node 3 lists geometry 7.
    #[test]
    fn geometry_index_is_the_preorder_depth_first_count_in_child_order() {
        let nif = tree_nif(&[vec![1, 4], vec![5, 2, 6], vec![3], vec![7, u32::MAX]], 4);
        let indices = geometry_preorder_indices(&nif.blocks).unwrap();
        // Order visited: 5, 7, 6, 4.
        assert_eq!(indices, BTreeMap::from([(5, 0), (7, 1), (6, 2), (4, 3)]));
    }

    /// An unprovable count yields no indices rather than a guess: two roots, a block reached
    /// twice, or a child that is not a scene block.
    #[test]
    fn geometry_index_is_refused_when_the_count_is_not_provable() {
        // Two roots (nodes 0 and 1 are both unreferenced).
        assert!(geometry_preorder_indices(&tree_nif(&[vec![2], vec![3]], 2).blocks).is_none());
        // Geometry 1 is listed twice (retail would count it twice).
        assert!(geometry_preorder_indices(&tree_nif(&[vec![1, 1]], 1).blocks).is_none());
        // A child reference that is not a block.
        assert!(geometry_preorder_indices(&tree_nif(&[vec![99]], 0).blocks).is_none());
        // A node reachable by two parents.
        assert!(
            geometry_preorder_indices(&tree_nif(&[vec![1, 2], vec![3], vec![1], vec![]], 0).blocks)
                .is_none()
        );
        // A lone geometry root has index 0.
        let lone = tree_nif(&[Vec::new()], 1);
        assert_eq!(
            geometry_preorder_indices(&lone.blocks[1..]).unwrap(),
            BTreeMap::from([(0, 0)])
        );
    }

    /// RF-TEX-003: every geometry counts, but only a validated lighting-shader primitive carries
    /// the index. An effect shape and an excluded shape keep their place in the count and get no tag.
    #[test]
    fn only_lighting_primitives_are_tagged_and_other_shapes_still_count() {
        use crate::material::NifShaderFamily;
        let nif = tree_nif(&[vec![1, 2, 3, 4], vec![5]], 4);
        // Geometry blocks are 2 to 5 in file order, but the tree visits 5 first, so the indices
        // by block are {5: 0, 2: 1, 3: 2, 4: 3}.
        let indices = geometry_preorder_indices(&nif.blocks).unwrap();
        assert_eq!(indices, BTreeMap::from([(5, 0), (2, 1), (3, 2), (4, 3)]));
        let base = geometry_test_contract().remove(0);
        let shape = |block: u32| NifShapeMaterial {
            shape_block: block,
            ..base.clone()
        };
        let mut effect = shape(3);
        let NifMaterialDisposition::Validated { material } = &mut effect.disposition else {
            unreachable!()
        };
        material.shader_family = NifShaderFamily::Effect;
        let mut excluded = shape(4);
        excluded.disposition = NifMaterialDisposition::Excluded {
            reason: "fixture".into(),
        };
        let contract = [shape(5), shape(2), effect, excluded];
        let shape_blocks = [5, 2, 3, 4];
        let document = serde_json::json!({"asset":{"version":"2.0"},"meshes":[
            {"primitives":[{}]},
            {"primitives":[{"extras":{"retained":"control"}}]},
            {"primitives":[{}]},
            {"primitives":[{"extras":{"openSkyrim":{"materialExclusion":"fixture"}}}]},
        ]});
        let glb = glb_bytes(&document, b"geometry control");
        let tagged = tag_geometry_indices(glb.clone(), &contract, &shape_blocks, &nif).unwrap();
        let tagged = glb_json_from_bytes(&tagged).unwrap();
        let index = |mesh: usize| {
            tagged["meshes"][mesh]["primitives"][0]["extras"]["mudcrabGeometryIndex"].clone()
        };
        assert_eq!(index(0), 0);
        assert_eq!(index(1), 1);
        assert!(index(2).is_null(), "effect shape");
        assert!(index(3).is_null(), "excluded shape");
        assert_eq!(
            tagged["meshes"][1]["primitives"][0]["extras"]["retained"],
            "control"
        );
        assert_eq!(
            tagged["meshes"][3]["primitives"][0]["extras"]["openSkyrim"]["materialExclusion"],
            "fixture"
        );
        // With no lighting shape to tag, or no provable count, the bytes are unchanged.
        let no_tag = [contract[2].clone(), contract[3].clone()];
        assert_eq!(
            tag_geometry_indices(glb.clone(), &no_tag, &[3, 4], &tree_nif(&[vec![1, 2]], 2),)
                .unwrap(),
            glb
        );
        assert_eq!(
            tag_geometry_indices(
                glb.clone(),
                &contract,
                &shape_blocks,
                &tree_nif(&[Vec::new(), Vec::new()], 0),
            )
            .unwrap(),
            glb
        );
        // A mesh count that does not match the exported blocks is never tagged.
        assert_eq!(
            tag_geometry_indices(glb.clone(), &contract, &[5, 2], &nif).unwrap(),
            glb
        );
    }

    /// The converted fixture is tagged with its index, whatever depth its shape sits at.
    #[test]
    fn converted_single_shape_models_carry_their_geometry_index() {
        let directory = tempfile::tempdir().unwrap();
        for (name, source) in [
            ("static", static_nif()),
            ("chain", static_scene_chain_nif(5)),
        ] {
            let input = directory.path().join(format!("{name}.nif"));
            let output = directory.path().join(format!("{name}.glb"));
            fs::write(&input, source).unwrap();
            MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
            let document = glb_json_from_bytes(&fs::read(output).unwrap()).unwrap();
            assert_eq!(
                document["meshes"][0]["primitives"][0]["extras"]["mudcrabGeometryIndex"], 0,
                "{name}"
            );
        }
    }

    #[test]
    fn snapshot_export_keeps_geometry_and_collision_after_source_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let relative = Path::new("meshes/rigs/skeleton.nif");
        let input = directory.path().join("skeleton.nif");
        let control = directory.path().join("control.glb");
        let output = directory.path().join("snapshot.glb");
        let source = collidable_static_nif();
        fs::write(&input, &source).unwrap();
        let snapshot = NifSnapshot::read(&input).unwrap();
        let sources = BTreeMap::from([(relative.to_owned(), input.clone())]);
        let index = SkeletonIndex::from_sources(&sources);
        MeshConverter::convert_nif_to_glb_with_skeleton(&input, &control, None).unwrap();
        let original = fs::read(control).unwrap();
        let document = glb_json_from_bytes(&original).unwrap();
        assert!(document["extras"]["mudcrabRoomSphere"].is_object());
        assert_eq!(
            document["scenes"][0]["extras"]["mudcrabCollision"]["bodies"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        fs::write(&input, b"replacement is not a NIF").unwrap();
        MeshConverter::convert_nif_snapshot(&snapshot, &output, None, None).unwrap();
        assert_eq!(fs::read(&output).unwrap(), original);

        MeshConverter::convert_nif_to_glb_from_index(relative, &input, &output, &index).unwrap();
        assert_eq!(fs::read(&output).unwrap(), original);
        let next = SkeletonIndex::from_sources(&sources);
        assert!(
            MeshConverter::convert_nif_to_glb_from_index(relative, &input, &output, &next).is_err()
        );
    }

    #[test]
    fn local_rig_ranking_uses_captured_mesh_and_live_sibling_evidence() {
        use dummy_content::nif::{LegacySkin, SkinInstanceKind, legacy_skinned_shape};

        let directory = tempfile::tempdir().unwrap();
        let actor = directory.path().join("actors/test");
        let rigs = actor.join("rigs");
        fs::create_dir_all(&rigs).unwrap();
        let mesh = actor.join("skin.nif");
        let skin = LegacySkin {
            name: "SourceSkin",
            instance_kind: SkinInstanceKind::Plain,
            bone_transforms: [([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0); 2],
            palette: &[1, 0],
            weights: &[[0.75, 0.25, 0.0, 0.0]; 4],
            with_geometry: true,
            joint_override: None,
        };
        let original = legacy_skinned_shape(&skin).unwrap();
        fs::write(&mesh, &original).unwrap();
        let source = NifSnapshot::read(&mesh).unwrap();

        let rig_hash = |filename, name| {
            let path = rigs.join(filename);
            let mut bytes = legacy_skinned_shape(&LegacySkin {
                name,
                with_geometry: false,
                ..skin
            })
            .unwrap();
            let (blocks, _) = parse_skyrim_header(&bytes, &path).unwrap();
            let root = bytes.len() - blocks.len();
            bytes[root..root + 4].copy_from_slice(&0u32.to_le_bytes());
            fs::write(&path, &bytes).unwrap();
            format!("{}:{}", source.hash(), crate::cache::hash_bytes(&bytes))
        };
        let first_rig = rig_hash("skeleton_a.nif", "First");
        let second_rig = rig_hash("skeleton_b.nif", "Extra");

        let output = directory.path().join("output.glb");
        let convert = |expected_hash: &str| {
            MeshConverter::convert_nif_with_local_resolution(&source, &output, Some(expected_hash))
                .unwrap();
        };
        convert(&first_rig);

        let mut replacement = original;
        let joint = replacement
            .windows(5)
            .position(|bytes| bytes == b"Bone1")
            .unwrap();
        replacement[joint..joint + 5].copy_from_slice(b"Extra");
        fs::write(&mesh, &replacement).unwrap();
        convert(&first_rig);

        fs::write(actor.join("sibling.nif"), replacement).unwrap();
        convert(&second_rig);
    }

    #[test]
    fn rig_resolution_and_export_use_the_same_mesh_and_skeleton_snapshots() {
        use dummy_content::nif::{LegacySkin, SkinInstanceKind, legacy_skinned_shape};

        let directory = tempfile::tempdir().unwrap();
        let relative = PathBuf::from("meshes/actors/a/skin.nif");
        let skeleton_key = PathBuf::from("meshes/actors/b/skeleton.nif");
        let input = directory.path().join(&relative);
        let skeleton = directory.path().join(&skeleton_key);
        for path in [&input, &skeleton] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        let skin = LegacySkin {
            name: "SourceSkin",
            instance_kind: SkinInstanceKind::Plain,
            bone_transforms: [([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0); 2],
            palette: &[1, 0],
            weights: &[[0.75, 0.25, 0.0, 0.0]; 4],
            with_geometry: true,
            joint_override: None,
        };
        let mesh_bytes = legacy_skinned_shape(&skin).unwrap();
        let mut skeleton_bytes = legacy_skinned_shape(&LegacySkin {
            name: "ExternalRoot",
            with_geometry: false,
            ..skin
        })
        .unwrap();
        let (blocks, header) = parse_skyrim_header(&skeleton_bytes, &skeleton).unwrap();
        let root = skeleton_bytes.len() - blocks.len();
        skeleton_bytes[root..root + 4].copy_from_slice(&0u32.to_le_bytes());
        fs::write(&input, &mesh_bytes).unwrap();
        fs::write(&skeleton, &skeleton_bytes).unwrap();

        let direct = directory.path().join("direct.glb");
        MeshConverter::convert_nif_to_glb_with_skeleton(&input, &direct, Some(&skeleton)).unwrap();
        let expected = fs::read(direct).unwrap();
        let document = glb_json_from_bytes(&expected).unwrap();
        assert!(document.get("skins").is_some());
        assert!(document["extras"]["mudcrabRoomSphere"].is_null());
        assert!(
            document["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["name"] == "ExternalRoot")
        );

        let source = NifSnapshot::read(&input).unwrap();
        let sources = BTreeMap::from([
            (relative.clone(), input.clone()),
            (skeleton_key.clone(), skeleton.clone()),
        ]);
        let index = SkeletonIndex::from_sources(&sources);
        fs::write(&input, static_nif()).unwrap();
        assert_eq!(find_skeleton(&input), SkeletonResolution::SelfContained);
        assert_eq!(
            resolve_skeleton(&relative, &sources),
            SkeletonResolution::SelfContained
        );

        let standalone = directory.path().join("standalone.glb");
        MeshConverter::convert_nif_with_local_resolution(&source, &standalone, None).unwrap();
        assert_eq!(fs::read(standalone).unwrap(), expected);
        // Replace the rig with the same joint names and different Bone0 translation.
        let mut replacement = skeleton_bytes.clone();
        let bone0_x = root + header.block_size_index[0] as usize + 16;
        replacement[bone0_x..bone0_x + 4].copy_from_slice(&17.0f32.to_le_bytes());
        fs::write(&skeleton, &replacement).unwrap();
        fs::write(&input, &mesh_bytes).unwrap();
        let rebuilt = SkeletonIndex::from_sources(&sources);
        let indexed = directory.path().join("indexed.glb");
        for (batch, bytes) in [(&index, &skeleton_bytes), (&rebuilt, &replacement)] {
            let SkeletonResolution::Skeleton(entry) =
                resolve_skeleton_from_snapshot(&relative, &source, batch)
            else {
                panic!("expected external rig")
            };
            assert_eq!(entry.path, skeleton_key);
            assert_eq!(
                entry.snapshot().unwrap().hash(),
                crate::cache::hash_bytes(bytes)
            );
            MeshConverter::convert_nif_to_glb_from_index(&relative, &input, &indexed, batch)
                .unwrap();
            if bytes == &skeleton_bytes {
                assert_eq!(fs::read(&indexed).unwrap(), expected);
            } else {
                let control = directory.path().join("replacement.glb");
                MeshConverter::convert_nif_to_glb_with_skeleton(&input, &control, Some(&skeleton))
                    .unwrap();
                let control = fs::read(control).unwrap();
                assert_ne!(control, expected);
                assert_eq!(fs::read(&indexed).unwrap(), control);
            }
        }
    }

    #[test]
    fn directory_rig_ranking_is_shared_by_workers_and_refreshes_next_batch() {
        use dummy_content::nif::{LegacySkin, SkinInstanceKind, legacy_skinned_shape};
        let dir = tempfile::tempdir().unwrap();
        let mut skin = legacy_skinned_shape(&LegacySkin {
            name: "Skin",
            instance_kind: SkinInstanceKind::Plain,
            bone_transforms: [([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0); 2],
            palette: &[1, 0],
            weights: &[[0.75, 0.25, 0.0, 0.0]; 4],
            with_geometry: true,
            joint_override: None,
        })
        .unwrap();
        let (blocks, _) = parse_skyrim_header(&skin, Path::new("Skin.nif")).unwrap();
        let root = skin.len() - blocks.len();
        skin[root..root + 4].copy_from_slice(&0u32.to_le_bytes());
        let rename = |name: &[u8; 5]| {
            let mut bytes = skin.clone();
            let at = bytes.windows(5).position(|s| s == b"Bone1").unwrap();
            bytes[at..at + 5].copy_from_slice(name);
            bytes
        };
        let a = PathBuf::from("meshes/actors/test/a.nif");
        let b = a.with_file_name("b.nif");
        let x = PathBuf::from("meshes/actors/external/x/skeleton.nif");
        let y = PathBuf::from("meshes/actors/external/y/skeleton.nif");
        let mut sources = BTreeMap::new();
        for (key, bytes) in [
            (&a, rename(b"Bone0")),
            (&b, rename(b"Bone0")),
            (&x, rename(b"WingX")),
            (&y, rename(b"WingY")),
        ] {
            let path = dir.path().join(key);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            sources.insert(key.clone(), path);
        }
        let index = SkeletonIndex::from_sources(&sources);
        let b_snapshot = NifSnapshot::read(&sources[&b]).unwrap();
        let pick = |batch: &SkeletonIndex| {
            resolve_skeleton_from_snapshot(&b, &b_snapshot, batch).map(|entry| entry.path.clone())
        };
        fs::write(&sources[&b], rename(b"WingY")).unwrap();
        // When another worker goes first, the replacement is genuine sibling evidence
        // for A, but must not influence the ranking of B's already captured bytes.
        let sibling_first = SkeletonIndex::from_sources(&sources);
        let a_snapshot = NifSnapshot::read(&sources[&a]).unwrap();
        assert_eq!(
            resolve_skeleton_from_snapshot(&a, &a_snapshot, &sibling_first)
                .map(|entry| entry.path.clone()),
            SkeletonResolution::Skeleton(y.clone())
        );
        assert_eq!(
            pick(&sibling_first),
            SkeletonResolution::Skeleton(x.clone())
        );
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| assert_eq!(pick(&index), SkeletonResolution::Skeleton(x.clone())));
            }
        });
        fs::write(&sources[&a], rename(b"WingY")).unwrap();
        assert_eq!(pick(&index), SkeletonResolution::Skeleton(x));
        let next_batch = SkeletonIndex::from_sources(&sources);
        assert_eq!(pick(&next_batch), SkeletonResolution::Skeleton(y.clone()));
        // Historical replay includes the model itself in the directory-wide union.
        let historical = format!(
            "{}:{}",
            b_snapshot.hash(),
            crate::cache::hash_file(&sources[&y]).unwrap()
        );
        MeshConverter::convert_nif_with_local_resolution(
            &b_snapshot,
            &dir.path().join("replay.glb"),
            Some(&historical),
        )
        .unwrap();
    }

    /// A single-shape NIF whose shape node scale is replaced after generation. A scale of
    /// 1e35 on a position of 123456 overflows `f32` in the bounds, so the primary export has
    /// invalid bounds. The generator keeps its scale at 1, so the bytes are patched.
    fn single_shape_nif_with_node_scale(scale: Option<f32>) -> Vec<u8> {
        let mut bytes = dummy_content::nif::static_shape(&dummy_content::nif::StaticShape {
            name: "PropertyQuad",
            positions: &[
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 123_456.0],
                [-1.0, 1.0, 0.0],
            ],
            normals: &[[0.0, 0.0, 1.0]; 4],
            uvs: &[[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
            indices: &[[0, 1, 2], [0, 2, 3]],
            diffuse: "textures/generated_color.dds",
            normal_texture: "textures/generated_normal.dds",
        })
        .unwrap();
        if let Some(scale) = scale {
            let identity: Vec<u8> = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
            let hits: Vec<usize> = bytes
                .windows(identity.len())
                .enumerate()
                .filter(|(_, window)| *window == identity.as_slice())
                .map(|(at, _)| at)
                .collect();
            for at in hits {
                let scale_at = at + 36;
                bytes[scale_at..scale_at + 4].copy_from_slice(&scale.to_le_bytes());
            }
        }
        bytes
    }

    /// The bounds-recovery branch exports a static fallback. A fallback keeps no source-shape
    /// proof, so it must not carry the geometry index that a normal export of the same shape does.
    #[test]
    fn geometry_index_is_absent_from_a_bounds_recovered_static_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let tag = |name: &str, scale: Option<f32>| {
            let input = directory.path().join(format!("{name}.nif"));
            let output = directory.path().join(format!("{name}.glb"));
            fs::write(&input, single_shape_nif_with_node_scale(scale)).unwrap();
            MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
            glb_json_from_bytes(&fs::read(output).unwrap()).unwrap()["meshes"][0]["primitives"][0]
                ["extras"]["mudcrabGeometryIndex"]
                .clone()
        };
        // Control: the same single shape with finite bounds is tagged.
        assert_eq!(tag("control", None), 0);
        // An enormous node scale gives the primary export invalid bounds, so the static
        // fallback replaces it.
        assert!(tag("recovered", Some(1.0e35)).is_null());
    }

    /// [`static_nif`]'s shape plus a fixed box body on its own node. The Havok blocks
    /// (`bhkCollisionObject`, `bhkRigidBody`, `bhkBoxShape`) hang off that node's collision
    /// reference, as in a retail collidable static, and the render parser leaves them unparsed.
    fn collidable_static_nif() -> Vec<u8> {
        dummy_content::nif::static_shape_with_bodies(
            &dummy_content::nif::StaticShape {
                name: "PropertyQuad",
                positions: &[
                    [-1.0, -1.0, 0.0],
                    [1.0, -1.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [-1.0, 1.0, 0.0],
                ],
                normals: &[[0.0, 0.0, 1.0]; 4],
                uvs: &[[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
                indices: &[[0, 1, 2], [0, 2, 3]],
                diffuse: "textures/generated_color.dds",
                normal_texture: "textures/generated_normal.dds",
            },
            &[dummy_content::nif::fixed_box_body("Collision")],
        )
        .unwrap()
    }

    /// #723: unparsed Havok collision is not scene data, so it does not stop the tag. Renaming the
    /// collision object's header type to a non-Havok name (same length, so the header stays
    /// valid) leaves the scene and the count unchanged, but that unparsed block may be scene
    /// data, so the same model is not tagged.
    #[test]
    fn havok_collision_does_not_stop_the_geometry_tag_but_other_unparsed_blocks_do() {
        let directory = tempfile::tempdir().unwrap();
        let convert = |name: &str, bytes: &[u8]| {
            let input = directory.path().join(format!("{name}.nif"));
            let output = input.with_extension("glb");
            fs::write(&input, bytes).unwrap();
            let fallbacks = MeshConverter::inspect_nif(&input).unwrap().fallback_blocks;
            MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
            (
                fallbacks,
                glb_json_from_bytes(&fs::read(output).unwrap()).unwrap(),
            )
        };
        let collidable = collidable_static_nif();
        let (fallbacks, document) = convert("collidable", &collidable);
        assert_eq!(
            fallbacks.keys().map(String::as_str).collect::<Vec<_>>(),
            ["bhkBoxShape", "bhkCollisionObject", "bhkRigidBody"],
            "the fixture's only unparsed blocks are Havok collision"
        );
        assert_eq!(
            document["meshes"][0]["primitives"][0]["extras"]["mudcrabGeometryIndex"], 0,
            "the collidable static's lighting shape carries its 3D Index"
        );
        let bodies = &document["scenes"][0]["extras"]["mudcrabCollision"]["bodies"];
        assert_eq!(
            bodies.as_array().map(Vec::len),
            Some(1),
            "collision is kept"
        );

        let mut renamed = collidable;
        let from = b"bhkCollisionObject";
        let to = b"MudcrabUnknownData";
        assert_eq!(from.len(), to.len());
        let at = renamed
            .windows(from.len())
            .position(|window| window == from)
            .unwrap();
        renamed[at..at + to.len()].copy_from_slice(to);
        let (fallbacks, document) = convert("unknown", &renamed);
        assert!(fallbacks.contains_key("MudcrabUnknownData"));
        assert!(
            document["meshes"][0]["primitives"][0]["extras"]["mudcrabGeometryIndex"].is_null(),
            "a non-Havok unparsed block still refuses the tag"
        );
    }

    /// Only Havok type names count as known non-scene data. A skin, an unparsed geometry or node
    /// type, or any unknown type still refuses the tag, alone or next to collision.
    #[test]
    fn only_havok_fallbacks_are_known_non_scene_blocks() {
        let refuses = |types: &[&str]| {
            has_unparsed_non_havok_block(&NifParseDiagnostics {
                fallback_blocks: types.iter().map(|name| ((*name).to_owned(), 1)).collect(),
                ..Default::default()
            })
        };
        assert!(!refuses(&[]));
        assert!(!refuses(&[
            "bhkCollisionObject",
            "bhkRigidBody",
            "bhkRigidBodyT",
            "bhkMoppBvTreeShape",
            "bhkCompressedMeshShape",
            "bhkCompressedMeshShapeData",
            "bhkConvexVerticesShape",
            "bhkPackedNiTriStripsShape",
            "hkPackedNiTriStripsData",
        ]));
        for other in [
            "NiTriStrips",
            "BSMeshLODTriShape",
            "NiNode",
            "NiSkinInstance",
            "NiCollisionObject",
            "hkUnknown",
        ] {
            assert!(refuses(&[other]), "{other}");
            assert!(
                refuses(&["bhkCollisionObject", other]),
                "{other} with collision"
            );
        }
    }

    /// An unparsed block in a child list (for example an unhandled geometry type) is not
    /// countable, whatever its type name.
    #[test]
    fn geometry_index_is_refused_for_an_unparsed_scene_child() {
        let mut nif = tree_nif(&[vec![1, 2]], 2);
        assert_eq!(
            geometry_preorder_indices(&nif.blocks),
            Some(BTreeMap::from([(1, 0), (2, 1)]))
        );
        nif.blocks[2] = NifBlock::Unhandled;
        assert!(geometry_preorder_indices(&nif.blocks).is_none());
    }

    fn body_at(node: u32, target: &str) -> shared::collision::CollisionBody {
        use shared::collision::{BodyKind, CollisionBody, HavokBodyInfo};
        CollisionBody {
            node,
            target: target.to_owned(),
            shapes: vec![0],
            kind: BodyKind::Fixed,
            havok: HavokBodyInfo {
                motion_system: 7,
                quality_type: 1,
                deactivator_type: 1,
                collision_layer: 1,
            },
            mass: 0.0,
            inertia: [0.0; 9],
            center_of_mass: [0.0; 3],
            linear_damping: 0.0,
            angular_damping: 0.0,
            friction: 0.0,
            restitution: 0.0,
            max_linear_velocity: 0.0,
            max_angular_velocity: 0.0,
            convex: true,
        }
    }

    fn retained(target: &str, node: u32, names: &[&str], unique: bool) -> (usize, usize) {
        let mut collision = CollisionAsset {
            version: 2,
            authored: true,
            shapes: Vec::new(),
            skipped: Vec::new(),
            bodies: vec![body_at(node, target)],
        };
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        retain_bodies_with_nodes(&mut collision, &names, unique);
        (collision.bodies.len(), collision.skipped.len())
    }

    #[test]
    fn retain_keeps_a_body_only_at_its_non_empty_named_node() {
        let names = ["Root", "Shape", "Crate"];
        for unique in [false, true] {
            assert_eq!(retained("Crate", 2, &names, unique), (1, 0));
            // an empty name never identifies a node
            assert_eq!(retained("", 2, &["Root", "Shape", ""], unique), (0, 1));
        }
        // In static-scene order a matching predicted index is kept. A unique name found at
        // another index is still rebound, because a mixed skinned and static export can shift
        // the index the NIF predicted.
        assert_eq!(retained("Crate", 1, &names, false), (1, 0));
        assert_eq!(retained("Crate", 9, &names, false), (1, 0));
        // A GLB that did not keep the static-scene order trusts only a unique name.
        assert_eq!(retained("Crate", 1, &names, true), (1, 0));
        assert_eq!(retained("Crate", 9, &names, true), (1, 0));
        // A reused name: the conversion path (GLB built from this NIF) trusts the index;
        // the annotate path (GLB from elsewhere) needs the name to be unique.
        let reused = ["Potato", "Shape", "Potato"];
        assert_eq!(retained("Potato", 2, &reused, false), (1, 0));
        assert_eq!(retained("Potato", 1, &reused, false), (0, 1));
        assert_eq!(retained("Potato", 2, &reused, true), (0, 1));
    }

    fn retained_in_glb(
        node: u32,
        target: &str,
        glb: &[u8],
        order: Option<&[String]>,
    ) -> CollisionAsset {
        let mut collision = CollisionAsset {
            version: 2,
            authored: true,
            shapes: Vec::new(),
            skipped: Vec::new(),
            bodies: vec![body_at(node, target)],
        };
        retain_bodies_for_written_glb(&mut collision, glb, order).unwrap();
        collision
    }

    #[test]
    fn bodies_are_checked_against_the_written_glb_node_order() {
        // The written GLB names the crate at index 1, while the source model order names the
        // shape there. A source order that the GLB keeps trusts the predicted index; a GLB
        // that changed the order rebinds the unique name to the index it really has.
        let glb = glb_bytes(
            &serde_json::json!({
                "asset": {"version": "2.0"},
                "nodes": [{"name": "Root"}, {"name": "Crate"}, {"name": "Shape"}, {}],
            }),
            b"",
        );
        assert_eq!(
            glb_node_names(&glb).unwrap(),
            ["Root", "Crate", "Shape", ""]
        );
        let source_order: Vec<String> = ["Root", "Crate", "Shape"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let kept = |node: u32, target: &str, glb: &[u8], order: Option<&[String]>| {
            retained_in_glb(node, target, glb, order).bodies.len()
        };
        // The GLB keeps the static-scene order: the predicted index is relied on when it names
        // the target. A mixed skinned and static export can still shift that index, so a name
        // found elsewhere is rebound when it is unique.
        assert_eq!(kept(1, "Crate", &glb, Some(&source_order)), 1);
        assert_eq!(kept(2, "Crate", &glb, Some(&source_order)), 1);
        assert_eq!(
            retained_in_glb(2, "Crate", &glb, Some(&source_order)).bodies[0].node,
            1
        );
        // No usable source order (annotate): a unique name is rebound to its real index.
        assert_eq!(kept(2, "Crate", &glb, None), 1);
        assert_eq!(retained_in_glb(2, "Crate", &glb, None).bodies[0].node, 1);
        // A name that the written GLB does not carry is still skipped.
        assert_eq!(kept(1, "Barrel", &glb, None), 0);
    }

    /// A skinned NIF exports its skeleton in a different node order than the static scene, so a
    /// predicted body index can name the wrong node. A target name that appears exactly once is
    /// rebound to the index it really has. The shapes already carry the baked node and body
    /// transforms, so the index only binds the body to its node name.
    #[test]
    fn a_skinned_body_is_rebound_to_its_unique_node_name() {
        let glb = glb_bytes(
            &serde_json::json!({
                "asset": {"version": "2.0"},
                "nodes": [
                    {"name": "Root"},
                    {"name": "Bone01"},
                    {"name": "TrunkBone"},
                    {"name": "Shape"},
                ],
            }),
            b"",
        );
        assert_eq!(
            glb_node_names(&glb).unwrap(),
            ["Root", "Bone01", "TrunkBone", "Shape"]
        );
        // A skeletal NIF has no static-scene order to vouch for the predicted index.
        let collision = retained_in_glb(1, "TrunkBone", &glb, Some(&[]));
        assert_eq!(collision.bodies.len(), 1);
        assert_eq!(collision.bodies[0].node, 2);
        assert!(collision.skipped.is_empty());
    }

    #[test]
    fn a_reordered_body_whose_name_is_absent_is_skipped() {
        let glb = glb_bytes(
            &serde_json::json!({
                "asset": {"version": "2.0"},
                "nodes": [{"name": "Root"}, {"name": "Bone01"}, {"name": "Shape"}],
            }),
            b"",
        );
        let collision = retained_in_glb(1, "TrunkBone", &glb, Some(&[]));
        assert!(collision.bodies.is_empty());
        assert_eq!(collision.skipped.len(), 1);
        assert!(collision.skipped[0].contains("TrunkBone"));
        assert!(collision.skipped[0].contains("no matching glTF node"));
    }

    #[test]
    fn a_reordered_body_with_a_repeated_node_name_is_skipped() {
        let glb = glb_bytes(
            &serde_json::json!({
                "asset": {"version": "2.0"},
                "nodes": [
                    {"name": "Root"},
                    {"name": "TrunkBone"},
                    {"name": "TrunkBone"},
                    {"name": "Shape"},
                ],
            }),
            b"",
        );
        let collision = retained_in_glb(1, "TrunkBone", &glb, Some(&[]));
        assert!(collision.bodies.is_empty());
        assert_eq!(collision.skipped.len(), 1);
        assert!(collision.skipped[0].contains("TrunkBone"));
    }

    #[test]
    fn a_reused_node_name_is_trusted_only_when_the_glb_keeps_the_source_order() {
        let glb = glb_bytes(
            &serde_json::json!({
                "asset": {"version": "2.0"},
                "nodes": [{"name": "Potato"}, {"name": "Shape"}, {"name": "Potato"}, {}],
            }),
            b"",
        );
        let check = |order: Option<Vec<&str>>| {
            let order: Option<Vec<String>> =
                order.map(|names| names.into_iter().map(str::to_owned).collect());
            let mut collision = CollisionAsset {
                version: 2,
                authored: true,
                shapes: Vec::new(),
                skipped: Vec::new(),
                bodies: vec![body_at(2, "Potato")],
            };
            retain_bodies_for_written_glb(&mut collision, &glb, order.as_deref()).unwrap();
            collision.bodies.len()
        };
        // Same order as the static scene: the index is reliable, the body is kept.
        assert_eq!(check(Some(vec!["Potato", "Shape", "Potato"])), 1);
        // The GLB reordered the nodes: a reused name could be the wrong node, so it is skipped.
        assert_eq!(check(Some(vec!["Shape", "Potato", "Potato"])), 0);
        // No source order (annotate): reused names are skipped.
        assert_eq!(check(None), 0);
        // An empty static scene (skeletal NIFs) does not vouch for any order.
        assert_eq!(check(Some(Vec::new())), 0);
    }

    /// Verifies legacy skin partitions parse without an SSE vertex buffer and fail under the wrong layout.
    #[test]
    fn skin_partition_uses_legacy_layout_and_rejects_oversized_sse_buffer() {
        // Counts from the KS Hairdos file that previously requested 56,716,014,720 bytes.
        let mut bytes = 1u32.to_le_bytes().to_vec();
        for count in [9938u16, 13522, 2, 0, 4] {
            bytes.extend_from_slice(&count.to_le_bytes());
        }
        bytes.extend_from_slice(&[0; 4]); // two bones
        bytes.extend_from_slice(&[0, 0, 1]); // no vertex map/weights; faces present
        for _ in 0..13522 {
            bytes.extend_from_slice(&[0, 0, 1, 0, 2, 0]);
        }
        bytes.extend_from_slice(&[0, 0, 0]); // no bone indices; partition flags
        let (remaining, block) =
            NifBlock::parse_with_version(&bytes, "NiSkinPartition".into(), 83).unwrap();
        assert!(remaining.is_empty());
        let NifBlock::NiSkinPartition(partition) = block else {
            panic!("wrong block type")
        };
        assert_eq!(partition.triangles.len(), 13522);
        assert!(partition.vertex_data.is_empty());
        assert!(NifBlock::parse_with_version(&bytes, "NiSkinPartition".into(), 100).is_err());
        let mut sse = vec![0; 8]; // zero partitions and zero vertex bytes
        sse.extend_from_slice(&4u32.to_le_bytes());
        sse.extend_from_slice(&1u64.to_le_bytes()); // four-byte vertex stride
        assert!(NifBlock::parse_with_version(&sse, "NiSkinPartition".into(), 100).is_ok());
    }

    #[test]
    fn rejects_invalid_nif_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("bad.nif");
        let output = dir.path().join("bad.glb");
        fs::write(&input, b"not a nif").unwrap();
        assert!(MeshConverter::convert_nif_to_glb(&input, &output).is_err());
        assert!(!output.exists());
    }

    fn single_block_nif(block_type: &str, block: &[u8], trailer: &[u8]) -> Vec<u8> {
        let mut bytes = b"Gamebryo File Format, Version 20.2.0.7\n".to_vec();
        bytes.extend(0x1402_0007u32.to_le_bytes());
        bytes.push(1);
        bytes.extend(12u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(100u32.to_le_bytes());
        bytes.extend([0, 0, 0]); // absent author/process/export strings
        bytes.extend(1u16.to_le_bytes());
        bytes.extend((block_type.len() as u32).to_le_bytes());
        bytes.extend(block_type.as_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend((block.len() as u32).to_le_bytes());
        bytes.extend([0; 12]); // no strings, maximum string length zero, no groups
        bytes.extend(block);
        bytes.extend(trailer);
        bytes
    }

    #[test]
    fn nif_audit_accounts_for_known_block_tails_and_file_trailers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tail.nif");
        let block = [
            u32::MAX.to_le_bytes().as_slice(),
            &0u32.to_le_bytes(),
            &[9, 7, 5],
        ]
        .concat();
        let bytes = single_block_nif("BSXFlags", &block, &[3, 1]);
        fs::write(&path, &bytes).unwrap();
        let audit = MeshConverter::inspect_nif(&path).unwrap();
        assert_eq!(audit.parsed_block_count, 1);
        assert!(audit.fallback_blocks.is_empty());
        assert_eq!(audit.unparsed_regions, 2);
        assert_eq!(audit.unparsed_bytes, 5);
        let tail = &audit.unparsed_samples[0];
        assert_eq!(tail.reason, "unparsed_block_tail");
        assert_eq!(tail.block_index, Some(0));
        assert_eq!(tail.block_type.as_deref(), Some("BSXFlags"));
        assert_eq!(tail.byte_length, 3);
        assert_eq!(tail.sha256, crate::cache::hash_bytes(&[9, 7, 5]));
        assert_eq!(
            &bytes[tail.file_offset..tail.file_offset + tail.byte_length],
            &[9, 7, 5]
        );
        let trailer = &audit.unparsed_samples[1];
        assert_eq!(trailer.reason, "unparsed_file_trailer");
        assert_eq!(trailer.block_index, None);
        assert_eq!(trailer.file_offset, bytes.len() - 2);
        assert_eq!(trailer.sha256, crate::cache::hash_bytes(&[3, 1]));

        // Changing one ignored input changes its receipt, not its unaffected control.
        let changed = single_block_nif("BSXFlags", &block, &[3, 2]);
        fs::write(&path, changed).unwrap();
        let changed = MeshConverter::inspect_nif(&path).unwrap();
        assert_eq!(changed.unparsed_samples[0].sha256, tail.sha256);
        assert_ne!(changed.unparsed_samples[1].sha256, trailer.sha256);
    }

    #[test]
    fn nif_audit_accounts_for_whole_fallback_blocks_with_bounded_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unknown.nif");
        let bytes = single_block_nif("UnknownModBlock", &[0, 1, 2, 3], &[]);
        fs::write(&path, &bytes).unwrap();
        let mut audit = MeshConverter::inspect_nif(&path).unwrap();
        assert_eq!(audit.unparsed_regions, 1);
        assert_eq!(audit.unparsed_bytes, 4);
        assert_eq!(audit.unparsed_samples[0].file_offset, bytes.len() - 4);
        assert_eq!(audit.unparsed_samples[0].reason, "unhandled_block");
        for index in 1..20 {
            audit.record_unparsed(
                "unhandled_block",
                Some((index, "UnknownModBlock")),
                index * 4,
                &[0; 4],
            );
        }
        assert_eq!(audit.unparsed_regions, 20);
        assert_eq!(audit.unparsed_bytes, 80);
        assert_eq!(audit.unparsed_samples.len(), 8);
    }

    #[test]
    #[ignore = "requires MUDCRAB_NIF_FIXTURE with a locally installed Skyrim NIF"]
    fn converts_installed_non_renderable_nif_to_empty_scene() {
        let path = std::env::var_os("MUDCRAB_NIF_FIXTURE")
            .or_else(|| std::env::var_os("OPENSKYRIM_NIF_FIXTURE"))
            .map(PathBuf::from)
            .expect("set MUDCRAB_NIF_FIXTURE to a Skyrim NIF");
        let diagnostics = MeshConverter::inspect_nif(&path).unwrap();
        assert_eq!(diagnostics.geometry_block_count, 0);
        assert!(
            !diagnostics
                .block_types
                .keys()
                .any(|block_type| is_declared_geometry_block(block_type))
        );
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("non-renderable.glb");
        MeshConverter::convert_nif_to_glb(&path, &output).unwrap();
        let document = glb_json_from_bytes(&fs::read(output).unwrap()).unwrap();
        assert_eq!(document["scenes"][0]["nodes"], serde_json::json!([]));
        assert!(document.get("meshes").is_none());
    }

    #[test]
    #[ignore = "requires MUDCRAB_STATIC_NIF_FIXTURE with a locally installed Skyrim NIF"]
    fn static_fallback_converts_installed_nif_fixture() {
        let path = std::env::var_os("MUDCRAB_STATIC_NIF_FIXTURE")
            .or_else(|| std::env::var_os("OPENSKYRIM_STATIC_NIF_FIXTURE"))
            .map(PathBuf::from)
            .expect("set MUDCRAB_STATIC_NIF_FIXTURE to a Skyrim NIF");
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("static-fallback.glb");
        MeshConverter::convert_nif_to_glb(&path, &output).unwrap();
        MeshConverter::glb_bounds(&output).unwrap();
    }

    #[test]
    fn merged_skeleton_uses_winner_not_physical_neighbor() {
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("low/skeleton.nif");
        let winner = dir.path().join("overwrite/skeleton.nif");
        let relative = Path::new("meshes/actors/character/armor/body.nif");
        let key = PathBuf::from("meshes/actors/character/character assets/skeleton.nif");
        let mut sources = std::collections::BTreeMap::from([(key.clone(), lower.clone())]);
        assert_eq!(
            resolve_skeleton(relative, &sources),
            SkeletonResolution::Skeleton(key.clone())
        );
        sources.insert(key.clone(), winner);
        assert_eq!(
            resolve_skeleton(relative, &sources),
            SkeletonResolution::Skeleton(key.clone())
        );
        sources.remove(&key);
        assert_eq!(
            resolve_skeleton(relative, &sources),
            SkeletonResolution::SelfContained
        );
    }

    #[test]
    fn shared_blob_skeleton_resolves_to_the_chosen_key() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("shared-blob");
        let relative = Path::new("meshes/actors/character/armor/body.nif");
        // Sorts first, so a path-to-key lookup would find this entry before
        // the candidate that was actually chosen.
        let other = PathBuf::from("meshes/actors/aaa/skeleton.nif");
        let chosen = PathBuf::from("meshes/actors/character/character assets/skeleton.nif");
        let sources =
            std::collections::BTreeMap::from([(other, blob.clone()), (chosen.clone(), blob)]);
        assert_eq!(
            resolve_skeleton(relative, &sources),
            SkeletonResolution::Skeleton(chosen)
        );
    }

    #[test]
    fn derives_actor_root_case_insensitively() {
        let (root, actor) = actor_root(Path::new(
            "vfs/Meshes/Actors/Dragon/character assets/dragon.nif",
        ))
        .unwrap();
        assert_eq!(root, PathBuf::from("vfs/Meshes/Actors"));
        assert_eq!(actor, PathBuf::from("Dragon"));
    }

    #[test]
    fn derives_dlc_actor_root() {
        let (root, actor) = actor_root(Path::new(
            "Meshes/Actors/DLC02/Werebear/character assets/werebear.nif",
        ))
        .unwrap();
        assert_eq!(root, PathBuf::from("Meshes/Actors"));
        assert_eq!(actor, PathBuf::from("DLC02/Werebear"));
    }

    #[test]
    fn defers_only_dynamic_runtime_geometry() {
        assert!(is_deferred_dynamic_mesh(Path::new(
            "vfs/Meshes/Actors/Character/FaceGenData/FaceGeom/Skyrim.esm/00045CB1.nif"
        )));
        assert!(is_deferred_dynamic_mesh(Path::new(
            "vfs/meshes/actors/character/character assets/hair/elf/female/hair03.nif"
        )));
        assert!(is_deferred_dynamic_mesh(Path::new(
            "vfs/meshes/magic/lightningbolt01.nif"
        )));
        assert!(is_deferred_dynamic_mesh(Path::new(
            "vfs/meshes/effects/fxemptycontroller.nif"
        )));
        assert!(is_deferred_dynamic_mesh(Path::new(
            "vfs/meshes/creationclub/vsvsse003/actors/character/facegendata/facegeom/mod.esp/0000091a.nif"
        )));
        assert!(is_deferred_dynamic_mesh(Path::new(
            "vfs/meshes/creationclub/bgssse067/actors/ccbgssse067_deadrichorse.nif"
        )));
        assert!(!is_deferred_dynamic_mesh(Path::new(
            "vfs/meshes/architecture/whiterun/wrwall.nif"
        )));
        assert!(!is_deferred_dynamic_mesh(Path::new(
            "vfs/meshes/creationclub/bgssse001/clutter/tent.nif"
        )));
    }

    #[test]
    fn rewrites_glb_material_chunk_without_corrupting_the_container() {
        let mut json = br#"{"asset":{"version":"2.0"},"meshes":[{"primitives":[{}]}]}"#.to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 20 + json.len();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        let contract = [NifShapeMaterial {
            shape_block: 7,
            shape_name: Some("excluded".to_owned()),
            shader_property_block: None,
            alpha_property_block: None,
            disposition: NifMaterialDisposition::Excluded {
                reason: "fixture".to_owned(),
            },
        }];
        let rewritten =
            rewrite_materials_and_texture_uris(glb, &contract, &[7], Path::new("meshes/a.glb"))
                .unwrap();
        assert_eq!(
            u32::from_le_bytes(rewritten[8..12].try_into().unwrap()) as usize,
            rewritten.len()
        );
        let length = u32::from_le_bytes(rewritten[12..16].try_into().unwrap()) as usize;
        let document: serde_json::Value =
            serde_json::from_slice(&rewritten[20..20 + length]).unwrap();
        assert_eq!(document["materials"][0]["alphaMode"], "MASK");
        assert_eq!(document["meshes"][0]["primitives"][0]["material"], 0);
        assert_eq!(
            document["meshes"][0]["primitives"][0]["extras"]["openSkyrim"]["shapeBlock"],
            7
        );
    }

    #[test]
    fn extracts_bounds_with_node_transform() {
        let mut json = br#"{
            "asset":{"version":"2.0"},
            "scene":0,
            "scenes":[{"nodes":[0]}],
            "nodes":[{"mesh":0,"translation":[10,20,30],"scale":[2,3,4]}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"min":[-1,-2,-3],"max":[1,2,3]}]
        }"#
        .to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 20 + json.len();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        let bounds = glb_bounds_from_bytes(&glb).unwrap();
        assert_eq!(bounds.min, [8.0, 14.0, 18.0]);
        assert_eq!(bounds.max, [12.0, 26.0, 42.0]);
    }

    #[test]
    fn extracts_bounds_through_rotated_non_uniform_hierarchy() {
        let half_sqrt = std::f64::consts::FRAC_1_SQRT_2;
        let mut json = format!(
            r#"{{
                "asset":{{"version":"2.0"}},
                "scene":0,
                "scenes":[{{"nodes":[0]}}],
                "nodes":[
                    {{"children":[1],"translation":[10,0,0],"rotation":[0,0,{half_sqrt},{half_sqrt}],"scale":[2,1,1]}},
                    {{"mesh":0,"translation":[1,2,0],"rotation":[{half_sqrt},0,0,{half_sqrt}],"scale":[1,3,2]}}
                ],
                "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
                "accessors":[{{"min":[-1,-1,-1],"max":[1,1,1]}}]
            }}"#
        )
        .into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 20 + json.len();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);

        let bounds = glb_bounds_from_bytes(&glb).unwrap();
        for (actual, expected) in bounds.min.into_iter().zip([6.0, 0.0, -3.0]) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in bounds.max.into_iter().zip([10.0, 4.0, 3.0]) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }

    #[test]
    fn deep_glb_scene_keeps_the_leaf_bounds() {
        let mut nodes: Vec<_> = (1..20_000)
            .map(|i| serde_json::json!({"children": [i]}))
            .collect();
        nodes.push(serde_json::json!({"mesh":0}));
        let document = serde_json::json!({
            "scenes":[{"nodes":[0]}], "nodes":nodes,
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"min":[0,0,0],"max":[1,1,1]}]
        });
        let bounds = drawable_bounds(&document).unwrap().unwrap();
        assert!(
            bounds
                .max
                .iter()
                .all(|&value| (value - 1.0).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn glb_bounds_share_visit_budget_across_scene_roots() {
        let mut document = serde_json::json!({
            "scenes": [{"nodes": [0, 1]}],
            "nodes": [
                {"children": [2], "translation": [3, 0, 0]},
                {"children": [3], "translation": [-4, 0, 0]},
                {"mesh": 0},
                {"mesh": 0}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"min": [-1, -1, -1], "max": [1, 1, 1]}]
        });
        let bounds = drawable_bounds(&document).unwrap().unwrap();
        for (actual, expected) in bounds.min.into_iter().zip([-5.0, -1.0, -1.0]) {
            assert!((actual - expected).abs() < f32::EPSILON);
        }
        for (actual, expected) in bounds.max.into_iter().zip([4.0, 1.0, 1.0]) {
            assert!((actual - expected).abs() < f32::EPSILON);
        }

        document["nodes"][1]["children"][0] = 2.into();
        let repeated = drawable_bounds(&document).unwrap_err();
        assert_eq!(
            repeated.to_string(),
            "glTF node 2 occurs more than once in the scene"
        );
        document["nodes"].as_array_mut().unwrap().pop();
        let error = drawable_bounds(&document).unwrap_err();
        assert_eq!(
            error.to_string(),
            "glTF node traversal exceeds the node count"
        );
    }

    #[test]
    fn lists_external_glb_textures_deterministically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mesh.glb");
        let mut json = br#"{
            "asset":{"version":"2.0"},
            "images":[
                {"uri":"../textures/B.ktx2"},
                {"bufferView":0,"mimeType":"image/png"},
                {"uri":"../textures/a.ktx2"},
                {"uri":"../textures/A.ktx2"}
            ]
        }"#
        .to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 20 + json.len();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        fs::write(&path, glb).unwrap();

        assert_eq!(
            MeshConverter::glb_texture_uris(&path).unwrap(),
            vec!["../textures/a.ktx2", "../textures/B.ktx2"]
        );
    }

    #[test]
    fn classifies_required_and_optional_texture_dependencies() {
        let document = serde_json::json!({
            "images": [
                {"uri": "../textures/diffuse.ktx2"},
                {"uri": "../textures/normal.ktx2"}
            ],
            "textures": [{"source": 0}, {"source": 1}],
            "materials": [{
                "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}},
                "normalTexture": {"index": 1}
            }]
        });
        let dependencies = texture_dependencies(&document);
        assert_eq!(dependencies.len(), 2);
        assert!(dependencies.iter().any(|dependency| {
            dependency.semantic == TextureSemantic::BaseColor && dependency.required
        }));
        assert!(dependencies.iter().any(|dependency| {
            dependency.semantic == TextureSemantic::Normal && !dependency.required
        }));
    }

    fn glb_bytes(document: &serde_json::Value, binary: &[u8]) -> Vec<u8> {
        let mut json = serde_json::to_vec(document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut padded = binary.to_vec();
        while !padded.len().is_multiple_of(4) {
            padded.push(0);
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((20 + json.len() + 8 + padded.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&padded);
        glb
    }

    fn write_prune_fixture(root: &Path, document: &serde_json::Value) -> PathBuf {
        fs::create_dir_all(root.join("meshes/a")).unwrap();
        fs::create_dir_all(root.join("textures")).unwrap();
        fs::write(root.join("textures/keep.ktx2"), b"ktx2").unwrap();
        let glb = root.join("meshes/a/model.glb");
        fs::write(&glb, glb_bytes(document, b"\x01\x02\x03\x04\x05")).unwrap();
        glb
    }

    /// Legacy byte replay never repairs; the pipeline's repair-only pass preserves unrelated images.
    #[test]
    fn volcanic_legacy_prune_replay_stays_repair_free_and_repair_only_preserves_images() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let glb = root.join("meshes/landscape/grass/dlc02volcanicashgrass01.glb");
        fs::create_dir_all(glb.parent().unwrap()).unwrap();
        let target = root.join(VOLCANIC_NORMAL_TARGET);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, b"published target").unwrap();
        let original = "../../../textures/landscape/grass/volcanicashgrass02_n.ktx2";
        let missing = "../../../textures/unrelated-missing.ktx2";
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "images": [{"uri": original}, {"uri": missing}],
            "textures": [{"source": 0, "sampler": 1}, {"source": 1, "sampler": 0}],
            "samplers": [{"wrapS": 33071}, {"wrapS": 10497}],
            "materials": [{
                "normalTexture": {"index": 0},
                "emissiveTexture": {"index": 1},
                "extensions": {"KHR_materials_specular": {"specularTexture": {"index": 0}}}
            }]
        });
        let bytes = glb_bytes(&document, b"geometry survives both paths");
        let (legacy, removed) = prune_glb_texture_bytes(root, &glb, &bytes)
            .unwrap()
            .unwrap();
        assert_eq!(removed, [original, missing]);
        let legacy_document = glb_json_from_bytes(&legacy).unwrap();
        assert!(legacy_document["images"].as_array().unwrap().is_empty());
        assert!(legacy_document.get("extras").is_none());

        fs::write(&glb, &bytes).unwrap();
        let reports = MeshConverter::repair_volcanic_grass_normals(root, &BTreeSet::new()).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].removed_uris.is_empty());
        let replacement = "../../../textures/dlc02/landscape/grass/volcanicashgrass02_n.ktx2";
        assert_eq!(reports[0].repaired_uris.get(original).unwrap(), replacement);
        let repaired = glb_json_from_bytes(&fs::read(&glb).unwrap()).unwrap();
        assert_eq!(repaired["images"][0]["uri"], replacement);
        assert_eq!(repaired["images"][1]["uri"], missing);
        assert_eq!(repaired["textures"], document["textures"]);
        assert_eq!(repaired["materials"], document["materials"]);
        assert_eq!(repaired["samplers"], document["samplers"]);
        assert_eq!(
            repaired["extras"]["mudcrab"]["texturePathRepairs"][original],
            replacement
        );
    }

    /// The shipped two-model repair preserves all uses and does not affect other assets.
    #[test]
    fn volcanic_normal_repair_is_scoped_preserves_overrides_and_all_texture_uses() {
        for model in [
            "dlc02volcanicashgrass01",
            "dlc02volcanicashgrass02",
            "control",
        ] {
            for suffix in std::iter::once("").chain(shared::texture_aliases::WRAP_TEXTURE_SUFFIXES)
            {
                for (original, target, original_source) in [
                    (false, true, false),
                    (true, true, true),
                    (false, false, false),
                    (false, true, true),
                ] {
                    let temp = tempfile::tempdir().unwrap();
                    let root = temp.path();
                    let glb = root.join(format!("meshes/landscape/grass/{model}.glb"));
                    fs::create_dir_all(glb.parent().unwrap()).unwrap();
                    for (present, path) in [
                        (
                            original,
                            "textures/landscape/grass/volcanicashgrass02_n.ktx2",
                        ),
                        (
                            target,
                            "textures/dlc02/landscape/grass/volcanicashgrass02_n.ktx2",
                        ),
                    ] {
                        if present {
                            fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
                            fs::write(root.join(path), b"texture").unwrap();
                            fs::write(
                                root.join(path.replace(".ktx2", &format!("{suffix}.ktx2"))),
                                b"texture",
                            )
                            .unwrap();
                        }
                    }
                    let document = serde_json::json!({"asset":{"version":"2.0"},
                    "images":[{"uri":format!("../../../textures/landscape/grass/volcanicashgrass02_n{suffix}.ktx2")}],
                    "textures":[{"source":0,"sampler":0},{"source":0,"sampler":1}], "samplers":[{"wrapS":10497,"wrapT":10497},{"wrapS":33071,"wrapT":33648}],
                    "materials":[{"normalTexture":{"index":1},"extensions":{"KHR_materials_specular":{"specularTexture":{"index":1}}}}]});
                    let before = glb_bytes(&document, b"distinct geometry bytes");
                    fs::write(&glb, &before).unwrap();
                    let sources = if original_source {
                        BTreeSet::from([
                            "textures/landscape/grass/volcanicashgrass02_n.dds".to_owned()
                        ])
                    } else {
                        BTreeSet::new()
                    };
                    MeshConverter::prune_dangling_texture_uris_with_sources(root, &sources)
                        .unwrap();
                    let after = fs::read(&glb).unwrap();
                    let actual = glb_json_from_bytes(&after).unwrap();
                    let repaired = model != "control" && !original && !original_source && target;
                    if original || original_source || repaired {
                        assert_eq!(
                            actual["images"][0]["uri"],
                            if repaired {
                                format!(
                                    "../../../textures/dlc02/landscape/grass/volcanicashgrass02_n{suffix}.ktx2"
                                )
                            } else {
                                format!(
                                    "../../../textures/landscape/grass/volcanicashgrass02_n{suffix}.ktx2"
                                )
                            }
                        );
                        assert_eq!(actual["textures"], document["textures"]);
                        assert_eq!(actual["materials"], document["materials"]);
                        assert_eq!(actual["samplers"], document["samplers"]);
                    } else {
                        assert!(actual["images"].as_array().unwrap().is_empty());
                        assert!(actual["materials"][0].get("normalTexture").is_none());
                    }
                    let before_len =
                        u32::from_le_bytes(before[12..16].try_into().unwrap()) as usize;
                    let after_len = u32::from_le_bytes(after[12..16].try_into().unwrap()) as usize;
                    assert_eq!(&before[20 + before_len..], &after[20 + after_len..]);
                    assert!(
                        MeshConverter::prune_dangling_texture_uris_with_sources(root, &sources)
                            .unwrap()
                            .is_empty()
                    );
                    assert_eq!(fs::read(&glb).unwrap(), after);
                }
            }
        }
    }

    #[test]
    fn prunes_missing_images_and_remaps_dependents() {
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "images": [
                {"uri": "../../textures/keep.ktx2"},
                {"uri": "../../textures/gone.ktx2"},
                {"bufferView": 0, "mimeType": "image/png"}
            ],
            "textures": [{"source": 0}, {"source": 1}, {"source": 2}],
            "materials": [{
                "pbrMetallicRoughness": {
                    "baseColorTexture": {"index": 0},
                    "metallicRoughnessTexture": {"index": 1}
                },
                "normalTexture": {"index": 1},
                "emissiveTexture": {"index": 2},
                "extensions": {"OPEN_SKYRIM_material": {"textureSlots": [
                    {"slot": "glow", "texture": 1},
                    {"slot": "detail", "texture": 2}
                ]}}
            }]
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let glb = write_prune_fixture(root, &document);
        let before = fs::read(&glb).unwrap();

        let report = MeshConverter::prune_dangling_texture_uris(root).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].glb, "meshes/a/model.glb");
        assert_eq!(
            report[0].removed_uris,
            vec!["../../textures/gone.ktx2".to_owned()]
        );

        let bytes = fs::read(&glb).unwrap();
        let pruned = glb_json_from_bytes(&bytes).unwrap();
        assert_eq!(pruned["images"].as_array().unwrap().len(), 2);
        assert_eq!(pruned["images"][0]["uri"], "../../textures/keep.ktx2");
        assert!(pruned["images"][1].get("uri").is_none());
        assert_eq!(pruned["textures"].as_array().unwrap().len(), 2);
        assert_eq!(pruned["textures"][0]["source"], 0);
        assert_eq!(pruned["textures"][1]["source"], 1);
        let material = &pruned["materials"][0];
        assert_eq!(
            material["pbrMetallicRoughness"]["baseColorTexture"]["index"],
            0
        );
        assert!(
            material["pbrMetallicRoughness"]
                .get("metallicRoughnessTexture")
                .is_none()
        );
        assert!(material.get("normalTexture").is_none());
        assert_eq!(material["emissiveTexture"]["index"], 1);
        assert_eq!(
            material["extensions"]["OPEN_SKYRIM_material"]["textureSlots"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            material["extensions"]["OPEN_SKYRIM_material"]["textureSlots"][0]["texture"],
            1
        );
        // The binary chunk survives byte-identical.
        let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let before_json_length = u32::from_le_bytes(before[12..16].try_into().unwrap()) as usize;
        assert_eq!(
            &bytes[20 + json_length..],
            &before[20 + before_json_length..]
        );

        // Pruning is idempotent: a second pass rewrites nothing.
        assert!(
            MeshConverter::prune_dangling_texture_uris(root)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(&glb).unwrap(), bytes);
    }

    #[test]
    fn v11_prunes_and_remaps_both_specular_textures_without_changing_strength() {
        let mut document = serde_json::json!({
            "images": [{"uri":"a.ktx2"},{"uri":"gone.ktx2"},{"uri":"b.ktx2"}],
            "textures": [{"source":0},{"source":1},{"source":2}],
            "materials": [
                {"normalTexture":{"index":2},"extensions":{"KHR_materials_specular":{
                    "specularFactor":0.8,"specularTexture":{"index":2},"specularColorTexture":{"index":0}
                }}},
                {"extensions":{"KHR_materials_specular":{
                    "specularFactor":0.8,"specularTexture":{"index":1},"specularColorTexture":{"index":2}
                }}}
            ]
        });
        prune_document_images(&mut document, &HashSet::from([1]));
        let retained = &document["materials"][0];
        let specular = &retained["extensions"]["KHR_materials_specular"];
        assert_eq!(specular["specularTexture"]["index"], 1);
        assert_eq!(specular["specularColorTexture"]["index"], 0);
        assert_eq!(retained["normalTexture"]["index"], 1);
        assert_eq!(specular["specularFactor"], 0.8);
        let dropped = &document["materials"][1]["extensions"]["KHR_materials_specular"];
        assert!(dropped.get("specularTexture").is_none());
        assert_eq!(dropped["specularColorTexture"]["index"], 1);
        assert_eq!(dropped["specularFactor"], 0.8);
    }

    #[test]
    fn leaves_clean_trees_untouched() {
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "images": [{"uri": "../../textures/keep.ktx2"}],
            "textures": [{"source": 0}],
            "materials": [{
                "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}
            }]
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let glb = write_prune_fixture(root, &document);
        let before = fs::read(&glb).unwrap();
        assert!(
            MeshConverter::prune_dangling_texture_uris(root)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(&glb).unwrap(), before);
    }

    #[test]
    fn keeps_embedded_and_remote_uris() {
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "images": [
                {"uri": "data:image/png;base64,iVBORw0KGgo="},
                {"uri": "https://example.com/textures/remote.ktx2"}
            ],
            "textures": [{"source": 0}, {"source": 1}]
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let glb = write_prune_fixture(root, &document);
        let before = fs::read(&glb).unwrap();
        assert!(
            MeshConverter::prune_dangling_texture_uris(root)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(&glb).unwrap(), before);
    }

    #[test]
    fn v14_vertex_channels_follow_flags_and_tree_exclusions() {
        use crate::material::{LightingShaderType, NifShaderFamily, ValidatedNifMaterial};
        use project_wormhole_nif::model::all::{Model, StaticMesh, StaticSceneNode};
        use project_wormhole_shared::glam::{Mat3, Vec3, Vec4};
        use project_wormhole_shared::prelude::BSVec4;

        fn node(block_index: u32, mesh: usize) -> StaticSceneNode {
            StaticSceneNode {
                block_index,
                name: None,
                translation: Vec3::ZERO,
                rotation: Mat3::IDENTITY,
                scale: 1.0,
                children: Vec::new(),
                mesh: Some(mesh),
            }
        }

        fn shape(block: u32, alpha_mode: NifAlphaMode) -> NifShapeMaterial {
            NifShapeMaterial {
                shape_block: block,
                shape_name: None,
                shader_property_block: None,
                alpha_property_block: None,
                disposition: NifMaterialDisposition::Validated {
                    material: ValidatedNifMaterial {
                        uv_offset: [0.0; 2],
                        uv_scale: [1.0; 2],
                        texture_clamp_mode: 3,
                        tint_color: None,
                        shader_family: NifShaderFamily::Lighting,
                        lighting_shader_type: Some(LightingShaderType::Default),
                        shader_block: 0,
                        texture_set_block: None,
                        alpha_property_block: None,
                        alpha_property: None,
                        shader_flags_1: if alpha_mode == NifAlphaMode::Opaque {
                            1 << 3
                        } else {
                            0
                        },
                        shader_flags_2: 1 << 5,
                        base_color: [1.0, 1.0, 1.0, 1.0],
                        alpha: 1.0,
                        alpha_mode,
                        alpha_threshold: Some(112),
                        glossiness: 0.0,
                        specular_color: [0.0, 0.0, 0.0],
                        specular_strength: 0.0,
                        emissive_color: [0.0, 0.0, 0.0],
                        emissive_multiple: 1.0,
                        double_sided: true,
                        textures: Vec::new(),
                    },
                },
            }
        }

        for rgb in [false, true] {
            for alpha in [false, true] {
                for tree in [false, true] {
                    let mut contract = shape(7, NifAlphaMode::Cutout);
                    let NifMaterialDisposition::Validated { material } = &mut contract.disposition
                    else {
                        unreachable!()
                    };
                    material.shader_flags_1 = if alpha { 1 << 3 } else { 0 };
                    material.shader_flags_2 =
                        if rgb { 1 << 5 } else { 0 } | if tree { 1 << 29 } else { 0 };
                    assert_eq!(vertex_color_channels(material), (rgb, alpha && !tree));
                    for family in [
                        LightingShaderType::TreeAnimation,
                        LightingShaderType::LodObjects,
                        LightingShaderType::LodObjectsHd,
                    ] {
                        material.lighting_shader_type = Some(family);
                        assert_eq!(vertex_color_channels(material), (rgb, false));
                    }
                }
            }
        }

        let mut model = Model {
            name: None,
            static_meshes: vec![
                StaticMesh {
                    colors: vec![
                        BSVec4(Vec4::new(1.0, 0.5, 0.25, 0.0)),
                        BSVec4(Vec4::new(0.0, 1.0, 0.5, 0.44)),
                    ],
                    ..StaticMesh::default()
                },
                StaticMesh {
                    colors: vec![BSVec4(Vec4::new(1.0, 1.0, 1.0, 0.0))],
                    ..StaticMesh::default()
                },
            ],
            static_nodes: vec![node(7, 0), node(9, 1)],
            skeletal_meshes: Vec::new(),
            materials: Vec::new(),
            material_indices: Vec::new(),
            scene_root_rotation: None,
        };
        let nif = NifFile {
            header: NifHeader {
                file_desc: StringN {
                    value: String::new(),
                },
                nif_version: NifFileVersion(0),
                endian_type: Endianess::Little,
                user_version: 0,
                block_count: 0,
                bethesda_version: 0,
                author: None,
                process_script: None,
                export_script: None,
                max_filepath: None,
                block_types: Vec::new(),
                block_type_index: Vec::new(),
                block_size_index: Vec::new(),
                string_count: 0,
                string_max_size: 0,
                strings: Vec::new(),
                groups: Vec::new(),
            },
            blocks: Vec::new(),
        };
        let contract = vec![
            shape(7, NifAlphaMode::Cutout),
            shape(9, NifAlphaMode::Opaque),
        ];

        apply_vertex_color_contract(&mut model, &nif, &contract).unwrap();

        let cutout = &model.static_meshes[0].colors;
        assert_eq!(cutout.len(), 2);
        for color in cutout {
            assert_eq!(color.0.w, 1.0);
        }
        assert_eq!(cutout[0].0.x, 1.0);
        assert_eq!(cutout[0].0.y, 0.5);
        assert_eq!(cutout[0].0.z, 0.25);
        let opaque = &model.static_meshes[1].colors;
        assert_eq!(opaque.len(), 1);
        assert_eq!(opaque[0].0.w, 0.0);
    }

    #[test]
    fn v16_only_declared_markers_and_refraction_are_excluded() {
        use nom_derive::Parse;
        use project_wormhole_nif::nif_block::{BSXFlags, Fallout4ShaderPropertyFlags1};
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("source.nif");
        fs::write(&input, static_nif()).unwrap();
        for marker_name in [false, true] {
            for marker_flag in [false, true] {
                for refraction in [false, true] {
                    let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
                    nif.header.strings[0] = SizedString32(
                        if marker_name {
                            "EditorMarker"
                        } else {
                            "Visible"
                        }
                        .into(),
                    );
                    nif.blocks.push(NifBlock::BSXFlags(BSXFlags {
                        ni_extra_data: 0,
                        integer_data: if marker_flag { 1 << 5 } else { 0 },
                    }));
                    let NifBlock::BSLightingShaderProperty(p) = &mut nif.blocks[2] else {
                        unreachable!()
                    };
                    p.shader_flags_1 = Fallout4ShaderPropertyFlags1::parse(
                        &(if refraction { 1u32 << 16 } else { 0 }).to_le_bytes(),
                    )
                    .unwrap()
                    .1;
                    let contract = build_nif_material_contract(&nif, &input).unwrap();
                    assert_eq!(
                        matches!(
                            &contract[0].disposition,
                            NifMaterialDisposition::Excluded { .. }
                        ),
                        refraction || (marker_name && marker_flag)
                    );
                    let mut document = serde_json::json!({"asset":{"version":"2.0"},"meshes":[{"primitives":[{}]}]});
                    publish_gltf_materials(&mut document, &contract, &[1], Path::new("mesh.glb"))
                        .unwrap();
                    if refraction || (marker_name && marker_flag) {
                        assert_eq!(document["materials"][0]["alphaMode"], "MASK");
                        assert_eq!(
                            document["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"][3],
                            0.0
                        );
                        assert!(document["meshes"][0]["primitives"][0]["extras"]["openSkyrim"]["materialExclusion"].as_str().is_some());
                    }
                }
            }
        }
    }

    /// `weapons/iron/irondagger.nif` carries its blood overlays as shapes with the `Hidden` flag
    /// set; `dlc2smelter.nif` hides a whole node. Neither draws in Skyrim.
    #[test]
    fn hidden_shapes_and_shapes_under_hidden_nodes_are_excluded() {
        use nom_derive::Parse;
        use project_wormhole_nif::{legacy::NiTriShape, nif_block::NiAVObjectFlags};
        use project_wormhole_shared::glam::{Mat3, Vec3};
        let flags = |hidden: bool| {
            NiAVObjectFlags::parse(&(0x0008_000E_u32 | u32::from(hidden)).to_le_bytes())
                .unwrap()
                .1
        };
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("source.nif");
        fs::write(&input, static_nif()).unwrap();
        let cases = [false, true].into_iter().flat_map(|node| {
            [false, true]
                .into_iter()
                .flat_map(move |shape| [(node, shape, false), (node, shape, true)])
        });
        for (node_hidden, shape_hidden, legacy) in cases {
            let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
            let (NifBlock::NiNode(node) | NifBlock::BSFadeNode(node)) = &mut nif.blocks[0] else {
                unreachable!()
            };
            node.set_flags(flags(node_hidden));
            let NifBlock::BSTriShape(shape) = &mut nif.blocks[1] else {
                unreachable!()
            };
            shape.av.flags = flags(shape_hidden);
            if legacy {
                // The same shape as a legacy NiTriShape, whose own flags the parser keeps too.
                nif.blocks[1] = NifBlock::NiTriShape(NiTriShape {
                    name: shape.av.object.name,
                    flags: flags(shape_hidden),
                    translation: Vec3::ZERO,
                    rotation: Mat3::IDENTITY,
                    scale: 1.0,
                    data: u32::MAX,
                    shader_property: shape.shader_property,
                    alpha_property: shape.alpha_property,
                });
            }
            let contract = build_nif_material_contract(&nif, &input).unwrap();
            let hidden = node_hidden || shape_hidden;
            match &contract[0].disposition {
                NifMaterialDisposition::Excluded { reason } => {
                    assert!(hidden, "legacy={legacy}");
                    assert_eq!(reason, "hidden NiAVObject");
                }
                NifMaterialDisposition::Validated { .. } => assert!(!hidden, "legacy={legacy}"),
            }
        }
    }

    #[test]
    fn v15_source_uv_fields_survive_nif_contract() {
        use project_wormhole_nif::nif_enum::TexClampMode;
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("uv.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let NifBlock::BSLightingShaderProperty(p) = &mut nif.blocks[2] else {
            unreachable!()
        };
        p.uv_offset.0 = project_wormhole_shared::glam::Vec2::new(0.25, -0.125);
        p.uv_scale.0 = project_wormhole_shared::glam::Vec2::new(2.0, -3.0);
        p.texture_clamp_mode = TexClampMode::WrapSClampT;
        let contract = build_nif_material_contract(&nif, &input).unwrap();
        let NifMaterialDisposition::Validated { material } = &contract[0].disposition else {
            unreachable!()
        };
        assert_eq!(material.uv_offset, [0.25, -0.125]);
        assert_eq!(material.uv_scale, [2.0, -3.0]);
        assert_eq!(material.texture_clamp_mode, 2);
    }

    #[test]
    fn v14_modern_nif_colors_survive_export_with_authored_alpha() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("colors.nif");
        let output = directory.path().join("colors.glb");
        let colors = [[64, 128, 192, 0], [255, 32, 16, 96], [10, 20, 30, 255]];
        let bytes = dummy_content::nif::static_shape_with_colors(
            &dummy_content::nif::StaticShape {
                name: "Colors",
                positions: &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
                normals: &[[0., 0., 1.]; 3],
                uvs: &[[0., 0.], [1., 0.], [0., 1.]],
                indices: &[[0, 1, 2]],
                diffuse: "textures/color.dds",
                normal_texture: "textures/normal.dds",
            },
            &colors,
        )
        .unwrap();
        fs::write(&input, bytes).unwrap();
        MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
        let bytes = fs::read(output).unwrap();
        let gltf = gltf::Gltf::from_slice(&bytes).unwrap();
        let mesh = gltf.meshes().next().unwrap();
        let primitive = mesh.primitives().next().unwrap();
        let actual: Vec<_> = primitive
            .reader(|_| gltf.blob.as_deref())
            .read_colors(0)
            .unwrap()
            .into_rgba_f32()
            .collect();
        for (actual, source) in actual.iter().zip(colors) {
            for (a, b) in actual.iter().zip(source) {
                assert!((a - f32::from(b) / 255.).abs() < 1e-6);
            }
        }
        assert_eq!(
            primitive.material().alpha_mode(),
            gltf::material::AlphaMode::Opaque
        );
    }

    #[test]
    fn assignment_sphere_limits_repeated_child_expansion() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("shared-children.nif");
        fs::write(&input, static_scene_chain_nif(12)).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let NifBlock::BSTriShape(shape) = &mut nif.blocks[1] else {
            unreachable!()
        };
        shape.bounding_sphere.radius = 2.0;
        assert!(room_bounds::source_sphere(&nif).is_some());
        for block in &mut nif.blocks {
            if let NifBlock::NiNode(node) | NifBlock::BSFadeNode(node) = block {
                node.children.extend_from_within(..);
            }
        }
        assert!(room_bounds::source_sphere(&nif).is_none());
    }

    #[test]
    fn switch_assignment_bound_uses_the_selected_child_or_merges_children() {
        use project_wormhole_nif::nif_block::NiSwitchNode;
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("switch.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let mut root = nif.blocks[0].as_node().unwrap().clone();
        let mut first = room_bounds::source_sphere(&nif).unwrap();
        first.center[0] += 17.0;
        let (mut other, _, _) = open_nif_resilient(&input).unwrap();
        let mut child = other.blocks.remove(1);
        let NifBlock::BSTriShape(shape) = &mut child else {
            unreachable!()
        };
        shape.av.translation.0.0.x += 1000.0;
        let mut second = first;
        second.center[0] += 1000.0;
        let branch_index = u32::try_from(nif.blocks.len()).unwrap();
        let mut branch = root.clone();
        branch.children = vec![branch_index + 1];
        root.children.push(branch_index);
        nif.blocks.push(NifBlock::NiNode(branch));
        nif.blocks.push(child);
        let switch_index = nif.blocks.len();
        let switch_block = u32::try_from(switch_index).unwrap();
        let mut scene = root.clone();
        scene.children = vec![switch_block];
        nif.blocks[0] = NifBlock::NiNode(scene);
        nif.blocks.push(NifBlock::Unhandled);
        root.av.translation.0.0.x = 17.0;
        for (flags, active_child, expected, visible) in [
            (3, 0, Some(first), [true, false]),
            (3, 1, Some(second), [false, true]),
            (0, 1, Some(first.merge(second)), [false, true]),
            (1, -1, None, [false, false]),
            (1, 2, None, [true, true]),
        ] {
            nif.blocks[switch_index] = NifBlock::NiSwitchNode(NiSwitchNode {
                node: root.clone(),
                flags,
                active_child,
            });
            assert_eq!(room_bounds::source_sphere(&nif), expected);
            let model = nif_to_static_model(&nif).unwrap();
            assert_eq!(model.static_nodes[0].children, [switch_block]);
            let switch = model.static_nodes.last().unwrap();
            assert_eq!(switch.children, root.children);
            assert!((switch.translation.x - 17.0).abs() < f32::EPSILON);
            let contract = build_nif_material_contract(&nif, &input).unwrap();
            let blocks = exported_shape_blocks(&nif, &model, &contract).unwrap();
            let glb = model.to_glb("switch".into()).unwrap();
            let mut document = glb_json_from_bytes(&glb).unwrap();
            room_bounds::select_switch_geometry(&nif, &blocks, &mut document).unwrap();
            let bytes = rebuild_glb_with_document(&glb, &document).unwrap();
            let gltf = gltf::Gltf::from_slice(&bytes).unwrap();
            let mut drawn = [false; 2];
            let mut stack: Vec<_> = gltf.scenes().next().unwrap().nodes().collect();
            while let Some(node) = stack.pop() {
                if let Some(mesh) = node.mesh() {
                    drawn[mesh.index()] = true;
                }
                stack.extend(node.children());
            }
            assert_eq!(drawn, visible);
            let file = directory.path().join("switch.glb");
            std::fs::write(&file, &bytes).unwrap();
            assert_eq!(
                MeshConverter::drawable_bounds(&file).unwrap().is_some(),
                visible.contains(&true)
            );
            assert_eq!(
                glb_bounds_from_bytes(&bytes).is_ok(),
                visible.contains(&true)
            );
            if active_child < 0 {
                // Skinned meshes can be independent scene roots. An inactive switch keeps both detached.
                let mut direct_roots = serde_json::json!({
                    "nodes": [{"mesh": 0, "skin": 0}, {"mesh": 1, "skin": 0}],
                    "scenes": [{"nodes": [0, 1]}]
                });
                room_bounds::select_switch_geometry(&nif, &blocks, &mut direct_roots).unwrap();
                assert_eq!(direct_roots["scenes"][0]["nodes"], serde_json::json!([2]));
                assert_eq!(direct_roots["nodes"][2], serde_json::json!({}));
                assert_eq!(drawable_bounds(&direct_roots).unwrap(), None);
            }
        }
    }

    #[test]
    fn signed_switch_selectors_control_converted_geometry_and_bounds() {
        use shared::room_assignment::ModelSphere;

        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("switch.nif");
        let output = directory.path().join("switch.glb");
        let first = ModelSphere {
            center: [0.0; 3],
            radius: std::f32::consts::SQRT_2.next_up(),
        };
        let second = ModelSphere {
            center: [1000.0, 0.0, 0.0],
            ..first
        };
        for (flags, selector, mesh_count, x_bounds, sphere) in [
            (1, -2, 0, None, None),
            (1, -1, 0, None, None),
            (1, 1, 1, Some((999.0, 1001.0)), Some(second)),
            (1, 2, 2, Some((-1.0, 1001.0)), None),
            (0, -2, 0, None, Some(first.merge(second))),
            (0, 2, 2, Some((-1.0, 1001.0)), Some(first.merge(second))),
        ] {
            let nif = dummy_content::nif::static_switch(&static_fixture_shape(), flags, selector)
                .unwrap();
            fs::write(&input, nif).unwrap();
            let diagnostics = MeshConverter::inspect_nif(&input).unwrap();
            assert!(diagnostics.conversion_refusals.is_empty());
            let warnings = diagnostics.conversion_warnings;
            if selector == 2 {
                assert_eq!(warnings.len(), 1);
                let warning = &warnings[0];
                assert!(warning.contains("NiSwitchNode block 4"));
                assert!(warning.contains("selector 2 outside 2 child slots"));
            } else {
                assert!(warnings.is_empty());
            }
            MeshConverter::convert_nif_to_glb(&input, &output).unwrap();

            let bytes = fs::read(&output).unwrap();
            let gltf = gltf::Gltf::from_slice(&bytes).unwrap();
            let mut pending: Vec<_> = gltf.default_scene().unwrap().nodes().collect();
            let mut drawn = 0;
            while let Some(node) = pending.pop() {
                drawn += usize::from(node.mesh().is_some());
                pending.extend(node.children());
            }
            let actual_bounds = MeshConverter::drawable_bounds(&output)
                .unwrap()
                .map(|bounds| (bounds.min[0], bounds.max[0]));
            let document = glb_json_from_bytes(&bytes).unwrap();
            let actual_sphere: Option<ModelSphere> =
                serde_json::from_value(document["extras"]["mudcrabRoomSphere"].clone()).unwrap();
            assert_eq!(
                (drawn, actual_bounds, actual_sphere),
                (mesh_count, x_bounds, sphere),
                "flags {flags}, selector {selector}"
            );
        }
    }

    #[test]
    fn model_box_supplies_room_sphere_even_with_empty_value_nodes() {
        use project_wormhole_nif::nif_block::{BSMultiBound, BSMultiBoundNode, BSValueNode};
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("box.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let mut root = nif.blocks[0].as_node().unwrap().clone();
        let mut empty = root.clone();
        empty.children.clear();
        let value = u32::try_from(nif.blocks.len()).unwrap();
        nif.blocks.push(NifBlock::BSValueNode(BSValueNode {
            node: empty,
            value: 49,
            value_node_flags: 1,
        }));
        let container = u32::try_from(nif.blocks.len()).unwrap();
        let mut bound_ref = root.av.collision_object;
        bound_ref.0 = Some(container + 1);
        let mut data_ref = bound_ref;
        data_ref.0 = Some(container + 2);
        nif.blocks
            .push(NifBlock::BSMultiBoundNode(BSMultiBoundNode {
                node: root.clone(),
                bound: bound_ref,
                culling_mode: Some(1),
            }));
        nif.blocks
            .push(NifBlock::BSMultiBound(BSMultiBound { data: data_ref }));
        let values: [f32; 15] = [1., 2., 3., 3., 4., 12., 1., 0., 0., 0., 1., 0., 0., 0., 1.];
        let bytes: Vec<u8> = values.into_iter().flat_map(f32::to_le_bytes).collect();
        nif.blocks
            .push(NifBlock::parse(&bytes, "BSMultiBoundOBB".into()).unwrap().1);
        root.children = vec![value, container];
        nif.blocks[0] = NifBlock::NiNode(root);
        assert_eq!(
            room_bounds::source_sphere(&nif),
            Some(shared::room_assignment::ModelSphere {
                center: [1., 3., -2.],
                radius: 13.,
            })
        );
        nif.blocks.pop();
        assert_eq!(room_bounds::source_sphere(&nif), None);
    }

    #[test]
    fn assignment_sphere_declines_disconnected_exported_geometry() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("disconnected.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let (mut second, _, _) = open_nif_resilient(&input).unwrap();
        let mut orphan = second.blocks.remove(1);
        let NifBlock::BSTriShape(shape) = &mut orphan else {
            unreachable!()
        };
        shape.av.translation.0.0.x = 1000.0;
        nif.blocks.push(orphan);
        let model = project_wormhole_nif::nif_file::nif_to_static_model(&nif).unwrap();
        assert_eq!(model.static_meshes.len(), 2);
        assert!(room_bounds::source_sphere(&nif).is_none());
    }

    #[test]
    fn assignment_sphere_keeps_source_bounds_and_ignores_the_reference_root_transform() {
        use project_wormhole_shared::glam::{Mat3, Vec3};
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("source.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let (NifBlock::NiNode(root) | NifBlock::BSFadeNode(root)) = &mut nif.blocks[0] else {
            unreachable!()
        };
        root.av.translation.0.0 = Vec3::splat(1000.0);
        root.av.scale.0 = 10.0;
        root.av.rotation.0.0 = Mat3::from_diagonal(Vec3::new(-1.0, 1.0, 1.0));
        let NifBlock::BSTriShape(shape) = &mut nif.blocks[1] else {
            unreachable!()
        };
        shape.bounding_sphere.center = [10.0, 20.0, 30.0];
        shape.bounding_sphere.radius = 2.0;
        shape.av.translation.0.0 = Vec3::new(3.0, 4.0, 5.0);
        shape.av.rotation.0.0 = Mat3::IDENTITY;
        shape.av.scale.0 = 2.0;
        assert_eq!(
            room_bounds::source_sphere(&nif),
            Some(shared::room_assignment::ModelSphere {
                center: [23.0, 65.0, -44.0],
                radius: 4.0,
            })
        );
        for (rotation, scale, center) in [
            (Mat3::IDENTITY, -2.0, [-17.0, -55.0, 36.0]),
            (
                Mat3::from_rotation_z(std::f32::consts::FRAC_PI_2),
                2.0,
                [-37.0, 65.0, -24.0],
            ),
        ] {
            let NifBlock::BSTriShape(shape) = &mut nif.blocks[1] else {
                unreachable!()
            };
            shape.av.rotation.0.0 = rotation;
            shape.av.scale.0 = scale;
            let sphere = room_bounds::source_sphere(&nif).unwrap();
            assert!((sphere.radius - 4.0).abs() < f32::EPSILON);
            for (actual, expected) in sphere.center.into_iter().zip(center) {
                assert!((actual - expected).abs() < 1.0e-4);
            }
        }
        nif.blocks[1] = NifBlock::Unhandled;
        assert!(room_bounds::source_sphere(&nif).is_none());
    }

    #[test]
    fn nested_leaf_nodes_export_their_transform_and_child_geometry() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("nested-leaf.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let index = u32::try_from(nif.blocks.len()).unwrap();
        let mut leaf = nif.blocks[0].as_node().unwrap().clone();
        leaf.av.translation.0.0.x = 3.0;
        match &mut nif.blocks[0] {
            NifBlock::BSFadeNode(root) => root.children = vec![index],
            _ => unreachable!(),
        }
        nif.blocks.push(NifBlock::BSLeafAnimNode(leaf));
        for model in [
            nif_to_static_model(&nif).unwrap(),
            nif_to_model_with_skin_fallback(&nif, None, None).unwrap().0,
        ] {
            assert_eq!(model.static_meshes.len(), 1);
            let root = model
                .static_nodes
                .iter()
                .find(|node| node.block_index == 0)
                .unwrap();
            assert_eq!(root.children, vec![index]);
            let leaf = model
                .static_nodes
                .iter()
                .find(|node| node.block_index == index)
                .unwrap();
            assert!((leaf.translation.x - 3.0).abs() < f32::EPSILON);
            assert_eq!(leaf.children, vec![1]);
        }
    }

    #[test]
    fn leaf_roots_export_assignment_bounds_and_keep_child_geometry() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("leaf.nif");
        let output = directory.path().join("leaf.glb");
        let mut bytes = static_nif();
        let old = b"BSFadeNode";
        let start = bytes
            .windows(old.len())
            .position(|part| part == old)
            .unwrap();
        let name = b"BSLeafAnimNode";
        bytes.splice(
            start - 4..start + old.len(),
            u32::try_from(name.len())
                .unwrap()
                .to_le_bytes()
                .into_iter()
                .chain(name.iter().copied()),
        );
        fs::write(&input, bytes).unwrap();
        let (nif, diagnostics, _) = open_nif_resilient(&input).unwrap();
        assert!(diagnostics.fallback_blocks.is_empty());
        assert_eq!(nif.blocks[0].as_node().unwrap().children, vec![1]);
        let expected = room_bounds::source_sphere(&nif).unwrap();
        MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
        let bytes = fs::read(output).unwrap();
        let document = glb_json_from_bytes(&bytes).unwrap();
        assert_eq!(
            document["extras"]["mudcrabRoomSphere"]["radius"],
            expected.radius
        );
        assert_eq!(
            document["extras"]["mudcrabRoomSphere"]["center"],
            serde_json::json!(expected.center)
        );
        let gltf = gltf::Gltf::from_slice(&bytes).unwrap();
        assert_eq!(gltf.meshes().count(), 1);
        assert_eq!(
            gltf.meshes()
                .next()
                .unwrap()
                .primitives()
                .next()
                .unwrap()
                .indices()
                .unwrap()
                .count(),
            6
        );
    }

    #[test]
    fn ordered_node_derives_fixed_bound_from_static_byte_and_radius() {
        use nom_derive::Parse;
        use project_wormhole_nif::nif_block::{BSOrderedNode, NiAVObjectFlags};
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("source.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let child_bound = room_bounds::source_sphere(&nif).unwrap();
        let root = nif.blocks[0].as_node().unwrap().clone();
        let sort_bound = shared::room_assignment::ModelSphere {
            center: [1000.0, 3000.0, -2000.0],
            radius: 4000.0,
        };
        let cases = [
            (0u32, 2, 4000.0, sort_bound),
            (0x2000, 1, 0.0, child_bound),
            (0x2000, 1, -0.0, child_bound),
            (0x2000, 1, f32::NAN, child_bound),
            (0x2000, 0, 4000.0, child_bound),
        ];
        let actual = cases.map(|(flags, static_bound, radius, _)| {
            let mut node = root.clone();
            node.av.flags = NiAVObjectFlags::parse(&flags.to_le_bytes()).unwrap().1;
            nif.blocks[0] = NifBlock::BSOrderedNode(BSOrderedNode {
                node,
                alpha_sort_bound: [1000.0, 2000.0, 3000.0, radius],
                static_bound,
            });
            room_bounds::source_sphere(&nif)
        });
        assert_eq!(actual, cases.map(|(_, _, _, expected)| Some(expected)));
    }

    #[test]
    fn empty_damage_stage_nodes_do_not_discard_an_assignment_bound() {
        use project_wormhole_nif::nif_block::BSBlastNode;
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("source.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let expected = room_bounds::source_sphere(&nif).unwrap();
        let mut empty = nif.blocks[0].as_node().unwrap().clone();
        empty.children.clear();
        let added = u32::try_from(nif.blocks.len()).unwrap();
        let NifBlock::BSFadeNode(root) = &mut nif.blocks[0] else {
            unreachable!()
        };
        root.children.push(added);
        nif.blocks.push(NifBlock::BSBlastNode(BSBlastNode {
            node: empty,
            minimum: 1,
            maximum: 1,
            current: 0,
        }));
        assert_eq!(room_bounds::source_sphere(&nif), Some(expected));
        let NifBlock::BSBlastNode(stage) = &mut nif.blocks[added as usize] else {
            unreachable!()
        };
        stage.node.children.push(1);
        assert!(
            room_bounds::source_sphere(&nif).is_none(),
            "nonempty damage ranges remain unsupported"
        );
    }

    #[test]
    fn assignment_sphere_declines_reflected_or_sheared_child_rotations() {
        use project_wormhole_shared::glam::{Mat3, Vec3};
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("rotation.nif");
        fs::write(&input, static_nif()).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let root = nif.blocks[0].as_node().unwrap().clone();
        for rotation in [
            Mat3::from_diagonal(Vec3::new(-1.0, 1.0, 1.0)),
            Mat3::from_cols(Vec3::X, Vec3::new(0.25, 1.0, 0.0), Vec3::Z),
        ] {
            nif.blocks[0] = NifBlock::BSFadeNode(root.clone());
            let NifBlock::BSTriShape(shape) = &mut nif.blocks[1] else {
                unreachable!()
            };
            shape.av.rotation.0.0 = rotation;
            let model = project_wormhole_nif::nif_file::nif_to_static_model(&nif).unwrap();
            assert_eq!(model.static_meshes.len(), 1);
            assert!(model.to_glb("rotation".into()).is_ok());
            assert!(room_bounds::source_sphere(&nif).is_none());
            nif.blocks[0] =
                NifBlock::BSOrderedNode(project_wormhole_nif::nif_block::BSOrderedNode {
                    node: root.clone(),
                    alpha_sort_bound: [0.0, 0.0, 0.0, 100.0],
                    static_bound: 1,
                });
            assert!(room_bounds::source_sphere(&nif).is_none());
        }
    }

    #[test]
    fn authored_shape_sphere_must_enclose_the_exported_quad() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("quad.nif");
        let output = input.with_extension("glb");
        let mut bytes = static_nif();
        let (data, header) = parse_skyrim_header(&bytes, &input).unwrap();
        // BSTriShape's authored radius follows its 72-byte AV object and sphere center.
        let radius_offset = bytes.len() - data.len() + header.block_size_index[0] as usize + 84;
        let [enclosing, undersized, rounded_down] = [2.0f32, 0.25, 2.0f32.sqrt()].map(|radius| {
            bytes[radius_offset..radius_offset + 4].copy_from_slice(&radius.to_le_bytes());
            fs::write(&input, &bytes).unwrap();
            MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
            fs::read(&output).unwrap()
        });
        let mut enclosing_json = glb_json_from_bytes(&enclosing).unwrap();
        let undersized_json = glb_json_from_bytes(&undersized).unwrap();
        let sphere = enclosing_json["extras"]
            .as_object_mut()
            .unwrap()
            .remove("mudcrabRoomSphere")
            .unwrap();
        assert_eq!(sphere["radius"], 2.0);
        assert!(undersized_json["extras"]["mudcrabRoomSphere"].is_null());
        let drawable = enclosing_json["meshes"][0]
            .as_object_mut()
            .unwrap()
            .remove("extras")
            .unwrap();
        let key = shared::room_assignment::DRAWABLE_SPHERE_EXTRA;
        assert_eq!(drawable[key]["center"], serde_json::json!([0.0, 0.0, 0.0]));
        assert_eq!(drawable[key]["radius"], 2.0);
        assert!(undersized_json["meshes"][0]["extras"][key].is_null());
        let rounded_down_json = glb_json_from_bytes(&rounded_down).unwrap();
        assert!(rounded_down_json["meshes"][0]["extras"][key].is_null());
        for key in ["nodes", "meshes", "accessors", "scenes"] {
            assert_eq!(enclosing_json[key], undersized_json[key]);
        }
        assert_eq!(
            gltf::binary::Glb::from_slice(&enclosing).unwrap().bin,
            gltf::binary::Glb::from_slice(&undersized).unwrap().bin
        );
    }

    #[test]
    fn assignment_sphere_composes_normalized_child_rotations() {
        use project_wormhole_shared::glam::{Mat3, Vec3};
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("composed-shear.nif");
        fs::write(&input, static_scene_chain_nif(4)).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let NifBlock::BSTriShape(shape) = &mut nif.blocks[1] else {
            unreachable!()
        };
        shape.bounding_sphere.radius = 1.0;
        assert!(room_bounds::source_sphere(&nif).is_some());
        let shear = Mat3::from_cols(Vec3::X, Vec3::new(8e-5, 1.0, 0.0), Vec3::Z);
        let NifBlock::BSTriShape(shape) = &mut nif.blocks[1] else {
            unreachable!()
        };
        shape.av.rotation.0.0 = shear;
        assert!(room_bounds::source_sphere(&nif).is_some());
        for block in nif.blocks.iter_mut().skip(1) {
            if let NifBlock::NiNode(node) | NifBlock::BSFadeNode(node) = block {
                node.av.rotation.0.0 = shear;
            }
        }
        assert!(room_bounds::source_sphere(&nif).is_some());
    }

    #[test]
    fn ordered_static_bound_must_enclose_the_exported_quad() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("ordered.nif");
        let output = input.with_extension("glb");
        let source = static_nif();
        let (data, header) = parse_skyrim_header(&source, &input).unwrap();
        let root_end = source.len() - data.len() + header.block_size_index[0] as usize;
        let root_type = source
            .windows(10)
            .position(|ty| ty == b"BSFadeNode")
            .unwrap();
        let indices_start = root_type
            + header
                .block_types
                .iter()
                .map(|ty| 4 + ty.0.len())
                .sum::<usize>()
            - 4;
        let root_size = indices_start + header.block_count as usize * 2;
        let glbs = [2.0f32, 0.25].map(|radius| {
            let mut bytes = source.clone();
            bytes.splice(
                root_end..root_end,
                [0.0f32, 0.0, 0.0, radius]
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .chain([1]),
            );
            bytes[root_size..root_size + 4]
                .copy_from_slice(&(header.block_size_index[0] + 17).to_le_bytes());
            bytes.splice(
                root_type - 4..root_type + 10,
                13u32.to_le_bytes().into_iter().chain(*b"BSOrderedNode"),
            );
            fs::write(&input, bytes).unwrap();
            MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
            fs::read(&output).unwrap()
        });
        let [mut enclosing, undersized] = glbs.map(|bytes| glb_json_from_bytes(&bytes).unwrap());
        let sphere = enclosing["extras"]
            .as_object_mut()
            .unwrap()
            .remove("mudcrabRoomSphere");
        assert_eq!(sphere.unwrap()["radius"], 2.0);
        assert!(undersized["extras"]["mudcrabRoomSphere"].is_null());
        for key in ["nodes", "meshes", "accessors", "scenes"] {
            assert_eq!(enclosing[key], undersized[key]);
        }
    }

    #[test]
    fn room_sphere_uses_the_rigid_writers_normalized_child_rotation() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("near-rigid.nif");
        let output = input.with_extension("glb");
        let mut bytes = static_nif_with_positions(&[
            [999.0, 0.0, 0.0],
            [1001.0, 0.0, 0.0],
            [1000.0, 1.0, 0.0],
            [1000.0, -1.0, 0.0],
        ]);
        let (data, header) = parse_skyrim_header(&bytes, &input).unwrap();
        let shape = bytes.len() - data.len() + header.block_size_index[0] as usize;
        bytes[shape + 28..shape + 32].copy_from_slice(&0.99996f32.to_le_bytes());
        fs::write(&input, bytes).unwrap();
        MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
        let bytes = fs::read(&output).unwrap();
        let doc = glb_json_from_bytes(&bytes).unwrap();
        let sphere = &doc["extras"]["mudcrabRoomSphere"];
        assert_eq!(sphere["center"], serde_json::json!([1000.0, 0.0, 0.0]));
        assert_eq!(sphere["radius"], 1.0);
        let bounds = glb_bounds_from_bytes(&bytes).unwrap();
        assert!((bounds.max[0] - 1001.0).abs() < f32::EPSILON);
    }

    #[test]
    fn room_sphere_rejects_a_vertex_just_outside_the_surface() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("boundary.nif");
        let output = input.with_extension("glb");
        for (offset, enclosed) in [(0.0, true), (1.0 / 4096.0, false)] {
            let mut bytes = static_nif_with_positions(&[
                [1.0, offset, 0.0],
                [-1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, -1.0, 0.0],
            ]);
            let (data, header) = parse_skyrim_header(&bytes, &input).unwrap();
            let shape = bytes.len() - data.len() + header.block_size_index[0] as usize;
            bytes[shape + 72..shape + 84].fill(0);
            bytes[shape + 84..shape + 88].copy_from_slice(&1.0f32.to_le_bytes());
            fs::write(&input, &bytes).unwrap();
            MeshConverter::convert_nif_to_glb(&input, &output).unwrap();
            let glb = fs::read(&output).unwrap();
            let document = glb_json_from_bytes(&glb).unwrap();
            assert_eq!(
                document["extras"]["mudcrabRoomSphere"].is_object(),
                enclosed
            );
        }
    }

    fn static_nif() -> Vec<u8> {
        dummy_content::nif::static_shape(&static_fixture_shape()).unwrap()
    }

    fn static_fixture_shape() -> dummy_content::nif::StaticShape<'static> {
        dummy_content::nif::StaticShape {
            name: "PropertyQuad",
            positions: &[
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
            ],
            normals: &[[0.0, 0.0, 1.0]; 4],
            uvs: &[[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
            indices: &[[0, 1, 2], [0, 2, 3]],
            diffuse: "textures/generated_color.dds",
            normal_texture: "textures/generated_normal.dds",
        }
    }

    fn static_nif_with_positions(positions: &[[f32; 3]; 4]) -> Vec<u8> {
        dummy_content::nif::static_shape(&dummy_content::nif::StaticShape {
            positions,
            ..static_fixture_shape()
        })
        .unwrap()
    }

    /// A NIF whose only blocks are `len` chained `NiNode`s, each naming the next
    /// as its child. Written as raw bytes because no fixture generator produces
    /// a scene graph this deep.
    fn node_chain_nif(len: usize) -> Vec<u8> {
        let children: Vec<Vec<u32>> = (0..len)
            .map(|index| {
                if index + 1 < len {
                    vec![(index + 1) as u32]
                } else {
                    Vec::new()
                }
            })
            .collect();
        node_graph_nif(&children)
    }

    /// A NIF of `NiNode` blocks where block `i` lists `children[i]`.
    fn node_graph_nif(children: &[Vec<u32>]) -> Vec<u8> {
        const NIF_VERSION: u32 = 0x1402_0007;
        const USER_VERSION: u32 = 12;
        const BETHESDA_VERSION: u32 = 100;
        /// Append little-endian transform components to a procedural node block.
        fn push_f32(bytes: &mut Vec<u8>, values: impl IntoIterator<Item = f32>) {
            for value in values {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        /// Encode one node with the requested child references and an identity transform.
        fn node(children: &[u32]) -> Vec<u8> {
            let mut bytes = Vec::new();
            for value in [0u32, u32::MAX, u32::MAX, 0] {
                // name, extra data, controller, flags
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            push_f32(&mut bytes, [0.0, 0.0, 0.0]); // translation
            push_f32(&mut bytes, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]); // rotation
            push_f32(&mut bytes, [1.0]); // scale
            bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // collision object
            bytes.extend_from_slice(&(children.len() as u32).to_le_bytes());
            for child in children {
                bytes.extend_from_slice(&child.to_le_bytes());
            }
            bytes.extend_from_slice(&0u32.to_le_bytes()); // effects
            bytes
        }
        let blocks: Vec<Vec<u8>> = children.iter().map(|children| node(children)).collect();
        let mut bytes = b"Gamebryo File Format, Version 20.2.0.7\n".to_vec();
        bytes.extend_from_slice(&NIF_VERSION.to_le_bytes());
        bytes.push(1); // little-endian
        bytes.extend_from_slice(&USER_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&BETHESDA_VERSION.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]); // empty author, process and export script
        bytes.extend_from_slice(&1u16.to_le_bytes()); // one block type
        bytes.extend_from_slice(&("NiNode".len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"NiNode");
        for _ in 0..blocks.len() {
            bytes.extend_from_slice(&0u16.to_le_bytes());
        }
        for block in &blocks {
            bytes.extend_from_slice(&(block.len() as u32).to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no strings
        bytes.extend_from_slice(&0u32.to_le_bytes()); // longest string
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no groups
        for block in blocks {
            bytes.extend_from_slice(&block);
        }
        bytes
    }

    /// Extend the static fixture's root-to-shape path while retaining all material references.
    fn static_scene_chain_nif(depth: usize) -> Vec<u8> {
        scene_chain_from_shape(depth, static_nif())
    }

    fn scene_chain_from_shape(depth: usize, source: Vec<u8>) -> Vec<u8> {
        assert!(depth > 0);
        let (data, header) = parse_skyrim_header(&source, Path::new("fixture.nif")).unwrap();
        let data_start = source.len() - data.len();
        let mut remaining = data;
        let mut blocks: Vec<Vec<u8>> = header
            .block_size_index
            .iter()
            .map(|size| {
                let (block, rest) = remaining.split_at(*size as usize);
                remaining = rest;
                block.to_vec()
            })
            .collect();
        let original_count = blocks.len();
        if depth > 1 {
            // The fixture root has one child, its shape. Appended nodes keep every
            // original block index stable and end at that same shape (block 1).
            blocks[0][76..80].copy_from_slice(&(original_count as u32).to_le_bytes());
            for index in 0..depth - 1 {
                let child = if index + 1 == depth - 1 {
                    1
                } else {
                    (original_count + index + 1) as u32
                };
                let node_file = node_graph_nif(&[vec![child]]);
                let (node, _) = parse_skyrim_header(&node_file, Path::new("node.nif")).unwrap();
                blocks.push(node.to_vec());
            }
        }
        let count_offset = b"Gamebryo File Format, Version 20.2.0.7\n".len() + 4 + 1 + 4;
        let mut type_offset = count_offset + 4 + 4;
        for _ in 0..3 {
            type_offset += 1 + source[type_offset] as usize;
        }
        let indices_start = type_offset
            + 2
            + header
                .block_types
                .iter()
                .map(|kind| 4 + kind.0.len())
                .sum::<usize>();
        let strings_start = indices_start + original_count * 6;
        let mut bytes = source[..indices_start].to_vec();
        bytes[count_offset..count_offset + 4].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
        for index in 0..blocks.len() {
            let kind = if index < original_count {
                header.block_type_index[index]
            } else {
                header.block_type_index[0]
            };
            bytes.extend_from_slice(&kind.to_le_bytes());
        }
        for block in &blocks {
            bytes.extend_from_slice(&(block.len() as u32).to_le_bytes());
        }
        bytes.extend_from_slice(&source[strings_start..data_start]);
        for block in blocks {
            bytes.extend_from_slice(&block);
        }
        bytes
    }

    /// Export real geometry through the old and new boundaries on a default worker stack.
    #[test]
    fn scene_chains_through_256_export_geometry_and_walk_bounds() {
        std::thread::spawn(|| {
            let directory = tempfile::tempdir().unwrap();
            // Each corner is exactly five units from the center in both f32 and f64.
            let shape = static_nif_with_positions(&[
                [-3.0, -4.0, 0.0],
                [3.0, -4.0, 0.0],
                [3.0, 4.0, 0.0],
                [-3.0, 4.0, 0.0],
            ]);
            for (depth, exported_nodes) in [(64, 66), (65, 67), (256, 258)] {
                let input = directory.path().join(format!("chain-{depth}.nif"));
                let output = input.with_extension("glb");
                fs::write(&input, scene_chain_from_shape(depth, shape.clone())).unwrap();
                let diagnostics = MeshConverter::inspect_nif(&input).unwrap();
                assert_eq!(diagnostics.max_scene_depth, depth);
                assert!(
                    SkeletonEntry::load(Path::new("skeleton.nif"), &input)
                        .nodes
                        .is_some()
                );
                MeshConverter::convert_nif_to_glb_with_skeleton(&input, &output, None).unwrap();
                assert!(MeshConverter::glb_bounds(&output).is_ok());
                let glb = fs::read(&output).unwrap();
                let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
                let json: serde_json::Value =
                    serde_json::from_slice(&glb[20..20 + json_len]).unwrap();
                assert_eq!(json["nodes"].as_array().unwrap().len(), exported_nodes);
                assert_eq!(json["meshes"].as_array().unwrap().len(), 1);
                assert!(
                    json["extras"]["mudcrabRoomSphere"]["radius"]
                        .as_f64()
                        .is_some_and(|radius| radius > 0.0),
                    "{depth} supported scene nodes must retain the assignment sphere"
                );
            }
            let input = directory.path().join("chain-257.nif");
            fs::write(&input, scene_chain_from_shape(257, shape)).unwrap();
            let (nif, diagnostics, _) = open_nif_resilient(&input).unwrap();
            assert_eq!(diagnostics.max_scene_depth, 257);
            assert!(room_bounds::source_sphere(&nif).is_none());
        })
        .join()
        .unwrap();
    }

    /// Scenes well under the old 64-node collision limit convert to the same GLB bytes as before
    /// the shared 256-node bound. The pinned hashes come from the converter before that bound,
    /// except that both single-shape cases also carry the `mudcrabGeometryIndex` primitive tag
    /// that `object-texture-swap-geometry-index-v1` adds; each case asserts that tag below.
    /// Exclude newer room metadata and the explicit absence of a NIF alpha property.
    #[test]
    fn shallow_scene_glb_bytes_are_unchanged_by_the_depth_bound() {
        let directory = tempfile::tempdir().unwrap();
        let cases = [
            (
                "static",
                static_nif(),
                "18b02762207493e8ad2489cc340839a3fb0a9383824034d5026081b7b0f2b773",
            ),
            (
                "chain-40",
                static_scene_chain_nif(40),
                "ae4eb9a35aebaa7873a88fc3f51f10b35f501f296286fbb9cb46f79922d15fdc",
            ),
        ];
        for (name, source, expected) in cases {
            let input = directory.path().join(format!("{name}.nif"));
            let output = input.with_extension("glb");
            fs::write(&input, source).unwrap();
            MeshConverter::convert_nif_to_glb_with_skeleton(&input, &output, None).unwrap();
            let glb = fs::read(&output).unwrap();
            let mut document = glb_json_from_bytes(&glb).unwrap();
            assert_eq!(
                document["meshes"][0]["primitives"][0]["extras"]["mudcrabGeometryIndex"], 0,
                "{name}: the single shape carries its geometry index"
            );
            // These metadata fields postdate the reference GLBs. Every other byte must match.
            let strip_bound = |owner: &mut serde_json::Value, key: &str| {
                if let Some(extras) = owner
                    .get_mut("extras")
                    .and_then(|value| value.as_object_mut())
                {
                    extras.remove(key);
                    if extras.is_empty() {
                        owner.as_object_mut().unwrap().remove("extras");
                    }
                }
            };
            strip_bound(&mut document, "mudcrabRoomSphere");
            for mesh in document["meshes"].as_array_mut().unwrap() {
                strip_bound(mesh, shared::room_assignment::DRAWABLE_SPHERE_EXTRA);
            }
            for material in document["materials"].as_array_mut().unwrap() {
                let extras = material["extras"]["openSkyrim"].as_object_mut().unwrap();
                assert_eq!(
                    extras.remove("alphaProperty"),
                    Some(serde_json::Value::Null)
                );
            }
            assert_eq!(
                crate::cache::hash_bytes(&rebuild_glb_with_document(&glb, &document).unwrap()),
                expected,
                "{name}: GLB bytes changed"
            );
        }
    }

    /// Inspection measures deep skeletons while every consuming entry point refuses them.
    #[test]
    fn deep_skeleton_is_inspectable_but_ineligible_for_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let skeleton = directory.path().join("skeleton.nif");
        fs::write(&skeleton, node_chain_nif(257)).unwrap();
        assert_eq!(
            MeshConverter::inspect_nif(&skeleton)
                .unwrap()
                .max_scene_depth,
            257
        );
        assert!(
            SkeletonEntry::load(Path::new("skeleton.nif"), &skeleton)
                .nodes
                .is_none()
        );
        assert!(MeshConverter::extract_collision(&skeleton).is_err());
        let input = directory.path().join("skin.nif");
        fs::write(
            &input,
            dummy_content::nif::legacy_skinned_shape(&dummy_content::nif::LegacySkin {
                name: "SkeletonBoundary",
                instance_kind: dummy_content::nif::SkinInstanceKind::Plain,
                bone_transforms: [([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0); 2],
                palette: &[1, 0],
                weights: &[[0.75, 0.25, 0.0, 0.0]; 4],
                with_geometry: true,
                joint_override: None,
            })
            .unwrap(),
        )
        .unwrap();
        let output = directory.path().join("skin.glb");
        let error =
            MeshConverter::convert_nif_to_glb_with_skeleton(&input, &output, Some(&skeleton))
                .unwrap_err();
        assert!(format!("{error:#}").contains("257 scene nodes"));
        assert!(build_skinned_model_with_materials(&input, &skeleton).is_err());
        assert!(!output.exists());
    }

    /// A deep scene retains its measured depth when its material contract is also invalid.
    #[test]
    fn deep_scene_diagnostics_survive_material_inspection_failure() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("deep-material.nif");
        let mut bytes = static_scene_chain_nif(257);
        let (data, header) = parse_skyrim_header(&bytes, &input).unwrap();
        // Geometry remains block 1; its shader reference follows AV/bounds/skin.
        let shape = bytes.len() - data.len() + header.block_size_index[0] as usize;
        bytes[shape + 92..shape + 96].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
        fs::write(&input, bytes).unwrap();
        let diagnostics = MeshConverter::inspect_nif(&input)
            .expect("material failure must preserve an already refused scene's depth");
        assert_eq!(diagnostics.max_scene_depth, 257);
        assert_eq!(diagnostics.conversion_refusals.len(), 2);
        assert!(diagnostics.conversion_refusals[0].contains("257 scene nodes"));
        assert!(
            diagnostics.conversion_refusals[1].starts_with("material inspection unavailable: ")
        );
        assert!(diagnostics.conversion_refusals[1].chars().count() <= 512);
        assert!(MeshConverter::validate_nif_for_conversion(&input).is_err());
        let output = input.with_extension("glb");
        assert!(MeshConverter::convert_nif_to_glb(&input, &output).is_err());
        assert!(!output.exists());
    }

    /// Reject a long procedural chain before recursive export can exhaust the stack.
    #[test]
    fn crafted_deep_scene_chain_is_refused_not_walked() {
        let directory = tempfile::tempdir().unwrap();
        // Well inside the block cap, far past any authored scene: recursion
        // here would exhaust the stack and abort the whole conversion run.
        let input = directory.path().join("deep.nif");
        fs::write(&input, node_chain_nif(20_000)).unwrap();
        let output = directory.path().join("deep.glb");
        let error = MeshConverter::convert_nif_to_glb(&input, &output).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("20000 scene nodes"), "{message}");
        assert!(message.contains("256-node limit"), "{message}");
        assert!(!output.exists(), "a refused scene must not publish a GLB");
        let diagnostics = MeshConverter::inspect_nif(&input)
            .expect("inspection must retain measurements for refused scenes");
        assert_eq!(diagnostics.max_scene_depth, 20_000);
    }

    /// Accept a chain at the configured node limit.
    #[test]
    fn scene_chain_at_the_depth_limit_is_readable() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("limit.nif");
        fs::write(&input, node_chain_nif(MAX_SCENE_DEPTH)).unwrap();
        let diagnostics = MeshConverter::inspect_nif(&input).unwrap();
        assert_eq!(diagnostics.max_scene_depth, MAX_SCENE_DEPTH);
    }

    /// Geometry and other non-node child blocks do not consume the scene-node budget.
    #[test]
    fn scene_depth_refusal_counts_only_nodes_including_at_the_limit() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("limit.nif");
        fs::write(&input, node_chain_nif(256)).unwrap();
        let (mut nif, _, _) = open_nif_resilient(&input).unwrap();
        let NifBlock::NiNode(last) = &mut nif.blocks[255] else {
            panic!("fixture must end in a scene node");
        };
        last.children.push(256);
        let geometry = directory.path().join("geometry.nif");
        fs::write(&geometry, static_nif()).unwrap();
        let (geometry, _, _) = open_nif_resilient(&geometry).unwrap();
        let shape = geometry
            .blocks
            .into_iter()
            .find(|block| matches!(block, NifBlock::BSTriShape(_)))
            .unwrap();
        nif.blocks.push(shape);
        assert_eq!(nif_scene_depth(&nif.blocks).unwrap(), 256);
        assert_eq!(nif_scene_depth(&[NifBlock::Unhandled]).unwrap(), 0);
        assert_eq!(nif_scene_depth(&[]).unwrap(), 0);
        fs::write(&input, node_chain_nif(257)).unwrap();
        let diagnostics = MeshConverter::inspect_nif(&input).unwrap();
        assert_eq!(diagnostics.max_scene_depth, 257);
        assert!(diagnostics.conversion_refusals[0].contains("257 scene nodes"));
        assert!(MeshConverter::validate_nif_for_conversion(&input).is_err());
    }

    /// Bound malformed-instance diagnostics and distinguish a caught parser panic.
    #[test]
    fn skin_parse_refusal_names_only_the_first_offsets() {
        let mut diagnostics = NifParseDiagnostics::default();
        let block_type = "NiSkinInstance".to_owned();
        diagnostics.fallback_blocks.insert(block_type.clone(), 20);
        let mut offsets: Vec<usize> = (0..19).map(|index| 100 + index).collect();
        offsets.insert(0, usize::MAX);
        diagnostics.fallback_offsets.insert(block_type, offsets);
        let message = ensure_declared_skins_parsed(&diagnostics, Path::new("many.nif"))
            .unwrap_err()
            .to_string();
        assert!(message.contains("20 NiSkinInstance block(s)"), "{message}");
        assert!(
            message.contains("(parser panicked, offset 100,"),
            "{message}"
        );
        assert!(message.contains("offset 106, and 12 more)"), "{message}");
        assert!(!message.contains("offset 107"), "{message}");
    }

    /// Reject an ancestor back edge in a disconnected-capable block walk.
    #[test]
    fn scene_graph_loop_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("loop.nif");
        fs::write(&input, node_graph_nif(&[vec![1], vec![2], vec![1]])).unwrap();
        let diagnostics = MeshConverter::inspect_nif(&input).unwrap();
        assert_eq!(diagnostics.max_scene_depth, 0);
        assert!(diagnostics.conversion_refusals[0].contains("loops through block 1"));
        assert!(MeshConverter::validate_nif_for_conversion(&input).is_err());
    }

    /// Reuse completed depths without losing a shared child's longest parent chain.
    #[test]
    fn shared_scene_children_count_their_depth_once_from_every_parent() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("shared.nif");
        // Block 0 is a leaf that the outer walk finishes first, before block 3
        // reaches it as a child; blocks 1 and 2 then share block 3, whose depth
        // the second parent reads from the cache.
        let graph = [vec![], vec![3], vec![1, 3], vec![0]];
        fs::write(&input, node_graph_nif(&graph)).unwrap();
        let diagnostics = MeshConverter::inspect_nif(&input).unwrap();
        // 2 -> 1 -> 3 -> 0 is the deepest chain.
        assert_eq!(diagnostics.max_scene_depth, 4);
    }

    /// Refuse an undecodable binding instead of publishing a silently rigid model.
    #[test]
    fn unparsable_skin_instance_is_an_error_not_a_static_mesh() {
        use dummy_content::nif::{LegacySkin, SkinInstanceKind, legacy_skinned_shape};
        const BONE_TRANSFORMS: [([f32; 3], [f32; 4], f32); 2] = [
            (
                [0.0, 0.0, 0.0],
                [
                    0.0,
                    0.0,
                    std::f32::consts::FRAC_1_SQRT_2,
                    std::f32::consts::FRAC_1_SQRT_2,
                ],
                1.0,
            ),
            ([0.0, 0.0, 2.0], [0.0, 0.0, 0.0, 1.0], 1.0),
        ];
        const WEIGHTS: [[f32; 4]; 4] = [
            [0.7, 0.3, 0.0, 0.0],
            [0.25, 0.75, 0.0, 0.0],
            [0.5, 0.2, 0.0, 0.0],
            [0.4, 0.6, 0.0, 0.0],
        ];
        const PALETTE: [u16; 2] = [1, 0];
        let fixture = LegacySkin {
            name: "GeneratedSkin",
            instance_kind: SkinInstanceKind::Plain,
            bone_transforms: BONE_TRANSFORMS,
            palette: &PALETTE,
            weights: &WEIGHTS,
            with_geometry: true,
            joint_override: None,
        };
        let mut bytes = legacy_skinned_shape(&fixture).unwrap();
        let path = Path::new("broken-skin.nif");
        let (remaining, header) = parse_skyrim_header(&bytes, path).unwrap();
        let block_data = bytes.len() - remaining.len();
        let skin = (0..header.block_count as usize)
            .find(|index| {
                header
                    .get_block_type(*index)
                    .is_ok_and(|block_type| block_type == "NiSkinInstance")
            })
            .expect("the fixture declares a NiSkinInstance");
        let block_start = block_data
            + header.block_size_index[..skin]
                .iter()
                .map(|size| *size as usize)
                .sum::<usize>();
        // A bone count larger than the remaining block bytes is a count the
        // parser must reject, so the block comes back unhandled. The three
        // references ahead of it are `data`, `skin partition`, `skeleton root`.
        let bone_count = block_start + 3 * 4;
        bytes[bone_count..bone_count + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join(path);
        fs::write(&input, bytes).unwrap();
        let output = directory.path().join("broken-skin.glb");
        let error = MeshConverter::convert_nif_to_glb(&input, &output).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("NiSkinInstance"), "{message}");
        assert!(message.contains("offset"), "{message}");
        assert!(
            !output.exists(),
            "a skinned mesh must not publish a static GLB"
        );
        let diagnostics = MeshConverter::inspect_nif(&input)
            .expect("inspection must retain the declared, unparsed skin block");
        assert_eq!(diagnostics.fallback_blocks["NiSkinInstance"], 1);
        assert_eq!(diagnostics.fallback_offsets["NiSkinInstance"].len(), 1);
    }

    fn inspect_and_convert(bytes: &[u8]) {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("property.nif");
        fs::write(&input, bytes).unwrap();
        let _ = MeshConverter::inspect_nif(&input);
        let _ = MeshConverter::convert_nif_to_glb(&input, &directory.path().join("property.glb"));
    }

    proptest! {
        // Each case writes a file and runs the exporter, so fewer cases keep
        // the suite quick.
        #![proptest_config(config(64))]

        #[test]
        fn nif_never_panics_on_arbitrary_bytes(tail in arbitrary_bytes(512)) {
            // The Skyrim SE signature line, so the header fields are reached.
            let mut bytes = b"Gamebryo File Format, Version 20.2.0.7\n".to_vec();
            bytes.extend_from_slice(&tail);
            inspect_and_convert(&bytes);
        }

        /// Exercise header and block-table recovery beyond the small arbitrary-input budget.
        #[test]
        fn nif_never_panics_on_larger_arbitrary_bytes(tail in arbitrary_bytes(4096)) {
            // Longer than the 512-byte budget, so whole headers and block tables
            // are reached; most random tails still stop at header validation.
            // Scene graphs get their own property below.
            let mut bytes = b"Gamebryo File Format, Version 20.2.0.7\n".to_vec();
            bytes.extend_from_slice(&tail);
            inspect_and_convert(&bytes);
        }

        /// Exercise loops, shared subgraphs and out-of-range references in procedural graphs.
        #[test]
        fn nif_scene_graphs_never_panic_and_stay_within_the_depth_limit(
            // Child indices run past the last block too: the walk skips those.
            children in prop::collection::vec(prop::collection::vec(0u32..96, 0..4), 1..80)
        ) {
            let directory = tempfile::tempdir().unwrap();
            let input = directory.path().join("graph.nif");
            fs::write(&input, node_graph_nif(&children)).unwrap();
            // Inspection retains refusals; only conversion-eligible graphs are in bounds.
            if let Ok(diagnostics) = MeshConverter::inspect_nif(&input)
                && diagnostics.conversion_refusals.is_empty()
            {
                prop_assert!(diagnostics.max_scene_depth <= MAX_SCENE_DEPTH);
            }
            let _ = MeshConverter::convert_nif_to_glb(&input, &directory.path().join("graph.glb"));
        }

        #[test]
        fn corrupted_nif_never_panics(bytes in corrupted(static_nif())) {
            inspect_and_convert(&bytes);
        }
    }
}
