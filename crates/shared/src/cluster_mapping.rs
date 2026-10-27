//! The reference mapping of the cluster store (`mudcrab.clusters`, wrapper v1): which placed
//! reference and which primitive each instance record of the object blocks came from, and, for
//! every reference the converter examined, exactly which primitives of its model are clustered,
//! rejected or not covered.
//!
//! Three tables, all owned by the base extension and hashed with it:
//! * `cluster_provenance`: one row binding the mapping to the core dataset (`built_from`, the core
//!   `lod_build.build_identity`), to the cluster builder identity, to the versions of the map, the
//!   coverage and the input digest, and to the scope the classifier examined.
//! * `cluster_instance_map`: one row per record of every object block in `cluster_chunks`, keyed by
//!   the block's logical identity (worldspace, tier, anchor) and the record's final zero-based
//!   ordinal, which comes from the writer's real sort order (`write_mci_ordered`); the block's
//!   content hash is carried and checked against the chunk row.
//! * `cluster_reference_coverage`: for every examined reference exactly one reference-scope row
//!   stating what is known (`expanded`: the model resolved and primitive rows enumerate the
//!   expected set; `unknown`: the model could not be resolved, so the set is unknown; or
//!   `not_eligible`: refused as a whole, with the reason), and, only under `expanded`, one
//!   primitive-scope row per expected primitive of the resolved model, `clustered` or `rejected`
//!   with the reason. The expected set comes from the source model, never from what was emitted. A
//!   reference with no row is unknown, never complete.
//!
//! Terrain blocks are not placed references and are not in `cluster_chunks`; their instance files
//! belong to the terrain extension and never get rows here. Reference ids are form ids of one
//! dataset and mean something only against `built_from`; `InstanceRecord::stable_id` is untouched.
//! One placed reference contributes exactly one record per primitive: that is the invariant the
//! unique `cluster_instance_map_source` index enforces, and a format that needed several pieces per
//! primitive would need a new map version.
//!
//! Rows alone never mean "draw-ready": that is a per-view state of the runtime's payloads and
//! pipelines. The mapping only says what the converter intended and can prove.

use crate::cluster::{canonical_json, hex, parse_hash, read_mci};
use rusqlite::{Connection, OptionalExtension};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt, fs,
    path::Path,
};

/// `cluster_provenance.instance_map_version`, declared in `cluster-manifest.json`.
pub const INSTANCE_MAP_VERSION: u32 = 1;
/// `cluster_provenance.coverage_version`, declared in `cluster-manifest.json`.
pub const COVERAGE_VERSION: u32 = 1;
/// `cluster_provenance.digest_version`: the canonical input schema of [`input_digest`].
pub const DIGEST_VERSION: u32 = 1;

pub const CLUSTER_MAPPING_DDL: &str = "
CREATE TABLE IF NOT EXISTS cluster_provenance (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    instance_map_version INTEGER NOT NULL CHECK (instance_map_version = 1),
    coverage_version INTEGER NOT NULL CHECK (coverage_version = 1),
    digest_version INTEGER NOT NULL CHECK (digest_version = 1),
    digest_algorithm TEXT NOT NULL CHECK (digest_algorithm = 'sha256'),
    built_from TEXT NOT NULL CHECK (length(built_from) = 64 AND built_from NOT GLOB '*[^0-9a-f]*'),
    builder_identity TEXT NOT NULL
        CHECK (length(builder_identity) = 64 AND builder_identity NOT GLOB '*[^0-9a-f]*'),
    classifier_scope TEXT NOT NULL CHECK (classifier_scope <> '')
);
CREATE TABLE IF NOT EXISTS cluster_instance_map (
    worldspace_id INTEGER NOT NULL, tier INTEGER NOT NULL,
    anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
    chunk_content_hash TEXT NOT NULL
        CHECK (length(chunk_content_hash) = 64 AND chunk_content_hash NOT GLOB '*[^0-9a-f]*'),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    reference_id INTEGER NOT NULL CHECK (reference_id > 0),
    placed_worldspace_id INTEGER NOT NULL CHECK (placed_worldspace_id > 0),
    cell_x INTEGER NOT NULL, cell_y INTEGER NOT NULL,
    model_hash TEXT NOT NULL CHECK (length(model_hash) = 64 AND model_hash NOT GLOB '*[^0-9a-f]*'),
    primitive_index INTEGER NOT NULL CHECK (primitive_index >= 0),
    input_digest TEXT NOT NULL
        CHECK (length(input_digest) = 64 AND input_digest NOT GLOB '*[^0-9a-f]*'),
    PRIMARY KEY (worldspace_id, tier, anchor_x, anchor_y, ordinal)
) WITHOUT ROWID;
CREATE UNIQUE INDEX IF NOT EXISTS cluster_instance_map_source
    ON cluster_instance_map(reference_id, model_hash, primitive_index);
CREATE TABLE IF NOT EXISTS cluster_reference_coverage (
    reference_id INTEGER NOT NULL CHECK (reference_id > 0),
    placed_worldspace_id INTEGER NOT NULL CHECK (placed_worldspace_id > 0),
    source_cell_id INTEGER NOT NULL CHECK (source_cell_id >= 0),
    scope TEXT NOT NULL CHECK (scope IN ('reference', 'primitive')),
    model_hash TEXT NOT NULL
        CHECK (model_hash = '' OR (length(model_hash) = 64 AND model_hash NOT GLOB '*[^0-9a-f]*')),
    primitive_index INTEGER NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('expanded', 'unknown', 'not_eligible', 'clustered', 'rejected')),
    reason TEXT NOT NULL DEFAULT '',
    input_digest TEXT NOT NULL
        CHECK (length(input_digest) = 64 AND input_digest NOT GLOB '*[^0-9a-f]*'),
    PRIMARY KEY (reference_id, scope, model_hash, primitive_index),
    CHECK ((scope = 'reference') = (primitive_index = -1)),
    CHECK (scope = 'reference' OR (model_hash <> '' AND primitive_index >= 0)),
    CHECK (scope = 'reference' OR status IN ('clustered', 'rejected')),
    CHECK (scope = 'primitive' OR status IN ('expanded', 'unknown', 'not_eligible')),
    CHECK ((status IN ('clustered', 'expanded')) = (reason = '')),
    CHECK (status <> 'expanded' OR model_hash <> ''),
    CHECK (status <> 'unknown' OR model_hash = '')
) WITHOUT ROWID;
CREATE UNIQUE INDEX IF NOT EXISTS cluster_reference_coverage_state
    ON cluster_reference_coverage(reference_id) WHERE scope = 'reference';
";

/// The resolved inputs the converter used for one reference. Every row of the mapping and of the
/// coverage for that reference carries the digest of these, so a row built from other inputs is
/// recognisable as stale.
#[derive(Debug, Clone, Copy)]
pub struct ReferenceInputs<'a> {
    pub reference_id: u32,
    pub base_form_id: u32,
    pub record_type: &'a str,
    pub placed_worldspace_id: u32,
    /// The cell record that owns the reference in the core database, which is not necessarily the
    /// cell its position lies in (persistent references).
    pub source_cell_id: u32,
    pub model_path: Option<&'a str>,
    /// Content hash of the converted GLB the model path resolved to.
    pub model_hash: Option<&'a str>,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
    pub header_flags: u32,
    pub enable_parent_id: Option<u32>,
    pub snow_material: u32,
    /// SHA-256 (hex) of the reference's raw subrecord bytes in the core database, so any
    /// override carried there, resolved or not, changes the digest.
    pub data_hash: Option<&'a str>,
    /// SHA-256 (hex) of the base record's raw subrecord bytes (alternate textures, material
    /// object and the rest of what the winning base record says).
    pub base_record_hash: Option<&'a str>,
    /// SHA-256 (hex) over the sorted (path, content hash) pairs of every converted texture the
    /// model's materials use: the GLB hash names its texture paths, not their bytes.
    pub texture_set_hash: Option<&'a str>,
}

/// SHA-256 (hex) of the canonical JSON of [`ReferenceInputs`] under [`DIGEST_VERSION`], floats as
/// their bit patterns.
pub fn input_digest(inputs: &ReferenceInputs<'_>) -> String {
    let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let value = json!({
        "digest_version": DIGEST_VERSION,
        "reference_id": inputs.reference_id,
        "base_form_id": inputs.base_form_id,
        "record_type": inputs.record_type,
        "placed_worldspace_id": inputs.placed_worldspace_id,
        "source_cell_id": inputs.source_cell_id,
        "model_path": inputs.model_path,
        "model_hash": inputs.model_hash,
        "position": bits(&inputs.position),
        "rotation": bits(&inputs.rotation),
        "scale": inputs.scale.to_bits(),
        "header_flags": inputs.header_flags,
        "enable_parent_id": inputs.enable_parent_id,
        "snow_material": inputs.snow_material,
        "data_hash": inputs.data_hash,
        "base_record_hash": inputs.base_record_hash,
        "texture_set_hash": inputs.texture_set_hash,
    });
    hex(&Sha256::digest(canonical_json(&value)))
}

/// What the coverage table says about one reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageState {
    /// No row: the converter did not look at it, or this package predates the mapping. Unknown.
    Absent,
    /// Refused as a whole (classifier rule, unresolved override or input). Stays on the stock path.
    NotEligible,
    /// The model could not be resolved, so the expected primitive set is unknown. Stock path.
    Unknown,
    /// The model resolved; some expected primitives are rejected or none is clustered. Hand off per
    /// primitive only where mapped.
    Partial,
    /// The model resolved and every expected primitive is clustered.
    Complete,
}

/// The coverage state of one reference. Only meaningful after [`validate_mapping`] passed.
pub fn coverage_state(
    connection: &Connection,
    reference_id: u32,
) -> rusqlite::Result<CoverageState> {
    let status: Option<String> = connection
        .query_row(
            "SELECT status FROM cluster_reference_coverage
             WHERE reference_id = ?1 AND scope = 'reference'",
            [reference_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match status.as_deref() {
        None => CoverageState::Absent,
        Some("not_eligible") => CoverageState::NotEligible,
        Some("unknown") => CoverageState::Unknown,
        Some(_) => {
            let (rows, clustered): (i64, Option<i64>) = connection.query_row(
                "SELECT COUNT(*), SUM(status = 'clustered') FROM cluster_reference_coverage
                 WHERE reference_id = ?1 AND scope = 'primitive'",
                [reference_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if rows > 0 && clustered.unwrap_or(0) == rows {
                CoverageState::Complete
            } else {
                CoverageState::Partial
            }
        }
    })
}

/// Why a mapping failed validation. Any of these turns the mapping off for the package: nothing
/// may be suppressed or handed off from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingError {
    NoProvenance,
    UnsupportedVersion {
        map: u32,
        coverage: u32,
    },
    BadBuiltFrom,
    /// `built_from` differs from the core dataset the caller holds.
    StaleBuiltFrom,
    /// The provenance builder identity differs from `cluster_build`.
    StaleBuilderIdentity,
    /// A map row names a block (worldspace/tier/anchor) that `cluster_chunks` does not list.
    UnknownChunk(String),
    /// A terrain instance file is listed as an object block.
    TerrainInObjectChunks(String),
    /// The `.mci` header disagrees with its `cluster_chunks` row (worldspace, tier, anchor,
    /// instance count) or was built with another builder identity than the provenance names.
    ChunkHeaderMismatch(String),
    /// The `.mci` of a block cannot be read or is invalid.
    ChunkFile(String),
    /// The `.mci` bytes do not hash to the chunk's content hash, or a map row carries another
    /// hash than its chunk row.
    ChunkHashMismatch(String),
    /// A block's map rows are not exactly ordinals 0..instance_count.
    OrdinalGap {
        chunk: String,
        expected: u32,
    },
    /// A map row disagrees with the record at its ordinal (`what` names the field).
    RecordMismatch {
        chunk: String,
        ordinal: u32,
        what: &'static str,
    },
    UnknownMesh {
        mesh_hash: String,
        primitive_index: u32,
    },
    /// The same (reference, model, primitive) is mapped twice.
    DuplicateInstance(u32),
    /// An instance has no `clustered` coverage row.
    CoverageMissing(u32),
    /// A `clustered` coverage row has no instance.
    CoverageWithoutInstance(u32),
    /// A reference has primitive or map rows but no reference-scope row.
    ReferenceStateMissing(u32),
    /// Primitive rows under a reference that is not `expanded`, or for another model than the one
    /// it expanded to.
    PrimitiveRowsWithoutExpansion(u32),
    /// A reference has both instances and a whole-reference `unknown`/`not_eligible` state.
    ContradictoryCoverage(u32),
    /// A reference's rows for its model do not list the primitives of the model that
    /// `cluster_meshes` and `cluster_rejects` know (a sanity check only).
    IncompleteExpectedSet(u32),
    /// Rows of one reference carry different input digests.
    DigestMismatch(u32),
    /// Rows of one reference name different placing worldspaces or source cells.
    WorldspaceMismatch(u32),
    /// A `rejected` coverage row does not match `cluster_rejects`.
    RejectMismatch {
        mesh_hash: String,
        primitive_index: u32,
    },
    /// The authority knows no such reference.
    UnknownReference(u32),
    /// The authority examined this reference but the coverage has no row for it (`in_scope`), or
    /// the coverage has a row for a reference nobody examined (`!in_scope`).
    ScopeMismatch {
        reference_id: u32,
        in_scope: bool,
    },
    /// The reference's input digest differs from the one the authority recomputed.
    DigestNotAuthoritative(u32),
    /// The resolved model differs from the authority's (or the authority resolves a model the
    /// package claims unknown).
    ModelMismatch(u32),
    /// An instance record's transform differs from the authority's for that reference.
    TransformMismatch(u32),
    /// A typed field of the package (`what` names it) differs from the authority's snapshot of the
    /// reference: base record, snow material, placing worldspace, owning cell or grid cell. The
    /// digest alone never vouches for these; the stored rows and records are compared field by
    /// field.
    SnapshotMismatch {
        reference_id: u32,
        what: &'static str,
    },
    /// The coverage state or reason of a reference differs from the authority's verdict.
    EligibilityMismatch(u32),
    /// The provenance `classifier_scope` is not the scope the authority examined.
    ScopeDeclarationMismatch,
    /// A mesh row's material key differs from the one the authority derives from the source GLB
    /// at its canonical path.
    MaterialMismatch {
        mesh_hash: String,
        primitive_index: u32,
    },
    /// A reference's rows for its model do not list the primitives the authority says the model
    /// has.
    ExpectedPrimitivesMismatch {
        reference_id: u32,
        mesh_hash: String,
    },
    Database(String),
}

impl fmt::Display for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cluster mapping invalid: {self:?}")
    }
}

impl std::error::Error for MappingError {}

impl From<rusqlite::Error> for MappingError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error.to_string())
    }
}

/// Reference subrecords treated as unresolved overrides: `XMSP` (material swap), `XALP` (alpha),
/// `XEMI` (emittance). A MINIMUM, not a certified exhaustive list; written into the provenance
/// `classifier_scope`. The parser that finds them in a record is the converter's.
pub const MINIMUM_REFERENCE_OVERRIDE_TAGS: [&str; 3] = ["XMSP", "XALP", "XEMI"];
/// Material interpretation shared with the stock converted-GLB path. Change when either resolver changes.
pub const MATERIAL_RESOLVER: &str = "stock_original_materials/1";

/// Coverage `reason` values the classifier and the guards write, so a reader can name them.
/// A reference with a guarded override is refused as `not_eligible` with this prefix and the tag.
pub const REASON_UNRESOLVED_REFERENCE_OVERRIDE: &str = "unresolved_reference_override:";
/// The reference's data could not be parsed: nothing is known, so `unknown`.
pub const REASON_UNPARSED_REFERENCE_DATA: &str = "unparsed_reference_data";
/// The base record could not be parsed: `unknown`.
pub const REASON_UNPARSED_BASE_RECORD: &str = "unparsed_base_record";
/// The converted model could not be read: `unknown`.
pub const REASON_MODEL_NOT_LOADED: &str = "model_not_loaded";
/// The conversion published no GLB for the model path: `unknown`.
pub const REASON_MODEL_NOT_CONVERTED: &str = "model_not_converted";
/// The published GLB's bytes no longer hash to the manifest's content hash: `unknown`.
pub const REASON_MODEL_CONTENT_CHANGED: &str = "model_content_changed";
/// The model's GLB names a texture the conversion did not publish: `unknown`.
pub const REASON_TEXTURE_NOT_CONVERTED: &str = "texture_not_converted";
/// The model's bytes are shared with a GLB in another folder whose textures resolve differently,
/// so the one mesh payload cannot carry both materials: the later path is `unknown`.
pub const REASON_MODEL_MATERIAL_PATH_CONFLICT: &str = "model_material_path_conflict";
/// A published texture file is missing or unreadable: `unknown`.
pub const REASON_TEXTURE_NOT_LOADED: &str = "texture_not_loaded";
/// A published texture's bytes no longer hash to the manifest's content hash: `unknown`.
pub const REASON_TEXTURE_CONTENT_CHANGED: &str = "texture_content_changed";

/// The authority's verdict on whether the cluster path may draw a reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eligibility {
    /// Every input is resolved and no guard refuses it: the model is known and expandable.
    Eligible,
    /// Refused as a whole, with the `cluster_reference_coverage.reason` the package must carry.
    NotEligible(String),
    /// An input is unknown or unparsed (the model, the override list, the base record); the
    /// package must say `unknown` with this reason, never `expanded`.
    Unknown(String),
}

/// What the converter's own inputs say about one reference, as typed fields. The mapping tables
/// are compared against every field, not only against the digest.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceSnapshot {
    pub base_form_id: u32,
    pub snow_material: u32,
    /// The worldspace that places the reference (the child worldspace for pulled-in children).
    pub placed_worldspace_id: u32,
    /// The cell record that owns the reference in the core database.
    pub source_cell_id: u32,
    /// The grid cell the position lies in; the instance records carry it.
    pub cell: (i32, i32),
    /// The object block the reference belongs to: (block worldspace, tier, anchor x, anchor y),
    /// from the block worldspace's LOD origin and the cell. A child worldspace's reference joins
    /// its parent's block grid, so this is not the placing worldspace.
    pub block: (u32, u32, i32, i32),
    /// [`input_digest`] of the inputs the snapshot was built from.
    pub digest: String,
    /// The content hash of the converted model; `None` when it cannot be resolved.
    pub model: Option<String>,
    /// The 3x4 transform the instance records must carry, in the payload's axes.
    pub transform: Option<[f32; 12]>,
    pub eligibility: Eligibility,
}

/// What a `cluster_meshes` row must say about where its material comes from, derived from the
/// source GLB at the canonical path: the GLB the reader loads, the material slot in it, the key of
/// the material with its textures resolved, and the render flags the reader takes from the row.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialSource {
    /// The canonical published GLB path (`cluster_meshes.model_path`).
    pub model_path: String,
    /// The primitive's material slot in that GLB; `None` when it has none.
    pub material_index: Option<u32>,
    pub material_key: String,
    /// `alpha_mode` is `mask` when set, `opaque` otherwise (blended primitives are not meshes).
    pub alpha_mask: bool,
    pub alpha_cutoff: Option<f32>,
    pub double_sided: bool,
}

/// What the converter's inputs say, independent of anything the cluster stage emitted. The
/// mapping tables are the stage's claims; a claim is only proof of coverage when it matches this.
/// The converter implements it over the core database and the resolved GLBs; the caller of
/// [`validate_mapping`] supplies it.
pub trait AuthoritativeInputs {
    /// The current non-empty core `lod_build.build_identity` of the dataset (the caller reads it
    /// with `shared::extensions::core_build_identity`). Provenance `built_from` must equal it.
    /// `None` when the dataset has no `lod_build` row, which certifies nothing.
    fn core_build_identity(&self) -> Option<String>;
    /// The builder identity (hex) the current converter derives from its own sources and
    /// configuration. Provenance and every payload header must carry it.
    fn builder_identity(&self) -> String;
    /// The snapshot of one reference, recomputed from the core database and the resolved inputs;
    /// `None` when the dataset has no such reference.
    fn reference(&self, reference_id: u32) -> Option<ReferenceSnapshot>;
    /// Optional borrowed form of an already computed snapshot. The default retains the
    /// original owned lookup; implementations must return the same facts as `reference`.
    fn reference_view(&self, _reference_id: u32) -> Option<&ReferenceSnapshot> {
        None
    }
    /// The canonical JSON of the scope the converter examined (worldspaces, child worldspaces,
    /// base record types, area, guarded tag lists). Provenance `classifier_scope` must equal it,
    /// so the declared scope is the examined one.
    fn classifier_scope(&self) -> String;
    /// The LOD origin (grid cell) of a worldspace in the core database: terrain blocks anchor to
    /// it; `None` when the worldspace has none.
    fn lod_origin(&self, worldspace_id: u32) -> Option<(i32, i32)>;
    /// Every primitive index of the model with this content hash, taken from the GLB itself and
    /// not from the cluster tables; `None` when the model is unknown.
    fn model_primitives(&self, model_hash: &str) -> Option<BTreeSet<u32>>;
    /// The material key of every primitive of the model with this content hash, derived from its
    /// GLB at the canonical path (the first published path of that content): the hash of the
    /// material with its texture references resolved against that path. `None` when unknown.
    fn model_materials(&self, model_hash: &str) -> Option<BTreeMap<u32, MaterialSource>>;
    /// Every reference the converter examined in the scope the package declares. The coverage
    /// must have a reference row for exactly these: a reference that was not asked about is not in
    /// the scope, and one that was asked about is never silently absent.
    fn examined_references(&self) -> BTreeSet<u32>;
}

/// An [`AuthoritativeInputs`] over fixed tables, for fixtures and for callers that precompute.
#[derive(Debug, Clone, Default)]
pub struct FixedAuthority {
    pub core_build_identity: Option<String>,
    pub builder_identity: String,
    pub classifier_scope: String,
    pub references: HashMap<u32, ReferenceSnapshot>,
    pub lod_origins: HashMap<u32, (i32, i32)>,
    pub models: HashMap<String, BTreeSet<u32>>,
    pub materials: HashMap<String, BTreeMap<u32, MaterialSource>>,
}

impl AuthoritativeInputs for FixedAuthority {
    fn core_build_identity(&self) -> Option<String> {
        self.core_build_identity.clone()
    }

    fn builder_identity(&self) -> String {
        self.builder_identity.clone()
    }

    fn reference(&self, reference_id: u32) -> Option<ReferenceSnapshot> {
        self.references.get(&reference_id).cloned()
    }

    fn reference_view(&self, reference_id: u32) -> Option<&ReferenceSnapshot> {
        self.references.get(&reference_id)
    }

    fn classifier_scope(&self) -> String {
        self.classifier_scope.clone()
    }

    fn lod_origin(&self, worldspace_id: u32) -> Option<(i32, i32)> {
        self.lod_origins.get(&worldspace_id).copied()
    }

    fn model_primitives(&self, model_hash: &str) -> Option<BTreeSet<u32>> {
        self.models.get(model_hash).cloned()
    }

    fn model_materials(&self, model_hash: &str) -> Option<BTreeMap<u32, MaterialSource>> {
        self.materials.get(model_hash).cloned()
    }

    fn examined_references(&self) -> BTreeSet<u32> {
        self.references.keys().copied().collect()
    }
}

/// One instance record of an object block, as read back during validation.
#[derive(Debug, Clone)]
pub struct MappedInstance {
    /// The original record ordinal within its object block.
    pub ordinal: u32,
    pub reference_id: u32,
    pub model_hash: String,
    pub primitive_index: u32,
    pub transform: [f32; 12],
    /// Read from the record itself, not from the map row.
    pub base_form_id: u32,
    pub snow_material: u32,
    pub cell: (i32, i32),
    pub placed_worldspace_id: u32,
    /// The logical block of the `.mci` that holds the record: (worldspace, tier, anchor x, y).
    pub block: (u32, u32, i32, i32),
}

/// One reference as the coverage table states it.
#[derive(Default)]
struct CoverageGroup {
    /// Atypical rows retain the original runtime SQL projection instead of reusing this group.
    projection_disallowed: bool,
    /// Status, model and reason of the reference-scope row.
    state: Option<(String, String, String)>,
    /// (model, primitive, status, reason) of the primitive-scope rows.
    primitives: Vec<(String, u32, String, String)>,
    /// (placed worldspace, source cell, digest) shared by every row of the reference.
    identity: Option<(u32, u32, String)>,
}

/// Freshly checked package structure, not yet authorized by source inputs. Retain the dataset
/// source lock and use the same dataset connection/root through the consuming validation call.
/// This opaque value must never authorize runtime ownership by itself or survive a launch.
pub struct PreparedMapping<'a> {
    connection: &'a Connection,
    instances: Vec<MappedInstance>,
    groups: BTreeMap<u32, CoverageGroup>,
}

/// Runtime coverage facts projected only from fully validated, SQL-equivalent coverage rows.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedCoverage {
    pub state: CoverageState,
    pub unclustered_primitives: BTreeSet<u32>,
}

/// Checked artifacts from this launch. `None` coverage requires the original runtime SQL reads;
/// it never changes acceptance or grants missing/unknown references any coverage.
pub struct ValidatedMapping {
    pub instances: Vec<MappedInstance>,
    pub coverage: Option<BTreeMap<u32, ValidatedCoverage>>,
}

/// Performs every original structural check serially, retaining compact checked instances and
/// coverage groups so independent source preparation can overlap under the caller's source lock.
pub fn prepare_mapping<'a>(
    connection: &'a Connection,
    package: &Path,
    expected_built_from: Option<&str>,
) -> Result<PreparedMapping<'a>, MappingError> {
    let (instances, groups) =
        check_structure_impl(connection, package, expected_built_from, false)?;
    Ok(PreparedMapping {
        connection,
        instances,
        groups,
    })
}

/// Consumes launch-local prepared structure, then performs every original provenance, scope,
/// reference, material and instance authority check in its original refusal order.
pub fn validate_prepared_mapping_parallel(
    prepared: PreparedMapping<'_>,
    authority: &(dyn AuthoritativeInputs + Sync),
) -> Result<ValidatedMapping, MappingError> {
    validate_mapping_authority(
        prepared.connection,
        &prepared.instances,
        &prepared.groups,
        authority,
        Some(authority),
    )?;
    let coverage = project_validated_coverage(prepared.connection, prepared.groups);
    Ok(ValidatedMapping {
        instances: prepared.instances,
        coverage,
    })
}

/// Validates the mapping against the tables, the `.mci` files and the authority: everything
/// [`validate_mapping_structure`] checks, then that every reference's digest, resolved model,
/// transform and expected primitive set are what the converter's own inputs say. Only after this
/// passes does `Complete` in [`coverage_state`] mean the converter covered every primitive.
pub fn validate_mapping(
    connection: &Connection,
    package: &Path,
    expected_built_from: Option<&str>,
    authority: &dyn AuthoritativeInputs,
) -> Result<(), MappingError> {
    validate_mapping_impl(connection, package, expected_built_from, authority, None)
}

/// Full mapping certification using a freshly populated immutable authority on scoped workers.
/// It performs the same checks and reports the same first refusal as [`validate_mapping`].
pub fn validate_mapping_parallel(
    connection: &Connection,
    package: &Path,
    expected_built_from: Option<&str>,
    authority: &(dyn AuthoritativeInputs + Sync),
) -> Result<(), MappingError> {
    validate_mapping_impl(
        connection,
        package,
        expected_built_from,
        authority,
        Some(authority),
    )
}

fn validate_mapping_impl(
    connection: &Connection,
    package: &Path,
    expected_built_from: Option<&str>,
    authority: &dyn AuthoritativeInputs,
    parallel_authority: Option<&(dyn AuthoritativeInputs + Sync)>,
) -> Result<(), MappingError> {
    let (instances, groups) = check_structure_impl(
        connection,
        package,
        expected_built_from,
        parallel_authority.is_some(),
    )?;
    validate_mapping_authority(
        connection,
        &instances,
        &groups,
        authority,
        parallel_authority,
    )
}

fn validate_mapping_authority(
    connection: &Connection,
    instances: &[MappedInstance],
    groups: &BTreeMap<u32, CoverageGroup>,
    authority: &dyn AuthoritativeInputs,
    parallel_authority: Option<&(dyn AuthoritativeInputs + Sync)>,
) -> Result<(), MappingError> {
    let (provenance_built_from, provenance_builder, provenance_scope): (String, String, String) =
        connection.query_row(
            "SELECT built_from, builder_identity, classifier_scope FROM cluster_provenance WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    match authority.core_build_identity() {
        Some(core) if !core.is_empty() && core == provenance_built_from => {}
        _ => return Err(MappingError::StaleBuiltFrom),
    }
    if provenance_builder != authority.builder_identity() {
        return Err(MappingError::StaleBuilderIdentity);
    }
    if provenance_scope != authority.classifier_scope() {
        return Err(MappingError::ScopeDeclarationMismatch);
    }
    let examined = authority.examined_references();
    for reference_id in groups.keys() {
        if !examined.contains(reference_id) {
            return Err(MappingError::ScopeMismatch {
                reference_id: *reference_id,
                in_scope: false,
            });
        }
    }
    if let Some(reference_id) = examined.iter().find(|id| !groups.contains_key(id)) {
        return Err(MappingError::ScopeMismatch {
            reference_id: *reference_id,
            in_scope: true,
        });
    }
    // A model's primitive set is invariant for this authority. Resolve it once rather than
    // cloning the same set and acquiring the model-catalog lock for every placed reference.
    let mut primitives = HashMap::new();
    for group in groups.values() {
        if let Some((status, model, _)) = &group.state
            && status == "expanded"
        {
            primitives
                .entry(model.as_str())
                .or_insert_with(|| authority.model_primitives(model));
        }
    }
    let references: Vec<_> = groups.iter().collect();
    let checked = if let Some(authority) = parallel_authority {
        parallel_ordered(&references, |&(reference_id, group)| {
            check_reference(authority, *reference_id, group, &primitives)
        })?
    } else {
        references
            .iter()
            .map(|&(reference_id, group)| {
                check_reference(authority, *reference_id, group, &primitives)
            })
            .collect()
    };
    let materials = read_material_rows(connection);
    let mut checked_materials = HashSet::new();
    for result in checked {
        if let Some(model) = result?
            && checked_materials.insert(model)
        {
            match &materials {
                Ok(rows) => check_material_rows(authority, model, rows.get(model))?,
                // Preserve the original SQL refusal and its position if the bulk read fails.
                Err(_) => check_materials(connection, authority, model)?,
            }
        }
    }
    if let Some(authority) = parallel_authority {
        for result in parallel_ordered(instances, |instance| check_instance(authority, instance))? {
            result?;
        }
    } else {
        for instance in instances {
            check_instance(authority, instance)?;
        }
    }
    Ok(())
}

fn project_validated_coverage(
    connection: &Connection,
    groups: BTreeMap<u32, CoverageGroup>,
) -> Option<BTreeMap<u32, ValidatedCoverage>> {
    // Row-group comparisons are Rust byte comparisons. Reject views, collated schemas and
    // temporary shadows rather than assuming they match the original runtime SQL WHERE/SUM.
    // The coverage table lives in the overlay database: `clusters` when the engine attaches it to the
    // base, `main` when a stage writes it. `locate_object` refuses a copy in two databases and a
    // temporary shadow, which the original single-schema query excluded the same way.
    let schema =
        crate::extensions::locate_object(connection, "table", "cluster_reference_coverage")
            .ok()??;
    let definition: String = connection
        .query_row(
            &format!(
                "SELECT sql FROM {}.sqlite_master
         WHERE type='table' AND name='cluster_reference_coverage'",
                crate::extensions::quote_identifier(&schema)
            ),
            [],
            |row| row.get(0),
        )
        .ok()?;
    let definition = definition.trim_start().to_ascii_uppercase();
    if !definition.starts_with("CREATE TABLE") || definition.contains("COLLATE") {
        return None;
    }
    if groups.values().any(|group| group.projection_disallowed) {
        return None;
    }
    groups
        .into_iter()
        .map(|(reference, group)| {
            let (status, _, _) = group.state?;
            let state = match status.as_str() {
                "not_eligible" => CoverageState::NotEligible,
                "unknown" => CoverageState::Unknown,
                "expanded"
                    if !group.primitives.is_empty()
                        && group
                            .primitives
                            .iter()
                            .all(|(_, _, status, _)| status == "clustered") =>
                {
                    CoverageState::Complete
                }
                "expanded" => CoverageState::Partial,
                _ => return None,
            };
            let unclustered_primitives = group
                .primitives
                .into_iter()
                .filter_map(|(_, primitive, status, _)| {
                    (status != "clustered").then_some(primitive)
                })
                .collect();
            Some((
                reference,
                ValidatedCoverage {
                    state,
                    unclustered_primitives,
                },
            ))
        })
        .collect()
}

fn check_reference<'a>(
    authority: &dyn AuthoritativeInputs,
    reference_id: u32,
    group: &'a CoverageGroup,
    primitives: &HashMap<&str, Option<BTreeSet<u32>>>,
) -> Result<Option<&'a str>, MappingError> {
    let Some((placed, source_cell, digest)) = &group.identity else {
        return Ok(None);
    };
    let snapshot = authority
        .reference_view(reference_id)
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| {
            authority
                .reference(reference_id)
                .map(std::borrow::Cow::Owned)
        })
        .ok_or(MappingError::UnknownReference(reference_id))?;
    if snapshot.digest != *digest {
        return Err(MappingError::DigestNotAuthoritative(reference_id));
    }
    let mismatch = |what| MappingError::SnapshotMismatch { reference_id, what };
    if snapshot.placed_worldspace_id != *placed {
        return Err(mismatch("placed_worldspace"));
    }
    if snapshot.source_cell_id != *source_cell {
        return Err(mismatch("source_cell"));
    }
    let Some((status, model, reason)) = &group.state else {
        return Ok(None);
    };
    // The verdict decides the state: nothing the authority refuses or cannot resolve is ever
    // `expanded`, so it can never reach `Complete`.
    match (&snapshot.eligibility, status.as_str()) {
        (Eligibility::Eligible, "expanded") => {
            if snapshot.model.as_ref() != Some(model) {
                return Err(MappingError::ModelMismatch(reference_id));
            }
            let expected = primitives.get(model.as_str()).and_then(Option::as_ref);
            // Keep set semantics (including duplicate-row tolerance) and O(P log P)
            // work for mod models with many primitives. The expected set is cached per model.
            let listed: BTreeSet<_> = group
                .primitives
                .iter()
                .map(|(_, primitive, _, _)| *primitive)
                .collect();
            let same = expected == Some(&listed);
            if !same {
                return Err(MappingError::ExpectedPrimitivesMismatch {
                    reference_id,
                    mesh_hash: model.clone(),
                });
            }
            return Ok(Some(model.as_str()));
        }
        (Eligibility::NotEligible(expected), "not_eligible")
        | (Eligibility::Unknown(expected), "unknown")
            if expected == reason => {}
        _ => return Err(MappingError::EligibilityMismatch(reference_id)),
    }
    Ok(None)
}

fn check_instance(
    authority: &dyn AuthoritativeInputs,
    instance: &MappedInstance,
) -> Result<(), MappingError> {
    let reference_id = instance.reference_id;
    let snapshot = authority
        .reference_view(reference_id)
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| {
            authority
                .reference(reference_id)
                .map(std::borrow::Cow::Owned)
        })
        .ok_or(MappingError::UnknownReference(reference_id))?;
    let mismatch = |what| MappingError::SnapshotMismatch { reference_id, what };
    let same_transform = snapshot
        .transform
        .is_some_and(|expected| expected.map(f32::to_bits) == instance.transform.map(f32::to_bits));
    if !same_transform {
        return Err(MappingError::TransformMismatch(reference_id));
    }
    if instance.base_form_id != snapshot.base_form_id {
        return Err(mismatch("base_form_id"));
    }
    if instance.snow_material != snapshot.snow_material {
        return Err(mismatch("snow_material"));
    }
    if instance.cell != snapshot.cell {
        return Err(mismatch("cell"));
    }
    if instance.placed_worldspace_id != snapshot.placed_worldspace_id {
        return Err(mismatch("placed_worldspace"));
    }
    if instance.block != snapshot.block {
        return Err(mismatch("block"));
    }
    Ok(())
}

/// Contiguous partitions keep the same first refusal as the sequential walk. Workers return
/// results instead of short-circuiting across partitions; the caller consumes them in order.
fn parallel_ordered<T: Sync, R: Send>(
    values: &[T],
    operation: impl Fn(&T) -> R + Sync,
) -> Result<Vec<R>, MappingError> {
    if values.is_empty() {
        return Ok(Vec::new());
    }
    let workers = std::thread::available_parallelism()
        .map_err(|error| MappingError::ChunkFile(format!("certification worker count: {error}")))?
        .get()
        .saturating_sub(2)
        .max(1)
        .min(values.len());
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        let mut spawn_error = None;
        for partition in values.chunks(values.len().div_ceil(workers)) {
            let operation = &operation;
            match std::thread::Builder::new()
                .name("cluster-mapping-certification".into())
                .spawn_scoped(scope, move || {
                    partition.iter().map(operation).collect::<Vec<_>>()
                }) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    spawn_error = Some(error);
                    break;
                }
            }
        }
        let mut output = Vec::with_capacity(values.len());
        let mut panicked = false;
        for handle in handles {
            match handle.join() {
                Ok(results) => output.extend(results),
                Err(_) => panicked = true,
            }
        }
        if let Some(error) = spawn_error {
            return Err(MappingError::ChunkFile(format!(
                "certification worker: {error}"
            )));
        }
        if panicked {
            return Err(MappingError::ChunkFile(
                "certification worker panicked".into(),
            ));
        }
        Ok(output)
    })
}

type MaterialRows = HashMap<String, Vec<(u32, MaterialSource)>>;

fn read_material_rows(connection: &Connection) -> Result<MaterialRows, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT mesh_hash, primitive_index, material_key, model_path, material_index, alpha_mode,
                alpha_cutoff, double_sided FROM cluster_meshes ORDER BY mesh_hash, primitive_index",
    )?;
    let mut rows = statement.query([])?;
    let mut models = MaterialRows::new();
    while let Some(row) = rows.next()? {
        let model: String = row.get(0)?;
        let source = (
            row.get(1)?,
            MaterialSource {
                material_key: row.get(2)?,
                model_path: row.get(3)?,
                material_index: row.get(4)?,
                alpha_mask: row.get::<_, String>(5)? == "mask",
                alpha_cutoff: row.get::<_, Option<f64>>(6)?.map(|value| value as f32),
                double_sided: row.get::<_, i64>(7)? != 0,
            },
        );
        models.entry(model).or_default().push(source);
    }
    Ok(models)
}

fn check_material_rows(
    authority: &dyn AuthoritativeInputs,
    model: &str,
    rows: Option<&Vec<(u32, MaterialSource)>>,
) -> Result<(), MappingError> {
    let expected = authority.model_materials(model);
    for (index, found) in rows.into_iter().flatten() {
        let same = expected
            .as_ref()
            .and_then(|sources| sources.get(index))
            .is_some_and(|source| {
                found.material_key == source.material_key
                    && found.model_path == source.model_path
                    && found.material_index == source.material_index
                    && found.alpha_mask == source.alpha_mask
                    && found.alpha_cutoff.map(f32::to_bits) == source.alpha_cutoff.map(f32::to_bits)
                    && found.double_sided == source.double_sided
            });
        if !same {
            return Err(MappingError::MaterialMismatch {
                mesh_hash: model.to_owned(),
                primitive_index: *index,
            });
        }
    }
    Ok(())
}

/// Every emitted mesh row of `model` names the material source the authority derives from the
/// source GLB at its canonical path: the model path the reader loads, the material slot in it, the
/// material key and the render flags the reader takes from the row.
fn check_materials(
    connection: &Connection,
    authority: &dyn AuthoritativeInputs,
    model: &str,
) -> Result<(), MappingError> {
    let expected = authority.model_materials(model);
    let mut statement = connection.prepare(
        "SELECT primitive_index, material_key, model_path, material_index, alpha_mode,
                alpha_cutoff, double_sided
         FROM cluster_meshes WHERE mesh_hash = ?1",
    )?;
    let mut rows = statement.query([model])?;
    while let Some(row) = rows.next()? {
        let index: u32 = row.get(0)?;
        let found = (
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<u32>>(3)?,
            row.get::<_, String>(4)? == "mask",
            row.get::<_, Option<f64>>(5)?
                .map(|cutoff| (cutoff as f32).to_bits()),
            row.get::<_, i64>(6)? != 0,
        );
        let same = expected
            .as_ref()
            .and_then(|sources| sources.get(&index))
            .is_some_and(|source| {
                found
                    == (
                        source.material_key.clone(),
                        source.model_path.clone(),
                        source.material_index,
                        source.alpha_mask,
                        source.alpha_cutoff.map(f32::to_bits),
                        source.double_sided,
                    )
            });
        if !same {
            return Err(MappingError::MaterialMismatch {
                mesh_hash: model.to_owned(),
                primitive_index: index,
            });
        }
    }
    Ok(())
}

/// Validates the internal consistency of a package's mapping: its tables against each other and
/// against its `.mci` files. This does not prove coverage, because every table checked here is
/// something the stage emitted; [`validate_mapping`] adds the authority. `package` is the folder
/// the payload paths are relative to; `expected_built_from` is the core dataset identity the
/// caller holds (`None` skips that comparison, e.g. for a standalone package).
pub fn validate_mapping_structure(
    connection: &Connection,
    package: &Path,
    expected_built_from: Option<&str>,
) -> Result<(), MappingError> {
    check_structure(connection, package, expected_built_from).map(|_| ())
}

fn read_coverage(connection: &Connection) -> Result<BTreeMap<u32, CoverageGroup>, MappingError> {
    let mut groups: BTreeMap<u32, CoverageGroup> = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT reference_id, placed_worldspace_id, source_cell_id, scope, model_hash,
                primitive_index, status, reason, input_digest
         FROM cluster_reference_coverage",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let reference_id: u32 = row.get(0)?;
        let identity: (u32, u32, String) = (row.get(1)?, row.get(2)?, row.get(8)?);
        let scope: String = row.get(3)?;
        let (model, primitive, status, reason): (String, i64, String, String) =
            (row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?);
        let group = groups.entry(reference_id).or_default();
        match &group.identity {
            Some(known) if known.0 != identity.0 || known.1 != identity.1 => {
                return Err(MappingError::WorldspaceMismatch(reference_id));
            }
            Some(known) if known.2 != identity.2 => {
                return Err(MappingError::DigestMismatch(reference_id));
            }
            Some(_) => {}
            None => group.identity = Some(identity),
        }
        if scope == "reference" {
            group.projection_disallowed |= group.state.is_some()
                || !matches!(status.as_str(), "expanded" | "unknown" | "not_eligible");
            group.state = Some((status, model, reason));
        } else {
            group.projection_disallowed |= scope != "primitive"
                || u32::try_from(primitive).is_err()
                || !matches!(status.as_str(), "clustered" | "rejected");
            group
                .primitives
                .push((model, primitive as u32, status, reason));
        }
    }
    Ok(groups)
}

struct InstanceMapRow {
    ordinal: Result<u32, MappingError>,
    chunk_hash: Result<String, MappingError>,
    reference_id: Result<u32, MappingError>,
    placed_worldspace: Result<u32, MappingError>,
    cell_x: Result<i32, MappingError>,
    cell_y: Result<i32, MappingError>,
    model_hash: Result<String, MappingError>,
    primitive_index: Result<u32, MappingError>,
    digest: Result<String, MappingError>,
}

impl InstanceMapRow {
    fn read(row: &rusqlite::Row<'_>) -> Self {
        Self {
            ordinal: row.get(0).map_err(Into::into),
            chunk_hash: row.get(1).map_err(Into::into),
            reference_id: row.get(2).map_err(Into::into),
            placed_worldspace: row.get(3).map_err(Into::into),
            cell_x: row.get(4).map_err(Into::into),
            cell_y: row.get(5).map_err(Into::into),
            model_hash: row.get(6).map_err(Into::into),
            primitive_index: row.get(7).map_err(Into::into),
            digest: row.get(8).map_err(Into::into),
        }
    }
}

fn check_structure(
    connection: &Connection,
    package: &Path,
    expected_built_from: Option<&str>,
) -> Result<Vec<MappedInstance>, MappingError> {
    check_structure_impl(connection, package, expected_built_from, false)
        .map(|(instances, _)| instances)
}

fn check_structure_impl(
    connection: &Connection,
    package: &Path,
    expected_built_from: Option<&str>,
    parallel: bool,
) -> Result<(Vec<MappedInstance>, BTreeMap<u32, CoverageGroup>), MappingError> {
    let (map_version, coverage_version, built_from, builder_identity): (u32, u32, String, String) =
        connection
            .query_row(
                "SELECT instance_map_version, coverage_version, built_from, builder_identity
                 FROM cluster_provenance WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or(MappingError::NoProvenance)?;
    if (map_version, coverage_version) != (INSTANCE_MAP_VERSION, COVERAGE_VERSION) {
        return Err(MappingError::UnsupportedVersion {
            map: map_version,
            coverage: coverage_version,
        });
    }
    if parse_hash(&built_from).is_none() {
        return Err(MappingError::BadBuiltFrom);
    }
    if expected_built_from.is_some_and(|expected| expected != built_from) {
        return Err(MappingError::StaleBuiltFrom);
    }
    let build_identity: String = connection.query_row(
        "SELECT builder_identity FROM cluster_build WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if build_identity != builder_identity {
        return Err(MappingError::StaleBuilderIdentity);
    }
    let builder_bytes = parse_hash(&builder_identity).ok_or(MappingError::StaleBuilderIdentity)?;

    let mut meshes: HashSet<(String, u32)> = HashSet::new();
    let mut statement =
        connection.prepare("SELECT mesh_hash, primitive_index FROM cluster_meshes")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        meshes.insert((row.get(0)?, row.get(1)?));
    }
    let mut rejects: HashMap<(String, u32), String> = HashMap::new();
    let mut statement =
        connection.prepare("SELECT mesh_hash, primitive_index, reason FROM cluster_rejects")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        rejects.insert((row.get(0)?, row.get(1)?), row.get(2)?);
    }

    // Blocks: the file is what its hash says, its header what its row says, and its records are
    // what the map says, ordinal by ordinal.
    struct Chunk {
        worldspace: u32,
        tier: u32,
        anchor: (i32, i32),
        path: String,
        hash: String,
        count: u32,
    }
    let mut chunks = Vec::new();
    let mut statement = connection.prepare(
        "SELECT worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash, instance_count
         FROM cluster_chunks",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        chunks.push(Chunk {
            worldspace: row.get(0)?,
            tier: row.get(1)?,
            anchor: (row.get(2)?, row.get(3)?),
            path: row.get(4)?,
            hash: row.get(5)?,
            count: row.get(6)?,
        });
    }
    let read_chunk = |chunk: &Chunk| -> Result<Vec<u8>, MappingError> {
        // Payload names are portable relative paths, independent of the reader's OS.
        // Refuse aliases before joining: they can disguise terrain ownership or escape the package.
        if chunk.path.contains('\\')
            || chunk.path.contains(':')
            || chunk.path.chars().any(char::is_control)
            || chunk.path.split('/').any(|part| {
                matches!(part, "" | "." | "..") || part.ends_with('.') || part.ends_with(' ')
            })
        {
            return Err(MappingError::ChunkFile(format!(
                "{}: expected a canonical relative payload path",
                chunk.path
            )));
        }
        let mut parts = chunk.path.split('/');
        if parts
            .next()
            .is_some_and(|part| part.eq_ignore_ascii_case("clusters"))
            && parts
                .next()
                .is_some_and(|part| part.eq_ignore_ascii_case("terrain"))
        {
            return Err(MappingError::TerrainInObjectChunks(chunk.path.clone()));
        }
        let bytes = fs::read(package.join(&chunk.path))
            .map_err(|error| MappingError::ChunkFile(format!("{}: {error}", chunk.path)))?;
        if hex(&Sha256::digest(&bytes)) != chunk.hash {
            return Err(MappingError::ChunkHashMismatch(chunk.hash.clone()));
        }
        Ok(bytes)
    };
    // Bound retained payload buffers by CPU-sized batches, including for larger mod worlds.
    let chunk_batch_size = if parallel {
        std::thread::available_parallelism()
            .map_err(|error| {
                MappingError::ChunkFile(format!("certification worker count: {error}"))
            })?
            .get()
            .saturating_sub(2)
            .max(1)
    } else {
        1
    };
    let mut chunk_keys = HashSet::new();
    // reference -> (placed worldspace, digest); the set of (reference, model, primitive).
    let mut references: HashMap<u32, (u32, String)> = HashMap::new();
    let mut instances: HashSet<(u32, String, u32)> = HashSet::new();
    let mut mapped = Vec::new();
    let mut statement = connection.prepare(
        "SELECT ordinal, chunk_content_hash, reference_id, placed_worldspace_id, cell_x, cell_y,
                model_hash, primitive_index, input_digest
         FROM cluster_instance_map
         WHERE worldspace_id = ?1 AND tier = ?2 AND anchor_x = ?3 AND anchor_y = ?4
         ORDER BY ordinal",
    )?;
    for chunk_batch in chunks.chunks(chunk_batch_size) {
        // Every byte is freshly read and hashed; parsing consumes those exact same bytes.
        let mut verified_bytes = if parallel {
            Some(parallel_ordered(chunk_batch, read_chunk)?.into_iter())
        } else {
            None
        };
        for chunk in chunk_batch {
            let bytes = match &mut verified_bytes {
                Some(bytes) => bytes.next().ok_or_else(|| {
                    MappingError::ChunkFile("certification chunk result missing".into())
                })??,
                None => read_chunk(chunk)?,
            };
            let view = read_mci(&bytes)
                .map_err(|error| MappingError::ChunkFile(format!("{}: {error}", chunk.path)))?;
            if view.builder_identity != builder_bytes
                || (
                    view.worldspace_id,
                    view.tier,
                    view.anchor_x,
                    view.anchor_y,
                    view.instance_count,
                ) != (
                    chunk.worldspace,
                    chunk.tier,
                    chunk.anchor.0,
                    chunk.anchor.1,
                    chunk.count,
                )
            {
                return Err(MappingError::ChunkHeaderMismatch(chunk.hash.clone()));
            }
            chunk_keys.insert((chunk.worldspace, chunk.tier, chunk.anchor.0, chunk.anchor.1));
            let key = (chunk.worldspace, chunk.tier, chunk.anchor.0, chunk.anchor.1);
            // Only this chunk's SQL rows are retained. The indexed query also preserves the
            // original per-chunk and per-column refusal order for malformed packages.
            let mut rows = statement.query(rusqlite::params![key.0, key.1, key.2, key.3])?;
            let mesh_hashes: Vec<_> = (0..view.mesh_count as usize)
                .map(|index| hex(&view.mesh(index).mesh_hash))
                .collect();
            let mut expected = 0u32;
            while let Some(row) = rows.next()? {
                let row = InstanceMapRow::read(row);
                let ordinal = row.ordinal?;
                let mismatch = |what| MappingError::RecordMismatch {
                    chunk: chunk.hash.clone(),
                    ordinal,
                    what,
                };
                if ordinal != expected || expected >= view.instance_count {
                    return Err(MappingError::OrdinalGap {
                        chunk: chunk.hash.clone(),
                        expected,
                    });
                }
                if row.chunk_hash? != chunk.hash {
                    return Err(MappingError::ChunkHashMismatch(chunk.hash.clone()));
                }
                let record = view.instance(expected as usize);
                let mesh = view.mesh(record.mesh as usize);
                let model_hash = row.model_hash?;
                let primitive_index = row.primitive_index?;
                if model_hash != mesh_hashes[record.mesh as usize]
                    || primitive_index != mesh.primitive_index
                {
                    return Err(mismatch("mesh"));
                }
                if !meshes.contains(&(model_hash.clone(), primitive_index)) {
                    return Err(MappingError::UnknownMesh {
                        mesh_hash: model_hash,
                        primitive_index,
                    });
                }
                if (row.cell_x?, row.cell_y?) != (record.cell_x, record.cell_y) {
                    return Err(mismatch("cell"));
                }
                let placed = if record.source_worldspace_id != 0 {
                    record.source_worldspace_id
                } else {
                    view.worldspace_id
                };
                let placed_in_row = row.placed_worldspace?;
                if placed_in_row != placed {
                    return Err(mismatch("placed_worldspace"));
                }
                let reference_id = row.reference_id?;
                let digest = row.digest?;
                match references.get(&reference_id) {
                    Some((world, _)) if *world != placed => {
                        return Err(MappingError::WorldspaceMismatch(reference_id));
                    }
                    Some((_, known)) if *known != digest => {
                        return Err(MappingError::DigestMismatch(reference_id));
                    }
                    Some(_) => {}
                    None => {
                        references.insert(reference_id, (placed, digest));
                    }
                }
                if !instances.insert((reference_id, model_hash.clone(), primitive_index)) {
                    return Err(MappingError::DuplicateInstance(reference_id));
                }
                mapped.push(MappedInstance {
                    ordinal: expected,
                    reference_id,
                    model_hash,
                    primitive_index,
                    transform: record.transform,
                    base_form_id: record.base_form_id,
                    snow_material: record.snow_material,
                    cell: (record.cell_x, record.cell_y),
                    placed_worldspace_id: placed,
                    block: (chunk.worldspace, chunk.tier, chunk.anchor.0, chunk.anchor.1),
                });
                expected += 1;
            }
            if expected != view.instance_count {
                return Err(MappingError::OrdinalGap {
                    chunk: chunk.hash.clone(),
                    expected,
                });
            }
        }
    }
    let mut statement = connection.prepare(
        "SELECT DISTINCT worldspace_id, tier, anchor_x, anchor_y FROM cluster_instance_map",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let key: (u32, u32, i32, i32) = (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        if !chunk_keys.contains(&key) {
            return Err(MappingError::UnknownChunk(format!(
                "{}/{}/{}/{}",
                key.0, key.1, key.2, key.3
            )));
        }
    }

    // Coverage: one state per reference, primitive rows only under `expanded`, the clustered rows
    // are exactly the instances, and the rejected rows are exactly the recorded rejects.
    let mut unmatched = instances;
    let mut known: HashMap<&str, BTreeSet<u32>> = HashMap::new();
    for (hash, primitive) in meshes.iter().chain(rejects.keys()) {
        known.entry(hash).or_default().insert(*primitive);
    }
    let groups = read_coverage(connection)?;
    let no_primitives = BTreeSet::new();
    for (&reference_id, group) in &groups {
        let Some((placed, _, digest)) = &group.identity else {
            continue;
        };
        if let Some((world, known_digest)) = references.get(&reference_id) {
            if world != placed {
                return Err(MappingError::WorldspaceMismatch(reference_id));
            }
            if known_digest != digest {
                return Err(MappingError::DigestMismatch(reference_id));
            }
        }
        let Some((status, model, _)) = &group.state else {
            return Err(MappingError::ReferenceStateMissing(reference_id));
        };
        if status != "expanded" {
            if !group.primitives.is_empty() {
                return Err(MappingError::PrimitiveRowsWithoutExpansion(reference_id));
            }
            if references.contains_key(&reference_id) {
                return Err(MappingError::ContradictoryCoverage(reference_id));
            }
            continue;
        }
        let mut listed = BTreeSet::new();
        for (row_model, primitive, row_status, reason) in &group.primitives {
            if row_model != model {
                return Err(MappingError::PrimitiveRowsWithoutExpansion(reference_id));
            }
            listed.insert(*primitive);
            if row_status == "clustered" {
                let key = (reference_id, row_model.clone(), *primitive);
                if !unmatched.remove(&key) {
                    return Err(MappingError::CoverageWithoutInstance(reference_id));
                }
            } else if rejects.get(&(row_model.clone(), *primitive)) != Some(reason) {
                return Err(MappingError::RejectMismatch {
                    mesh_hash: row_model.clone(),
                    primitive_index: *primitive,
                });
            }
        }
        // A model with no drawable geometry (a mod's empty NIF that hides a vanilla object, say)
        // has no rows in either table: its expected set is empty, not unknown.
        if known.get(model.as_str()).unwrap_or(&no_primitives) != &listed {
            return Err(MappingError::IncompleteExpectedSet(reference_id));
        }
    }
    if let Some(reference_id) = references
        .keys()
        .filter(|id| !groups.contains_key(id))
        .min()
    {
        return Err(MappingError::ReferenceStateMissing(*reference_id));
    }
    if let Some((reference_id, _, _)) = unmatched.into_iter().min() {
        return Err(MappingError::CoverageMissing(reference_id));
    }
    Ok((mapped, groups))
}

#[cfg(test)]
mod parallel_tests {
    use super::*;
    use crate::cluster::{CLUSTER_TABLES_DDL, MciInputs, write_mci};
    use std::path::PathBuf;

    struct Fixture {
        connection: Connection,
        root: PathBuf,
        authority: FixedAuthority,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
            assert!(
                self.root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mudcrab-parallel-mapping-")
            );
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn fixture() -> Fixture {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mudcrab-parallel-mapping-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("OWNER.txt"),
            "owner: parallel shared mapping test\nclass: A, regenerable\n",
        )
        .unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(CLUSTER_TABLES_DDL).unwrap();
        connection.execute_batch(CLUSTER_MAPPING_DDL).unwrap();
        let core = "ab".repeat(32);
        let builder = "cd".repeat(32);
        connection
            .execute(
                "INSERT INTO cluster_build VALUES(1, ?1, 1, 16, 'cluster')",
                [&builder],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cluster_provenance VALUES(1,1,1,1,'sha256',?1,?2,'fixture-scope')",
                rusqlite::params![core, builder],
            )
            .unwrap();
        let mut authority = FixedAuthority {
            core_build_identity: Some(core),
            builder_identity: builder,
            classifier_scope: "fixture-scope".into(),
            ..Default::default()
        };
        for id in 1..=32 {
            let digest = format!("{id:064x}");
            connection.execute("INSERT INTO cluster_reference_coverage VALUES(?1,7,9,'reference','',-1,'unknown','fixture-input',?2)", rusqlite::params![id,digest]).unwrap();
            authority.references.insert(
                id,
                ReferenceSnapshot {
                    base_form_id: 11,
                    snow_material: 0,
                    placed_worldspace_id: 7,
                    source_cell_id: 9,
                    cell: (0, 0),
                    block: (7, 16, 0, 0),
                    digest,
                    model: None,
                    transform: None,
                    eligibility: Eligibility::Unknown("fixture-input".into()),
                },
            );
        }
        for anchor in 0..4 {
            let bytes = write_mci(&MciInputs {
                builder_identity: [0xcd; 32],
                worldspace_id: 7,
                tier: 16,
                anchor_x: anchor,
                anchor_y: 0,
                instances: &[],
            })
            .unwrap();
            let hash = hex(&Sha256::digest(&bytes));
            let path = format!("chunk-{anchor}.mci");
            fs::write(root.join(&path), bytes).unwrap();
            connection
                .execute(
                    "INSERT INTO cluster_chunks VALUES(7,16,?1,0,?2,?3,'input',0,0,0,0,0,0,0,'')",
                    rusqlite::params![anchor, path, hash],
                )
                .unwrap();
        }
        Fixture {
            connection,
            root,
            authority,
        }
    }

    fn sequential(fixture: &Fixture) -> Result<(), MappingError> {
        validate_mapping(&fixture.connection, &fixture.root, None, &fixture.authority)
    }

    fn parallel(fixture: &Fixture) -> Result<(), MappingError> {
        validate_mapping_parallel(&fixture.connection, &fixture.root, None, &fixture.authority)
    }

    fn mapped_fixture() -> Fixture {
        use crate::cluster::{InstanceInput, MeshRef, write_mci_ordered};
        let mut fixture = fixture();
        let model = "ef".repeat(32);
        let transform = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let ids = [2u32, 1];
        let instances: Vec<_> = ids
            .iter()
            .map(|&id| InstanceInput {
                mesh: MeshRef {
                    mesh_hash: [0xef; 32],
                    primitive_index: 0,
                },
                transform,
                cell_x: 0,
                cell_y: 0,
                stable_id: [id as u8; 16],
                base_form_id: 11,
                snow_material: 0,
                source_worldspace_id: 0,
                world_min: [0.0; 3],
                world_max: [1.0; 3],
            })
            .collect();
        let (bytes, order) = write_mci_ordered(&MciInputs {
            builder_identity: [0xcd; 32],
            worldspace_id: 7,
            tier: 16,
            anchor_x: 0,
            anchor_y: 0,
            instances: &instances,
        })
        .unwrap();
        let hash = hex(&Sha256::digest(&bytes));
        fs::write(fixture.root.join("chunk-0.mci"), bytes).unwrap();
        fixture
            .connection
            .execute(
                "UPDATE cluster_chunks SET content_hash=?1, instance_count=2 WHERE anchor_x=0",
                [&hash],
            )
            .unwrap();
        fixture.connection.execute("INSERT INTO cluster_meshes VALUES(?1,0,'payload','content',0,0,0,0,'fixture-material','meshes/fixture.glb',NULL,'opaque',NULL,0)", [&model]).unwrap();
        fixture
            .authority
            .models
            .insert(model.clone(), BTreeSet::from([0]));
        fixture.authority.materials.insert(
            model.clone(),
            BTreeMap::from([(
                0,
                MaterialSource {
                    model_path: "meshes/fixture.glb".into(),
                    material_index: None,
                    material_key: "fixture-material".into(),
                    alpha_mask: false,
                    alpha_cutoff: None,
                    double_sided: false,
                },
            )]),
        );
        fixture.authority.lod_origins.insert(7, (0, 0));
        for (ordinal, &input) in order.iter().enumerate() {
            let id = ids[input];
            let digest = format!("{id:064x}");
            let snapshot = fixture.authority.references.get_mut(&id).unwrap();
            snapshot.eligibility = Eligibility::Eligible;
            snapshot.model = Some(model.clone());
            snapshot.transform = Some(transform);
            fixture.connection.execute("UPDATE cluster_reference_coverage SET status='expanded', model_hash=?1, reason='' WHERE reference_id=?2", rusqlite::params![model,id]).unwrap();
            fixture.connection.execute("INSERT INTO cluster_reference_coverage VALUES(?1,7,9,'primitive',?2,0,'clustered','',?3)", rusqlite::params![id,model,digest]).unwrap();
            fixture
                .connection
                .execute(
                    "INSERT INTO cluster_instance_map VALUES(7,16,0,0,?1,?2,?3,7,0,0,?4,0,?5)",
                    rusqlite::params![hash, ordinal as u32, id, model, digest],
                )
                .unwrap();
        }
        fixture
    }

    fn prepared(fixture: &Fixture) -> Result<ValidatedMapping, MappingError> {
        let prepared = prepare_mapping(&fixture.connection, &fixture.root, None)?;
        validate_prepared_mapping_parallel(prepared, &fixture.authority)
    }

    #[test]
    fn object_mapping_refuses_terrain_payloads_even_with_valid_bytes_and_hashes() {
        for path in ["clusters/terrain/chunk.mci", "Clusters/Terrain/chunk.mci"] {
            let fixture = fixture();
            assert_eq!(sequential(&fixture), Ok(()));
            let target = fixture.root.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(fixture.root.join("chunk-0.mci"), target).unwrap();
            fixture
                .connection
                .execute(
                    "UPDATE cluster_chunks SET payload_path=?1 WHERE anchor_x=0",
                    [path],
                )
                .unwrap();
            let expected = Err(MappingError::TerrainInObjectChunks(path.into()));
            assert_eq!(sequential(&fixture), expected);
            assert_eq!(parallel(&fixture), expected);
            assert_eq!(prepared(&fixture).map(|_| ()), expected);
        }
    }

    #[test]
    fn object_mapping_refuses_noncanonical_payload_paths_before_reading_files() {
        for path in [
            r"clusters\terrain\chunk.mci",
            "clusters/./terrain/chunk.mci",
            "clusters/objects/../terrain/chunk.mci",
            "clusters//terrain/chunk.mci",
            "clusters/terrain./chunk.mci",
            "clusters/terrain /chunk.mci",
            "clusters/terrain/chunk.mci\0",
            "clusters/terrain/chunk.mci\n",
            "../outside.mci",
            "/outside.mci",
            "Q:/outside.mci",
            "Q:outside.mci",
            "clusters/objects/chunk.mci:stream",
            "",
        ] {
            let fixture = fixture();
            fixture
                .connection
                .execute(
                    "UPDATE cluster_chunks SET payload_path=?1 WHERE anchor_x=0",
                    [path],
                )
                .unwrap();
            let error = sequential(&fixture).unwrap_err();
            assert!(
                matches!(&error, MappingError::ChunkFile(reason)
                if reason.contains("canonical relative payload path")),
                "{path:?}: {error:?}"
            );
            assert_eq!(parallel(&fixture), Err(error.clone()));
            assert_eq!(prepared(&fixture).map(|_| ()), Err(error));
        }
    }

    #[test]
    fn prepared_validation_preserves_real_payload_ordinals_and_acceptance() {
        let fixture = mapped_fixture();
        assert_eq!(sequential(&fixture), Ok(()));
        assert_eq!(parallel(&fixture), Ok(()));
        let validated = prepared(&fixture).unwrap();
        assert_eq!(
            validated
                .instances
                .iter()
                .map(|instance| (instance.block, instance.ordinal, instance.reference_id,))
                .collect::<Vec<_>>(),
            vec![((7, 16, 0, 0), 0, 1), ((7, 16, 0, 0), 1, 2)]
        );
        assert!(validated.coverage.is_some());
    }

    #[test]
    fn prepared_validation_matches_every_original_authority_phase_and_first_refusal() {
        for attack in [
            "core",
            "builder",
            "scope",
            "examined",
            "digest",
            "material",
            "instance",
            "structure",
        ] {
            let mut fixture = mapped_fixture();
            match attack {
                "core" => fixture.authority.core_build_identity = None,
                "builder" => fixture.authority.builder_identity = "ab".repeat(32),
                "scope" => fixture.authority.classifier_scope = "other-scope".into(),
                "examined" => {
                    fixture.authority.references.remove(&32);
                }
                "digest" => {
                    fixture.authority.references.get_mut(&1).unwrap().digest = "wrong-first".into();
                    fixture
                        .authority
                        .references
                        .get_mut(&2)
                        .unwrap()
                        .placed_worldspace_id = 99;
                }
                "material" => {
                    fixture
                        .connection
                        .execute("UPDATE cluster_meshes SET material_key='wrong'", [])
                        .unwrap();
                }
                "instance" => {
                    fixture
                        .authority
                        .references
                        .get_mut(&2)
                        .unwrap()
                        .transform
                        .as_mut()
                        .unwrap()[0] = -0.0;
                }
                "structure" => {
                    fixture.authority.references.get_mut(&1).unwrap().digest =
                        "wrong-authority".into();
                    fs::write(fixture.root.join("chunk-0.mci"), b"broken payload").unwrap();
                }
                _ => unreachable!(),
            }
            let original = sequential(&fixture);
            assert!(original.is_err(), "attack={attack}");
            assert_eq!(parallel(&fixture), original, "parallel attack={attack}");
            assert_eq!(
                prepared(&fixture).map(|_| ()),
                original,
                "prepared attack={attack}"
            );
            if attack == "digest" {
                assert_eq!(original, Err(MappingError::DigestNotAuthoritative(1)));
            }
            if attack == "structure" {
                assert!(matches!(original, Err(MappingError::ChunkHashMismatch(_))));
            }
        }
    }

    #[test]
    fn validated_coverage_matches_original_sql_states_and_unclustered_primitives() {
        let mut fixture = mapped_fixture();
        let model = "ef".repeat(32);
        fixture.authority.models.get_mut(&model).unwrap().insert(1);
        fixture
            .connection
            .execute(
                "INSERT INTO cluster_rejects VALUES(?1,1,'fixture-rejected','')",
                [&model],
            )
            .unwrap();
        for id in [1u32, 2] {
            fixture.connection.execute("INSERT INTO cluster_reference_coverage VALUES(?1,7,9,'primitive',?2,1,'rejected','fixture-rejected',?3)", rusqlite::params![id,model,format!("{id:064x}")]).unwrap();
        }
        fixture
            .authority
            .references
            .get_mut(&3)
            .unwrap()
            .eligibility = Eligibility::NotEligible("fixture-input".into());
        fixture
            .connection
            .execute(
                "UPDATE cluster_reference_coverage SET status='not_eligible' WHERE reference_id=3",
                [],
            )
            .unwrap();
        assert_eq!(sequential(&fixture), Ok(()));
        let coverage = prepared(&fixture).unwrap().coverage.unwrap();
        for (&id, found) in &coverage {
            assert_eq!(
                found.state,
                coverage_state(&fixture.connection, id).unwrap()
            );
            let mut sql = fixture.connection.prepare("SELECT primitive_index FROM cluster_reference_coverage WHERE reference_id=?1 AND scope='primitive' AND status <> 'clustered'").unwrap();
            let expected: BTreeSet<u32> = sql
                .query_map([id], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(found.unclustered_primitives, expected);
        }
        assert_eq!(coverage[&1].state, CoverageState::Partial);
        assert_eq!(coverage[&3].state, CoverageState::NotEligible);
        assert_eq!(coverage[&4].state, CoverageState::Unknown);
    }

    fn loosen_coverage_schema(connection: &Connection, collated: bool) {
        connection
            .execute_batch("ALTER TABLE cluster_reference_coverage RENAME TO old_coverage")
            .unwrap();
        let collation = if collated { " COLLATE NOCASE" } else { "" };
        connection
            .execute_batch(&format!(
                "CREATE TABLE cluster_reference_coverage (
                reference_id INTEGER, placed_worldspace_id INTEGER, source_cell_id INTEGER,
                scope TEXT{collation}, model_hash TEXT, primitive_index INTEGER,
                status TEXT, reason TEXT, input_digest TEXT
             );
             INSERT INTO cluster_reference_coverage SELECT * FROM old_coverage;
             DROP TABLE old_coverage;"
            ))
            .unwrap();
    }

    #[test]
    fn accepted_duplicate_reference_rows_keep_original_runtime_sql_fallback() {
        let mut fixture = mapped_fixture();
        loosen_coverage_schema(&fixture.connection, false);
        fixture.connection.execute("INSERT INTO cluster_reference_coverage SELECT reference_id, placed_worldspace_id, source_cell_id, scope, model_hash, primitive_index, 'not_eligible', reason, input_digest FROM cluster_reference_coverage WHERE reference_id=3", []).unwrap();
        fixture
            .authority
            .references
            .get_mut(&3)
            .unwrap()
            .eligibility = Eligibility::NotEligible("fixture-input".into());
        assert_eq!(sequential(&fixture), Ok(()));
        assert_eq!(
            coverage_state(&fixture.connection, 3).unwrap(),
            CoverageState::Unknown
        );
        assert!(prepared(&fixture).unwrap().coverage.is_none());
    }

    #[test]
    fn accepted_collated_coverage_schema_keeps_original_runtime_sql_fallback() {
        let fixture = mapped_fixture();
        loosen_coverage_schema(&fixture.connection, true);
        assert_eq!(sequential(&fixture), Ok(()));
        assert!(prepared(&fixture).unwrap().coverage.is_none());
    }

    #[test]
    fn accepted_wrapped_primitive_indices_keep_original_runtime_sql_fallback() {
        let fixture = mapped_fixture();
        fixture.connection.execute("UPDATE cluster_reference_coverage SET primitive_index=4294967296 WHERE scope='primitive'", []).unwrap();
        // The original mapping recipe casts i64 to u32. Preserve its acceptance, while
        // declining a runtime projection whose original SQL consumers decode u32 directly.
        assert_eq!(sequential(&fixture), Ok(()));
        assert!(prepared(&fixture).unwrap().coverage.is_none());
    }

    #[test]
    fn accepted_noncanonical_scope_keeps_original_runtime_sql_fallback() {
        let fixture = mapped_fixture();
        loosen_coverage_schema(&fixture.connection, false);
        fixture
            .connection
            .execute(
                "UPDATE cluster_reference_coverage SET scope='other' WHERE scope='primitive'",
                [],
            )
            .unwrap();
        assert_eq!(sequential(&fixture), Ok(()));
        assert!(prepared(&fixture).unwrap().coverage.is_none());
    }

    #[test]
    fn parallel_full_validation_matches_sequential_acceptance_and_first_refusals() {
        let mut fixture = fixture();
        assert_eq!(sequential(&fixture), Ok(()));
        assert_eq!(parallel(&fixture), sequential(&fixture));
        fixture.authority.references.get_mut(&2).unwrap().digest = "changed".into();
        fixture
            .authority
            .references
            .get_mut(&31)
            .unwrap()
            .placed_worldspace_id = 99;
        assert_eq!(
            parallel(&fixture),
            Err(MappingError::DigestNotAuthoritative(2))
        );
        assert_eq!(parallel(&fixture), sequential(&fixture));
        fixture.authority.references.get_mut(&2).unwrap().digest = format!("{:064x}", 2);
        assert_eq!(parallel(&fixture), sequential(&fixture));
        assert_eq!(
            parallel(&fixture),
            Err(MappingError::SnapshotMismatch {
                reference_id: 31,
                what: "placed_worldspace"
            })
        );
        fs::write(fixture.root.join("chunk-1.mci"), b"corrupt first payload").unwrap();
        fs::write(fixture.root.join("chunk-3.mci"), b"corrupt later payload").unwrap();
        assert_eq!(parallel(&fixture), sequential(&fixture));
        assert!(matches!(
            parallel(&fixture),
            Err(MappingError::ChunkHashMismatch(_))
        ));
    }

    #[test]
    fn material_batch_reports_the_same_first_primitive_as_the_legacy_indexed_query() {
        let fixture = fixture();
        let model = "ef".repeat(32);
        for index in [2, 0] {
            fixture.connection.execute("INSERT INTO cluster_meshes VALUES(?1,?2,'payload','content',0,0,0,0,'wrong','meshes/model.glb',NULL,'opaque',NULL,0)", rusqlite::params![model,index]).unwrap();
        }
        let rows = read_material_rows(&fixture.connection).unwrap();
        let expected = Err(MappingError::MaterialMismatch {
            mesh_hash: model.clone(),
            primitive_index: 0,
        });
        assert_eq!(
            check_materials(&fixture.connection, &fixture.authority, &model),
            expected
        );
        assert_eq!(
            check_material_rows(&fixture.authority, &model, rows.get(&model)),
            expected
        );
    }

    #[test]
    fn parallel_instance_checks_merge_in_original_order() {
        let mut fixture = fixture();
        let transform = [0.0; 12];
        for snapshot in fixture.authority.references.values_mut() {
            snapshot.transform = Some(transform);
        }
        let mut instances: Vec<_> = (1..=32)
            .map(|reference_id| MappedInstance {
                ordinal: reference_id - 1,
                reference_id,
                model_hash: "ef".repeat(32),
                primitive_index: 0,
                transform,
                base_form_id: 11,
                snow_material: 0,
                cell: (0, 0),
                placed_worldspace_id: 7,
                block: (7, 16, 0, 0),
            })
            .collect();
        instances[3].transform[0] = -0.0;
        instances[22].cell = (9, 9);
        let sequential: Vec<_> = instances
            .iter()
            .map(|instance| check_instance(&fixture.authority, instance))
            .collect();
        let parallel = parallel_ordered(&instances, |instance| {
            check_instance(&fixture.authority, instance)
        })
        .unwrap();
        assert_eq!(parallel, sequential);
        assert_eq!(
            parallel.into_iter().find_map(Result::err),
            Some(MappingError::TransformMismatch(4))
        );
    }

    #[test]
    fn mapping_checks_resolve_the_coverage_table_in_whichever_schema_holds_it() {
        // The engine's shape: the base is `main`, the overlay's cluster tables are attached.
        let path = std::env::temp_dir().join(format!(
            "shared-mapping-overlay-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Connection::open(&path)
            .unwrap()
            .execute_batch(CLUSTER_MAPPING_DDL)
            .unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("ATTACH DATABASE ?1 AS clusters", [path.to_str().unwrap()])
            .unwrap();
        assert_eq!(
            project_validated_coverage(&connection, BTreeMap::new()),
            Some(BTreeMap::new())
        );
        // A temp view of the same name would answer the unqualified runtime SQL: no projection.
        connection
            .execute_batch("CREATE TEMP VIEW cluster_reference_coverage AS SELECT 1 AS x")
            .unwrap();
        assert!(project_validated_coverage(&connection, BTreeMap::new()).is_none());
        drop(connection);
        let _ = fs::remove_file(path);
    }
}
