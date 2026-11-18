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
    /// A primitive uses a textured material but has no `TEXCOORD_0`. Bevy 0.19 then skips the
    /// base-colour sample, so the surface draws its base colour.
    PrimitiveWithoutUvs,
    /// A texture set (TXST) or water path names no converted texture.
    TextureSetUnresolved,
    /// A terrain layer has no diffuse image: its LTEX record has no texture set with a
    /// diffuse path, or a LAND record names an LTEX that no plugin defines.
    LandTextureUnresolved,
    /// An alternate-texture entry (`MODS`/`MO2S`/`MO3S`/`MO4S`) names a texture set that no
    /// plugin defines and that is not the engine's NullTextureSet (RF-TEX-005).
    OverrideTargetUnresolved,
    /// The audit cannot tell which GLB material belongs to the NIF shape (no material that a
    /// primitive uses carries its shape block, or several do), so it cannot say whether the
    /// slot is bound. The texture exists, so the slot is not counted as resolved.
    BindingAmbiguous,
}

impl Cause {
    pub const ALL: [Self; 9] = [
        Self::SourceTextureAbsent,
        Self::ConvertedTextureMissing,
        Self::SlotUnreferenced,
        Self::UnparseablePath,
        Self::PrimitiveWithoutUvs,
        Self::TextureSetUnresolved,
        Self::LandTextureUnresolved,
        Self::OverrideTargetUnresolved,
        Self::BindingAmbiguous,
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
    /// Of those, the GLBs audited from their own contents (image references and UVs).
    #[serde(default)]
    pub glb_files_audited_without_source: u64,
    /// Meshes the audit could not read (a GLB or its source NIF). Their findings are missing, so
    /// the audit is incomplete and no baseline check may pass.
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
    /// Selected archives the lister could not read, with the reason. Their textures are missing
    /// from the catalog, so the audit is incomplete.
    #[serde(default)]
    pub skipped_archives: Vec<String>,
    /// Archives left out because the plugin that owns them is installed but not active.
    #[serde(default)]
    pub inactive_archives: Vec<String>,
    /// True when the database was read (texture sets, terrain layers, alternate textures).
    #[serde(default)]
    pub records_audited: bool,
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

/// Why the audit did not look at everything it should have. Findings that a skipped mesh, archive
/// or table would have produced are missing, so a count that looks like an improvement may be a
/// blind spot.
pub fn incomplete(report: &Report) -> Vec<String> {
    let summary = &report.summary;
    let mut reasons = Vec::new();
    if summary.nif_files_failed != 0 {
        reasons.push(format!(
            "{} meshes could not be read, for example {}",
            summary.nif_files_failed,
            summary.failed_nifs.first().map_or("", String::as_str)
        ));
    }
    for archive in &summary.skipped_archives {
        reasons.push(format!("archive could not be listed: {archive}"));
    }
    if report.overrides.undecodable_records != 0 {
        reasons.push(format!(
            "{} records with alternate textures could not be decoded",
            report.overrides.undecodable_records
        ));
    }
    if summary.glb_files == 0 {
        reasons.push("no GLB was audited".to_owned());
    }
    if summary.glb_files_with_source_nif != 0 && summary.wanted_slots == 0 {
        reasons.push("no NIF texture slot was read".to_owned());
    }
    if summary.records_audited && summary.texture_set_paths == 0 {
        reasons.push("no texture set or terrain layer was read; did the schema change?".to_owned());
    }
    reasons
}

/// One message per cause whose count exceeds the baseline, and one per reason the audit is
/// incomplete; empty means the ratchet holds.
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
    regressions.extend(
        incomplete(report)
            .into_iter()
            .map(|reason| format!("incomplete audit: {reason}")),
    );
    regressions
}

/// One message per cause whose count is now below the baseline. The baseline is a ceiling that
/// has to follow the count down, or a later regression up to the old ceiling would pass.
pub fn improvements(report: &Report, baseline: &Baseline) -> Vec<String> {
    Cause::ALL
        .into_iter()
        .filter_map(|cause| {
            let now = report
                .summary
                .counts_by_cause
                .get(&cause)
                .copied()
                .unwrap_or(0);
            let allowed = baseline.max_by_cause.get(&cause).copied().unwrap_or(0);
            (now < allowed)
                .then(|| format!("{cause:?}: lower the baseline from {allowed} to {now}"))
        })
        .collect()
}

/// Reads the plugin load order of a pack's world database, read-only.
pub fn plugin_names(database: &Path) -> Result<Vec<String>> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .wrap_err_with(|| format!("failed to open {} read-only", database.display()))?;
    let mut statement = connection.prepare("SELECT name FROM plugins ORDER BY priority")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names)
}

/// Takes the shared asset lock of a pack, as the engine does. The lock does not block: while the
/// audit holds it, a conversion that wants the exclusive lock for the same output fails at once
/// with an error, and the audit itself fails at once when a conversion already holds the pack.
/// So the plugin order, the GLBs and the database the audit reads belong to one pack. Hold the
/// returned lock from the first read to the last. Taking it creates `<pack>.lock` beside the pack
/// when that file is missing, so the folder that holds the pack must be writable.
pub fn lock_pack(pack: &Path) -> Result<shared::asset_lock::AssetLock> {
    shared::asset_lock::AssetLock::acquire_shared(pack)
        .map_err(|error| color_eyre::eyre::eyre!("cannot lock {}: {error}", pack.display()))
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

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Every mesh and texture the game data supplies, keyed like the converter keys them
/// (`meshes/x.nif`, `textures/y.dds`). Later archives win, loose files win over archives.
#[derive(Debug, Default)]
pub struct SourceCatalog {
    meshes: HashMap<String, SourceLocation>,
    textures: HashMap<String, SourceLocation>,
    texture_stems: HashMap<String, Vec<String>>,
    /// Archives the lister could not read. Their files are missing from the catalog, so an
    /// audit that skipped one is incomplete and must not pass a baseline.
    skipped_archives: Vec<String>,
    /// Archives left out because the plugin that owns them is installed but not active.
    inactive_archives: Vec<String>,
}

impl SourceCatalog {
    /// Indexes the archives (`*.bsa`, `*.ba2`) and loose `meshes`/`textures` under `data_dir`.
    ///
    /// `plugin_names` is the active load order. Archives are selected the way the converter
    /// selects them: an archive owned by an installed plugin that is not active is left out, and
    /// an archive no plugin owns stays eligible. Selected archives rank by the load order of the
    /// plugin that owns them, with unowned archives last. A selected archive that cannot be
    /// listed is recorded in [`Self::skipped_archives`].
    pub fn build(data_dir: &Path, plugin_names: &[String]) -> Result<Self> {
        let mut files = Vec::new();
        let mut archives = Vec::new();
        let mut loose_roots = Vec::new();
        for entry in fs::read_dir(data_dir)
            .wrap_err_with(|| format!("failed to read {}", data_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            // A case-sensitive filesystem can hold `Meshes` and `Textures`; the converter reads
            // them, so the audit matches the folder names without regard to case.
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.eq_ignore_ascii_case("meshes") || name.eq_ignore_ascii_case("textures") {
                    loose_roots.push(path);
                }
                continue;
            }
            if path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bsa") || extension.eq_ignore_ascii_case("ba2")
            }) {
                archives.push(path.clone());
            }
            files.push(path);
        }
        let plugins: Vec<PathBuf> = plugin_names
            .iter()
            .map(|name| data_dir.join(name))
            .collect();
        let owners = crate::pipeline::installed_archive_owners(data_dir, &files, &plugins);
        let mut catalog = Self::default();
        archives.retain(|archive| {
            let active = crate::pipeline::non_mo2_archive_is_active(archive, &owners);
            if !active {
                catalog.inactive_archives.push(file_name(archive));
            }
            active
        });
        crate::pipeline::sort_archives_by_load_order(&mut archives, &plugins);
        for archive in &archives {
            let listed = match ArchiveExtractor::list_paths(archive) {
                Ok(listed) => listed,
                Err(error) => {
                    catalog
                        .skipped_archives
                        .push(format!("{}: {error:#}", file_name(archive)));
                    continue;
                }
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
        loose_roots.sort();
        for root in loose_roots {
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
        catalog.skipped_archives.sort();
        catalog.index_stems();
        Ok(catalog)
    }

    /// Archives that were selected but could not be listed, each with the reason.
    pub fn skipped_archives(&self) -> &[String] {
        &self.skipped_archives
    }

    /// Archives left out because their owning plugin is installed but not active.
    pub fn inactive_archives(&self) -> &[String] {
        &self.inactive_archives
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
            self.texture_stems
                .entry(stem)
                .or_default()
                .push(key.clone());
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

/// One external image of a GLB, as the engine requests it.
#[derive(Debug, Clone)]
struct ImageRef {
    /// The URI as written in the GLB.
    uri: String,
    /// The pack-relative path the URI names (lowercase, `/`): the file the engine loads.
    exact: String,
    /// The converted texture that file aliases, for example `textures/a/b.ktx2`.
    base: String,
}

/// The images a material binds, found through the NIF shape block the converter records.
///
/// Every slot the converter publishes is here: the glTF bindings (base colour, normal, emissive
/// for the glow slot, specular colour for the `_s` slot) and the other slots the converter keeps
/// in `OPEN_SKYRIM_material.textureSlots`, keyed by NIF slot index.
#[derive(Debug, Clone, Default)]
struct MaterialBinding {
    shape_block: Option<u32>,
    /// True when at least one primitive uses this material. A material nothing draws with
    /// proves nothing about a shape.
    used: bool,
    /// Indices into `GlbFacts::images`.
    diffuse: Option<usize>,
    normal: Option<usize>,
    emissive: Option<usize>,
    specular_color: Option<usize>,
    /// NIF slot index to image index.
    slots: Vec<(u8, usize)>,
}

impl MaterialBinding {
    /// The image this material binds for a NIF slot of the given meaning.
    fn image_for(&self, semantic: NifTextureSemantic, slot: u8) -> Option<usize> {
        match semantic {
            NifTextureSemantic::Diffuse => self.diffuse,
            NifTextureSemantic::Normal => self.normal,
            NifTextureSemantic::Glow => self.emissive,
            NifTextureSemantic::Specular => self.specular_color,
            _ => self
                .slots
                .iter()
                .find(|(index, _)| *index == slot)
                .map(|(_, image)| *image),
        }
    }
}

/// The GLB material that belongs to one NIF shape.
enum ShapeMaterial<'a> {
    /// Exactly one material that a primitive uses carries the shape block.
    Unique(&'a MaterialBinding),
    /// No used material, or several, carry the shape block; the reason says which.
    Unmatched(String),
}

/// What a GLB says about textures.
#[derive(Debug, Default)]
struct GlbFacts {
    /// Every image in glTF order; `None` for an embedded image or one with a refused URI.
    images: Vec<Option<ImageRef>>,
    /// The external images, once each by exact path.
    external: Vec<ImageRef>,
    /// Image URIs the resolver refused, with the reason (traversal, bad escape, not `.ktx2`).
    refused_uris: Vec<(String, String)>,
    bindings: Vec<MaterialBinding>,
    /// Texture path repairs the converter published: original base key to replacement base key.
    repairs: HashMap<String, String>,
    /// Primitives with a textured material and no `TEXCOORD_0`.
    no_uv_primitives: Vec<NoUv>,
}

#[derive(Debug)]
struct NoUv {
    mesh: String,
    material: String,
    texture: Option<String>,
}

/// Finds the material of a NIF shape by following the primitives: only a material that some
/// primitive uses counts, and it must be the only one that carries the shape block. A missing or
/// ambiguous match is reported, never read as "bound".
fn shape_material(facts: &GlbFacts, shape_block: u32) -> ShapeMaterial<'_> {
    let used: Vec<&MaterialBinding> = facts
        .bindings
        .iter()
        .filter(|binding| binding.used && binding.shape_block == Some(shape_block))
        .collect();
    match used.as_slice() {
        [only] => ShapeMaterial::Unique(only),
        [] => ShapeMaterial::Unmatched(format!(
            "no material used by a primitive carries shape block {shape_block}"
        )),
        several => ShapeMaterial::Unmatched(format!(
            "{} materials used by primitives carry shape block {shape_block}",
            several.len()
        )),
    }
}

/// Reads the JSON chunk of a GLB without reading geometry. The header must be a version-2 GLB
/// whose declared length matches the file and whose first chunk is JSON inside those bounds, so a
/// truncated or hostile file is refused before any allocation sized from its header.
fn read_glb_json(path: &Path) -> Result<serde_json::Value> {
    let mut file =
        fs::File::open(path).wrap_err_with(|| format!("failed to open {}", path.display()))?;
    let file_length = file.metadata()?.len();
    let mut header = [0u8; 20];
    file.read_exact(&mut header)
        .wrap_err_with(|| format!("truncated GLB header in {}", path.display()))?;
    let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let total = u32::from_le_bytes(header[8..12].try_into().unwrap());
    let json_length = u32::from_le_bytes(header[12..16].try_into().unwrap());
    ensure!(
        &header[..4] == b"glTF"
            && version == 2
            && &header[16..20] == b"JSON"
            && u64::from(total) == file_length
            && u64::from(json_length) + 20 <= u64::from(total),
        "invalid GLB JSON chunk in {}",
        path.display()
    );
    serde_json::from_reader(std::io::BufReader::new(file.take(u64::from(json_length))))
        .wrap_err_with(|| format!("invalid GLB JSON in {}", path.display()))
}

/// Resolves one image URI to the file the engine loads, with the converter's resolver.
fn image_ref(pack: &Path, glb: &Path, uri: &str) -> std::result::Result<ImageRef, String> {
    let path = resolve_asset_uri(pack, glb, uri).map_err(|error| format!("{error:#}"))?;
    let exact = path
        .strip_prefix(pack)
        .map_err(|_| "the URI names a path outside the pack".to_owned())?
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let base = runtime_texture_source(&exact)
        .ok_or_else(|| "the URI is not a converted .ktx2 texture".to_owned())?;
    Ok(ImageRef {
        uri: uri.to_owned(),
        exact,
        base,
    })
}

fn glb_facts(pack: &Path, glb: &Path) -> Result<GlbFacts> {
    let document = read_glb_json(glb)?;
    let mut facts = GlbFacts::default();
    let mut seen = HashSet::new();
    for image in document
        .get("images")
        .and_then(|images| images.as_array())
        .into_iter()
        .flatten()
    {
        let uri = image
            .get("uri")
            .and_then(|uri| uri.as_str())
            .filter(|uri| !uri.starts_with("data:"));
        let resolved = match uri {
            Some(uri) => match image_ref(pack, glb, uri) {
                Ok(reference) => Some(reference),
                Err(reason) => {
                    facts.refused_uris.push((uri.to_owned(), reason));
                    None
                }
            },
            None => None,
        };
        if let Some(reference) = &resolved
            && seen.insert(reference.exact.clone())
        {
            facts.external.push(reference.clone());
        }
        facts.images.push(resolved);
    }
    // The converter records the images it replaced (the two volcanic grass normals). The audit
    // accepts the replacement where the authored path is the original.
    if let Some(repairs) = document
        .pointer("/extras/mudcrab/texturePathRepairs")
        .and_then(|repairs| repairs.as_object())
    {
        for (original, replacement) in repairs {
            let Some(replacement) = replacement.as_str() else {
                continue;
            };
            if let (Ok(original), Ok(replacement)) = (
                image_ref(pack, glb, original),
                image_ref(pack, glb, replacement),
            ) {
                facts.repairs.insert(original.base, replacement.base);
            }
        }
    }
    let textures = document
        .get("textures")
        .and_then(|textures| textures.as_array())
        .cloned()
        .unwrap_or_default();
    // glTF texture index to glTF image index.
    let image_of_texture = |texture: Option<&serde_json::Value>| -> Option<usize> {
        let index = usize::try_from(texture?.get("index")?.as_u64()?).ok()?;
        usize::try_from(textures.get(index)?.get("source")?.as_u64()?).ok()
    };
    let materials = document
        .get("materials")
        .and_then(|materials| materials.as_array())
        .cloned()
        .unwrap_or_default();
    for material in &materials {
        let slots = material
            .pointer("/extensions/OPEN_SKYRIM_material/textureSlots")
            .and_then(|slots| slots.as_array())
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let slot = u8::try_from(entry.get("slot")?.as_u64()?).ok()?;
                let texture = serde_json::json!({ "index": entry.get("texture")? });
                Some((slot, image_of_texture(Some(&texture))?))
            })
            .collect();
        facts.bindings.push(MaterialBinding {
            shape_block: material
                .pointer("/extras/openSkyrim/shapeBlock")
                .and_then(|block| block.as_u64())
                .and_then(|block| u32::try_from(block).ok()),
            used: false,
            diffuse: image_of_texture(material.pointer("/pbrMetallicRoughness/baseColorTexture")),
            normal: image_of_texture(material.get("normalTexture")),
            emissive: image_of_texture(material.get("emissiveTexture")),
            specular_color: image_of_texture(
                material.pointer("/extensions/KHR_materials_specular/specularColorTexture"),
            ),
            slots,
        });
    }
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
            let Some(index) = primitive
                .get("material")
                .and_then(|material| material.as_u64())
                .and_then(|material| usize::try_from(material).ok())
            else {
                continue;
            };
            let (Some(binding), Some(material)) =
                (facts.bindings.get_mut(index), materials.get(index))
            else {
                continue;
            };
            binding.used = true;
            let binding = binding.clone();
            let has_uv = primitive
                .get("attributes")
                .and_then(|attributes| attributes.get("TEXCOORD_0"))
                .is_some();
            if has_uv {
                continue;
            }
            if let Some(image) = binding.diffuse.or(binding.normal) {
                facts.no_uv_primitives.push(NoUv {
                    mesh: mesh_name.to_owned(),
                    material: material
                        .get("name")
                        .and_then(|name| name.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    texture: facts
                        .images
                        .get(image)
                        .and_then(|image| image.as_ref())
                        .map(|image| image.base.clone()),
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
    shape_block: u32,
    /// The NIF texture slot index (`BSShaderTextureSet` order).
    slot_index: u8,
    semantic: NifTextureSemantic,
    shape: String,
    slot: String,
    path: String,
    came_from: String,
}

/// What the GLB can show for one wanted texture.
enum SlotView<'a> {
    /// A record path: the engine loads the converted file that
    /// [`shared::texture_aliases::converted_record_texture_path`] names.
    Record,
    /// The image the shape's own material binds to this slot. The material is the only one that
    /// a primitive uses and that carries the shape block. `None` means it binds no image there.
    Bound {
        image: Option<&'a ImageRef>,
        repairs: &'a HashMap<String, String>,
    },
    /// No single material can be tied to the shape; the reason says why. The slot is never
    /// cleared from some other image of the GLB.
    Unmatched { reason: String },
}

struct SlotCheck<'a> {
    pack_textures: &'a PackTextures,
    sources: &'a SourceCatalog,
}

impl SlotCheck<'_> {
    /// Classifies one wanted texture. Returns `None` when it reaches the renderer: the GLB binds
    /// it and the exact file the engine requests exists in the pack.
    #[allow(clippy::too_many_arguments)]
    fn check(
        &self,
        origin: Origin,
        mesh: &str,
        shape: &str,
        slot: &str,
        wanted_path: &str,
        came_from: &str,
        view: SlotView<'_>,
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
        let (Ok(source_key), Ok(wanted_base)) = (
            canonical_asset_path(wanted_path, AssetKind::Texture, "dds"),
            canonical_asset_path(wanted_path, AssetKind::Texture, "ktx2"),
        ) else {
            finding.cause = Cause::UnparseablePath;
            return Some(finding);
        };
        let source = self.sources.texture(&source_key);
        finding.source_has_dds = source.is_some();
        finding.source_archive = source.map(SourceLocation::label);
        match view {
            SlotView::Record => {
                // The engine reads a record path with its own rule, which refuses a `.` segment
                // that the source-path canonicalizer would quietly remove.
                let Some(engine_key) =
                    shared::texture_aliases::converted_record_texture_path(wanted_path)
                else {
                    finding.cause = Cause::UnparseablePath;
                    return Some(finding);
                };
                finding.pack_has_ktx2 = self.pack_textures.has_file(&engine_key);
                finding.wanted_key = Some(engine_key);
                if finding.pack_has_ktx2 {
                    return None;
                }
                finding.cause = unresolved_cause;
            }
            SlotView::Bound { image, repairs } => {
                let candidates: Vec<&ImageRef> = image
                    .into_iter()
                    .filter(|image| accepts(&wanted_base, repairs, &image.base))
                    .collect();
                if self.classify(&mut finding, &wanted_base, &candidates) {
                    return None;
                }
            }
            SlotView::Unmatched { reason } => {
                finding.pack_has_ktx2 = self.pack_textures.has_base(&wanted_base);
                finding.cause = if finding.pack_has_ktx2 {
                    // The texture exists, but nothing shows that the shape binds it.
                    Cause::BindingAmbiguous
                } else {
                    // Nothing exists to bind: the same cause as for a matched shape.
                    absent_or_missing(finding.source_has_dds)
                };
                finding.came_from = format!("{came_from}; {reason}");
            }
        }
        if finding.wanted_key.is_none() {
            finding.wanted_key = Some(wanted_base);
        }
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

    /// Sets the cause of a GLB slot from the images that could carry it. Returns true when the
    /// slot reaches the renderer, and then leaves the cause alone.
    fn classify(&self, finding: &mut Finding, wanted_base: &str, candidates: &[&ImageRef]) -> bool {
        let referenced = !candidates.is_empty();
        let exact_present = candidates
            .iter()
            .any(|image| self.pack_textures.has_file(&image.exact));
        finding.glb_references_it = referenced;
        finding.pack_has_ktx2 = if referenced {
            exact_present
        } else {
            self.pack_textures.has_base(wanted_base)
        };
        if referenced && exact_present {
            return true;
        }
        finding.cause = if referenced {
            // The GLB names an image the pack does not hold: the engine requests that exact file.
            absent_or_missing(finding.source_has_dds)
        } else if finding.pack_has_ktx2 {
            // The pack holds the converted texture, but this slot does not bind it.
            Cause::SlotUnreferenced
        } else {
            absent_or_missing(finding.source_has_dds)
        };
        false
    }

    /// Checks one image a GLB references when there is no source NIF to say what was wanted.
    fn check_reference(&self, image: &ImageRef, mesh: &str) -> Option<Finding> {
        if self.pack_textures.has_file(&image.exact) {
            return None;
        }
        let source_key = image
            .base
            .strip_suffix(".ktx2")
            .map(|stem| format!("{stem}.dds"))?;
        let source = self.sources.texture(&source_key);
        Some(Finding {
            cause: absent_or_missing(source.is_some()),
            origin: Origin::GlbPrimitive,
            mesh: mesh.to_owned(),
            shape: String::new(),
            slot: "image".to_owned(),
            wanted_path: source_key,
            wanted_key: Some(image.base.clone()),
            came_from: format!("GLB image URI {} (no source NIF)", image.uri),
            source_has_dds: source.is_some(),
            source_archive: source.map(SourceLocation::label),
            pack_has_ktx2: false,
            glb_references_it: true,
            same_name_elsewhere: None,
            used_by: UsedBy::default(),
        })
    }
}

fn absent_or_missing(source_has_dds: bool) -> Cause {
    if source_has_dds {
        Cause::ConvertedTextureMissing
    } else {
        Cause::SourceTextureAbsent
    }
}

/// Whether a bound image's base key satisfies the wanted one: the same texture, or the texture the
/// converter published as its repair.
fn accepts(wanted_base: &str, repairs: &HashMap<String, String>, bound_base: &str) -> bool {
    bound_base == wanted_base
        || repairs
            .get(wanted_base)
            .is_some_and(|repair| repair == bound_base)
}

/// A finding for an image URI the resolver refused, so a broken image cannot vanish.
fn refused_uri_finding(mesh: &str, uri: &str, reason: &str) -> Finding {
    Finding {
        cause: Cause::UnparseablePath,
        origin: Origin::GlbPrimitive,
        mesh: mesh.to_owned(),
        shape: String::new(),
        slot: "image".to_owned(),
        wanted_path: uri.to_owned(),
        wanted_key: None,
        came_from: format!("GLB image URI refused: {reason}"),
        source_has_dds: false,
        source_archive: None,
        pack_has_ktx2: false,
        glb_references_it: true,
        same_name_elsewhere: None,
        used_by: UsedBy::default(),
    }
}

/// Every GLB under `meshes/`. A folder that cannot be read is an error, not a smaller audit.
fn glb_files(pack: &Path, filter: Option<&str>) -> Result<Vec<(String, PathBuf)>> {
    let root = pack.join("meshes");
    ensure!(
        root.is_dir(),
        "{} has no meshes folder; is this a converted pack?",
        pack.display()
    );
    let mut files = Vec::new();
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry.wrap_err_with(|| format!("failed to scan {}", root.display()))?;
        if !entry.file_type().is_file()
            || !entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("glb"))
        {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(pack)?
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        if filter.is_none_or(|text| relative.contains(&text.to_ascii_lowercase())) {
            files.push((relative, entry.into_path()));
        }
    }
    files.sort();
    Ok(files)
}

/// Audits a pack. Reads the pack, the sources and the database; writes only to `scratch`.
///
/// A mesh or archive that cannot be read is recorded in the summary rather than dropped, and
/// [`compare_to_baseline`] refuses to pass such an incomplete audit.
pub fn audit_pack(options: &AuditOptions<'_>) -> Result<Report> {
    // One normalized root serves discovery, URI resolution and `strip_prefix`, so `./pack` and
    // `pack` give the same report.
    let pack = fs::canonicalize(options.pack)
        .wrap_err_with(|| format!("failed to open pack {}", options.pack.display()))?;
    let pack = pack.as_path();
    let pack_textures = PackTextures::scan(pack)?;
    let check = SlotCheck {
        pack_textures: &pack_textures,
        sources: options.sources,
    };
    let glbs = glb_files(pack, options.mesh_filter)?;
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
        /// A GLB with no source NIF, audited from its own contents only.
        generated: bool,
        failed: Option<String>,
    }

    let results: Vec<MeshResult> = glbs
        .par_iter()
        .map(|(relative, glb_path)| {
            let mut result = MeshResult {
                findings: Vec::new(),
                wanted: 0,
                has_source: false,
                generated: false,
                failed: None,
            };
            let nif_key = format!("{}.nif", relative.trim_end_matches(".glb"));
            let facts = match glb_facts(pack, glb_path) {
                Ok(facts) => facts,
                Err(error) => {
                    result.failed = Some(format!("{relative}: {error:#}"));
                    return result;
                }
            };
            // Findings every GLB can have, with or without a source NIF.
            let common_findings = |result: &mut MeshResult| {
                for (uri, reason) in &facts.refused_uris {
                    result
                        .findings
                        .push(refused_uri_finding(relative, uri, reason));
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
            };
            if options.sources.mesh(&nif_key).is_none() {
                // A composed or generated model has no source NIF to compare with, so only
                // what the GLB itself says can be checked: every image it references must
                // exist in the pack, and no textured primitive may lack UVs.
                result.generated = true;
                for image in &facts.external {
                    if let Some(finding) = check.check_reference(image, relative) {
                        result.findings.push(finding);
                    }
                }
                common_findings(&mut result);
                return result;
            }
            result.has_source = true;
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
                            shape_block: shape.shape_block,
                            slot_index: slot.slot,
                            semantic: slot.semantic,
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
                // Every slot follows the material of the primitive that draws the shape. When no
                // single used material carries the shape block, the slot stays unproven.
                let view = match shape_material(&facts, slot.shape_block) {
                    ShapeMaterial::Unique(material) => SlotView::Bound {
                        image: material
                            .image_for(slot.semantic, slot.slot_index)
                            .and_then(|index| facts.images.get(index))
                            .and_then(|image| image.as_ref()),
                        repairs: &facts.repairs,
                    },
                    ShapeMaterial::Unmatched(reason) => SlotView::Unmatched { reason },
                };
                if let Some(finding) = check.check(
                    Origin::NifShape,
                    relative,
                    &slot.shape,
                    &slot.slot,
                    &slot.path,
                    &slot.came_from,
                    view,
                    Cause::SourceTextureAbsent,
                ) {
                    result.findings.push(finding);
                }
            }
            common_findings(&mut result);
            result
        })
        .collect();

    let mut summary = Summary {
        glb_files: glbs.len() as u64,
        skipped_archives: options.sources.skipped_archives().to_vec(),
        inactive_archives: options.sources.inactive_archives().to_vec(),
        ..Summary::default()
    };
    let mut findings = Vec::new();
    for result in results {
        if result.has_source {
            summary.glb_files_with_source_nif += 1;
        } else {
            summary.glb_files_without_source_nif += 1;
        }
        if result.generated {
            summary.glb_files_audited_without_source += 1;
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
        summary.records_audited = true;
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
    summary.resolved_slots = (summary.wanted_slots + summary.texture_set_paths).saturating_sub(
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
        // A missing column is an error, not an empty result: a renamed column would otherwise
        // pass the baseline with no texture set read.
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
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
                SlotView::Record,
                Cause::TextureSetUnresolved,
            ) {
                findings.push(finding);
            }
        }
    }
    {
        let mut statement = connection.prepare(
            "SELECT id,flow_normal_path FROM waters WHERE flow_normal_path IS NOT NULL AND flow_normal_path <> '' ORDER BY id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
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
                SlotView::Record,
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

fn table_exists(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get::<_, u32>(0),
        )
        .unwrap_or(0)
        > 0
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
    {
        let mut statement = connection.prepare(
            "SELECT l.id, l.editor_id FROM landscape_textures l
             LEFT JOIN texture_sets t ON t.id = l.texture_set_id
             WHERE t.diffuse_path IS NULL OR t.diffuse_path = '' ORDER BY l.id",
        )?;
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
    // Only packs read with the in-house record reader have this table.
    if table_exists(connection, "inhouse_unresolved_terrain_textures") {
        let mut statement = connection.prepare(
            "SELECT texture_form_id, count(*) FROM inhouse_unresolved_terrain_textures
             GROUP BY texture_form_id ORDER BY texture_form_id",
        )?;
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
    /// The placements found for one mesh.
    #[derive(Default)]
    struct MeshUse {
        statics: HashSet<u32>,
        references: u64,
        cells: HashMap<u32, u64>,
    }
    let mut by_mesh: HashMap<String, MeshUse> = HashMap::new();
    for row in rows {
        let (model, static_id, cell_id, count) = row?;
        let Ok(key) = canonical_asset_path(&model, AssetKind::Mesh, "glb") else {
            continue;
        };
        if !wanted.contains(key.as_str()) {
            continue;
        }
        let entry = by_mesh.entry(key).or_default();
        entry.statics.insert(static_id);
        if let Some(cell_id) = cell_id {
            entry.references += count;
            *entry.cells.entry(cell_id).or_default() += count;
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
        let Some(MeshUse {
            statics,
            references,
            cells,
        }) = by_mesh.get(&finding.mesh)
        else {
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
        Some(u32::from_le_bytes(
            body.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
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

/// The alternate-texture subrecords of one record payload. The payload keeps its source framing,
/// so the converter's checked decoder reads it: it follows `XXXX` extended lengths and refuses a
/// truncated subrecord instead of losing alignment.
fn alternate_texture_subrecords(payload: &[u8]) -> Result<Vec<Vec<AlternateTexture>>> {
    let mut found = Vec::new();
    for (tag, body) in crate::esm::extractors::extract_subrecords(payload)? {
        if !ALTERNATE_TEXTURE_SUBRECORDS
            .iter()
            .any(|name| tag.as_slice() == *name)
        {
            continue;
        }
        found.push(parse_alternate_textures(&body).ok_or_else(|| {
            color_eyre::eyre::eyre!("malformed {} subrecord", String::from_utf8_lossy(&tag))
        })?);
    }
    Ok(found)
}

/// What the alternate-texture entries of a pack come to, by the retail rules (RF-TEX-001..005).
///
/// A pack that publishes `base_texture_swaps` carries the entries the engine applies; an older
/// pack is read from the source records instead and nothing applies them
/// (`engine_applies_entries` is false). The counts separate retail behaviour (a hidden shape, an
/// empty set) from misses (a target no plugin defines).
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
    /// Source records that held alternate-texture bytes the decoder could not read. The audit is
    /// incomplete while this is not zero.
    #[serde(default)]
    pub undecodable_records: u64,
    /// True when the pack publishes `base_texture_swaps`, which the engine applies.
    pub engine_applies_entries: bool,
    /// Entries whose texture set fills a slot other than diffuse (TX00) or normal (TX01). The
    /// engine applies only those two, so such a shape keeps its model material for the rest.
    #[serde(default)]
    pub entries_with_unapplied_slots: u64,
}

fn audit_overrides(connection: &Connection) -> Result<(OverrideAudit, Vec<Finding>)> {
    let published = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='base_texture_swaps'",
            [],
            |row| row.get::<_, u32>(0),
        )
        .unwrap_or(0)
        > 0;
    if published {
        audit_published_swaps(connection)
    } else {
        audit_source_overrides(connection)
    }
}

/// Audits the `base_texture_swaps` table the converter publishes for the engine.
fn audit_published_swaps(connection: &Connection) -> Result<(OverrideAudit, Vec<Finding>)> {
    let mut audit = OverrideAudit {
        engine_applies_entries: true,
        ..OverrideAudit::default()
    };
    let mut findings = Vec::new();
    // (has any path, has a path in a slot other than diffuse and normal)
    let mut sets: HashMap<u32, (bool, bool)> = HashMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT id,
                    coalesce(diffuse_path,'') || coalesce(normal_path,''),
                    coalesce(glow_path,'') || coalesce(height_path,'') || coalesce(environment_path,'')
                    || coalesce(mask_path,'') || coalesce(specular_path,'') || coalesce(detail_path,'')
             FROM texture_sets",
        )?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (id, applied, other) = row?;
            sets.insert(
                id,
                (!applied.is_empty() || !other.is_empty(), !other.is_empty()),
            );
        }
    }
    let mut statement = connection.prepare(
        "SELECT b.base_id, b.position, b.shape_name, b.shape_index, b.texture_set_id,
                b.disposition, coalesce(f.record_type, '')
         FROM base_texture_swaps b LEFT JOIN formid_map f ON f.form_id = b.base_id
         ORDER BY b.base_id, b.position",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut records = HashSet::new();
    for (base, shape, index, texture_set, disposition, record_type) in rows {
        if records.insert(base) {
            audit.records_with_alternate_textures += 1;
            *audit.by_record_type.entry(record_type.clone()).or_default() += 1;
        }
        audit.entries += 1;
        if index < 0 {
            audit.skipped_index_minus_one += 1;
            continue;
        }
        match disposition.as_str() {
            "null_texture_set" => audit.hide_shape_entries += 1,
            "texture_set" => match sets.get(&texture_set) {
                Some((true, unapplied)) => {
                    audit.entries_with_texture_paths += 1;
                    audit.entries_with_unapplied_slots += u64::from(*unapplied);
                }
                Some((false, _)) => audit.empty_texture_set_entries += 1,
                None => {
                    audit.unresolved_target_entries += 1;
                    findings.push(unresolved_override(
                        &record_type,
                        base,
                        &shape,
                        index,
                        texture_set,
                    ));
                }
            },
            _ => {
                audit.unresolved_target_entries += 1;
                findings.push(unresolved_override(
                    &record_type,
                    base,
                    &shape,
                    index,
                    texture_set,
                ));
            }
        }
    }
    Ok((audit, findings))
}

fn unresolved_override(
    record_type: &str,
    base: u32,
    shape: &str,
    index: i32,
    texture_set: u32,
) -> Finding {
    Finding {
        cause: Cause::OverrideTargetUnresolved,
        origin: Origin::TextureSet,
        mesh: format!("{record_type} {base:08X}"),
        shape: shape.to_owned(),
        slot: format!("3D index {index}"),
        wanted_path: format!("TXST {texture_set:08X}"),
        wanted_key: None,
        came_from: format!("{record_type} {base:08X} alternate textures"),
        source_has_dds: false,
        source_archive: None,
        pack_has_ktx2: false,
        glb_references_it: false,
        same_name_elsewhere: None,
        used_by: UsedBy::default(),
    }
}

/// Reads the alternate-texture subrecords of the source records, for a pack that does not
/// publish `base_texture_swaps`.
fn audit_source_overrides(connection: &Connection) -> Result<(OverrideAudit, Vec<Finding>)> {
    let mut audit = OverrideAudit::default();
    let mut findings = Vec::new();
    // Only packs read with the in-house record reader keep the source records.
    if !table_exists(connection, "inhouse_source_records") {
        return Ok((audit, findings));
    }
    let mut records = connection.prepare(
        "SELECT form_id, record_type, load_order, payload FROM inhouse_source_records
         WHERE instr(payload, X'4D4F4453') > 0 OR instr(payload, X'4D4F3253') > 0
            OR instr(payload, X'4D4F3353') > 0 OR instr(payload, X'4D4F3453') > 0",
    )?;
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
    {
        let mut statement = connection.prepare(
            "SELECT id, coalesce(diffuse_path,'') || coalesce(normal_path,'') || coalesce(glow_path,'')
                    || coalesce(height_path,'') || coalesce(environment_path,'')
                    || coalesce(mask_path,'') || coalesce(specular_path,'') || coalesce(detail_path,'')
             FROM texture_sets",
        )?;
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
        let lists = match alternate_texture_subrecords(&payload) {
            Ok(lists) => lists,
            Err(_) => {
                audit.undecodable_records += 1;
                continue;
            }
        };
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
            let target_plugin = masters
                .get(master)
                .unwrap_or(&own_name)
                .to_ascii_lowercase();
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
        out.push_str(
            &row.iter()
                .map(|value| field(value))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with(counts: &[(Cause, u64)]) -> Report {
        let mut report = Report::default();
        // A complete audit looked at one mesh; an empty report would count as incomplete.
        report.summary.glb_files = 1;
        for (cause, count) in counts {
            report.summary.counts_by_cause.insert(*cause, *count);
        }
        report
    }

    #[test]
    fn ratchet_fails_only_when_a_cause_grows() {
        let baseline = Baseline::from_report(
            &report_with(&[
                (Cause::SourceTextureAbsent, 10),
                (Cause::SlotUnreferenced, 2),
            ]),
            "test",
        );
        assert!(
            compare_to_baseline(
                &report_with(&[
                    (Cause::SourceTextureAbsent, 9),
                    (Cause::SlotUnreferenced, 2)
                ]),
                &baseline
            )
            .is_empty()
        );
        let regressions = compare_to_baseline(
            &report_with(&[
                (Cause::SourceTextureAbsent, 10),
                (Cause::PrimitiveWithoutUvs, 1),
            ]),
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
    fn an_empty_or_failed_audit_is_incomplete_and_fails_every_baseline() {
        let baseline = Baseline::from_report(&report_with(&[]), "test");
        assert!(compare_to_baseline(&report_with(&[]), &baseline).is_empty());
        let mut report = report_with(&[]);
        report.summary.glb_files = 0;
        report.summary.nif_files_failed = 2;
        report.summary.failed_nifs = vec!["meshes/a.glb: truncated".into()];
        report.summary.skipped_archives = vec!["Skyrim - Textures9.bsa: bad magic".into()];
        report.summary.records_audited = true;
        report.overrides.undecodable_records = 1;
        let problems = compare_to_baseline(&report, &baseline);
        for expected in [
            "2 meshes could not be read, for example meshes/a.glb: truncated",
            "archive could not be listed: Skyrim - Textures9.bsa: bad magic",
            "1 records with alternate textures could not be decoded",
            "no GLB was audited",
            "no texture set or terrain layer was read",
        ] {
            assert!(
                problems.iter().any(|line| line.contains(expected)),
                "{expected}: {problems:?}"
            );
        }
    }

    #[test]
    fn alternate_texture_entries_parse_and_truncated_bodies_do_not() {
        let body = mods_body(&[
            ("TundraStreamBend01:0", 0x0002_4E2F, 4),
            ("DirtDecal", 0x28, -1),
        ]);
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
        let lists = alternate_texture_subrecords(&payload).unwrap();
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].len(), 2);
        // An `XXXX` extended length carries a body longer than the 16-bit size can hold; the
        // decoder follows it instead of reading an empty body and losing alignment.
        let mut extended = b"XXXX\x04\x00".to_vec();
        extended.extend((body.len() as u32).to_le_bytes());
        extended.extend(b"MODS\x00\x00");
        extended.extend(&body);
        let lists = alternate_texture_subrecords(&extended).unwrap();
        assert_eq!(lists[0].len(), 2);
        // A truncated subrecord is an error, never an empty list.
        assert!(alternate_texture_subrecords(&payload[..payload.len() - 3]).is_err());
    }

    #[test]
    fn published_swaps_separate_retail_behaviour_from_misses() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY, diffuse_path TEXT, normal_path TEXT,
                    glow_path TEXT, height_path TEXT, environment_path TEXT, mask_path TEXT,
                    specular_path TEXT, detail_path TEXT);
                 CREATE TABLE formid_map(form_id INTEGER PRIMARY KEY, plugin_name TEXT,
                    internal_id INTEGER, record_type TEXT);
                 CREATE TABLE base_texture_swaps(base_id INTEGER, position INTEGER, shape_name TEXT,
                    shape_index INTEGER, texture_set_id INTEGER, disposition TEXT,
                    PRIMARY KEY(base_id, position));
                 INSERT INTO formid_map VALUES (1,'skyrim.esm',1,'STAT');
                 INSERT INTO texture_sets(id, diffuse_path) VALUES (10, 'textures/a.dds');
                 INSERT INTO texture_sets(id, diffuse_path, glow_path)
                    VALUES (11, 'textures/b.dds', 'textures/b_g.dds');
                 INSERT INTO texture_sets(id) VALUES (12);
                 INSERT INTO base_texture_swaps VALUES
                    (1,0,'a',0,10,'texture_set'),
                    (1,1,'b',1,11,'texture_set'),
                    (1,2,'c',2,12,'texture_set'),
                    (1,3,'d',3,0,'null_texture_set'),
                    (1,4,'e',4,99,'missing_texture_set'),
                    (1,5,'f',-1,10,'texture_set');",
            )
            .unwrap();
        let (audit, findings) = audit_overrides(&connection).unwrap();
        assert!(audit.engine_applies_entries);
        assert_eq!(audit.records_with_alternate_textures, 1);
        assert_eq!(audit.entries, 6);
        assert_eq!(audit.skipped_index_minus_one, 1);
        assert_eq!(audit.hide_shape_entries, 1);
        assert_eq!(audit.empty_texture_set_entries, 1);
        assert_eq!(audit.entries_with_texture_paths, 2);
        assert_eq!(audit.entries_with_unapplied_slots, 1);
        assert_eq!(audit.unresolved_target_entries, 1);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].cause, Cause::OverrideTargetUnresolved);
    }

    /// Writes a JSON-only GLB into a temporary pack and returns the pack root and the GLB path.
    fn write_json_glb(document: &serde_json::Value) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let pack = fs::canonicalize(root.path()).unwrap();
        let path = pack.join("meshes/m.glb");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut json = serde_json::to_vec(document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bytes = b"glTF".to_vec();
        bytes.extend(2u32.to_le_bytes());
        bytes.extend((20 + json.len() as u32).to_le_bytes());
        bytes.extend((json.len() as u32).to_le_bytes());
        bytes.extend(b"JSON");
        bytes.extend(json);
        fs::write(&path, bytes).unwrap();
        (root, pack, path)
    }

    #[test]
    fn every_slot_the_converter_publishes_is_read_from_the_used_material() {
        // Six images: 0 unused-material diffuse, 1 diffuse, 2 normal, 3 glow, 4 `_s`, 5 height.
        let uris: Vec<_> = (0..6)
            .map(|index| serde_json::json!({"uri": format!("../textures/t{index}.ktx2")}))
            .collect();
        let textures: Vec<_> = (0..6)
            .map(|index| serde_json::json!({"source": index}))
            .collect();
        let shape = |block: u32| serde_json::json!({"openSkyrim": {"shapeBlock": block}});
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "images": uris,
            "textures": textures,
            "materials": [
                // Nothing draws with this one, though it carries shape block 5 and binds image 0.
                {"extras": shape(5), "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}},
                {
                    "extras": shape(5),
                    "pbrMetallicRoughness": {"baseColorTexture": {"index": 1}},
                    "normalTexture": {"index": 2},
                    "emissiveTexture": {"index": 3},
                    "extensions": {
                        "KHR_materials_specular": {"specularColorTexture": {"index": 4}},
                        "OPEN_SKYRIM_material": {"textureSlots": [{"slot": 3, "texture": 5}]}
                    }
                },
                {"extras": shape(9)},
                {"extras": shape(9)}
            ],
            "meshes": [{"name": "m", "primitives": [
                {"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "material": 1},
                {"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "material": 2},
                {"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "material": 3}
            ]}]
        });
        let (_root, pack, path) = write_json_glb(&document);
        let facts = glb_facts(&pack, &path).unwrap();
        let ShapeMaterial::Unique(material) = shape_material(&facts, 5) else {
            panic!("shape block 5 has one used material");
        };
        // The unused material (image 0) is ignored; every slot kind comes from the used one.
        assert_eq!(material.image_for(NifTextureSemantic::Diffuse, 0), Some(1));
        assert_eq!(material.image_for(NifTextureSemantic::Normal, 1), Some(2));
        assert_eq!(material.image_for(NifTextureSemantic::Glow, 2), Some(3));
        assert_eq!(material.image_for(NifTextureSemantic::Specular, 7), Some(4));
        // A slot the converter keeps in `textureSlots` is found by its NIF slot index.
        assert_eq!(material.image_for(NifTextureSemantic::Height, 3), Some(5));
        // A slot the material does not bind is not cleared by another image of the GLB.
        assert_eq!(material.image_for(NifTextureSemantic::Height, 4), None);
        assert_eq!(
            material.image_for(NifTextureSemantic::EnvironmentCube, 4),
            None
        );
        // Two used materials carry shape block 9, and none carries 7.
        assert!(matches!(
            shape_material(&facts, 9),
            ShapeMaterial::Unmatched(reason) if reason.starts_with("2 materials")
        ));
        assert!(matches!(
            shape_material(&facts, 7),
            ShapeMaterial::Unmatched(reason) if reason.contains("shape block 7")
        ));
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
