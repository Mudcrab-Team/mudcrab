//! Land texture arrays for merged terrain (#215).
//!
//! [`build_land_texture_arrays`] turns the converted land textures of a mod list into one KTX2
//! texture array per class (usage, block format, size), plus `terrain/land-textures.json`
//! ([`LandTextureTable`]), which maps each LTEX FormID to its array and layer.
//!
//! # No quality is lowered
//!
//! Every array of a class stores the full mip chain of the class size
//! (`max_mip_levels(size, size, 1)` levels). A source that is square at the class size has its
//! authored levels block-copied exactly as they are, in whatever depth it has them (the vanilla
//! land textures stop at 2 x 2); only the levels it lacks are generated, from the smallest
//! authored level, and the report says how many ([`TextureAction::CopiedWithGeneratedTail`]). A
//! UASTC source (the converter's fallback for L8, 16-bit and palette textures) is transcoded to
//! BC7 level by level with the converter's basis-universal transcoder; any other block format
//! outside the usage's list ([`shared::terrain_mesh::DIFFUSE_FORMATS`] and `NORMAL_FORMATS`) is
//! decoded and re-encoded to BC7 per authored level. A source of a different size (or one
//! downscaled on the user's option) is decoded, resized with Lanczos3 and rebuilt. Every
//! texture that was not copied block for block is named by [`ArrayReport`].
//!
//! A source the builder cannot read (a missing or damaged file, a 3D or cubemap texture, an
//! unsupported supercompression, a format with no decoder) never stops the conversion: it is
//! skipped by name with its reason in [`ArrayReport::skipped`], gets no table entry, and its
//! cells fall back to the default texture. Only the size limits stay errors (a texture above the
//! largest class without `allow_downscale`, a class above the layer limit), and the default
//! texture of entry 0, which the table cannot do without: a given default that cannot be read at
//! planning time is replaced by the generated white one (and named as skipped), one that fails
//! while building stops the build.
//!
//! # The files
//!
//! Level data is stored in each file from the smallest mip to the largest while the level index
//! keeps its largest-first entries, as KTX2 specification sections 3.9.7 and 3.13 require
//! (<https://registry.khronos.org/KTX/specs/2.0/ktxspec.v2.html>). Every level is one Zstandard
//! frame holding all layers of that level.
//!
//! # Memory and disk
//!
//! Members are streamed: each member's levels are written into that level's own Zstandard
//! encoder, which spools into a file in the staging folder, and the array file is assembled from
//! the spool files at the end. Memory is therefore bounded by one source texture's working set
//! (its file, its decompressed levels and, for a rebuild, a few RGBA copies: about 0.3 GiB for a
//! 4096 x 4096 source) plus one encoder state per level (13 for a 4096 class, a few MiB each),
//! about 0.5 GiB at most, however many layers the class has. A 2048-layer 4096 BC3 class is about
//! 43 GiB uncompressed and is never held; the transient disk use is its compressed size (spool
//! files, then the assembled file) inside the staging folder.
//!
//! # Publication
//!
//! Nothing is written under `output_root/terrain` until every array and the table are complete:
//! the build works in `output_root/terrain.staging/`, then renames the array files over
//! their targets one by one and the table last, so a table is never missing (a rename over an
//! existing file is atomic on one volume). Between the first and last rename the old table can
//! meet a new array, so the conversion's own staging folder is the outer transaction: it is
//! published as a whole, and nothing reads this output while it is being published. The staging
//! folder is removed when the build ends, however it ends.
//!
//! Wiring this into the conversion pipeline is a later task: this module takes paths to
//! already-converted `.ktx2` files and an output root. The caller supplies the sources in any
//! order; the table's entries are the default texture first, then the sources sorted by LTEX
//! FormID.

use crate::texture::max_mip_levels;
use basis_universal::{
    DecodeFlags, LowLevelUastcTranscoder, SliceParametersUastc, TranscoderBlockFormat,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
use image_dds::image::{
    Rgba, RgbaImage,
    imageops::{FilterType, resize},
};
use serde::{Deserialize, Serialize};
use shared::terrain_mesh::{
    ArrayLayer, DEFAULT_LAND_TEXTURE_FORM_ID, DEFAULT_MAX_LAND_TEXTURE_ARRAY_LAYERS,
    DIFFUSE_FORMATS, LAND_TEXTURE_SIZE_CLASSES, LAND_TEXTURE_TABLE_PATH, LandTextureArray,
    LandTextureEntry, LandTextureFormat, LandTextureTable, LandTextureUsage, NORMAL_FORMATS,
    TERRAIN_MESH_FORMAT_VERSION,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

/// The directory the array files and the table live in, relative to the output root.
const TERRAIN_DIR: &str = "terrain";
/// Zstandard level of the array files, the converter's usual one.
const ZSTD_LEVEL: i32 = 6;
/// Side of the generated white default texture, in texels.
const WHITE_SIDE: u32 = 4;
/// The largest mip depth a KTX2 level index can address.
const MAX_MIP_DEPTH: u32 = u32::BITS;

/// One land texture of the mod list: an LTEX record's diffuse file and, if the texture set has
/// one, its normal map. Both are converted `.ktx2` files under the output root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandTextureSource {
    /// The LTEX FormID in load-order numbering; 0 names the default texture and is not a source.
    pub ltex_form_id: u32,
    /// The converted diffuse texture.
    pub diffuse: PathBuf,
    /// The converted normal map, when the texture set has one.
    pub normal: Option<PathBuf>,
}

/// What [`build_land_texture_arrays`] may be told to do differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayOptions {
    /// The size classes, in texels per side, ascending. Production uses
    /// [`LAND_TEXTURE_SIZE_CLASSES`]; [`LandTextureTable::validate`] accepts only that set, so
    /// the built table is validated only for it, and a caller that changes the set (a test with
    /// tiny fixtures) must not expect the engine to accept the table.
    pub size_classes: [u32; 3],
    /// The layer limit written into the table: the converter never writes more layers into one
    /// array, and the engine refuses the table when its device allows fewer.
    pub max_layers_per_array: u32,
    /// Whether a texture larger than the largest class is downscaled to it (the report labels
    /// every one). Without this, such a texture stops the build.
    pub allow_downscale: bool,
}

impl Default for ArrayOptions {
    fn default() -> Self {
        Self {
            size_classes: LAND_TEXTURE_SIZE_CLASSES,
            max_layers_per_array: DEFAULT_MAX_LAND_TEXTURE_ARRAY_LAYERS,
            allow_downscale: false,
        }
    }
}

/// Where entry 0's texture came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum DefaultTexture {
    /// The caller's default source; its own LTEX is named, entry 0's slot is
    /// [`DEFAULT_LAND_TEXTURE_FORM_ID`].
    Provided { ltex_form_id: u32 },
    /// No usable default source (none given, or the given one was skipped, see
    /// [`ArrayReport::skipped`]): entry 0 is a generated 4 x 4 white diffuse with no normal map,
    /// upscaled into the smallest diffuse class the content uses.
    White,
}

/// What happened to one texture on its way into an array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum TextureAction {
    /// The source was square at the class size, in a format its usage keeps, and stored the
    /// class's whole mip chain: every level's blocks were copied unchanged.
    Copied,
    /// Every authored level was copied unchanged, but the source stores only `authored_levels`
    /// of the class's chain, so `generated_levels` more were generated (decoded from the
    /// smallest authored level, downscaled and encoded in the class's format).
    CopiedWithGeneratedTail {
        authored_levels: u32,
        generated_levels: u32,
    },
    /// A UASTC source: each authored level was transcoded to BC7, and `generated_levels` more
    /// were generated when the source's chain was shorter than the class's.
    Transcoded { generated_levels: u32 },
    /// The source was decoded and rebuilt at the class size; `reencoded_from` names its format
    /// when that format is outside the usage's list and its texels went through the class's
    /// encoder. `generated_levels` counts the levels that are not an authored level of the
    /// source (all of them when the source had to be resized).
    Rebuilt {
        reencoded_from: Option<String>,
        generated_levels: u32,
    },
    /// The source is larger than the largest size class, and `allow_downscale` was set;
    /// `from_size` is its larger side.
    Downscaled {
        from_size: u32,
        reencoded_from: Option<String>,
    },
}

/// One texture in [`ArrayReport::textures`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextureReport {
    /// The table id, entry 0 being the default texture.
    pub id: u16,
    /// The LTEX FormID the entry carries ([`DEFAULT_LAND_TEXTURE_FORM_ID`] for the default).
    pub ltex_form_id: u32,
    pub usage: LandTextureUsage,
    /// The source file, or `None` for the generated white default texture.
    pub source: Option<String>,
    /// The source KTX2's vkFormat as the KTX2 crate spells it (e.g. `BC1_RGBA_SRGB_BLOCK`),
    /// `UASTC` for a universal texture, or `generated` for the white default texture.
    pub source_format: String,
    pub source_width: u32,
    pub source_height: u32,
    /// The class the layer went into.
    pub format: LandTextureFormat,
    pub size: u32,
    /// The array file, relative to the output root (a [`LandTextureArray::path`]).
    pub array: String,
    pub layer: u16,
    pub action: TextureAction,
}

/// One array file in [`ArrayReport::arrays`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrayClassReport {
    pub usage: LandTextureUsage,
    pub format: LandTextureFormat,
    pub size: u32,
    /// The array file, relative to the output root.
    pub path: String,
    /// Layers in the file, including the copy that pads a single member to two.
    pub layers: u32,
    /// Mip levels in the file; every layer has this many, the full chain of the class size.
    pub levels: u32,
    /// The file's size in bytes.
    pub bytes: u64,
}

/// A texture the builder left out, and why. Its LTEX has no table entry (or no normal map), so
/// its cells fall back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedTexture {
    /// The LTEX FormID of the source the texture belongs to.
    pub ltex_form_id: u32,
    pub usage: LandTextureUsage,
    /// The source file.
    pub source: String,
    pub reason: String,
}

/// What [`build_land_texture_arrays`] did, for the conversion report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrayReport {
    /// Where entry 0's texture came from.
    pub default_texture: DefaultTexture,
    /// One entry per array file, in the order of [`LandTextureTable::arrays`].
    pub arrays: Vec<ArrayClassReport>,
    /// One entry per texture in the table: the default first, then the sources by LTEX FormID,
    /// diffuse before its normal map.
    pub textures: Vec<TextureReport>,
    /// Every texture left out, with its reason; the plan-time skips first, then those found
    /// while building.
    pub skipped: Vec<SkippedTexture>,
}

impl ArrayReport {
    /// The report lines the conversion should show: where the default came from, every texture
    /// that was skipped, and every texture that was not copied block for block.
    pub fn warnings(&self) -> Vec<String> {
        let mut lines = vec![match self.default_texture {
            DefaultTexture::Provided { ltex_form_id } => format!(
                "land texture arrays: entry 0 is the default land texture of LTEX {ltex_form_id:08X}"
            ),
            DefaultTexture::White => {
                let class = self.textures.first().map_or(String::new(), |texture| {
                    format!(
                        " in the {} {} {} array",
                        usage_name(texture.usage),
                        format_label(texture.format),
                        texture.size
                    )
                });
                format!(
                    "land texture arrays: entry 0 is a generated {WHITE_SIDE}x{WHITE_SIDE} white diffuse{class}"
                )
            }
        }];
        for skipped in &self.skipped {
            lines.push(format!(
                "land texture {:08X} {} skipped, its cells fall back: {}: {}",
                skipped.ltex_form_id,
                usage_name(skipped.usage),
                skipped.reason,
                skipped.source
            ));
        }
        for texture in &self.textures {
            if let Some(summary) = texture.action_summary() {
                let source = texture.source.as_deref().unwrap_or("generated");
                lines.push(format!(
                    "land texture {:08X} {}: {summary}: {source}",
                    texture.ltex_form_id,
                    usage_name(texture.usage),
                ));
            }
        }
        lines
    }
}

impl TextureReport {
    /// The words the report uses for this texture's action, or `None` for a plain copy.
    fn action_summary(&self) -> Option<String> {
        match &self.action {
            TextureAction::Copied => None,
            TextureAction::CopiedWithGeneratedTail {
                authored_levels,
                generated_levels,
            } => Some(format!(
                "copied {authored_levels} authored mip levels, generated {generated_levels} more"
            )),
            TextureAction::Transcoded { generated_levels } => {
                let mut summary = format!("re-encoded: UASTC -> {}", format_label(self.format));
                if *generated_levels > 0 {
                    summary.push_str(&format!(", generated {generated_levels} mip levels"));
                }
                Some(summary)
            }
            TextureAction::Rebuilt {
                reencoded_from,
                generated_levels,
            } => {
                let mut notes = Vec::new();
                if let Some(from) = reencoded_from {
                    notes.push(format!(
                        "re-encoded: {from} -> {}",
                        format_label(self.format)
                    ));
                }
                if self.source_width != self.size || self.source_height != self.size {
                    notes.push(format!(
                        "upscaled {}x{} -> {}x{}",
                        self.source_width, self.source_height, self.size, self.size
                    ));
                }
                if *generated_levels > 0 {
                    notes.push(format!("generated {generated_levels} mip levels"));
                }
                if notes.is_empty() {
                    notes.push("rebuilt".to_string());
                }
                Some(notes.join(", "))
            }
            TextureAction::Downscaled {
                from_size,
                reencoded_from,
            } => {
                let mut summary = format!(
                    "downscaled (user option): {}x{} -> {}x{}",
                    self.source_width, self.source_height, self.size, self.size
                );
                if let Some(from) = reencoded_from {
                    summary.push_str(&format!(
                        ", re-encoded: {from} -> {}",
                        format_label(self.format)
                    ));
                }
                debug_assert!(*from_size == self.source_width.max(self.source_height));
                Some(summary)
            }
        }
    }
}

/// Builds the land texture arrays and their lookup table from already-converted textures.
///
/// `sources` are the mod list's LTEX records, in any order; `default` is the texture entry 0
/// stands for (the quadrant base of a cell with no BTXT). Every array is written under
/// `output_root` (see the module documentation for how it is published) and every source is
/// accounted for in the returned report, either as a table entry or in
/// [`ArrayReport::skipped`].
///
/// Caller contract: `output_root` is the conversion's staging folder (or another folder nothing
/// reads while this runs). The arrays are replaced one by one before the table, so only an
/// outer staging step makes the whole set appear at once.
pub fn build_land_texture_arrays(
    sources: &[LandTextureSource],
    default: Option<LandTextureSource>,
    output_root: &Path,
    options: &ArrayOptions,
) -> Result<(LandTextureTable, ArrayReport)> {
    check_options(options)?;
    check_sources(sources)?;

    // One table entry per LTEX, the sources by FormID; the default is entry 0 and is planned
    // after them, because a generated white texture needs the classes the sources define.
    let mut skipped: Vec<SkippedTexture> = Vec::new();
    let mut sorted: Vec<&LandTextureSource> = sources.iter().collect();
    sorted.sort_by_key(|source| source.ltex_form_id);
    let mut planned: Vec<PlannedEntry> = Vec::new();
    for source in sorted {
        if let Some(entry) = plan_entry(source, source.ltex_form_id, options, &mut skipped)? {
            planned.push(entry);
        }
    }

    // Entry 0 is the default texture: the caller's, or a white one in the smallest diffuse
    // class the sources define (the smallest class, BC1, when they define none).
    let provided = match &default {
        Some(source) => plan_entry(source, DEFAULT_LAND_TEXTURE_FORM_ID, options, &mut skipped)?
            .map(|entry| (source.ltex_form_id, entry)),
        None => None,
    };
    let (default_texture, default_entry) = match provided {
        Some((ltex_form_id, entry)) => (DefaultTexture::Provided { ltex_form_id }, entry),
        None => (
            DefaultTexture::White,
            PlannedEntry {
                ltex_form_id: DEFAULT_LAND_TEXTURE_FORM_ID,
                diffuse: MemberPlan::white(white_class(&planned, options)),
                normal: None,
            },
        ),
    };

    // The default's members come first, so entry 0's layers are the first layer everywhere.
    let mut members: Vec<MemberPlan> = Vec::new();
    let mut entries: Vec<EntryPlan> = Vec::new();
    for entry in std::iter::once(default_entry).chain(planned) {
        let PlannedEntry {
            ltex_form_id,
            diffuse,
            normal,
        } = entry;
        let diffuse_index = members.len();
        members.push(diffuse);
        let normal_index = normal.map(|mut normal| {
            normal.depends_on = Some(diffuse_index);
            members.push(normal);
            members.len() - 1
        });
        entries.push(EntryPlan {
            ltex_form_id,
            diffuse: diffuse_index,
            normal: normal_index,
        });
    }

    // Group the members into classes; the grouping order is the layer order within each class.
    let mut classes: Vec<(ClassKey, Vec<usize>)> = Vec::new();
    for (index, member) in members.iter().enumerate() {
        match classes.iter_mut().find(|(key, _)| *key == member.class) {
            Some((_, indexes)) => indexes.push(index),
            None => classes.push((member.class, vec![index])),
        }
    }
    classes.sort_by_key(|(key, _)| key.order());

    // The size limits are checked before anything is written.
    for (key, indexes) in &classes {
        let layers = indexes.len().max(2);
        ensure!(
            u32::try_from(layers).unwrap_or(u32::MAX) <= options.max_layers_per_array,
            "land texture class {} needs {layers} layers, more than max_layers_per_array {}",
            array_path(*key),
            options.max_layers_per_array
        );
    }

    // Everything is built inside the staging folder; it is removed however this function ends.
    let staging = Staging::create(output_root)?;
    let mut arrays: Vec<LandTextureArray> = Vec::with_capacity(classes.len());
    let mut class_reports: Vec<ArrayClassReport> = Vec::with_capacity(classes.len());
    for (key, indexes) in &classes {
        let natural = max_mip_levels(key.size, key.size, 1);
        let mut writer = ArrayWriter::create(&staging.spool_dir(), *key, natural)?;
        // The first member's levels are kept only until a second one arrives: a class with one
        // member is padded with a copy of it, so the file still loads as a D2Array (the
        // engine's loader builds one only for layer_count > 1).
        let mut first: Option<Vec<Vec<u8>>> = None;
        let mut placed: Vec<(usize, u16, TextureAction)> = Vec::new();
        for &index in indexes {
            if let Some(owner) = members[index].depends_on
                && members[owner].failed.is_some()
            {
                members[index].failed = Some("its diffuse texture was skipped".to_string());
                continue;
            }
            match build_member(&members[index]) {
                Ok(BuiltMember { levels, action }) => {
                    writer.push_layer(&levels)?;
                    let layer = u16::try_from(placed.len())
                        .wrap_err("more than 65535 layers in one array")?;
                    first = if placed.is_empty() {
                        Some(levels)
                    } else {
                        None
                    };
                    placed.push((index, layer, action));
                }
                Err(error) => members[index].failed = Some(format!("{error:#}")),
            }
        }
        if placed.is_empty() {
            // Every member of the class was skipped: there is no such array.
            continue;
        }
        if placed.len() == 1
            && let Some(levels) = first.take()
        {
            writer.push_layer(&levels)?;
        }
        let layers = writer.layers();
        let path = array_path(*key);
        let bytes = writer.finish(&staging.path().join(&path))?;
        let array = arrays.len();
        for (index, layer, action) in placed {
            members[index].placed = Some(Placement {
                array,
                layer,
                action,
            });
        }
        arrays.push(LandTextureArray {
            path: path.clone(),
            usage: key.usage,
            format: key.format,
            size: key.size,
            layers,
        });
        class_reports.push(ArrayClassReport {
            usage: key.usage,
            format: key.format,
            size: key.size,
            path,
            layers,
            levels: natural,
            bytes,
        });
    }

    // The table and the report, from what was actually placed.
    let mut table_entries = Vec::with_capacity(entries.len());
    let mut textures = Vec::with_capacity(entries.len() * 2);
    for entry in &entries {
        let Some(diffuse) = &members[entry.diffuse].placed else {
            ensure!(
                entry.ltex_form_id != DEFAULT_LAND_TEXTURE_FORM_ID,
                "the default land texture {} cannot be built: {}",
                members[entry.diffuse]
                    .path
                    .as_ref()
                    .map_or("generated".to_string(), |path| path.display().to_string()),
                members[entry.diffuse]
                    .failed
                    .as_deref()
                    .unwrap_or("unknown reason")
            );
            // A source that failed while building: it is in the skipped list below.
            continue;
        };
        let id = u16::try_from(table_entries.len()).wrap_err("more than 65535 land textures")?;
        let diffuse_layer = array_layer(diffuse)?;
        let normal = entry
            .normal
            .and_then(|index| members[index].placed.as_ref().map(|placed| (index, placed)));
        let normal_layer = normal.map(|(_, placed)| array_layer(placed)).transpose()?;
        table_entries.push(LandTextureEntry {
            id,
            ltex_form_id: entry.ltex_form_id,
            diffuse: diffuse_layer,
            normal: normal_layer,
        });
        textures.push(report_of(
            &members[entry.diffuse],
            diffuse,
            id,
            entry.ltex_form_id,
            &arrays,
        )?);
        if let Some((index, placed)) = normal {
            textures.push(report_of(
                &members[index],
                placed,
                id,
                entry.ltex_form_id,
                &arrays,
            )?);
        }
    }
    for member in &members {
        if let Some(reason) = &member.failed {
            skipped.push(SkippedTexture {
                ltex_form_id: member.source_ltex_form_id,
                usage: member.usage,
                source: member
                    .path
                    .as_ref()
                    .map_or("generated".to_string(), |path| path.display().to_string()),
                reason: reason.clone(),
            });
        }
    }

    let table = LandTextureTable {
        format_version: TERRAIN_MESH_FORMAT_VERSION,
        max_layers_per_array: options.max_layers_per_array,
        arrays,
        entries: table_entries,
        settings: None,
    };
    // `validate` accepts only the production size classes, so a test class set skips it.
    if options.size_classes == LAND_TEXTURE_SIZE_CLASSES {
        table
            .validate()
            .map_err(|error| eyre!("the built land texture table is invalid: {error}"))?;
    }
    let table_file = staging.path().join(LAND_TEXTURE_TABLE_PATH);
    fs::write(&table_file, serde_json::to_vec_pretty(&table)?)
        .wrap_err_with(|| format!("failed to write {}", table_file.display()))?;

    let array_paths: Vec<&str> = table
        .arrays
        .iter()
        .map(|array| array.path.as_str())
        .collect();
    publish_staged(staging.path(), output_root, &array_paths)?;

    Ok((
        table,
        ArrayReport {
            default_texture,
            arrays: class_reports,
            textures,
            skipped,
        },
    ))
}

/// The table row being planned, with its members.
struct PlannedEntry {
    ltex_form_id: u32,
    diffuse: MemberPlan,
    normal: Option<MemberPlan>,
}

/// The table row being built, with the member indexes of its textures.
struct EntryPlan {
    ltex_form_id: u32,
    diffuse: usize,
    normal: Option<usize>,
}

/// One class: the usage, block format and size its members share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClassKey {
    usage: LandTextureUsage,
    format: LandTextureFormat,
    size: u32,
}

impl ClassKey {
    /// The order classes are written in: diffuse before normal, then by format, then size.
    fn order(self) -> (u8, u8, u32) {
        (usage_rank(self.usage), format_rank(self.format), self.size)
    }
}

/// How a source stores its texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceFormat {
    /// A KTX2 with a vkFormat.
    Native(ktx2::Format),
    /// A universal (UASTC) KTX2, vkFormat 0, as the converter writes for L8, 16-bit and palette
    /// sources.
    Uastc,
}

impl SourceFormat {
    /// The spelling the report uses.
    fn name(self) -> String {
        match self {
            Self::Native(format) => format!("{format:?}"),
            Self::Uastc => "UASTC".to_string(),
        }
    }
}

/// Where a built member ended up.
struct Placement {
    /// Index into the table's arrays.
    array: usize,
    layer: u16,
    action: TextureAction,
}

/// One texture on its way into an array.
struct MemberPlan {
    /// The LTEX FormID of the source it was read from, for the skipped list.
    source_ltex_form_id: u32,
    class: ClassKey,
    /// The source file, or `None` for the generated white default texture.
    path: Option<PathBuf>,
    format: SourceFormat,
    /// `format`'s spelling, for the report.
    format_name: String,
    width: u32,
    height: u32,
    /// Mip levels the source stores.
    level_count: u32,
    usage: LandTextureUsage,
    /// The source is larger than the largest class and `allow_downscale` is set.
    downscaled: bool,
    /// For a normal map, the member index of its diffuse: a normal is built only if that
    /// diffuse was.
    depends_on: Option<usize>,
    /// Why it was left out, filled in while its class is built.
    failed: Option<String>,
    /// Where it went, filled in while its class is built.
    placed: Option<Placement>,
}

impl MemberPlan {
    /// The generated white default texture, in the class it will be upscaled into.
    fn white(class: ClassKey) -> Self {
        Self {
            source_ltex_form_id: DEFAULT_LAND_TEXTURE_FORM_ID,
            class,
            path: None,
            format: SourceFormat::Native(array_vk_format(class.usage, class.format)),
            format_name: "generated".to_string(),
            width: WHITE_SIDE,
            height: WHITE_SIDE,
            level_count: 1,
            usage: class.usage,
            downscaled: false,
            depends_on: None,
            failed: None,
            placed: None,
        }
    }

    /// Whether the source is square at the class size, so its authored levels line up with the
    /// class's levels.
    fn same_size(&self) -> bool {
        self.path.is_some()
            && !self.downscaled
            && self.width == self.class.size
            && self.height == self.class.size
    }

    /// Whether the source's block format is one its usage keeps as it is.
    fn kept(&self) -> bool {
        match self.format {
            SourceFormat::Native(format) => {
                block_format(format).is_some_and(|kept| usage_formats(self.usage).contains(&kept))
            }
            SourceFormat::Uastc => false,
        }
    }
}

/// The result of planning one source texture.
enum Planned {
    Member(Box<MemberPlan>),
    /// The texture cannot be read; the reason is for the report.
    Skipped(String),
}

/// Plans a source's diffuse and normal. A skipped diffuse drops the whole entry (`None`); a
/// skipped normal only drops the normal map. Only a size-limit error is returned as an error.
fn plan_entry(
    source: &LandTextureSource,
    entry_form_id: u32,
    options: &ArrayOptions,
    skipped: &mut Vec<SkippedTexture>,
) -> Result<Option<PlannedEntry>> {
    let diffuse = match plan_source(
        &source.diffuse,
        source.ltex_form_id,
        LandTextureUsage::DiffuseSrgb,
        options,
    )? {
        Planned::Member(member) => *member,
        Planned::Skipped(reason) => {
            skipped.push(SkippedTexture {
                ltex_form_id: source.ltex_form_id,
                usage: LandTextureUsage::DiffuseSrgb,
                source: source.diffuse.display().to_string(),
                reason,
            });
            // The whole entry goes, so its normal map is not used either: say so.
            if let Some(normal) = &source.normal {
                skipped.push(SkippedTexture {
                    ltex_form_id: source.ltex_form_id,
                    usage: LandTextureUsage::NormalLinear,
                    source: normal.display().to_string(),
                    reason: "its diffuse texture was skipped, so the land texture has no entry"
                        .to_owned(),
                });
            }
            return Ok(None);
        }
    };
    let normal = match &source.normal {
        None => None,
        Some(path) => match plan_source(
            path,
            source.ltex_form_id,
            LandTextureUsage::NormalLinear,
            options,
        )? {
            Planned::Member(member) => Some(*member),
            Planned::Skipped(reason) => {
                skipped.push(SkippedTexture {
                    ltex_form_id: source.ltex_form_id,
                    usage: LandTextureUsage::NormalLinear,
                    source: path.display().to_string(),
                    reason,
                });
                None
            }
        },
    };
    Ok(Some(PlannedEntry {
        ltex_form_id: entry_form_id,
        diffuse,
        normal,
    }))
}

/// Reads one source's header and works out the class it belongs to.
fn plan_source(
    path: &Path,
    source_form_id: u32,
    usage: LandTextureUsage,
    options: &ArrayOptions,
) -> Result<Planned> {
    let header = match read_header(path) {
        Ok(header) => header,
        Err(reason) => return Ok(Planned::Skipped(reason)),
    };
    let format = match header.format {
        // A universal texture is transcoded to BC7, which both usages keep.
        SourceFormat::Uastc => LandTextureFormat::Bc7,
        SourceFormat::Native(native) => {
            match block_format(native).filter(|kept| usage_formats(usage).contains(kept)) {
                Some(kept) => kept,
                None if dxgi_format(native).is_some() => LandTextureFormat::Bc7,
                None => {
                    return Ok(Planned::Skipped(format!(
                        "{native:?} has no decoder here, so it cannot be re-encoded to BC7"
                    )));
                }
            }
        }
    };
    let side = header.width.max(header.height);
    let size = match options
        .size_classes
        .iter()
        .copied()
        .find(|size| *size >= side)
    {
        Some(size) => size,
        None if options.allow_downscale => *options
            .size_classes
            .last()
            .expect("size classes are non-empty"),
        None => {
            return Err(eyre!(
                "land texture {} is {}x{}, larger than the largest size class {}; pass \
                 allow_downscale to downscale it",
                path.display(),
                header.width,
                header.height,
                options.size_classes[2]
            ));
        }
    };
    Ok(Planned::Member(Box::new(MemberPlan {
        source_ltex_form_id: source_form_id,
        class: ClassKey {
            usage,
            format,
            size,
        },
        path: Some(path.to_path_buf()),
        format: header.format,
        format_name: header.format.name(),
        width: header.width,
        height: header.height,
        level_count: header.level_count,
        usage,
        downscaled: side > size,
        depends_on: None,
        failed: None,
        placed: None,
    })))
}

/// The class a generated white texture goes into: the smallest diffuse class the content uses
/// (the smallest one in production, 1024 BC1), so the default never invents a new array.
fn white_class(planned: &[PlannedEntry], options: &ArrayOptions) -> ClassKey {
    planned
        .iter()
        .map(|entry| entry.diffuse.class)
        .min_by_key(|class| (class.size, format_rank(class.format)))
        .unwrap_or(ClassKey {
            usage: LandTextureUsage::DiffuseSrgb,
            format: LandTextureFormat::Bc1,
            size: options.size_classes[0],
        })
}

/// One member's levels at the class size, every level of the class's chain.
struct BuiltMember {
    /// One block buffer per level, level 0 first.
    levels: Vec<Vec<u8>>,
    action: TextureAction,
}

/// Builds one member's levels for its class; see the module documentation for the rules. An
/// error here means the texture is skipped, with the error as its reason.
fn build_member(member: &MemberPlan) -> Result<BuiltMember> {
    let class = member.class;
    let natural = max_mip_levels(class.size, class.size, 1);
    let built = match &member.path {
        None => {
            let white = RgbaImage::from_pixel(WHITE_SIDE, WHITE_SIDE, Rgba([255, 255, 255, 255]));
            BuiltMember {
                levels: rebuild_levels(&white, class, natural)?,
                action: TextureAction::Rebuilt {
                    reencoded_from: None,
                    generated_levels: natural,
                },
            }
        }
        Some(path) => {
            let source_levels = read_levels(path, member)?;
            if member.same_size() {
                build_same_size(member, source_levels)?
            } else {
                build_resized(member, &source_levels)?
            }
        }
    };
    ensure!(
        built.levels.len() == natural as usize,
        "built {} levels, expected {natural}",
        built.levels.len()
    );
    for (level, data) in built.levels.iter().enumerate() {
        let expected = level_bytes(class.format, class.size, level as u32);
        ensure!(
            data.len() == expected,
            "mip {level} holds {} bytes, expected {expected}",
            data.len()
        );
    }
    Ok(built)
}

/// A source square at the class size: its authored levels are used as the class's levels of the
/// same size (copied, transcoded or re-encoded one by one), and only the missing tail is
/// generated.
fn build_same_size(member: &MemberPlan, source_levels: Vec<Vec<u8>>) -> Result<BuiltMember> {
    let class = member.class;
    let natural = max_mip_levels(class.size, class.size, 1);
    let authored = source_levels.len() as u32;
    let mut levels: Vec<Vec<u8>> = Vec::with_capacity(natural as usize);
    // The decoded RGBA of the last re-encoded level, so the tail is made from the source's own
    // texels and not from a second round of encoding.
    let mut last_decoded: Option<RgbaImage> = None;
    for (level, data) in source_levels.into_iter().enumerate() {
        let side = mip_dimensions(class.size, level as u32);
        match member.format {
            SourceFormat::Uastc => levels.push(transcode_uastc_level(&data, side, side)?),
            SourceFormat::Native(_) if member.kept() => {
                let expected = level_bytes(class.format, class.size, level as u32);
                ensure!(
                    data.len() == expected,
                    "mip {level} holds {} bytes, expected {expected}",
                    data.len()
                );
                levels.push(data);
            }
            SourceFormat::Native(format) => {
                let image = decode_blocks(format, side, side, &data)?;
                levels.push(encode_blocks(class.format, side, side, image.as_raw())?);
                last_decoded = Some(image);
            }
        }
    }
    let generated = natural.saturating_sub(authored);
    if generated > 0 {
        let from_level = authored - 1;
        let base = match last_decoded {
            Some(image) => image,
            None => {
                let side = mip_dimensions(class.size, from_level);
                let last = levels
                    .last()
                    .ok_or_else(|| eyre!("the source has no levels"))?;
                decode_blocks(array_vk_format(class.usage, class.format), side, side, last)?
            }
        };
        levels.extend(generate_levels(base, class, authored, natural)?);
    }
    let action = match member.format {
        SourceFormat::Uastc => TextureAction::Transcoded {
            generated_levels: generated,
        },
        SourceFormat::Native(_) if member.kept() => {
            if generated == 0 {
                TextureAction::Copied
            } else {
                TextureAction::CopiedWithGeneratedTail {
                    authored_levels: authored,
                    generated_levels: generated,
                }
            }
        }
        SourceFormat::Native(_) => TextureAction::Rebuilt {
            reencoded_from: Some(member.format_name.clone()),
            generated_levels: generated,
        },
    };
    Ok(BuiltMember { levels, action })
}

/// A source of another size (or one downscaled on the user's option): level 0 is decoded,
/// resized to the class size and the whole chain generated from it.
fn build_resized(member: &MemberPlan, source_levels: &[Vec<u8>]) -> Result<BuiltMember> {
    let class = member.class;
    let natural = max_mip_levels(class.size, class.size, 1);
    let first = source_levels
        .first()
        .ok_or_else(|| eyre!("the source has no levels"))?;
    let image = match member.format {
        SourceFormat::Native(format) => decode_blocks(format, member.width, member.height, first)?,
        SourceFormat::Uastc => {
            let blocks = transcode_uastc_level(first, member.width, member.height)?;
            decode_blocks(
                ktx2::Format::BC7_UNORM_BLOCK,
                member.width,
                member.height,
                &blocks,
            )?
        }
    };
    let levels = rebuild_levels(&image, class, natural)?;
    let reencoded_from = (!member.kept()).then(|| member.format_name.clone());
    let action = if member.downscaled {
        TextureAction::Downscaled {
            from_size: member.width.max(member.height),
            reencoded_from,
        }
    } else {
        TextureAction::Rebuilt {
            reencoded_from,
            generated_levels: natural,
        }
    };
    Ok(BuiltMember { levels, action })
}

/// Decodes, resizes and re-encodes one texture at the class size, `depth` levels deep.
fn rebuild_levels(image: &RgbaImage, class: ClassKey, depth: u32) -> Result<Vec<Vec<u8>>> {
    let mut current = if image.width() == class.size && image.height() == class.size {
        image.clone()
    } else {
        resize(image, class.size, class.size, FilterType::Lanczos3)
    };
    let mut encoded = Vec::with_capacity(depth as usize);
    for level in 0..depth {
        if level > 0 {
            let side = mip_dimensions(class.size, level);
            current = resize(&current, side, side, FilterType::Lanczos3);
        }
        let side = mip_dimensions(class.size, level);
        encoded.push(encode_blocks(class.format, side, side, current.as_raw())?);
    }
    Ok(encoded)
}

/// Generates the levels `from_level..natural` of a class from `base`, the decoded texels of
/// level `from_level - 1`: each level is a Lanczos3 downscale of the one above it.
fn generate_levels(
    base: RgbaImage,
    class: ClassKey,
    from_level: u32,
    natural: u32,
) -> Result<Vec<Vec<u8>>> {
    let mut current = base;
    let mut encoded = Vec::with_capacity(natural.saturating_sub(from_level) as usize);
    for level in from_level..natural {
        let side = mip_dimensions(class.size, level);
        current = resize(&current, side, side, FilterType::Lanczos3);
        encoded.push(encode_blocks(class.format, side, side, current.as_raw())?);
    }
    Ok(encoded)
}

/// One source texture's KTX2 header, as far as the builder needs it.
struct SourceHeader {
    format: SourceFormat,
    width: u32,
    height: u32,
    level_count: u32,
}

/// Reads the header of one converted texture. An `Err` is the reason the texture is skipped: a
/// layout a land texture cannot have, or a file that cannot be read.
fn read_header(path: &Path) -> std::result::Result<SourceHeader, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("cannot be opened: {error}"))?;
    let mut head = [0u8; ktx2::Header::LENGTH];
    file.read_exact(&mut head)
        .map_err(|error| format!("is not a KTX2 texture: {error}"))?;
    let header = ktx2::Header::from_bytes(&head)
        .map_err(|error| format!("is not a KTX2 texture: {error:?}"))?;
    if header.pixel_depth > 1 || header.face_count > 1 || header.layer_count > 1 {
        return Err("is a 3D, cubemap or array texture; a land texture is one 2D image".into());
    }
    if !matches!(
        header.supercompression_scheme,
        None | Some(ktx2::SupercompressionScheme::Zstandard)
    ) {
        return Err(format!(
            "uses {:?} supercompression, which the converter does not write for land textures",
            header.supercompression_scheme
        ));
    }
    if header.pixel_width == 0 || header.pixel_height == 0 {
        return Err("has no width or height".into());
    }
    if header.level_count == 0 {
        return Err("has no mip levels".into());
    }
    if header.level_count > MAX_MIP_DEPTH
        || header.level_count > max_mip_levels(header.pixel_width, header.pixel_height, 1)
    {
        return Err(format!(
            "{}x{} declares {} mip levels, more than its dimensions allow",
            header.pixel_width, header.pixel_height, header.level_count
        ));
    }
    let format = match header.format {
        Some(format) => SourceFormat::Native(format),
        None => {
            // No vkFormat: the converter's UASTC fallback. The colour model tells it from other
            // universal formats; only the whole file has the descriptor.
            let bytes = fs::read(path).map_err(|error| format!("cannot be read: {error}"))?;
            let reader = ktx2::Reader::new(&bytes)
                .map_err(|error| format!("is not a KTX2 texture: {error:?}"))?;
            if reader.color_model() != Some(ktx2::ColorModel::UASTC) {
                return Err(format!(
                    "has no vkFormat and its colour model is {:?}, not UASTC",
                    reader.color_model()
                ));
            }
            SourceFormat::Uastc
        }
    };
    Ok(SourceHeader {
        format,
        width: header.pixel_width,
        height: header.pixel_height,
        level_count: header.level_count,
    })
}

/// Reads one converted texture and decompresses every mip level; the file must still be what
/// `member` planned.
fn read_levels(path: &Path, member: &MemberPlan) -> Result<Vec<Vec<u8>>> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let reader = ktx2::Reader::new(&bytes)
        .map_err(|error| eyre!("{} is not a KTX2 texture: {error:?}", path.display()))?;
    let header = reader.header();
    ensure!(
        header.pixel_width == member.width
            && header.pixel_height == member.height
            && header.level_count == member.level_count,
        "{} changed while the land texture arrays were built",
        path.display()
    );
    let scheme = header.supercompression_scheme;
    let mut levels = Vec::with_capacity(member.level_count as usize);
    for (level, stored) in reader.levels().enumerate() {
        let data = match scheme {
            None => stored.data.to_vec(),
            Some(ktx2::SupercompressionScheme::Zstandard) => zstd::stream::decode_all(stored.data)
                .wrap_err_with(|| {
                    format!("{} mip {level} cannot be decompressed", path.display())
                })?,
            Some(other) => {
                return Err(eyre!(
                    "{} uses {other:?} supercompression, which the converter does not write",
                    path.display()
                ));
            }
        };
        ensure!(
            stored.uncompressed_byte_length == 0
                || stored.uncompressed_byte_length == data.len() as u64,
            "{} mip {level} decompressed to {} bytes, not the {} the file declares",
            path.display(),
            data.len(),
            stored.uncompressed_byte_length
        );
        levels.push(data);
    }
    Ok(levels)
}

/// Transcodes one UASTC level to BC7 blocks with the converter's basis-universal transcoder.
fn transcode_uastc_level(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let blocks = LowLevelUastcTranscoder::new()
        .transcode_slice(
            data,
            SliceParametersUastc {
                num_blocks_x: width.div_ceil(4),
                num_blocks_y: height.div_ceil(4),
                has_alpha: true,
                original_width: width,
                original_height: height,
            },
            DecodeFlags::HIGH_QUALITY,
            TranscoderBlockFormat::BC7,
        )
        .map_err(|error| eyre!("a {width}x{height} UASTC level cannot be transcoded: {error:?}"))?;
    let expected = width.div_ceil(4) as usize * height.div_ceil(4) as usize * 16;
    ensure!(
        blocks.len() == expected,
        "a {width}x{height} UASTC level transcoded to {} bytes, expected {expected}",
        blocks.len()
    );
    Ok(blocks)
}

/// The block format a vkFormat keeps as-is, or `None` when its usage's list does not have it.
fn block_format(format: ktx2::Format) -> Option<LandTextureFormat> {
    match format {
        ktx2::Format::BC1_RGB_UNORM_BLOCK
        | ktx2::Format::BC1_RGB_SRGB_BLOCK
        | ktx2::Format::BC1_RGBA_UNORM_BLOCK
        | ktx2::Format::BC1_RGBA_SRGB_BLOCK => Some(LandTextureFormat::Bc1),
        ktx2::Format::BC3_UNORM_BLOCK | ktx2::Format::BC3_SRGB_BLOCK => {
            Some(LandTextureFormat::Bc3)
        }
        ktx2::Format::BC5_UNORM_BLOCK => Some(LandTextureFormat::Bc5),
        ktx2::Format::BC7_UNORM_BLOCK | ktx2::Format::BC7_SRGB_BLOCK => {
            Some(LandTextureFormat::Bc7)
        }
        _ => None,
    }
}

/// The DDS format one KTX2 format decodes through, or `None` when it has no equivalent.
fn dxgi_format(format: ktx2::Format) -> Option<DxgiFormat> {
    match format {
        ktx2::Format::BC1_RGB_UNORM_BLOCK
        | ktx2::Format::BC1_RGB_SRGB_BLOCK
        | ktx2::Format::BC1_RGBA_UNORM_BLOCK
        | ktx2::Format::BC1_RGBA_SRGB_BLOCK => Some(DxgiFormat::BC1_UNorm),
        ktx2::Format::BC2_UNORM_BLOCK | ktx2::Format::BC2_SRGB_BLOCK => Some(DxgiFormat::BC2_UNorm),
        ktx2::Format::BC3_UNORM_BLOCK | ktx2::Format::BC3_SRGB_BLOCK => Some(DxgiFormat::BC3_UNorm),
        ktx2::Format::BC4_UNORM_BLOCK => Some(DxgiFormat::BC4_UNorm),
        ktx2::Format::BC4_SNORM_BLOCK => Some(DxgiFormat::BC4_SNorm),
        ktx2::Format::BC5_UNORM_BLOCK => Some(DxgiFormat::BC5_UNorm),
        ktx2::Format::BC5_SNORM_BLOCK => Some(DxgiFormat::BC5_SNorm),
        ktx2::Format::BC6H_UFLOAT_BLOCK => Some(DxgiFormat::BC6H_UF16),
        ktx2::Format::BC6H_SFLOAT_BLOCK => Some(DxgiFormat::BC6H_SF16),
        ktx2::Format::BC7_UNORM_BLOCK | ktx2::Format::BC7_SRGB_BLOCK => Some(DxgiFormat::BC7_UNorm),
        ktx2::Format::R8G8B8A8_UNORM | ktx2::Format::R8G8B8A8_SRGB => {
            Some(DxgiFormat::R8G8B8A8_UNorm)
        }
        ktx2::Format::R8_UNORM => Some(DxgiFormat::R8_UNorm),
        _ => None,
    }
}

/// Decodes one level's blocks to RGBA8 through the crate that decodes the converted output. A
/// block-compressed level whose sides are not multiples of four (the 2 x 2 and 1 x 1 tail of a
/// mip chain) is decoded as the padded surface its blocks describe and cropped.
fn decode_blocks(
    format: ktx2::Format,
    width: u32,
    height: u32,
    blocks: &[u8],
) -> Result<RgbaImage> {
    let dxgi = dxgi_format(format).ok_or_else(|| {
        eyre!("{format:?} has no decoder here, so it cannot be re-encoded to BC7")
    })?;
    let compressed = matches!(
        dxgi,
        DxgiFormat::BC1_UNorm
            | DxgiFormat::BC2_UNorm
            | DxgiFormat::BC3_UNorm
            | DxgiFormat::BC4_UNorm
            | DxgiFormat::BC5_UNorm
            | DxgiFormat::BC6H_UF16
            | DxgiFormat::BC7_UNorm
    );
    let (padded_width, padded_height) = if compressed {
        (width.next_multiple_of(4), height.next_multiple_of(4))
    } else {
        (width, height)
    };
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: padded_height,
        width: padded_width,
        depth: None,
        format: dxgi,
        mipmap_levels: Some(1),
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
    })
    .wrap_err_with(|| format!("{width}x{height} {format:?} cannot be wrapped for decoding"))?;
    ensure!(
        dds.data.len() == blocks.len(),
        "a {width}x{height} {format:?} level holds {} bytes, expected {}",
        blocks.len(),
        dds.data.len()
    );
    dds.data.copy_from_slice(blocks);
    let image = image_dds::image_from_dds(&dds, 0)
        .wrap_err_with(|| format!("a {width}x{height} {format:?} level cannot be decoded"))?;
    if (padded_width, padded_height) == (width, height) {
        return Ok(image);
    }
    ensure!(
        image.width() == padded_width && image.height() == padded_height,
        "a {width}x{height} {format:?} level decoded to {}x{}",
        image.width(),
        image.height()
    );
    let stride = padded_width as usize * 4;
    let row_bytes = width as usize * 4;
    let mut cropped = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        cropped.extend_from_slice(&image.as_raw()[row * stride..][..row_bytes]);
    }
    RgbaImage::from_raw(width, height, cropped)
        .ok_or_else(|| eyre!("a cropped {width}x{height} level has the wrong size"))
}

/// Encodes one RGBA8 level to the class's block format, padding the edges of a level whose
/// dimensions are not multiples of four (the 2 x 2 and 1 x 1 tail of a full mip chain).
fn encode_blocks(
    format: LandTextureFormat,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<Vec<u8>> {
    let padded_width = width.next_multiple_of(4);
    let padded_height = height.next_multiple_of(4);
    let padded = (padded_width != width || padded_height != height)
        .then(|| pad_rgba(width, height, padded_width, padded_height, rgba));
    let (data, surface_width) = padded
        .as_deref()
        .map_or((rgba, width), |padded| (padded, padded_width));
    let surface = intel_tex_2::RgbaSurface {
        data,
        width: surface_width,
        height: padded_height,
        stride: surface_width * 4,
    };
    Ok(match format {
        LandTextureFormat::Bc1 => intel_tex_2::bc1::compress_blocks(&surface),
        LandTextureFormat::Bc3 => intel_tex_2::bc3::compress_blocks(&surface),
        LandTextureFormat::Bc5 => intel_tex_2::bc5::compress_blocks(&surface),
        LandTextureFormat::Bc7 => intel_tex_2::bc7::compress_blocks(&bc7_settings(data), &surface),
    })
}

/// The BC7 profile for decoded texels: the opaque one when every texel is opaque (BC7 mode 6
/// can land a step below 255, as the texture converter notes), the alpha one otherwise.
fn bc7_settings(rgba: &[u8]) -> intel_tex_2::bc7::EncodeSettings {
    if rgba.len().is_multiple_of(4) && rgba.as_chunks::<4>().0.iter().all(|texel| texel[3] == 255) {
        intel_tex_2::bc7::opaque_fast_settings()
    } else {
        intel_tex_2::bc7::alpha_fast_settings()
    }
}

/// Repeats the last row and column of an RGBA8 level until it covers whole 4 x 4 blocks.
fn pad_rgba(
    width: u32,
    height: u32,
    padded_width: u32,
    padded_height: u32,
    rgba: &[u8],
) -> Vec<u8> {
    let row_bytes = width as usize * 4;
    let mut padded = Vec::with_capacity(padded_width as usize * padded_height as usize * 4);
    for y in 0..padded_height as usize {
        let row = &rgba[y.min(height as usize - 1) * row_bytes..][..row_bytes];
        padded.extend_from_slice(row);
        for _ in width..padded_width {
            padded.extend_from_slice(&row[row_bytes - 4..]);
        }
    }
    padded
}

/// One level of an array being written: its Zstandard stream and the spool file it goes to.
struct LevelSpool {
    path: PathBuf,
    encoder: zstd::stream::write::Encoder<'static, BufWriter<fs::File>>,
    /// Bytes fed to the encoder so far.
    uncompressed: u64,
}

/// A level once its stream is finished.
struct FinishedLevel {
    path: PathBuf,
    compressed: u64,
    uncompressed: u64,
}

/// Writes one array file without holding it in memory: layers are appended level by level into
/// one Zstandard stream per level, spooled to files, and [`ArrayWriter::finish`] assembles the
/// KTX2 from the spool files.
struct ArrayWriter {
    key: ClassKey,
    layers: u32,
    spools: Vec<LevelSpool>,
}

impl ArrayWriter {
    fn create(spool_dir: &Path, key: ClassKey, levels: u32) -> Result<Self> {
        ensure!(levels >= 1, "a texture array holds at least one level");
        let stem = array_stem(key);
        let mut spools = Vec::with_capacity(levels as usize);
        for level in 0..levels {
            let path = spool_dir.join(format!("{stem}.level{level}.zst"));
            let file = fs::File::create(&path)
                .wrap_err_with(|| format!("failed to create {}", path.display()))?;
            let encoder = zstd::stream::write::Encoder::new(BufWriter::new(file), ZSTD_LEVEL)
                .wrap_err("Zstandard supercompression failed")?;
            spools.push(LevelSpool {
                path,
                encoder,
                uncompressed: 0,
            });
        }
        Ok(Self {
            key,
            layers: 0,
            spools,
        })
    }

    /// Layers appended so far.
    fn layers(&self) -> u32 {
        self.layers
    }

    /// Appends one layer: one buffer per level, level 0 first.
    fn push_layer(&mut self, levels: &[Vec<u8>]) -> Result<()> {
        ensure!(
            levels.len() == self.spools.len(),
            "a layer has {} levels, the array {}",
            levels.len(),
            self.spools.len()
        );
        for (level, (spool, data)) in self.spools.iter_mut().zip(levels).enumerate() {
            let expected = level_bytes(self.key.format, self.key.size, level as u32);
            ensure!(
                data.len() == expected,
                "the array's level {level} layer holds {} bytes, expected {expected}",
                data.len()
            );
            spool
                .encoder
                .write_all(data)
                .wrap_err_with(|| format!("failed to write {}", spool.path.display()))?;
            spool.uncompressed += data.len() as u64;
        }
        self.layers += 1;
        Ok(())
    }

    /// Assembles the KTX2 at `output` and returns its size. Level data is stored from the
    /// smallest level to the largest, the level index stays largest-first (KTX2 3.9.7, 3.13).
    fn finish(self, output: &Path) -> Result<u64> {
        let Self {
            key,
            layers,
            spools,
        } = self;
        ensure!(layers >= 2, "a texture array holds at least two layers");
        let mut finished = Vec::with_capacity(spools.len());
        for spool in spools {
            let LevelSpool {
                path,
                encoder,
                uncompressed,
            } = spool;
            let mut writer = encoder
                .finish()
                .wrap_err("Zstandard supercompression failed")?;
            writer
                .flush()
                .wrap_err_with(|| format!("failed to write {}", path.display()))?;
            drop(writer);
            let compressed = fs::metadata(&path)
                .wrap_err_with(|| format!("failed to measure {}", path.display()))?
                .len();
            finished.push(FinishedLevel {
                path,
                compressed,
                uncompressed,
            });
        }

        let format = array_vk_format(key.usage, key.format);
        let (dfd, type_size) = ktx2::dfd::Basic::from_format(format)
            .map_err(|error| eyre!("no KTX2 descriptor for {format:?}: {error:?}"))?;
        let dfd_bytes = ktx2::dfd::Block::Basic(dfd).to_vec();
        let level_count = u32::try_from(finished.len()).wrap_err("too many mip levels")?;
        let dfd_length = dfd_bytes.len() as u32 + 4;
        let level_table_end = ktx2::Header::LENGTH + finished.len() * ktx2::LevelIndex::LENGTH;
        let dfd_offset = level_table_end.next_multiple_of(4);

        // Smallest level first in the file, so the last level gets the lowest offset.
        let mut offsets = vec![0u64; finished.len()];
        let mut cursor = dfd_offset as u64 + u64::from(dfd_length);
        // The levels are Zstandard-supercompressed, and KTX2 requires an alignment of 1 for
        // supercompressed data: each level starts exactly where the previous one ends, with no
        // padding between payloads.
        for level in (0..finished.len()).rev() {
            offsets[level] = cursor;
            cursor += finished[level].compressed;
        }
        let total = cursor;

        let header = ktx2::Header {
            format: Some(format),
            type_size,
            pixel_width: key.size,
            pixel_height: key.size,
            pixel_depth: 0,
            layer_count: layers,
            face_count: 1,
            level_count,
            supercompression_scheme: Some(ktx2::SupercompressionScheme::Zstandard),
            index: ktx2::Index {
                dfd_byte_offset: dfd_offset as u32,
                dfd_byte_length: dfd_length,
                kvd_byte_offset: 0,
                kvd_byte_length: 0,
                sgd_byte_offset: 0,
                sgd_byte_length: 0,
            },
        };
        let file = fs::File::create(output)
            .wrap_err_with(|| format!("failed to create {}", output.display()))?;
        let mut out = BufWriter::new(file);
        let mut position = 0u64;
        write_counted(&mut out, &mut position, &header.as_bytes())?;
        for (level, entry) in finished.iter().enumerate() {
            let index = ktx2::LevelIndex {
                byte_offset: offsets[level],
                byte_length: entry.compressed,
                uncompressed_byte_length: entry.uncompressed,
            };
            write_counted(&mut out, &mut position, &index.as_bytes())?;
        }
        pad_to(&mut out, &mut position, dfd_offset as u64)?;
        write_counted(&mut out, &mut position, &dfd_length.to_le_bytes())?;
        write_counted(&mut out, &mut position, &dfd_bytes)?;
        for level in (0..finished.len()).rev() {
            pad_to(&mut out, &mut position, offsets[level])?;
            let entry = &finished[level];
            let spool = fs::File::open(&entry.path)
                .wrap_err_with(|| format!("failed to open {}", entry.path.display()))?;
            let copied = io::copy(&mut BufReader::new(spool), &mut out)
                .wrap_err_with(|| format!("failed to copy {}", entry.path.display()))?;
            ensure!(
                copied == entry.compressed,
                "{} changed while the array was assembled",
                entry.path.display()
            );
            position += copied;
        }
        ensure!(
            position == total,
            "the array file is {position} bytes, expected {total}"
        );
        out.flush()
            .wrap_err_with(|| format!("failed to write {}", output.display()))?;
        out.get_ref()
            .sync_all()
            .wrap_err_with(|| format!("failed to sync {}", output.display()))?;
        drop(out);
        for entry in &finished {
            fs::remove_file(&entry.path)
                .wrap_err_with(|| format!("failed to remove {}", entry.path.display()))?;
        }
        Ok(total)
    }
}

/// Writes `bytes` and advances the tracked file position.
fn write_counted(out: &mut impl Write, position: &mut u64, bytes: &[u8]) -> Result<()> {
    out.write_all(bytes).wrap_err("failed to write the array")?;
    *position += bytes.len() as u64;
    Ok(())
}

/// Writes zero bytes until the tracked position is `target`.
fn pad_to(out: &mut impl Write, position: &mut u64, target: u64) -> Result<()> {
    ensure!(*position <= target, "the array layout overlaps itself");
    const ZEROS: [u8; 16] = [0; 16];
    while *position < target {
        let step = (target - *position).min(ZEROS.len() as u64) as usize;
        write_counted(out, position, &ZEROS[..step])?;
    }
    Ok(())
}

/// The staging folder of one build: `terrain.staging` under the output root, mirroring its
/// layout, removed when dropped. The name is fixed, not per process: the conversion holds the
/// output's exclusive lock for the whole build, so no other build can be using it, and a build
/// that was interrupted (and so skipped its `Drop`) leaves nothing that a later build must
/// tolerate beside its own files. Whatever is at this name when a build starts is cleared.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(output_root: &Path) -> Result<Self> {
        let path = output_root.join(STAGING_DIR);
        // Only this fixed name is cleared; nothing else under the output root is touched.
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path)
                .wrap_err_with(|| format!("failed to clear {}", path.display()))?,
            Ok(_) => fs::remove_file(&path)
                .wrap_err_with(|| format!("failed to clear {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .wrap_err_with(|| format!("failed to inspect {}", path.display()));
            }
        }
        let staging = Self { path };
        for dir in [staging.path.join(TERRAIN_DIR), staging.spool_dir()] {
            fs::create_dir_all(&dir)
                .wrap_err_with(|| format!("failed to create {}", dir.display()))?;
        }
        Ok(staging)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Where the per-level spool files go.
    fn spool_dir(&self) -> PathBuf {
        self.path.join("spool")
    }
}

/// The staging folder's name under the output root.
const STAGING_DIR: &str = "terrain.staging";

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Moves the finished files out of the staging folder: the array files one by one, the table
/// last, each as a rename over its target.
fn publish_staged(staging: &Path, output_root: &Path, array_paths: &[&str]) -> Result<()> {
    let target_dir = output_root.join(TERRAIN_DIR);
    fs::create_dir_all(&target_dir)
        .wrap_err_with(|| format!("failed to create {}", target_dir.display()))?;
    for relative in array_paths.iter().copied().chain([LAND_TEXTURE_TABLE_PATH]) {
        let from = staging.join(relative);
        let to = output_root.join(relative);
        fs::rename(&from, &to).wrap_err_with(|| {
            format!("failed to publish {} over {}", from.display(), to.display())
        })?;
    }
    Ok(())
}

/// The table's reference to a placed member.
fn array_layer(placed: &Placement) -> Result<ArrayLayer> {
    Ok(ArrayLayer {
        array: u8::try_from(placed.array).wrap_err("more than 255 texture arrays")?,
        layer: placed.layer,
    })
}

/// One class's file stem, shared by the array file and its spool files.
fn array_stem(key: ClassKey) -> String {
    format!(
        "land-{}-{}-{}",
        usage_name(key.usage),
        format_name(key.format),
        key.size
    )
}

/// One class's file name, relative to the output root.
fn array_path(key: ClassKey) -> String {
    format!("{TERRAIN_DIR}/{}.ktx2", array_stem(key))
}

/// The vkFormat of an array: the sRGB variant for diffuse, the linear one for normal. The block
/// data is identical, so a UNORM-stored diffuse source needs no view aliasing.
fn array_vk_format(usage: LandTextureUsage, format: LandTextureFormat) -> ktx2::Format {
    match (usage, format) {
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc1) => {
            ktx2::Format::BC1_RGBA_SRGB_BLOCK
        }
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc3) => ktx2::Format::BC3_SRGB_BLOCK,
        (LandTextureUsage::NormalLinear, LandTextureFormat::Bc1) => {
            ktx2::Format::BC1_RGBA_UNORM_BLOCK
        }
        (LandTextureUsage::NormalLinear, LandTextureFormat::Bc3) => ktx2::Format::BC3_UNORM_BLOCK,
        (LandTextureUsage::NormalLinear, LandTextureFormat::Bc5) => ktx2::Format::BC5_UNORM_BLOCK,
        // BC7 is the only format both usages keep, and the only transcode target.
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc7) => ktx2::Format::BC7_SRGB_BLOCK,
        (LandTextureUsage::NormalLinear, LandTextureFormat::Bc7) => ktx2::Format::BC7_UNORM_BLOCK,
        // A diffuse BC5 class cannot be built: DIFFUSE_FORMATS has no BC5 and a transcode goes
        // to BC7. The arm exists so the match is total.
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc5) => ktx2::Format::BC5_UNORM_BLOCK,
    }
}

/// The block formats a usage keeps as-is.
fn usage_formats(usage: LandTextureUsage) -> [LandTextureFormat; 3] {
    match usage {
        LandTextureUsage::DiffuseSrgb => DIFFUSE_FORMATS,
        LandTextureUsage::NormalLinear => NORMAL_FORMATS,
    }
}

/// Bytes in one block of a class format.
fn block_size(format: LandTextureFormat) -> usize {
    match format {
        LandTextureFormat::Bc1 => 8,
        LandTextureFormat::Bc3 | LandTextureFormat::Bc5 | LandTextureFormat::Bc7 => 16,
    }
}

/// Bytes one layer's level holds: `size` is the class size.
fn level_bytes(format: LandTextureFormat, size: u32, level: u32) -> usize {
    let side = mip_dimensions(size, level) as usize;
    side.div_ceil(4) * side.div_ceil(4) * block_size(format)
}

/// The side of mip `level` of a class: halves each level, down to one texel.
fn mip_dimensions(size: u32, level: u32) -> u32 {
    (size >> level).max(1)
}

/// The numeric order of a usage, for the class order.
fn usage_rank(usage: LandTextureUsage) -> u8 {
    match usage {
        LandTextureUsage::DiffuseSrgb => 0,
        LandTextureUsage::NormalLinear => 1,
    }
}

/// The numeric order of a block format, for the class order.
fn format_rank(format: LandTextureFormat) -> u8 {
    match format {
        LandTextureFormat::Bc1 => 0,
        LandTextureFormat::Bc3 => 1,
        LandTextureFormat::Bc5 => 2,
        LandTextureFormat::Bc7 => 3,
    }
}

/// The word the array file names and the report use for a usage.
fn usage_name(usage: LandTextureUsage) -> &'static str {
    match usage {
        LandTextureUsage::DiffuseSrgb => "diffuse",
        LandTextureUsage::NormalLinear => "normal",
    }
}

/// The lowercase name of a block format, for array file names.
fn format_name(format: LandTextureFormat) -> &'static str {
    match format {
        LandTextureFormat::Bc1 => "bc1",
        LandTextureFormat::Bc3 => "bc3",
        LandTextureFormat::Bc5 => "bc5",
        LandTextureFormat::Bc7 => "bc7",
    }
}

/// The uppercase name of a block format, for report lines.
fn format_label(format: LandTextureFormat) -> &'static str {
    match format {
        LandTextureFormat::Bc1 => "BC1",
        LandTextureFormat::Bc3 => "BC3",
        LandTextureFormat::Bc5 => "BC5",
        LandTextureFormat::Bc7 => "BC7",
    }
}

/// Fills in one texture's report row.
fn report_of(
    member: &MemberPlan,
    placed: &Placement,
    id: u16,
    ltex_form_id: u32,
    arrays: &[LandTextureArray],
) -> Result<TextureReport> {
    let array = arrays.get(placed.array).ok_or_else(|| {
        eyre!(
            "land texture {ltex_form_id:08X} names array {}",
            placed.array
        )
    })?;
    Ok(TextureReport {
        id,
        ltex_form_id,
        usage: member.usage,
        source: member.path.as_ref().map(|path| path.display().to_string()),
        source_format: member.format_name.clone(),
        source_width: member.width,
        source_height: member.height,
        format: member.class.format,
        size: member.class.size,
        array: array.path.clone(),
        layer: placed.layer,
        action: placed.action.clone(),
    })
}

/// Refuses options that cannot produce a table the engine accepts.
fn check_options(options: &ArrayOptions) -> Result<()> {
    ensure!(
        options.max_layers_per_array >= 2,
        "max_layers_per_array must be at least 2, a single member is padded with a copy: {}",
        options.max_layers_per_array
    );
    ensure!(
        options.max_layers_per_array <= u32::from(u16::MAX) + 1,
        "max_layers_per_array must fit the table's u16 layer index: {}",
        options.max_layers_per_array
    );
    ensure!(
        options.size_classes.iter().all(|size| *size >= 4),
        "size classes must be at least 4 texels: {:?}",
        options.size_classes
    );
    ensure!(
        options
            .size_classes
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "size classes must be ascending: {:?}",
        options.size_classes
    );
    Ok(())
}

/// Refuses a source list that cannot become a dense table.
fn check_sources(sources: &[LandTextureSource]) -> Result<()> {
    let mut seen: BTreeMap<u32, &Path> = BTreeMap::new();
    for source in sources {
        ensure!(
            source.ltex_form_id != DEFAULT_LAND_TEXTURE_FORM_ID,
            "land texture {:08X} is the default slot; pass it as the default texture",
            source.ltex_form_id
        );
        if let Some(first) = seen.insert(source.ltex_form_id, &source.diffuse) {
            return Err(eyre!(
                "land texture {:08X} is listed twice ({} and {})",
                source.ltex_form_id,
                first.display(),
                source.diffuse.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::{TextureConverter, TextureEncoding};

    /// A tiny class set, so fixtures stay a few hundred bytes. The shared table validator
    /// accepts only the production sizes, so a build with this set skips it.
    const TINY: [u32; 3] = [8, 16, 32];

    fn tiny() -> ArrayOptions {
        ArrayOptions {
            size_classes: TINY,
            ..ArrayOptions::default()
        }
    }

    fn dds_params(format: DxgiFormat, width: u32, height: u32, levels: u32) -> NewDxgiParams {
        NewDxgiParams {
            height,
            width,
            depth: None,
            format,
            mipmap_levels: Some(levels),
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Straight,
        }
    }

    /// Converts a DDS the way the pipeline does and writes the `.ktx2`: native BC payloads go
    /// through the converter's own writer, which preserves the blocks.
    fn write_fixture(dir: &Path, name: &str, dds: &Dds) -> PathBuf {
        let mut bytes = Vec::new();
        dds.write(&mut bytes).unwrap();
        let ktx2 = TextureConverter::convert(&bytes, TextureEncoding::DataLinear).unwrap();
        let path = dir.join(name);
        fs::write(&path, &ktx2).unwrap();
        path
    }

    /// A converted `.ktx2` fixture whose payload is a byte pattern.
    fn fixture(
        dir: &Path,
        name: &str,
        format: DxgiFormat,
        width: u32,
        height: u32,
        levels: u32,
        seed: u8,
    ) -> PathBuf {
        let mut dds = Dds::new_dxgi(dds_params(format, width, height, levels)).unwrap();
        for (index, byte) in dds.data.iter_mut().enumerate() {
            *byte = seed.wrapping_add((index * 7) as u8);
        }
        write_fixture(dir, name, &dds)
    }

    /// A BC1 fixture of one solid colour (RGB565), `size` square with `levels` levels.
    fn solid_bc1_fixture(dir: &Path, name: &str, size: u32, levels: u32, colour: u16) -> PathBuf {
        let mut dds = Dds::new_dxgi(dds_params(DxgiFormat::BC1_UNorm, size, size, levels)).unwrap();
        let colour = colour.to_le_bytes();
        for block in dds.data.as_chunks_mut::<8>().0 {
            block[..2].copy_from_slice(&colour);
            block[2..4].copy_from_slice(&colour);
            block[4..].fill(0);
        }
        write_fixture(dir, name, &dds)
    }

    /// RGB565 of pure red, green and blue.
    const RED: u16 = 0xF800;
    const GREEN: u16 = 0x07E0;
    const BLUE: u16 = 0x001F;

    /// A converted UASTC `.ktx2` of one solid colour, through the converter's own UASTC path,
    /// with the full mip chain.
    fn uastc_fixture(dir: &Path, name: &str, side: u32, rgba: [u8; 4]) -> PathBuf {
        let mut mips = Vec::new();
        let mut level_side = side;
        loop {
            mips.push(rgba.repeat((level_side * level_side) as usize));
            if level_side == 1 {
                break;
            }
            level_side /= 2;
        }
        let bytes =
            TextureConverter::encode_rgba_mips(side, side, &mips, TextureEncoding::ColorSrgb)
                .unwrap();
        let path = dir.join(name);
        fs::write(&path, &bytes).unwrap();
        path
    }

    /// Reads a KTX2 back — an array or a source — decompressing every mip level.
    fn read_ktx2(path: &Path) -> (ktx2::Header, Vec<Vec<u8>>) {
        let bytes = fs::read(path).unwrap();
        let reader = ktx2::Reader::new(&bytes).unwrap();
        let header = reader.header();
        let levels = reader
            .levels()
            .map(|level| match header.supercompression_scheme {
                Some(ktx2::SupercompressionScheme::Zstandard) => {
                    zstd::stream::decode_all(level.data).unwrap()
                }
                _ => level.data.to_vec(),
            })
            .collect();
        (header, levels)
    }

    /// One layer of one level of an array's decompressed levels.
    fn layer_slice(levels: &[Vec<u8>], level: usize, layers: usize, layer: usize) -> &[u8] {
        let each = levels[level].len() / layers;
        &levels[level][layer * each..(layer + 1) * each]
    }

    /// The centre texel of a decoded image.
    fn centre(image: &RgbaImage) -> [u8; 4] {
        image.get_pixel(image.width() / 2, image.height() / 2).0
    }

    /// Whether the RGB of `texel` is within `tolerance` of `expected` in every channel.
    fn near(texel: [u8; 4], expected: [u8; 3], tolerance: u8) -> bool {
        (0..3).all(|channel| texel[channel].abs_diff(expected[channel]) <= tolerance)
    }

    /// A converted 1024 x 1024 texture with the vanilla land texture mip chain (10 levels, down
    /// to 2 x 2).
    fn vanilla_fixture(dir: &Path, name: &str, format: DxgiFormat, seed: u8) -> PathBuf {
        fixture(dir, name, format, 1024, 1024, 10, seed)
    }

    #[test]
    fn copies_sources_block_for_block_and_writes_a_valid_table() {
        let dir = tempfile::tempdir().unwrap();
        let default = vanilla_fixture(dir.path(), "default.ktx2", DxgiFormat::BC1_UNorm, 3);
        let first = vanilla_fixture(dir.path(), "first.ktx2", DxgiFormat::BC1_UNorm, 11);
        let second = vanilla_fixture(dir.path(), "second.ktx2", DxgiFormat::BC1_UNorm, 29);
        // Out of FormID order on purpose: the table sorts the sources.
        let sources = [
            LandTextureSource {
                ltex_form_id: 0x200,
                diffuse: second.clone(),
                normal: None,
            },
            LandTextureSource {
                ltex_form_id: 0x100,
                diffuse: first.clone(),
                normal: None,
            },
        ];
        let default_source = LandTextureSource {
            ltex_form_id: 0x010,
            diffuse: default.clone(),
            normal: None,
        };
        let (table, report) = build_land_texture_arrays(
            &sources,
            Some(default_source),
            dir.path(),
            &ArrayOptions::default(),
        )
        .unwrap();

        // Production classes: the shared validator ran inside the build, run it again here.
        table.validate().unwrap();
        assert_eq!(table.format_version, TERRAIN_MESH_FORMAT_VERSION);
        assert_eq!(
            table.max_layers_per_array,
            DEFAULT_MAX_LAND_TEXTURE_ARRAY_LAYERS
        );
        assert_eq!(table.arrays.len(), 1, "one diffuse BC1 1024 class");
        let array = &table.arrays[0];
        assert_eq!(array.path, "terrain/land-diffuse-bc1-1024.ktx2");
        assert_eq!(array.usage, LandTextureUsage::DiffuseSrgb);
        assert_eq!(array.format, LandTextureFormat::Bc1);
        assert_eq!(array.size, 1024);
        assert_eq!(array.layers, 3, "the default and the two sources");
        assert_eq!(table.entries.len(), 3);
        assert_eq!(table.entries[0].ltex_form_id, 0, "entry 0 is the default");
        assert_eq!(table.entries[0].diffuse.layer, 0);
        assert_eq!(table.entries[1].ltex_form_id, 0x100);
        assert_eq!(table.entries[1].diffuse.layer, 1);
        assert_eq!(table.entries[2].ltex_form_id, 0x200);
        assert_eq!(table.entries[2].diffuse.layer, 2);
        for (id, entry) in table.entries.iter().enumerate() {
            assert_eq!(usize::from(entry.id), id);
            assert_eq!(entry.normal, None);
        }

        assert_eq!(
            report.default_texture,
            DefaultTexture::Provided {
                ltex_form_id: 0x010
            }
        );
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(report.arrays.len(), 1);
        assert_eq!(report.arrays[0].layers, 3);
        assert_eq!(
            report.arrays[0].levels, 11,
            "the class holds the full chain of a 1024 texture"
        );
        assert!(report.arrays[0].bytes > 0);
        assert_eq!(report.textures.len(), 3);
        // Vanilla stops at 2 x 2, so the 1 x 1 level is the one generated level, and said so.
        assert!(
            report.textures.iter().all(|texture| texture.action
                == TextureAction::CopiedWithGeneratedTail {
                    authored_levels: 10,
                    generated_levels: 1
                }),
            "{:?}",
            report.textures
        );
        assert_eq!(report.warnings().len(), 4, "{:?}", report.warnings());

        // The array holds the sources' own blocks for every authored level: the default, then
        // 0x100, then 0x200.
        let (header, levels) = read_ktx2(&dir.path().join(&array.path));
        assert_eq!(header.format, Some(ktx2::Format::BC1_RGBA_SRGB_BLOCK));
        assert_eq!(header.layer_count, 3);
        assert_eq!(header.level_count, 11);
        assert_eq!(
            header.supercompression_scheme,
            Some(ktx2::SupercompressionScheme::Zstandard)
        );
        let expected = [default, first, second].map(|path| read_ktx2(&path).1);
        for (level, data) in levels.iter().enumerate().take(10) {
            let mut offset = 0;
            for source in &expected {
                let blocks = &source[level];
                assert_eq!(&data[offset..offset + blocks.len()], &blocks[..]);
                offset += blocks.len();
            }
            assert_eq!(offset, data.len());
        }
        assert_eq!(levels[10].len(), 3 * 8, "three 1 x 1 BC1 blocks");

        // The table JSON the build wrote round-trips, at the shared path.
        let json = fs::read(dir.path().join(LAND_TEXTURE_TABLE_PATH)).unwrap();
        let written: LandTextureTable = serde_json::from_slice(&json).unwrap();
        assert_eq!(written, table);
        assert_no_staging(dir.path());
    }

    /// A build that was interrupted left its fixed staging folder behind, spool files included. The
    /// next build clears that folder and builds from scratch, and the unrelated legacy
    /// `terrain.staging-<pid>` folder beside it is not touched.
    #[test]
    fn a_stale_staging_folder_from_an_interrupted_build_is_cleared_by_the_next_build() {
        let dir = tempfile::tempdir().unwrap();
        let red = solid_bc1_fixture(dir.path(), "red.ktx2", 8, 4, RED);
        let stale_spool = dir.path().join("terrain.staging/spool");
        fs::create_dir_all(&stale_spool).unwrap();
        fs::write(
            stale_spool.join("land-diffuse-bc1-8.spool"),
            b"partial spool",
        )
        .unwrap();
        let stale_output = dir.path().join("terrain.staging/terrain");
        fs::create_dir_all(&stale_output).unwrap();
        fs::write(
            stale_output.join("land-diffuse-bc1-1024.ktx2.partial"),
            b"partial",
        )
        .unwrap();
        let legacy = dir.path().join("terrain.staging-4242/spool");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("kept.spool"), b"another process's").unwrap();

        let sources = [LandTextureSource {
            ltex_form_id: 0x10,
            diffuse: red,
            normal: None,
        }];
        let (table, _) = build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap();

        assert!(!stale_spool.join("land-diffuse-bc1-8.spool").exists());
        assert!(
            !dir.path().join("terrain.staging").exists(),
            "the build removed its own folder"
        );
        assert!(
            !dir.path()
                .join("terrain/land-diffuse-bc1-1024.ktx2.partial")
                .exists()
        );
        assert!(
            legacy.join("kept.spool").is_file(),
            "legacy folders are not touched"
        );
        let written: LandTextureTable =
            serde_json::from_slice(&fs::read(dir.path().join(LAND_TEXTURE_TABLE_PATH)).unwrap())
                .unwrap();
        assert_eq!(written, table);
    }

    /// No staging folder or partial file is left beside the output.
    fn assert_no_staging(root: &Path) {
        for entry in fs::read_dir(root).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.starts_with("terrain.staging"), "left over: {name}");
        }
        for entry in fs::read_dir(root.join(TERRAIN_DIR)).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains("partial") && !name.contains("spool"),
                "{name}"
            );
        }
    }

    #[test]
    fn bc1_and_bc3_sources_use_their_own_classes_and_single_members_are_padded() {
        let dir = tempfile::tempdir().unwrap();
        let bc1 = vanilla_fixture(dir.path(), "diffuse-bc1.ktx2", DxgiFormat::BC1_UNorm, 5);
        let bc3 = vanilla_fixture(dir.path(), "diffuse-bc3.ktx2", DxgiFormat::BC3_UNorm, 21);
        let normal = vanilla_fixture(dir.path(), "normal.ktx2", DxgiFormat::BC3_UNorm, 9);
        let sources = [
            LandTextureSource {
                ltex_form_id: 0x1234,
                diffuse: bc1,
                normal: Some(normal),
            },
            LandTextureSource {
                ltex_form_id: 0x2000,
                diffuse: bc3,
                normal: None,
            },
        ];
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &ArrayOptions::default())
                .unwrap();

        table.validate().unwrap();
        assert_eq!(report.default_texture, DefaultTexture::White);
        // A BC1 and a BC3 diffuse source are two classes, and the white default goes into the
        // smallest one (BC1 first on a tie).
        let find = |usage: LandTextureUsage, format: Option<LandTextureFormat>| {
            table
                .arrays
                .iter()
                .find(|array| {
                    array.usage == usage && format.is_none_or(|format| array.format == format)
                })
                .unwrap()
        };
        let diffuse_bc1 = find(LandTextureUsage::DiffuseSrgb, Some(LandTextureFormat::Bc1));
        let diffuse_bc3 = find(LandTextureUsage::DiffuseSrgb, Some(LandTextureFormat::Bc3));
        let normal_class = find(LandTextureUsage::NormalLinear, None);
        assert_eq!(
            diffuse_bc1.layers, 2,
            "the white default and the BC1 source's diffuse"
        );
        assert_eq!(diffuse_bc1.size, 1024);
        assert_eq!(diffuse_bc3.layers, 2, "a single member is padded");
        assert_eq!(diffuse_bc3.size, 1024);
        assert_eq!(normal_class.format, LandTextureFormat::Bc3);
        assert_eq!(normal_class.layers, 2, "a single member is padded");
        assert_eq!(normal_class.size, 1024);

        let (bc1_header, bc1_levels) = read_ktx2(&dir.path().join(&diffuse_bc1.path));
        let (bc3_header, bc3_levels) = read_ktx2(&dir.path().join(&diffuse_bc3.path));
        let (normal_header, normal_levels) = read_ktx2(&dir.path().join(&normal_class.path));
        assert_eq!(bc1_header.format, Some(ktx2::Format::BC1_RGBA_SRGB_BLOCK));
        assert_eq!(bc3_header.format, Some(ktx2::Format::BC3_SRGB_BLOCK));
        assert_eq!(normal_header.format, Some(ktx2::Format::BC3_UNORM_BLOCK));
        for levels in [&bc3_levels, &normal_levels] {
            for level in levels {
                let (first, second) = level.split_at(level.len() / 2);
                assert_eq!(first, second, "the padding layer is a copy of the member");
            }
        }
        assert_eq!(bc1_levels.len(), 11, "the full chain of a 1024 class");
        assert_eq!(bc3_levels.len(), 11);
        assert_eq!(normal_levels.len(), 11);

        assert_eq!(table.entries[0].ltex_form_id, 0);
        assert_eq!(table.entries[0].normal, None, "white has no normal map");
        assert_eq!(table.entries[1].ltex_form_id, 0x1234);
        assert_eq!(table.entries[1].normal.unwrap().layer, 0);
        assert_eq!(table.entries[2].ltex_form_id, 0x2000);
        let white = &report.textures[0];
        assert_eq!(white.source, None);
        assert_eq!(white.source_format, "generated");
        assert_eq!(white.size, 1024);
        assert_eq!(
            white.action,
            TextureAction::Rebuilt {
                reencoded_from: None,
                generated_levels: 11
            }
        );
        assert!(
            report.warnings()[0].contains("generated 4x4 white diffuse"),
            "{:?}",
            report.warnings()
        );
        assert_eq!(report.textures.len(), 4);
        let copied = report
            .textures
            .iter()
            .filter(|texture| {
                texture.action
                    == TextureAction::CopiedWithGeneratedTail {
                        authored_levels: 10,
                        generated_levels: 1,
                    }
            })
            .count();
        assert_eq!(copied, 3, "every source texture is copied");
    }

    #[test]
    fn upscales_a_smaller_source_into_its_class_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let small = fixture(dir.path(), "small.ktx2", DxgiFormat::BC1_UNorm, 8, 8, 4, 7);
        let sources = [LandTextureSource {
            ltex_form_id: 0x300,
            diffuse: small,
            normal: None,
        }];
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &ArrayOptions::default())
                .unwrap();

        table.validate().unwrap();
        assert_eq!(table.arrays.len(), 1);
        assert_eq!(table.arrays[0].size, 1024);
        assert_eq!(table.arrays[0].format, LandTextureFormat::Bc1);
        assert_eq!(
            table.arrays[0].layers, 2,
            "the white default and the source"
        );
        let upscaled = report
            .textures
            .iter()
            .find(|texture| texture.ltex_form_id == 0x300)
            .unwrap();
        assert_eq!(upscaled.source_width, 8);
        assert_eq!(upscaled.source_height, 8);
        assert_eq!(upscaled.size, 1024);
        assert_eq!(upscaled.layer, 1);
        assert_eq!(
            upscaled.action,
            TextureAction::Rebuilt {
                reencoded_from: None,
                generated_levels: 11
            }
        );
        let warnings = report.warnings();
        assert!(
            warnings
                .iter()
                .any(|line| line.contains("upscaled 8x8 -> 1024x1024")),
            "{warnings:?}"
        );
        assert_eq!(report.arrays[0].levels, 11);
        let (_, levels) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        assert_eq!(levels.len(), 11);
        for (level, data) in levels.iter().enumerate() {
            let blocks = (1024u32 >> level).max(1).div_ceil(4);
            assert_eq!(data.len(), blocks as usize * blocks as usize * 8 * 2);
        }
    }

    #[test]
    fn a_resized_source_keeps_its_colour() {
        // A rebuild that came out white (or black) would pass every shape check; the decoded
        // centre texel does not.
        let dir = tempfile::tempdir().unwrap();
        let small = solid_bc1_fixture(dir.path(), "green.ktx2", 8, 4, GREEN);
        let sources = [LandTextureSource {
            ltex_form_id: 0x300,
            diffuse: small,
            normal: None,
        }];
        let options = ArrayOptions {
            size_classes: [16, 32, 64],
            ..ArrayOptions::default()
        };
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &options).unwrap();
        assert_eq!(table.arrays.len(), 1);
        assert_eq!(table.arrays[0].size, 16);
        assert_eq!(table.arrays[0].layers, 2);
        assert_eq!(report.arrays[0].levels, 5, "16, 8, 4, 2, 1");
        let (_, levels) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        for (level, side) in [(0, 16), (2, 4)] {
            let blocks = layer_slice(&levels, level, 2, 1);
            let image =
                decode_blocks(ktx2::Format::BC1_RGBA_SRGB_BLOCK, side, side, blocks).unwrap();
            assert!(
                near(centre(&image), [0, 255, 0], 12),
                "level {level} centre {:?}",
                centre(&image)
            );
        }
        // The white default is white in layer 0.
        let blocks = layer_slice(&levels, 0, 2, 0);
        let white = decode_blocks(ktx2::Format::BC1_RGBA_SRGB_BLOCK, 16, 16, blocks).unwrap();
        assert!(
            near(centre(&white), [255, 255, 255], 8),
            "{:?}",
            centre(&white)
        );
    }

    #[test]
    fn a_mixed_depth_class_copies_the_full_chain_and_only_generates_the_short_tail() {
        let dir = tempfile::tempdir().unwrap();
        let full = solid_bc1_fixture(dir.path(), "full.ktx2", 32, 6, RED);
        let short = solid_bc1_fixture(dir.path(), "short.ktx2", 32, 4, GREEN);
        let default = solid_bc1_fixture(dir.path(), "default.ktx2", 32, 6, BLUE);
        let sources = [
            LandTextureSource {
                ltex_form_id: 0x100,
                diffuse: full.clone(),
                normal: None,
            },
            LandTextureSource {
                ltex_form_id: 0x200,
                diffuse: short.clone(),
                normal: None,
            },
        ];
        let default_source = LandTextureSource {
            ltex_form_id: 0x10,
            diffuse: default,
            normal: None,
        };
        let (table, report) =
            build_land_texture_arrays(&sources, Some(default_source), dir.path(), &tiny()).unwrap();
        assert_eq!(table.arrays.len(), 1);
        assert_eq!(table.arrays[0].layers, 3);
        assert_eq!(report.arrays[0].levels, 6, "the full chain of a 32 class");

        let (header, levels) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        assert_eq!(header.level_count, 6);
        let full_levels = read_ktx2(&full).1;
        let short_levels = read_ktx2(&short).1;
        assert_eq!(full_levels.len(), 6);
        assert_eq!(short_levels.len(), 4);
        // The full chain is copied byte for byte at every level, whatever its neighbour has.
        for (level, expected) in full_levels.iter().enumerate() {
            assert_eq!(
                layer_slice(&levels, level, 3, 1),
                &expected[..],
                "full {level}"
            );
        }
        // The short chain's authored levels are copied too, never rebuilt.
        for (level, expected) in short_levels.iter().enumerate() {
            assert_eq!(
                layer_slice(&levels, level, 3, 2),
                &expected[..],
                "short {level}"
            );
        }
        // Its tail is generated from its own colour, not white and not another texture's.
        for (level, side) in [(4, 2), (5, 1)] {
            let blocks = layer_slice(&levels, level, 3, 2);
            let image =
                decode_blocks(ktx2::Format::BC1_RGBA_SRGB_BLOCK, side, side, blocks).unwrap();
            assert!(
                near(centre(&image), [0, 255, 0], 12),
                "generated level {level}: {:?}",
                centre(&image)
            );
        }
        let by_ltex = |ltex_form_id: u32| {
            report
                .textures
                .iter()
                .find(|texture| texture.ltex_form_id == ltex_form_id)
                .unwrap()
        };
        assert_eq!(by_ltex(0x0).action, TextureAction::Copied);
        assert_eq!(by_ltex(0x100).action, TextureAction::Copied);
        assert_eq!(
            by_ltex(0x200).action,
            TextureAction::CopiedWithGeneratedTail {
                authored_levels: 4,
                generated_levels: 2
            }
        );
        let warnings = report.warnings();
        assert!(
            warnings
                .iter()
                .any(|line| line.contains("copied 4 authored mip levels, generated 2 more")),
            "{warnings:?}"
        );
        assert_eq!(warnings.len(), 2, "the default line and the short source");
    }

    #[test]
    fn a_uastc_source_is_transcoded_to_bc7_and_keeps_its_colours() {
        let dir = tempfile::tempdir().unwrap();
        let uastc = uastc_fixture(dir.path(), "palette.ktx2", 8, [200, 50, 20, 255]);
        let header = read_header(&uastc).unwrap();
        assert_eq!(header.format, SourceFormat::Uastc, "the fixture is UASTC");
        let sources = [LandTextureSource {
            ltex_form_id: 0x700,
            diffuse: uastc,
            normal: None,
        }];
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap();

        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(table.arrays.len(), 1);
        assert_eq!(table.arrays[0].format, LandTextureFormat::Bc7);
        assert_eq!(table.arrays[0].size, 8);
        assert_eq!(
            table.arrays[0].layers, 2,
            "the white default and the source"
        );
        let texture = report
            .textures
            .iter()
            .find(|texture| texture.ltex_form_id == 0x700)
            .unwrap();
        assert_eq!(texture.source_format, "UASTC");
        assert_eq!(
            texture.action,
            TextureAction::Transcoded {
                generated_levels: 0
            }
        );
        assert!(
            report
                .warnings()
                .iter()
                .any(|line| line.contains("re-encoded: UASTC -> BC7")),
            "{:?}",
            report.warnings()
        );
        let (array_header, levels) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        assert_eq!(array_header.format, Some(ktx2::Format::BC7_SRGB_BLOCK));
        assert_eq!(array_header.level_count, 4);
        for (level, side) in [(0, 8), (1, 4), (3, 1)] {
            let blocks = layer_slice(&levels, level, 2, 1);
            let image = decode_blocks(ktx2::Format::BC7_SRGB_BLOCK, side, side, blocks).unwrap();
            assert!(
                near(centre(&image), [200, 50, 20], 24),
                "level {level} centre {:?}",
                centre(&image)
            );
        }
    }

    #[test]
    fn a_format_outside_the_usage_list_is_reencoded_to_bc7_level_by_level() {
        let dir = tempfile::tempdir().unwrap();
        // A diffuse landscape texture stored as BC4 keeps its blocks in the converted output,
        // and DIFFUSE_FORMATS has no BC4.
        let bc4 = fixture(dir.path(), "bc4.ktx2", DxgiFormat::BC4_UNorm, 8, 8, 4, 17);
        let sources = [LandTextureSource {
            ltex_form_id: 0x500,
            diffuse: bc4,
            normal: None,
        }];
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap();

        assert_eq!(table.arrays.len(), 1);
        assert_eq!(table.arrays[0].format, LandTextureFormat::Bc7);
        assert_eq!(table.arrays[0].size, 8);
        let transcoded = report
            .textures
            .iter()
            .find(|texture| texture.ltex_form_id == 0x500)
            .unwrap();
        assert_eq!(transcoded.source_format, "BC4_UNORM_BLOCK");
        assert_eq!(
            transcoded.action,
            TextureAction::Rebuilt {
                reencoded_from: Some("BC4_UNORM_BLOCK".to_string()),
                generated_levels: 0
            }
        );
        assert!(
            report
                .warnings()
                .iter()
                .any(|line| line.contains("re-encoded: BC4_UNORM_BLOCK -> BC7")),
            "{:?}",
            report.warnings()
        );
        let (header, _) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        assert_eq!(header.format, Some(ktx2::Format::BC7_SRGB_BLOCK));
    }

    #[test]
    fn refuses_a_source_larger_than_the_largest_class_and_downscales_it_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let wide = fixture(
            dir.path(),
            "wide.ktx2",
            DxgiFormat::BC1_UNorm,
            128,
            4,
            1,
            13,
        );
        let sources = [LandTextureSource {
            ltex_form_id: 0x400,
            diffuse: wide,
            normal: None,
        }];
        let error = build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("wide.ktx2")
                && message.contains("larger than the largest size class 32"),
            "{message}"
        );
        assert!(
            !dir.path().join(TERRAIN_DIR).exists(),
            "nothing was written"
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "only the fixture"
        );

        let options = ArrayOptions {
            allow_downscale: true,
            ..tiny()
        };
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &options).unwrap();
        let downscaled = report
            .textures
            .iter()
            .find(|texture| texture.ltex_form_id == 0x400)
            .unwrap();
        assert_eq!(downscaled.source_width, 128);
        assert_eq!(downscaled.source_height, 4);
        assert_eq!(downscaled.size, 32);
        assert_eq!(
            downscaled.action,
            TextureAction::Downscaled {
                from_size: 128,
                reencoded_from: None
            }
        );
        assert!(
            report
                .warnings()
                .iter()
                .any(|line| line.contains("downscaled (user option): 128x4 -> 32x32")),
            "{:?}",
            report.warnings()
        );
        assert_eq!(table.arrays.len(), 1);
        assert_eq!(table.arrays[0].size, 32);
        assert_eq!(table.arrays[0].layers, 2);
        let (_, levels) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        assert_eq!(levels.len(), 6, "the full chain of a 32 class");
        assert_eq!(levels[0].len(), 8 * 8 * 8 * 2);
    }

    #[test]
    fn refuses_a_class_over_the_layer_limit() {
        let dir = tempfile::tempdir().unwrap();
        let sources = [0x100, 0x200].map(|ltex_form_id| LandTextureSource {
            ltex_form_id,
            diffuse: vanilla_fixture(
                dir.path(),
                &format!("{ltex_form_id:X}.ktx2"),
                DxgiFormat::BC1_UNorm,
                (ltex_form_id >> 8) as u8,
            ),
            normal: None,
        });
        let options = ArrayOptions {
            max_layers_per_array: 2,
            ..ArrayOptions::default()
        };
        let error = build_land_texture_arrays(&sources, None, dir.path(), &options).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("land-diffuse-bc1-1024.ktx2")
                && message.contains("more than max_layers_per_array 2"),
            "{message}"
        );
        assert!(!dir.path().join(TERRAIN_DIR).exists());
    }

    #[test]
    fn refuses_a_repeated_ltex_and_a_default_slot_source() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.ktx2");
        let sources = [
            LandTextureSource {
                ltex_form_id: 0x100,
                diffuse: missing.clone(),
                normal: None,
            },
            LandTextureSource {
                ltex_form_id: 0x100,
                diffuse: missing.clone(),
                normal: None,
            },
        ];
        let error = build_land_texture_arrays(&sources, None, dir.path(), &ArrayOptions::default())
            .unwrap_err();
        assert!(format!("{error:#}").contains("listed twice"), "{error:#}");

        let sources = [LandTextureSource {
            ltex_form_id: DEFAULT_LAND_TEXTURE_FORM_ID,
            diffuse: missing,
            normal: None,
        }];
        let error = build_land_texture_arrays(&sources, None, dir.path(), &ArrayOptions::default())
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("the default slot"),
            "{error:#}"
        );
    }

    #[test]
    fn a_source_it_cannot_read_is_skipped_by_name_and_does_not_stop_the_build() {
        let dir = tempfile::tempdir().unwrap();
        let good = solid_bc1_fixture(dir.path(), "good.ktx2", 8, 4, RED);
        let missing = dir.path().join("missing.ktx2");
        let garbage = dir.path().join("garbage.ktx2");
        fs::write(&garbage, b"this is not a texture at all, just text").unwrap();
        let good_normal = solid_bc1_fixture(dir.path(), "good-normal.ktx2", 8, 4, BLUE);
        let sources = [
            LandTextureSource {
                ltex_form_id: 0x100,
                diffuse: good.clone(),
                normal: Some(garbage.clone()),
            },
            LandTextureSource {
                ltex_form_id: 0x200,
                diffuse: missing.clone(),
                normal: Some(good_normal),
            },
            LandTextureSource {
                ltex_form_id: 0x300,
                diffuse: garbage.clone(),
                normal: None,
            },
        ];
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap();

        // Entry 0 (white) and 0x100's diffuse; 0x100's normal, 0x200 (diffuse, and its good normal
        // with it) and 0x300 were skipped.
        let ids: Vec<u32> = table
            .entries
            .iter()
            .map(|entry| entry.ltex_form_id)
            .collect();
        assert_eq!(ids, [0, 0x100]);
        assert_eq!(table.entries[1].normal, None);
        assert_eq!(report.skipped.len(), 4, "{:?}", report.skipped);
        let skipped = |ltex_form_id: u32, usage: LandTextureUsage| {
            report
                .skipped
                .iter()
                .find(|skipped| skipped.ltex_form_id == ltex_form_id && skipped.usage == usage)
                .unwrap_or_else(|| panic!("{ltex_form_id:X} not skipped: {:?}", report.skipped))
        };
        assert!(
            skipped(0x100, LandTextureUsage::NormalLinear)
                .source
                .contains("garbage.ktx2")
        );
        assert!(
            skipped(0x200, LandTextureUsage::DiffuseSrgb)
                .source
                .contains("missing.ktx2")
        );
        assert!(
            skipped(0x300, LandTextureUsage::DiffuseSrgb)
                .reason
                .contains("not a KTX2 texture")
        );
        assert!(
            skipped(0x200, LandTextureUsage::NormalLinear)
                .reason
                .contains("its diffuse texture was skipped"),
            "a normal map whose diffuse is skipped is reported too"
        );
        let warnings = report.warnings();
        assert_eq!(
            warnings
                .iter()
                .filter(|line| line.contains("skipped, its cells fall back"))
                .count(),
            4,
            "{warnings:?}"
        );
        // The normal class got nothing, so there is no normal array.
        assert!(
            table
                .arrays
                .iter()
                .all(|array| array.usage == LandTextureUsage::DiffuseSrgb)
        );
        assert_no_staging(dir.path());
    }

    #[test]
    fn size_classes_decide_the_arrays_and_single_members_are_padded() {
        let dir = tempfile::tempdir().unwrap();
        let eight = solid_bc1_fixture(dir.path(), "eight.ktx2", 8, 4, RED);
        let sixteen = solid_bc1_fixture(dir.path(), "sixteen.ktx2", 16, 5, GREEN);
        let thirty_two = solid_bc1_fixture(dir.path(), "thirty-two.ktx2", 32, 6, BLUE);
        let sources = [
            LandTextureSource {
                ltex_form_id: 0x30,
                diffuse: thirty_two,
                normal: None,
            },
            LandTextureSource {
                ltex_form_id: 0x10,
                diffuse: eight,
                normal: None,
            },
            LandTextureSource {
                ltex_form_id: 0x20,
                diffuse: sixteen,
                normal: None,
            },
        ];
        let (table, report) =
            build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap();
        let paths: Vec<&str> = table
            .arrays
            .iter()
            .map(|array| array.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "terrain/land-diffuse-bc1-8.ktx2",
                "terrain/land-diffuse-bc1-16.ktx2",
                "terrain/land-diffuse-bc1-32.ktx2"
            ]
        );
        // The white default goes into the smallest class: white and 0x10 there, the others are
        // single members, padded to two layers.
        let layers: Vec<u32> = table.arrays.iter().map(|array| array.layers).collect();
        assert_eq!(layers, [2, 2, 2]);
        assert_eq!(table.entries[0].diffuse.array, 0);
        assert_eq!(table.entries[1].diffuse.array, 0);
        assert_eq!(table.entries[2].diffuse.array, 1);
        assert_eq!(table.entries[3].diffuse.array, 2);
        for (array, levels) in report.arrays.iter().zip([4, 5, 6]) {
            assert_eq!(array.levels, levels, "{}", array.path);
            let (header, data) = read_ktx2(&dir.path().join(&array.path));
            assert_eq!(header.level_count, levels);
            assert_eq!(header.layer_count, 2);
            if array.size != 8 {
                for level in &data {
                    let (first, second) = level.split_at(level.len() / 2);
                    assert_eq!(first, second, "{}: padding layer is a copy", array.path);
                }
            }
        }
        assert!(
            report
                .textures
                .iter()
                .skip(1)
                .all(|texture| texture.action == TextureAction::Copied),
            "{:?}",
            report.textures
        );
    }

    #[test]
    fn level_data_is_stored_smallest_mip_first_with_a_largest_first_index() {
        let dir = tempfile::tempdir().unwrap();
        let first = solid_bc1_fixture(dir.path(), "first.ktx2", 32, 6, RED);
        let second = solid_bc1_fixture(dir.path(), "second.ktx2", 32, 6, GREEN);
        let sources = [
            LandTextureSource {
                ltex_form_id: 0x10,
                diffuse: first,
                normal: None,
            },
            LandTextureSource {
                ltex_form_id: 0x20,
                diffuse: second,
                normal: None,
            },
        ];
        let (table, _) = build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap();
        let bytes = fs::read(dir.path().join(&table.arrays[0].path)).unwrap();
        let reader = ktx2::Reader::new(&bytes).unwrap();
        let header = reader.header();
        assert_eq!(header.level_count, 6);
        let dfd_end = (header.index.dfd_byte_offset + header.index.dfd_byte_length) as u64;

        // Read the level index straight from the file: byteOffset, byteLength, uncompressed.
        let entry = |level: usize| {
            let start = ktx2::Header::LENGTH + level * ktx2::LevelIndex::LENGTH;
            let word = |at: usize| {
                u64::from_le_bytes(bytes[start + at..start + at + 8].try_into().unwrap())
            };
            (word(0), word(8), word(16))
        };
        let entries: Vec<(u64, u64, u64)> = (0..6).map(entry).collect();
        // Uncompressed sizes still run largest first, level 0 first (the last two levels are a
        // single block each).
        for pair in entries.windows(2) {
            assert!(pair[0].2 >= pair[1].2, "{entries:?}");
        }
        assert!(entries[0].2 > entries[4].2);
        // The offsets increase from the last level to level 0: the smallest mip comes first in
        // the file, and the first level data starts after the descriptor.
        // Supercompressed levels are packed with no gaps (KTX2 alignment 1): each starts exactly
        // where the previous one ends, and the first starts right after the descriptor.
        for level in (0..5).rev() {
            let (offset, length, _) = entries[level + 1];
            assert_eq!(
                entries[level].0,
                offset + length,
                "level {level} must start where level {} ends",
                level + 1
            );
        }
        assert_eq!(entries[5].0, dfd_end);
        for (offset, length, _) in &entries {
            assert!(offset + length <= bytes.len() as u64);
        }
        let (last_offset, last_length, _) = entries[0];
        assert_eq!(
            last_offset + last_length,
            bytes.len() as u64,
            "no trailing bytes"
        );
        // And the reader still pairs every index entry with its own data.
        let (_, levels) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        assert_eq!(
            levels[0].len(),
            3 * 8 * 8 * 8,
            "three layers of 8 x 8 blocks"
        );
        assert_eq!(levels[5].len(), 3 * 8, "white, red and green 1 x 1 blocks");
    }

    #[test]
    fn a_second_build_replaces_the_first_output_and_leaves_no_staging_behind() {
        let dir = tempfile::tempdir().unwrap();
        let red = solid_bc1_fixture(dir.path(), "red.ktx2", 8, 4, RED);
        let green = solid_bc1_fixture(dir.path(), "green.ktx2", 8, 4, GREEN);
        let build = |diffuse: &Path| {
            let sources = [LandTextureSource {
                ltex_form_id: 0x10,
                diffuse: diffuse.to_path_buf(),
                normal: None,
            }];
            build_land_texture_arrays(&sources, None, dir.path(), &tiny()).unwrap()
        };
        let _first = build(&red);
        let (table, _) = build(&green);
        assert_no_staging(dir.path());
        let written: LandTextureTable =
            serde_json::from_slice(&fs::read(dir.path().join(LAND_TEXTURE_TABLE_PATH)).unwrap())
                .unwrap();
        assert_eq!(written, table);
        let (_, levels) = read_ktx2(&dir.path().join(&table.arrays[0].path));
        let blocks = layer_slice(&levels, 0, 2, 1);
        let image = decode_blocks(ktx2::Format::BC1_RGBA_SRGB_BLOCK, 8, 8, blocks).unwrap();
        assert!(
            near(centre(&image), [0, 255, 0], 12),
            "the second build's colour, not the first's: {:?}",
            centre(&image)
        );
    }
}
