//! Declarative audit of every texture a converted pack should resolve.
//!
//! The audit answers one question for each mesh, material slot and texture set: "does the
//! texture the game data asks for reach the renderer?" It compares three views of the same
//! texture, all read-only:
//!
//! 1. what the source NIF authored (a path in a `BSShaderTextureSet` slot),
//! 2. what the source archives and loose files contain (the `.dds` the path names), and
//! 3. what the converted pack contains and what each GLB references (`.ktx2` and its URIs).
//!
//! Resolution uses the converter's own helpers ([`canonical_asset_path`],
//! [`resolve_asset_uri`], [`runtime_texture_source`]) so the audit and the converter cannot
//! disagree about a path. The engine loads GLB images through Bevy with the same URIs, so a
//! URI that resolves here resolves there.
//!
//! A slot that cannot reach the renderer renders as Bevy's stand-in texture (white) or, when
//! the converter pruned the slot, as the material's base colour (also white). Every such slot
//! is reported with a [`Cause`]. [`compare_to_baseline`] turns the counts into a ratchet.

use crate::{
    archive::ArchiveExtractor,
    asset_path::{AssetKind, canonical_asset_path, resolve_asset_uri, runtime_texture_source},
    material::NifTextureSemantic,
    mesh::MeshConverter,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

/// Bumped when the report layout changes.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Up to this many cells are listed per mesh in `used_by`.
const MAX_CELLS_PER_MESH: usize = 5;

/// Why a texture slot does not reach the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The authored path names no file in the game data: Bethesda never shipped it, or the path
    /// has a typo. The converter prunes the slot, so the surface renders with no texture.
    SourceTextureAbsent,
    /// The game data has the `.dds`, but the pack has no converted `.ktx2` for it.
    ConvertedTextureMissing,
    /// The pack has the converted texture, but the GLB does not reference it for this mesh.
    SlotUnreferenced,
    /// The authored path cannot be turned into a texture key (bad extension, absolute path).
    UnparseablePath,
    /// A primitive uses a textured material but has no `TEXCOORD_0`, so every pixel samples one
    /// texel of the texture.
    PrimitiveWithoutUvs,
    /// A texture set (TXST) or water path names no converted texture.
    TextureSetUnresolved,
    /// A terrain layer has no diffuse image: its LTEX record has no texture set with a
    /// diffuse path, or a LAND record names an LTEX that no plugin defines.
    LandTextureUnresolved,
    /// An alternate-texture entry (`MODS`/`MO2S`/`MO3S`/`MO4S`) names a texture set that no
    /// plugin defines and that is not the engine's NullTextureSet (RF-TEX-005).
    OverrideTargetUnresolved,
}

impl Cause {
    pub const ALL: [Self; 8] = [
        Self::SourceTextureAbsent,
        Self::ConvertedTextureMissing,
        Self::SlotUnreferenced,
        Self::UnparseablePath,
        Self::PrimitiveWithoutUvs,
        Self::TextureSetUnresolved,
        Self::LandTextureUnresolved,
        Self::OverrideTargetUnresolved,
    ];
}

/// Which part of the data a finding comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A `BSShaderTextureSet` slot of a NIF shape.
    NifShape,
    /// A primitive of a GLB.
    GlbPrimitive,
    /// A `TXST` record's path.
    TextureSet,
    /// A `WATR` record's flow-normal path.
    Water,
}

/// A placement of a mesh, for finding where a miss is visible.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsedBy {
    /// Static base records whose model is this mesh.
    pub static_records: u64,
    /// References placing those records.
    pub references: u64,
    /// Up to [`MAX_CELLS_PER_MESH`] cells with the most references: label and count.
    pub cells: Vec<CellUse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellUse {
    pub cell_id: u32,
    pub label: String,
    pub references: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub cause: Cause,
    pub origin: Origin,
    /// The GLB (`meshes/...glb`) or the record (`TXST 0001ABCD`).
    pub mesh: String,
    /// The NIF shape or the GLB mesh name; empty for records.
    pub shape: String,
    /// `diffuse`, `normal`, ... or the TXST field name.
    pub slot: String,
    /// The path exactly as authored.
    pub wanted_path: String,
    /// The converted texture key the engine would load, for example `textures/a/b.ktx2`.
    pub wanted_key: Option<String>,
    /// Source of the wanted path: NIF block or record.
    pub came_from: String,
    pub source_has_dds: bool,
    /// The archive or loose file that supplies the `.dds`.
    pub source_archive: Option<String>,
    pub pack_has_ktx2: bool,
    pub glb_references_it: bool,
    /// A different texture key with the same file name, a likely intended target.
    pub same_name_elsewhere: Option<String>,
    pub used_by: UsedBy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub glb_files: u64,
    pub glb_files_with_source_nif: u64,
    pub glb_files_without_source_nif: u64,
    pub nif_files_failed: u64,
    pub wanted_slots: u64,
    pub resolved_slots: u64,
    pub unresolved_slots: u64,
    pub texture_set_paths: u64,
    pub counts_by_cause: BTreeMap<Cause, u64>,
    pub distinct_textures_by_cause: BTreeMap<Cause, u64>,
    pub distinct_meshes_by_cause: BTreeMap<Cause, u64>,
    /// Unresolved findings on meshes that some reference places.
    pub placed_findings_by_cause: BTreeMap<Cause, u64>,
    pub failed_nifs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub summary: Summary,
    /// Alternate-texture entries on base records, classified by the retail rules.
    #[serde(default)]
    pub overrides: OverrideAudit,
    pub findings: Vec<Finding>,
}

/// The checked-in ratchet: no cause may exceed its recorded count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub max_by_cause: BTreeMap<Cause, u64>,
    pub note: String,
}

impl Baseline {
    pub fn from_report(report: &Report, note: &str) -> Self {
        let mut max_by_cause = BTreeMap::new();
        for cause in Cause::ALL {
            max_by_cause.insert(
                cause,
                report
                    .summary
                    .counts_by_cause
                    .get(&cause)
                    .copied()
                    .unwrap_or(0),
            );
        }
        Self {
            max_by_cause,
            note: note.to_owned(),
        }
    }
}

/// One message per cause whose count exceeds the baseline; empty means the ratchet holds.
pub fn compare_to_baseline(report: &Report, baseline: &Baseline) -> Vec<String> {
    let mut regressions = Vec::new();
    for cause in Cause::ALL {
        let now = report
            .summary
            .counts_by_cause
            .get(&cause)
            .copied()
            .unwrap_or(0);
        let allowed = baseline.max_by_cause.get(&cause).copied().unwrap_or(0);
        if now > allowed {
            regressions.push(format!(
                "{cause:?}: {now} unresolved texture slots, baseline allows {allowed}"
            ));
        }
    }
    regressions
}

// ---------------------------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------------------------

/// Where a game-data file lives.
#[derive(Debug, Clone)]
pub enum SourceLocation {
    Loose(PathBuf),
    Archive { archive: PathBuf, inner: PathBuf },
}

impl SourceLocation {
    fn label(&self) -> String {
        match self {
            Self::Loose(path) => format!(
                "loose:{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            Self::Archive { archive, .. } => archive
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        }
    }
}

/// Every mesh and texture the game data supplies, keyed like the converter keys them
/// (`meshes/x.nif`, `textures/y.dds`). Later archives win, loose files win over archives.
#[derive(Debug, Default)]
pub struct SourceCatalog {
    meshes: HashMap<String, SourceLocation>,
    textures: HashMap<String, SourceLocation>,
    texture_stems: HashMap<String, Vec<String>>,
}

impl SourceCatalog {
    /// Indexes the archives (`*.bsa`, `*.ba2`) and loose `meshes`/`textures` under `data_dir`.
    /// `plugin_names` is the load order used to rank archives; an unknown archive ranks last,
    /// as in the converter.
    pub fn build(data_dir: &Path, plugin_names: &[String]) -> Result<Self> {
        let mut archives = Vec::new();
        for entry in fs::read_dir(data_dir)
            .wrap_err_with(|| format!("failed to read {}", data_dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bsa") || extension.eq_ignore_ascii_case("ba2")
            }) {
                archives.push(path);
            }
        }
        let plugins: Vec<PathBuf> = plugin_names.iter().map(|name| data_dir.join(name)).collect();
        crate::pipeline::sort_archives_by_load_order(&mut archives, &plugins);
        let mut catalog = Self::default();
        for archive in &archives {
            let listed = match ArchiveExtractor::list_paths(archive) {
                Ok(listed) => listed,
                // An archive of another kind (sounds, animations) can use a layout the lister
                // refuses; it holds no meshes or textures the audit needs.
                Err(_) => continue,
            };
            for inner in listed {
                let name = inner.to_string_lossy().into_owned();
                catalog.insert(
                    &name,
                    SourceLocation::Archive {
                        archive: archive.clone(),
                        inner,
                    },
                );
            }
        }
        for folder in ["meshes", "textures"] {
            let root = data_dir.join(folder);
            if !root.is_dir() {
                continue;
            }
            for entry in WalkDir::new(&root).follow_links(false) {
                let entry = entry?;
                if entry.file_type().is_file() {
                    let relative = entry.path().strip_prefix(data_dir)?;
                    catalog.insert(
                        &relative.to_string_lossy(),
                        SourceLocation::Loose(entry.path().to_owned()),
                    );
                }
            }
        }
        catalog.index_stems();
        Ok(catalog)
    }

    fn insert(&mut self, path: &str, location: SourceLocation) {
        let normalized = path.replace('\\', "/").to_ascii_lowercase();
        if normalized.starts_with("meshes/") && normalized.ends_with(".nif") {
            if let Ok(key) = canonical_asset_path(&normalized, AssetKind::Mesh, "nif") {
                self.meshes.insert(key, location);
            }
        } else if normalized.starts_with("textures/")
            && normalized.ends_with(".dds")
            && let Ok(key) = canonical_asset_path(&normalized, AssetKind::Texture, "dds")
        {
            self.textures.insert(key, location);
        }
    }

    fn index_stems(&mut self) {
        for key in self.textures.keys() {
            let stem = key.rsplit('/').next().unwrap_or(key).to_owned();
            self.texture_stems.entry(stem).or_default().push(key.clone());
        }
        for keys in self.texture_stems.values_mut() {
            keys.sort();
        }
    }

    pub fn mesh(&self, key: &str) -> Option<&SourceLocation> {
        self.meshes.get(key)
    }

    pub fn texture(&self, key: &str) -> Option<&SourceLocation> {
        self.textures.get(key)
    }

    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Extracts the given NIFs from their archives into `scratch`, one archive pass each.
    /// Loose files stay where they are.
    fn extract_meshes(&self, keys: &[String], scratch: &Path) -> Result<()> {
        let mut by_archive: BTreeMap<&Path, BTreeSet<PathBuf>> = BTreeMap::new();
        for key in keys {
            if let Some(SourceLocation::Archive { archive, inner }) = self.meshes.get(key) {
                by_archive
                    .entry(archive.as_path())
                    .or_default()
                    .insert(inner.clone());
            }
        }
        let jobs: Vec<_> = by_archive.into_iter().collect();
        jobs.par_iter().try_for_each(|(archive, inner)| {
            ArchiveExtractor::extract_paths(archive, scratch, inner).map(|_| ())
        })
    }

    /// Where the NIF for `key` can be opened after [`Self::extract_meshes`].
    fn mesh_path(&self, key: &str, scratch: &Path) -> Option<PathBuf> {
        match self.meshes.get(key)? {
            SourceLocation::Loose(path) => Some(path.clone()),
            SourceLocation::Archive { inner, .. } => Some(scratch.join(inner)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pack view
// ---------------------------------------------------------------------------------------------

/// The converted textures a pack holds, by base key (`textures/a/b.ktx2`), ignoring sampler and
/// transfer aliases.
#[derive(Debug, Default)]
pub struct PackTextures {
    bases: HashSet<String>,
    /// Lowercase relative path of every `.ktx2` file, as the engine resolves a URI.
    files: HashSet<String>,
}

impl PackTextures {
    pub fn scan(pack: &Path) -> Result<Self> {
        let mut index = Self::default();
        let root = pack.join("textures");
        if !root.is_dir() {
            return Ok(index);
        }
        for entry in WalkDir::new(&root).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(pack)?
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            if let Some(base) = runtime_texture_source(&relative) {
                index.bases.insert(base);
                index.files.insert(relative);
            }
        }
        Ok(index)
    }

    pub fn has_base(&self, base_key: &str) -> bool {
        self.bases.contains(base_key)
    }

    pub fn has_file(&self, relative: &str) -> bool {
        self.files.contains(&relative.to_ascii_lowercase())
    }
}

/// What a GLB says about textures.
#[derive(Debug, Default)]
struct GlbFacts {
    /// Base keys of every image URI that resolves inside the pack, resolved or not.
    referenced_bases: HashSet<String>,
    /// Image URIs that cannot be resolved to a key at all.
    unresolvable_uris: Vec<String>,
    /// Primitives with a textured material and no `TEXCOORD_0`.
    no_uv_primitives: Vec<NoUv>,
}

#[derive(Debug)]
struct NoUv {
    mesh: String,
    material: String,
    texture: Option<String>,
}

fn read_glb_json(path: &Path) -> Result<serde_json::Value> {
    let mut file =
        fs::File::open(path).wrap_err_with(|| format!("failed to open {}", path.display()))?;
    let mut header = [0u8; 20];
    file.read_exact(&mut header)?;
    ensure!(&header[..4] == b"glTF", "not a GLB: {}", path.display());
    let json_length = u32::from_le_bytes(header[12..16].try_into().unwrap());
    let mut json = vec![0u8; usize::try_from(json_length)?];
    file.read_exact(&mut json)?;
    Ok(serde_json::from_slice(&json)?)
}

fn glb_facts(pack: &Path, glb: &Path) -> Result<GlbFacts> {
    let document = read_glb_json(glb)?;
    let mut facts = GlbFacts::default();
    let images = document
        .get("images")
        .and_then(|images| images.as_array())
        .cloned()
        .unwrap_or_default();
    let mut image_keys: Vec<Option<String>> = Vec::with_capacity(images.len());
    for image in &images {
        let key = image
            .get("uri")
            .and_then(|uri| uri.as_str())
            .filter(|uri| !uri.starts_with("data:"))
            .and_then(|uri| match resolve_asset_uri(pack, glb, uri) {
                Ok(path) => path
                    .strip_prefix(pack)
                    .ok()
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                    .and_then(|relative| runtime_texture_source(&relative.to_ascii_lowercase())),
                Err(_) => {
                    facts.unresolvable_uris.push(uri.to_owned());
                    None
                }
            });
        if let Some(key) = &key {
            facts.referenced_bases.insert(key.clone());
        }
        image_keys.push(key);
    }
    let textures = document
        .get("textures")
        .and_then(|textures| textures.as_array())
        .cloned()
        .unwrap_or_default();
    let texture_key = |index: usize| -> Option<String> {
        textures
            .get(index)
            .and_then(|texture| texture.get("source"))
            .and_then(|source| source.as_u64())
            .and_then(|source| image_keys.get(usize::try_from(source).ok()?).cloned().flatten())
    };
    let materials = document
        .get("materials")
        .and_then(|materials| materials.as_array())
        .cloned()
        .unwrap_or_default();
    for mesh in document
        .get("meshes")
        .and_then(|meshes| meshes.as_array())
        .into_iter()
        .flatten()
    {
        let mesh_name = mesh
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or_default();
        for primitive in mesh
            .get("primitives")
            .and_then(|primitives| primitives.as_array())
            .into_iter()
            .flatten()
        {
            let has_uv = primitive
                .get("attributes")
                .and_then(|attributes| attributes.get("TEXCOORD_0"))
                .is_some();
            if has_uv {
                continue;
            }
            let Some(material) = primitive
                .get("material")
                .and_then(|material| material.as_u64())
                .and_then(|material| materials.get(usize::try_from(material).ok()?))
            else {
                continue;
            };
            let texture_index = |value: Option<&serde_json::Value>| {
                value
                    .and_then(|info| info.get("index"))
                    .and_then(|index| index.as_u64())
                    .and_then(|index| usize::try_from(index).ok())
            };
            let base = texture_index(
                material
                    .get("pbrMetallicRoughness")
                    .and_then(|pbr| pbr.get("baseColorTexture")),
            );
            let normal = texture_index(material.get("normalTexture"));
            if let Some(index) = base.or(normal) {
                facts.no_uv_primitives.push(NoUv {
                    mesh: mesh_name.to_owned(),
                    material: material
                        .get("name")
                        .and_then(|name| name.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    texture: texture_key(index),
                });
            }
        }
    }
    Ok(facts)
}

// ---------------------------------------------------------------------------------------------
// The audit
// ---------------------------------------------------------------------------------------------

pub struct AuditOptions<'a> {
    pub pack: &'a Path,
    pub sources: &'a SourceCatalog,
    /// Where source NIFs are written while they are read. Not inside the pack.
    pub scratch: &'a Path,
    /// `skyrim_world.db` of the pack, opened read-only; `None` skips records and `used_by`.
    pub database: Option<&'a Path>,
    /// Audit only GLBs whose pack-relative path contains this text.
    pub mesh_filter: Option<&'a str>,
}

fn semantic_name(semantic: NifTextureSemantic) -> &'static str {
    match semantic {
        NifTextureSemantic::Diffuse => "diffuse",
        NifTextureSemantic::Normal => "normal",
        NifTextureSemantic::Glow => "glow",
        NifTextureSemantic::Height => "height",
        NifTextureSemantic::Detail => "detail",
        NifTextureSemantic::Specular => "specular",
        NifTextureSemantic::EnvironmentCube => "environment_cube",
        NifTextureSemantic::EnvironmentMask => "environment_mask",
        NifTextureSemantic::InnerLayer => "inner_layer",
        NifTextureSemantic::FacegenTint => "facegen_tint",
        NifTextureSemantic::Greyscale => "greyscale",
        NifTextureSemantic::Unclassified => "unclassified",
    }
}

/// A slot the NIF authored, before it is checked.
struct WantedSlot {
    shape: String,
    slot: String,
    path: String,
    came_from: String,
}

struct SlotCheck<'a> {
    pack_textures: &'a PackTextures,
    sources: &'a SourceCatalog,
}

impl SlotCheck<'_> {
    /// Classifies one wanted texture. Returns `None` when it resolves.
    #[allow(clippy::too_many_arguments)]
    fn check(
        &self,
        origin: Origin,
        mesh: &str,
        shape: &str,
        slot: &str,
        wanted_path: &str,
        came_from: &str,
        glb_bases: Option<&HashSet<String>>,
        unresolved_cause: Cause,
    ) -> Option<Finding> {
        let mut finding = Finding {
            cause: unresolved_cause,
            origin,
            mesh: mesh.to_owned(),
            shape: shape.to_owned(),
            slot: slot.to_owned(),
            wanted_path: wanted_path.to_owned(),
            wanted_key: None,
            came_from: came_from.to_owned(),
            source_has_dds: false,
            source_archive: None,
            pack_has_ktx2: false,
            glb_references_it: false,
            same_name_elsewhere: None,
            used_by: UsedBy::default(),
        };
        let (Ok(source_key), Ok(base_key)) = (
            canonical_asset_path(wanted_path, AssetKind::Texture, "dds"),
            canonical_asset_path(wanted_path, AssetKind::Texture, "ktx2"),
        ) else {
            finding.cause = Cause::UnparseablePath;
            return Some(finding);
        };
        finding.wanted_key = Some(base_key.clone());
        let source = self.sources.texture(&source_key);
        finding.source_has_dds = source.is_some();
        finding.source_archive = source.map(SourceLocation::label);
        finding.pack_has_ktx2 = self.pack_textures.has_base(&base_key);
        finding.glb_references_it = glb_bases.is_some_and(|bases| bases.contains(&base_key));
        let referenced_ok = glb_bases.is_none_or(|_| finding.glb_references_it);
        if finding.pack_has_ktx2 && referenced_ok {
            return None;
        }
        finding.cause = match (finding.pack_has_ktx2, finding.source_has_dds) {
            (false, false) => unresolved_cause_for_absent(unresolved_cause),
            (false, true) => unresolved_cause_for_missing(unresolved_cause),
            (true, _) => Cause::SlotUnreferenced,
        };
        if !finding.source_has_dds {
            let stem = source_key.rsplit('/').next().unwrap_or(&source_key);
            finding.same_name_elsewhere = self
                .sources
                .texture_stems
                .get(stem)
                .and_then(|keys| keys.first().cloned());
        }
        Some(finding)
    }
}

fn unresolved_cause_for_absent(cause: Cause) -> Cause {
    match cause {
        Cause::TextureSetUnresolved => Cause::TextureSetUnresolved,
        _ => Cause::SourceTextureAbsent,
    }
}

fn unresolved_cause_for_missing(cause: Cause) -> Cause {
    match cause {
        Cause::TextureSetUnresolved => Cause::TextureSetUnresolved,
        _ => Cause::ConvertedTextureMissing,
    }
}

fn glb_files(pack: &Path, filter: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = WalkDir::new(pack.join("meshes"))
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("glb"))
        })
        .filter_map(|entry| {
            let relative = entry
                .path()
                .strip_prefix(pack)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            filter
                .is_none_or(|text| relative.contains(&text.to_ascii_lowercase()))
                .then(|| (relative, entry.into_path()))
        })
        .collect();
    files.sort();
    files
}

/// Audits a pack. Reads the pack, the sources and the database; writes only to `scratch`.
pub fn audit_pack(options: &AuditOptions<'_>) -> Result<Report> {
    let pack_textures = PackTextures::scan(options.pack)?;
    let check = SlotCheck {
        pack_textures: &pack_textures,
        sources: options.sources,
    };
    let glbs = glb_files(options.pack, options.mesh_filter);
    fs::create_dir_all(options.scratch)?;
    let needed: Vec<String> = glbs
        .iter()
        .map(|(relative, _)| format!("{}.nif", relative.trim_end_matches(".glb")))
        .filter(|key| options.sources.mesh(key).is_some())
        .collect();
    options.sources.extract_meshes(&needed, options.scratch)?;

    struct MeshResult {
        findings: Vec<Finding>,
        wanted: u64,
        has_source: bool,
        failed: Option<String>,
    }

    let results: Vec<MeshResult> = glbs
        .par_iter()
        .map(|(relative, glb_path)| {
            let mut result = MeshResult {
                findings: Vec::new(),
                wanted: 0,
                has_source: false,
                failed: None,
            };
            let nif_key = format!("{}.nif", relative.trim_end_matches(".glb"));
            let Some(_) = options.sources.mesh(&nif_key) else {
                return result;
            };
            result.has_source = true;
            let facts = match glb_facts(options.pack, glb_path) {
                Ok(facts) => facts,
                Err(error) => {
                    result.failed = Some(format!("{relative}: {error:#}"));
                    return result;
                }
            };
            let shapes = options
                .sources
                .mesh_path(&nif_key, options.scratch)
                .ok_or_else(|| color_eyre::eyre::eyre!("no source for {nif_key}"))
                .and_then(|nif| MeshConverter::inspect_nif_materials(&nif));
            let shapes = match shapes {
                Ok(shapes) => shapes,
                Err(error) => {
                    result.failed = Some(format!("{relative}: {error:#}"));
                    return result;
                }
            };
            let mut seen = HashSet::new();
            let mut wanted = Vec::new();
            for shape in shapes {
                let crate::material::NifMaterialDisposition::Validated { material } =
                    shape.disposition
                else {
                    continue;
                };
                let shape_name = shape
                    .shape_name
                    .unwrap_or_else(|| format!("block {}", shape.shape_block));
                for slot in material.textures {
                    if slot.path.trim().is_empty() {
                        continue;
                    }
                    if seen.insert((shape.shape_block, slot.slot)) {
                        wanted.push(WantedSlot {
                            shape: shape_name.clone(),
                            slot: semantic_name(slot.semantic).to_owned(),
                            path: slot.path,
                            came_from: format!(
                                "NIF block {} texture slot {}",
                                shape.shape_block, slot.slot
                            ),
                        });
                    }
                }
            }
            result.wanted = wanted.len() as u64;
            for slot in &wanted {
                if let Some(finding) = check.check(
                    Origin::NifShape,
                    relative,
                    &slot.shape,
                    &slot.slot,
                    &slot.path,
                    &slot.came_from,
                    Some(&facts.referenced_bases),
                    Cause::SourceTextureAbsent,
                ) {
                    result.findings.push(finding);
                }
            }
            for primitive in &facts.no_uv_primitives {
                result.findings.push(Finding {
                    cause: Cause::PrimitiveWithoutUvs,
                    origin: Origin::GlbPrimitive,
                    mesh: relative.clone(),
                    shape: primitive.mesh.clone(),
                    slot: "base_color".to_owned(),
                    wanted_path: primitive.texture.clone().unwrap_or_default(),
                    wanted_key: primitive.texture.clone(),
                    came_from: format!("GLB material {}", primitive.material),
                    source_has_dds: true,
                    source_archive: None,
                    pack_has_ktx2: primitive.texture.is_some(),
                    glb_references_it: primitive.texture.is_some(),
                    same_name_elsewhere: None,
                    used_by: UsedBy::default(),
                });
            }
            result
        })
        .collect();

    let mut summary = Summary {
        glb_files: glbs.len() as u64,
        ..Summary::default()
    };
    let mut findings = Vec::new();
    for result in results {
        if result.has_source {
            summary.glb_files_with_source_nif += 1;
        } else {
            summary.glb_files_without_source_nif += 1;
        }
        if let Some(failure) = result.failed {
            summary.nif_files_failed += 1;
            summary.failed_nifs.push(failure);
        }
        summary.wanted_slots += result.wanted;
        findings.extend(result.findings);
    }
    let mut texture_set_paths = 0u64;
    let mut overrides = OverrideAudit::default();
    if let Some(database) = options.database {
        let connection = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .wrap_err_with(|| format!("failed to open {} read-only", database.display()))?;
        let (paths, record_findings) = audit_records(&connection, &check)?;
        texture_set_paths = paths;
        findings.extend(record_findings);
        let (audit, override_findings) = audit_overrides(&connection)?;
        overrides = audit;
        findings.extend(override_findings);
        attach_used_by(&connection, &mut findings)?;
    }
    summary.texture_set_paths = texture_set_paths;
    findings.sort_by(|left, right| {
        (left.cause, &left.mesh, &left.shape, &left.slot).cmp(&(
            right.cause,
            &right.mesh,
            &right.shape,
            &right.slot,
        ))
    });
    summarize(&mut summary, &findings);
    Ok(Report {
        schema_version: REPORT_SCHEMA_VERSION,
        summary,
        overrides,
        findings,
    })
}

fn summarize(summary: &mut Summary, findings: &[Finding]) {
    summary.unresolved_slots = findings.len() as u64;
    summary.resolved_slots = (summary.wanted_slots + summary.texture_set_paths)
        .saturating_sub(
            findings
                .iter()
                .filter(|finding| finding.origin != Origin::GlbPrimitive)
                .count() as u64,
        );
    let mut textures: BTreeMap<Cause, BTreeSet<&str>> = BTreeMap::new();
    let mut meshes: BTreeMap<Cause, BTreeSet<&str>> = BTreeMap::new();
    for finding in findings {
        *summary.counts_by_cause.entry(finding.cause).or_default() += 1;
        textures.entry(finding.cause).or_default().insert(
            finding
                .wanted_key
                .as_deref()
                .unwrap_or(finding.wanted_path.as_str()),
        );
        meshes
            .entry(finding.cause)
            .or_default()
            .insert(finding.mesh.as_str());
        if finding.used_by.references > 0 {
            *summary
                .placed_findings_by_cause
                .entry(finding.cause)
                .or_default() += 1;
        }
    }
    for cause in Cause::ALL {
        summary.counts_by_cause.entry(cause).or_default();
    }
    summary.distinct_textures_by_cause = textures
        .into_iter()
        .map(|(cause, set)| (cause, set.len() as u64))
        .collect();
    summary.distinct_meshes_by_cause = meshes
        .into_iter()
        .map(|(cause, set)| (cause, set.len() as u64))
        .collect();
}

/// Checks the texture paths of TXST and WATR records against the pack and the sources.
fn audit_records(connection: &Connection, check: &SlotCheck<'_>) -> Result<(u64, Vec<Finding>)> {
    let mut findings = Vec::new();
    let mut checked = 0u64;
    let columns = [
        ("diffuse", "diffuse_path"),
        ("normal", "normal_path"),
        ("glow", "glow_path"),
        ("height", "height_path"),
        ("environment", "environment_path"),
        ("mask", "mask_path"),
        ("specular", "specular_path"),
        ("detail", "detail_path"),
    ];
    for (slot, column) in columns {
        let sql = format!(
            "SELECT id,{column} FROM texture_sets WHERE {column} IS NOT NULL AND {column} <> '' ORDER BY id"
        );
        let Ok(mut statement) = connection.prepare(&sql) else {
            // An older database without this column has nothing to audit for it.
            continue;
        };
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, path) in rows {
            checked += 1;
            if let Some(finding) = check.check(
                Origin::TextureSet,
                &format!("TXST {id:08X}"),
                "",
                slot,
                &path,
                &format!("TXST {id:08X} {column}"),
                None,
                Cause::TextureSetUnresolved,
            ) {
                findings.push(finding);
            }
        }
    }
    if let Ok(mut statement) = connection.prepare(
        "SELECT id,flow_normal_path FROM waters WHERE flow_normal_path IS NOT NULL AND flow_normal_path <> '' ORDER BY id",
    ) {
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, path) in rows {
            checked += 1;
            if let Some(mut finding) = check.check(
                Origin::TextureSet,
                &format!("WATR {id:08X}"),
                "",
                "flow_normal",
                &path,
                &format!("WATR {id:08X} flow_normal_path"),
                None,
                Cause::TextureSetUnresolved,
            ) {
                finding.origin = Origin::Water;
                findings.push(finding);
            }
        }
    }
    audit_land_textures(connection, &mut findings, &mut checked)?;
    Ok((checked, findings))
}

/// Terrain layers: an LTEX whose texture set has no diffuse path, and LAND layers that name an
/// LTEX no plugin defines. The engine draws both with Bevy's stand-in image.
fn audit_land_textures(
    connection: &Connection,
    findings: &mut Vec<Finding>,
    checked: &mut u64,
) -> Result<()> {
    let land_finding = |mesh: String, came_from: String, references: u64| Finding {
        cause: Cause::LandTextureUnresolved,
        origin: Origin::TextureSet,
        mesh,
        shape: String::new(),
        slot: "diffuse".to_owned(),
        wanted_path: String::new(),
        wanted_key: None,
        came_from,
        source_has_dds: false,
        source_archive: None,
        pack_has_ktx2: false,
        glb_references_it: false,
        same_name_elsewhere: None,
        used_by: UsedBy {
            static_records: 0,
            references,
            cells: Vec::new(),
        },
    };
    if let Ok(mut statement) = connection.prepare(
        "SELECT l.id, l.editor_id FROM landscape_textures l
         LEFT JOIN texture_sets t ON t.id = l.texture_set_id
         WHERE t.diffuse_path IS NULL OR t.diffuse_path = '' ORDER BY l.id",
    ) {
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        *checked += rows.len() as u64;
        for (id, editor_id) in rows {
            findings.push(land_finding(
                format!("LTEX {id:08X}"),
                format!(
                    "LTEX {id:08X} {} has no texture set with a diffuse path",
                    editor_id.unwrap_or_default()
                ),
                0,
            ));
        }
    }
    if let Ok(mut statement) = connection.prepare(
        "SELECT texture_form_id, count(*) FROM inhouse_unresolved_terrain_textures
         GROUP BY texture_form_id ORDER BY texture_form_id",
    ) {
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        *checked += rows.len() as u64;
        for (id, lands) in rows {
            findings.push(land_finding(
                format!("LTEX {id:08X}"),
                format!("{lands} LAND records name LTEX {id:08X}, which no plugin defines"),
                lands,
            ));
        }
    }
    Ok(())
}

/// Fills `used_by` from the static placements of each affected mesh.
fn attach_used_by(connection: &Connection, findings: &mut [Finding]) -> Result<()> {
    let wanted: HashSet<&str> = findings
        .iter()
        .filter(|finding| finding.origin != Origin::TextureSet && finding.origin != Origin::Water)
        .map(|finding| finding.mesh.as_str())
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let mut statement = connection.prepare(
        "SELECT s.model_path, s.id, r.cell_id, count(r.id)
         FROM statics s LEFT JOIN \"references\" r ON r.base_form_id = s.id
         WHERE s.model_path IS NOT NULL AND s.model_path <> ''
         GROUP BY s.model_path, s.id, r.cell_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, Option<u32>>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })?;
    let mut by_mesh: HashMap<String, (HashSet<u32>, u64, HashMap<u32, u64>)> = HashMap::new();
    for row in rows {
        let (model, static_id, cell_id, count) = row?;
        let Ok(key) = canonical_asset_path(&model, AssetKind::Mesh, "glb") else {
            continue;
        };
        if !wanted.contains(key.as_str()) {
            continue;
        }
        let entry = by_mesh.entry(key).or_default();
        entry.0.insert(static_id);
        if let Some(cell_id) = cell_id {
            entry.1 += count;
            *entry.2.entry(cell_id).or_default() += count;
        }
    }
    let cell_label = |cell_id: u32| -> String {
        connection
            .query_row(
                "SELECT c.interior_name, c.full_name, c.grid_x, c.grid_y, w.editor_id
                 FROM cells c LEFT JOIN worldspaces w ON w.id = c.worldspace_id WHERE c.id = ?1",
                [cell_id],
                |row| {
                    let interior: Option<String> = row.get(0)?;
                    let full: Option<String> = row.get(1)?;
                    let grid: (Option<i32>, Option<i32>) = (row.get(2)?, row.get(3)?);
                    let world: Option<String> = row.get(4)?;
                    Ok(match (interior, grid, world) {
                        (Some(name), _, Some(world)) if !name.is_empty() => {
                            format!("{world} {name}")
                        }
                        (Some(name), _, None) if !name.is_empty() => format!("interior {name}"),
                        (_, (Some(x), Some(y)), Some(world)) => format!("{world} ({x},{y})"),
                        _ => full.unwrap_or_else(|| format!("cell {cell_id:08X}")),
                    })
                },
            )
            .unwrap_or_else(|_| format!("cell {cell_id:08X}"))
    };
    let mut cache: HashMap<String, UsedBy> = HashMap::new();
    for finding in findings.iter_mut() {
        let Some((statics, references, cells)) = by_mesh.get(&finding.mesh) else {
            continue;
        };
        let used = cache.entry(finding.mesh.clone()).or_insert_with(|| {
            let mut ranked: Vec<(u32, u64)> = cells.iter().map(|(&c, &n)| (c, n)).collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            UsedBy {
                static_records: statics.len() as u64,
                references: *references,
                cells: ranked
                    .into_iter()
                    .take(MAX_CELLS_PER_MESH)
                    .map(|(cell_id, references)| CellUse {
                        cell_id,
                        label: cell_label(cell_id),
                        references,
                    })
                    .collect(),
            }
        });
        finding.used_by = used.clone();
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Alternate textures
// ---------------------------------------------------------------------------------------------

/// The alternate-texture subrecords: `MODS` on the first model, `MO2S`..`MO4S` on the others.
const ALTERNATE_TEXTURE_SUBRECORDS: [&[u8; 4]; 4] = [b"MODS", b"MO2S", b"MO3S", b"MO4S"];

/// The form id the retail engine gives its NullTextureSet singleton (RF-TEX-005, medium
/// confidence on the id). An entry that targets it hides the shape instead of retexturing it.
const NULL_TEXTURE_SET_ID: u32 = 0x28;

/// One alternate-texture entry as stored: the shape's name, the texture set, and the 3D index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternateTexture {
    pub name: String,
    /// The texture set's form id exactly as the plugin wrote it (master index in the top byte).
    pub texture_set: u32,
    /// The geometry-leaf number the entry applies to; -1 entries are skipped (RF-TEX-001).
    pub index: i32,
}

/// Reads the entries of one alternate-texture subrecord body: a `u32` count, then per entry a
/// `u32` length, that many name bytes, a `u32` texture-set id and an `i32` 3D index.
pub fn parse_alternate_textures(body: &[u8]) -> Option<Vec<AlternateTexture>> {
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(body.get(at..at.checked_add(4)?)?.try_into().ok()?))
    };
    let count = usize::try_from(word(0)?).ok()?;
    let mut entries = Vec::new();
    let mut at = 4usize;
    for _ in 0..count {
        let length = usize::try_from(word(at)?).ok()?;
        at += 4;
        let name = body.get(at..at.checked_add(length)?)?;
        at += length;
        entries.push(AlternateTexture {
            name: String::from_utf8_lossy(name).into_owned(),
            texture_set: word(at)?,
            index: i32::from_le_bytes(body.get(at + 4..at + 8)?.try_into().ok()?),
        });
        at += 8;
    }
    Some(entries)
}

/// The alternate-texture subrecords of one record payload (a run of `type, u16 size, data`).
fn alternate_texture_subrecords(payload: &[u8]) -> Vec<Vec<AlternateTexture>> {
    let mut found = Vec::new();
    let mut at = 0usize;
    while let Some(header) = payload.get(at..at + 6) {
        let size = usize::from(u16::from_le_bytes([header[4], header[5]]));
        let Some(body) = payload.get(at + 6..at + 6 + size) else {
            break;
        };
        if ALTERNATE_TEXTURE_SUBRECORDS
            .iter()
            .any(|name| &header[..4] == *name)
            && let Some(entries) = parse_alternate_textures(body)
        {
            found.push(entries);
        }
        at += 6 + size;
    }
    found
}

/// What the alternate-texture entries of a pack come to, by the retail rules (RF-TEX-001..005).
///
/// Neither the converter nor the engine applies these entries yet (CF-ALT-TEXTURE-001, issue
/// 508), so every surface they name keeps its model's own texture. The counts show how many
/// entries that affects and how many of them are retail behaviour rather than misses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverrideAudit {
    pub records_with_alternate_textures: u64,
    pub entries: u64,
    /// Entries with 3D index -1: the retail engine skips them (RF-TEX-001).
    pub skipped_index_minus_one: u64,
    /// Entries that target NullTextureSet: retail hides the shape (RF-TEX-005). Not a miss.
    pub hide_shape_entries: u64,
    /// Entries whose texture set has no path in any slot: retail builds an empty set and the
    /// shader defaults apply (RF-TEX-004). Not a miss.
    pub empty_texture_set_entries: u64,
    /// Entries whose texture set has at least one path. Those paths are audited as `TXST`.
    pub entries_with_texture_paths: u64,
    /// Entries whose texture set no plugin defines. Counted as a miss.
    pub unresolved_target_entries: u64,
    pub by_record_type: BTreeMap<String, u64>,
    /// True once the converter and engine apply the entries; false today.
    pub engine_applies_entries: bool,
}

fn audit_overrides(connection: &Connection) -> Result<(OverrideAudit, Vec<Finding>)> {
    let mut audit = OverrideAudit::default();
    let mut findings = Vec::new();
    let Ok(mut records) = connection.prepare(
        "SELECT form_id, record_type, load_order, payload FROM inhouse_source_records
         WHERE instr(payload, X'4D4F4453') > 0 OR instr(payload, X'4D4F3253') > 0
            OR instr(payload, X'4D4F3353') > 0 OR instr(payload, X'4D4F3453') > 0",
    ) else {
        return Ok((audit, findings));
    };
    let rows = records
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Texture sets that hold at least one path; any other defined set is empty.
    let mut with_paths: HashMap<u32, bool> = HashMap::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT id, coalesce(diffuse_path,'') || coalesce(normal_path,'') || coalesce(glow_path,'')
                || coalesce(height_path,'') || coalesce(environment_path,'')
                || coalesce(mask_path,'') || coalesce(specular_path,'') || coalesce(detail_path,'')
         FROM texture_sets",
    ) {
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, paths) = row?;
            with_paths.insert(id, !paths.is_empty());
        }
    }
    let plugin_name = |priority: u32| -> Option<(u32, String)> {
        connection
            .query_row(
                "SELECT id, name FROM plugins WHERE priority = ?1",
                [priority],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()
    };
    let mut masters_cache: HashMap<u32, Vec<String>> = HashMap::new();
    let mut global_cache: HashMap<(String, u32), Option<u32>> = HashMap::new();
    for (form_id, record_type, load_order, payload) in rows {
        let lists = alternate_texture_subrecords(&payload);
        if lists.is_empty() {
            continue;
        }
        audit.records_with_alternate_textures += 1;
        *audit.by_record_type.entry(record_type.clone()).or_default() += 1;
        let Some((plugin_id, own_name)) = plugin_name(load_order) else {
            continue;
        };
        let masters = masters_cache.entry(plugin_id).or_insert_with(|| {
            let mut masters: Vec<(u32, String)> = connection
                .prepare(
                    "SELECT master_index, master_name FROM inhouse_plugin_masters
                     WHERE plugin_id = ?1",
                )
                .and_then(|mut statement| {
                    statement
                        .query_map([plugin_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap_or_default();
            masters.sort();
            masters.into_iter().map(|(_, name)| name).collect()
        });
        for entry in lists.into_iter().flatten() {
            audit.entries += 1;
            if entry.index == -1 {
                audit.skipped_index_minus_one += 1;
                continue;
            }
            let master = usize::try_from(entry.texture_set >> 24).unwrap_or(usize::MAX);
            let target_plugin = masters.get(master).unwrap_or(&own_name).to_ascii_lowercase();
            let internal = entry.texture_set & 0x00FF_FFFF;
            if target_plugin == "skyrim.esm" && internal == NULL_TEXTURE_SET_ID {
                audit.hide_shape_entries += 1;
                continue;
            }
            let global = *global_cache
                .entry((target_plugin.clone(), internal))
                .or_insert_with(|| {
                    connection
                        .query_row(
                            "SELECT form_id FROM formid_map
                             WHERE plugin_name = ?1 AND internal_id = ?2",
                            rusqlite::params![target_plugin, internal],
                            |row| row.get(0),
                        )
                        .ok()
                });
            match global.and_then(|id| with_paths.get(&id).copied()) {
                Some(true) => audit.entries_with_texture_paths += 1,
                Some(false) => audit.empty_texture_set_entries += 1,
                None => {
                    audit.unresolved_target_entries += 1;
                    findings.push(Finding {
                        cause: Cause::OverrideTargetUnresolved,
                        origin: Origin::TextureSet,
                        mesh: format!("{record_type} {form_id:08X}"),
                        shape: entry.name.clone(),
                        slot: format!("3D index {}", entry.index),
                        wanted_path: format!("TXST {:08X}", entry.texture_set),
                        wanted_key: None,
                        came_from: format!("{record_type} {form_id:08X} alternate textures"),
                        source_has_dds: false,
                        source_archive: None,
                        pack_has_ktx2: false,
                        glb_references_it: false,
                        same_name_elsewhere: None,
                        used_by: UsedBy::default(),
                    });
                }
            }
        }
    }
    Ok((audit, findings))
}

// ---------------------------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------------------------

/// One CSV row per finding. Fields are quoted when they hold a comma, quote or newline.
pub fn findings_csv(report: &Report) -> String {
    fn field(value: &str) -> String {
        if value.contains([',', '"', '\n']) {
            format!("\"{}\"", value.replace('"', "\"\""))
        } else {
            value.to_owned()
        }
    }
    let mut out = String::from(
        "cause,origin,mesh,shape,slot,wanted_path,wanted_key,came_from,source_has_dds,source_archive,pack_has_ktx2,glb_references_it,same_name_elsewhere,references,cells\n",
    );
    for finding in &report.findings {
        let cells = finding
            .used_by
            .cells
            .iter()
            .map(|cell| format!("{} x{}", cell.label, cell.references))
            .collect::<Vec<_>>()
            .join("; ");
        let row = [
            format!("{:?}", finding.cause),
            format!("{:?}", finding.origin),
            finding.mesh.clone(),
            finding.shape.clone(),
            finding.slot.clone(),
            finding.wanted_path.clone(),
            finding.wanted_key.clone().unwrap_or_default(),
            finding.came_from.clone(),
            finding.source_has_dds.to_string(),
            finding.source_archive.clone().unwrap_or_default(),
            finding.pack_has_ktx2.to_string(),
            finding.glb_references_it.to_string(),
            finding.same_name_elsewhere.clone().unwrap_or_default(),
            finding.used_by.references.to_string(),
            cells,
        ];
        out.push_str(&row.iter().map(|value| field(value)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with(counts: &[(Cause, u64)]) -> Report {
        let mut report = Report::default();
        for (cause, count) in counts {
            report.summary.counts_by_cause.insert(*cause, *count);
        }
        report
    }

    #[test]
    fn ratchet_fails_only_when_a_cause_grows() {
        let baseline = Baseline::from_report(
            &report_with(&[(Cause::SourceTextureAbsent, 10), (Cause::SlotUnreferenced, 2)]),
            "test",
        );
        assert!(
            compare_to_baseline(
                &report_with(&[(Cause::SourceTextureAbsent, 9), (Cause::SlotUnreferenced, 2)]),
                &baseline
            )
            .is_empty()
        );
        let regressions = compare_to_baseline(
            &report_with(&[(Cause::SourceTextureAbsent, 10), (Cause::PrimitiveWithoutUvs, 1)]),
            &baseline,
        );
        assert_eq!(regressions.len(), 1);
        assert!(regressions[0].contains("PrimitiveWithoutUvs"));
    }

    fn mods_body(entries: &[(&str, u32, i32)]) -> Vec<u8> {
        let mut body = (entries.len() as u32).to_le_bytes().to_vec();
        for (name, texture_set, index) in entries {
            body.extend((name.len() as u32).to_le_bytes());
            body.extend(name.as_bytes());
            body.extend(texture_set.to_le_bytes());
            body.extend(index.to_le_bytes());
        }
        body
    }

    #[test]
    fn alternate_texture_entries_parse_and_truncated_bodies_do_not() {
        let body = mods_body(&[("TundraStreamBend01:0", 0x0002_4E2F, 4), ("DirtDecal", 0x28, -1)]);
        let entries = parse_alternate_textures(&body).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].texture_set, 0x0002_4E2F);
        assert_eq!(entries[0].index, 4);
        assert_eq!(entries[1].index, -1);
        assert!(parse_alternate_textures(&body[..body.len() - 1]).is_none());
        assert!(parse_alternate_textures(&[]).is_none());
        // A record payload is a run of subrecords; only the four alternate-texture ones count.
        let mut payload = b"EDID\x02\x00ab".to_vec();
        payload.extend(b"MODS");
        payload.extend((body.len() as u16).to_le_bytes());
        payload.extend(&body);
        let lists = alternate_texture_subrecords(&payload);
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].len(), 2);
    }

    #[test]
    fn csv_quotes_fields_with_commas() {
        let mut report = Report::default();
        report.findings.push(Finding {
            cause: Cause::SourceTextureAbsent,
            origin: Origin::NifShape,
            mesh: "meshes/a,b.glb".into(),
            shape: String::new(),
            slot: "diffuse".into(),
            wanted_path: "textures\\x.dds".into(),
            wanted_key: Some("textures/x.ktx2".into()),
            came_from: "NIF block 1 texture slot 0".into(),
            source_has_dds: false,
            source_archive: None,
            pack_has_ktx2: false,
            glb_references_it: false,
            same_name_elsewhere: None,
            used_by: UsedBy::default(),
        });
        let csv = findings_csv(&report);
        assert!(csv.lines().nth(1).unwrap().contains("\"meshes/a,b.glb\""));
    }
}
