//! In-memory composition of the Skyrim body, hands and feet NIFs into one
//! skinned model that shares a single skeleton.

use crate::actor_models::DEFAULT_RACE_FORM_ID;
use crate::material::{
    NifMaterialDisposition, NifShapeMaterial, NifSlotTextureSet, apply_skin_texture_set,
};
use crate::mesh::{
    build_skinned_model_with_materials, rewrite_materials_and_texture_uris, write_glb_atomic,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use project_wormhole_nif::model::all::{BoneTree, Model, MorphTarget, SkeletalMesh};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    fmt, fs,
    ops::Deref,
    path::{Path, PathBuf},
};

/// Which piece of the actor a part file supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyPartKind {
    Body,
    Hands,
    Feet,
}

impl fmt::Display for BodyPartKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slot())
    }
}

impl BodyPartKind {
    /// The `actor_model_parts.slot` string of the armature that supplies this part.
    #[must_use]
    pub const fn slot(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Hands => "hands",
            Self::Feet => "feet",
        }
    }
}

/// One NIF to merge into a composed actor body.
pub struct BodyPart<'a> {
    pub path: &'a Path,
    pub kind: BodyPartKind,
    /// The armature's skin texture set, applied over the NIF's `skin_tint` shapes the way the
    /// game applies `ARMA/NAM0`/`NAM1`. `None` keeps the NIF's own textures.
    pub skin_textures: Option<&'a NifSlotTextureSet>,
}

/// A `DefaultRace` armature's skin texture set for one sex and biped slot, read from the world
/// database's actor catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultSkinTextureSet {
    pub kind: BodyPartKind,
    /// The armature's `MOD2`/`MOD3` path, as the record wrote it.
    pub model_path: String,
    pub textures: NifSlotTextureSet,
}

/// The skin texture sets of the `DefaultRace` armatures, the ones the composed default body is
/// built from, for `sex` (`male` or `female`).
///
/// The catalog stores each `NPC_`'s armature parts with the armature's race and skin texture set;
/// the shared humanoid body is the one their `DefaultRace` armatures describe. Per slot, the set
/// most parts use wins, then the lowest FormID. A database without the catalog columns, or
/// without a catalog at all, yields no sets, and the composed body keeps the NIFs' textures.
pub fn default_skin_texture_sets(database: &Path, sex: &str) -> Result<Vec<DefaultSkinTextureSet>> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .wrap_err_with(|| format!("open {}", database.display()))?;
    let has_columns: bool = connection.query_row(
        "SELECT count(*) = 2 FROM pragma_table_info('actor_model_parts') \
         WHERE name IN ('armature_race_id', 'skin_texture_set_id')",
        [],
        |row| row.get(0),
    )?;
    let has_texture_sets: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'texture_sets')",
        [],
        |row| row.get(0),
    )?;
    if !has_columns || !has_texture_sets {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT p.slot, min(p.model_path), p.skin_texture_set_id, count(*) AS uses \
         FROM actor_model_parts p JOIN actor_models m ON m.id = p.npc_id \
         WHERE m.sex = ?1 AND p.armature_race_id = ?2 AND p.skin_texture_set_id IS NOT NULL \
         GROUP BY p.slot, lower(p.model_path), p.skin_texture_set_id \
         ORDER BY p.slot, uses DESC, p.skin_texture_set_id",
    )?;
    let candidates = statement
        .query_map(rusqlite::params![sex, DEFAULT_RACE_FORM_ID], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut sets = Vec::new();
    for kind in [BodyPartKind::Body, BodyPartKind::Hands, BodyPartKind::Feet] {
        let Some((_, model_path, texture_set_id)) =
            candidates.iter().find(|(slot, _, _)| slot == kind.slot())
        else {
            continue;
        };
        let Some(textures) = read_texture_set(&connection, *texture_set_id)? else {
            continue;
        };
        sets.push(DefaultSkinTextureSet {
            kind,
            model_path: model_path.clone(),
            textures,
        });
    }
    Ok(sets)
}

/// A `texture_sets` row in NIF slot order (see [`NifSlotTextureSet`]).
fn read_texture_set(connection: &Connection, id: u32) -> Result<Option<NifSlotTextureSet>> {
    let slots = connection
        .query_row(
            "SELECT diffuse_path, normal_path, glow_path, height_path, environment_path, \
             mask_path, detail_path, specular_path FROM texture_sets WHERE id = ?1",
            [id],
            |row| {
                let mut slots: [Option<String>; 8] = Default::default();
                for (index, slot) in slots.iter_mut().enumerate() {
                    *slot = row.get(index)?;
                }
                Ok(slots)
            },
        )
        .optional()?;
    Ok(slots.map(|slots| NifSlotTextureSet { form_id: id, slots }))
}

/// The set whose armature uses `part`'s NIF in `kind`'s slot. A set authored for another mesh
/// (a load order whose `DefaultRace` armature points elsewhere) does not apply.
#[must_use]
pub fn skin_texture_set_for<'a>(
    sets: &'a [DefaultSkinTextureSet],
    kind: BodyPartKind,
    part: &Path,
) -> Option<&'a NifSlotTextureSet> {
    let file_name = |path: &str| {
        path.rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let part_name = file_name(&part.to_string_lossy());
    sets.iter()
        .find(|set| set.kind == kind && file_name(&set.model_path) == part_name)
        .map(|set| &set.textures)
}

/// A composed body together with the material contract its exported meshes need.
#[derive(Debug)]
pub struct ComposedBody {
    model: Model,
    material_contract: Vec<NifShapeMaterial>,
    shape_blocks: Vec<u32>,
}

impl Deref for ComposedBody {
    type Target = Model;

    fn deref(&self) -> &Self::Target {
        &self.model
    }
}

/// Merges the given parts into one model whose skeletal meshes all share a
/// skeleton. Parts are built through the skinned export path, so a part that
/// declares a skin can never silently fall back to static geometry.
pub fn compose_body(parts: &[BodyPart<'_>], skeleton_path: &Path) -> Result<ComposedBody> {
    ensure!(
        !parts.is_empty(),
        "a composed body needs at least one body part"
    );
    let mut model = Model {
        name: Some("composed_body".to_owned()),
        ..Model::default()
    };
    let mut material_contract = Vec::new();
    let mut shape_blocks = Vec::new();
    let mut shared: Option<(BodyPartKind, &Path, BoneTree)> = None;

    for part in parts {
        let build =
            build_skinned_model_with_materials(part.path, skeleton_path).wrap_err_with(|| {
                format!(
                    "failed to compose {} part {}",
                    part.kind,
                    part.path.display()
                )
            })?;
        let part_model = build.model;
        ensure!(
            !part_model.skeletal_meshes.is_empty(),
            "{} part {} produced no skeletal mesh",
            part.kind,
            part.path.display()
        );

        for (mesh_index, mesh) in part_model.skeletal_meshes.iter().enumerate() {
            match &shared {
                Some((first_kind, first_path, bones)) if &mesh.bones != bones => {
                    return Err(eyre!(
                        "{} part {} skeletal mesh {mesh_index} has a bone tree that differs from {} part {}",
                        part.kind,
                        part.path.display(),
                        first_kind,
                        first_path.display(),
                    ));
                }
                Some(_) => {}
                None => shared = Some((part.kind, part.path, mesh.bones.clone())),
            }
        }

        let offset = u8::try_from(model.materials.len())
            .map_err(|_| eyre!("composed body has more than 255 materials"))?;
        for index in &part_model.material_indices {
            let shifted = index
                .checked_add(offset)
                .ok_or_else(|| eyre!("composed body has more than 255 materials"))?;
            model.material_indices.push(shifted);
        }
        model.skeletal_meshes.extend(part_model.skeletal_meshes);
        model.materials.extend(part_model.materials);

        for mut shape in build.material_contract {
            if let (Some(textures), NifMaterialDisposition::Validated { material }) =
                (part.skin_textures, &mut shape.disposition)
            {
                apply_skin_texture_set(material, textures).wrap_err_with(|| {
                    format!(
                        "failed to apply skin texture set {:08X} to {} part {}",
                        textures.form_id,
                        part.kind,
                        part.path.display()
                    )
                })?;
            }
            let block = u32::try_from(material_contract.len())
                .map_err(|_| eyre!("composed body has more than u32::MAX exported shapes"))?;
            material_contract.push(NifShapeMaterial {
                shape_block: block,
                ..shape
            });
            shape_blocks.push(block);
        }
    }

    ensure!(
        !model.skeletal_meshes.is_empty(),
        "composed body has no skeletal meshes"
    );
    ensure!(
        shape_blocks.len() == model.static_meshes.len() + model.skeletal_meshes.len(),
        "composed body exported {} shapes for {} meshes",
        shape_blocks.len(),
        model.static_meshes.len() + model.skeletal_meshes.len(),
    );
    model.scene_root_rotation = Some(shared::coordinates::CREATION_TO_RUNTIME_ROTATION);
    model
        .validate()
        .map_err(|error| eyre!("invalid composed body model: {error}"))?;
    Ok(ComposedBody {
        model,
        material_contract,
        shape_blocks,
    })
}

/// Renders a composed model as a GLB whose published materials and runtime texture URIs
/// address the KTX2 tree relative to `output`.
pub fn composed_body_glb(body: &ComposedBody, output: &Path) -> Result<Vec<u8>> {
    let name = body
        .model
        .name
        .clone()
        .unwrap_or_else(|| "composed_body".to_owned());
    let glb = body
        .model
        .to_glb(name)
        .map_err(|error| eyre!("composed body GLB export failed: {error}"))?;
    rewrite_materials_and_texture_uris(glb, &body.material_contract, &body.shape_blocks, output)
}

/// Whether the two meshes can share per-vertex morph deltas.
fn check_same_topology(thin: &SkeletalMesh, heavy: &SkeletalMesh) -> Option<&'static str> {
    let (Some(thin_mesh), Some(heavy_mesh)) = (&thin.mesh, &heavy.mesh) else {
        return Some("skeletal mesh geometry is missing");
    };
    if thin_mesh.positions.len() != heavy_mesh.positions.len() {
        return Some("skeletal mesh vertex counts differ");
    }
    (thin_mesh.triangles != heavy_mesh.triangles)
        .then_some("skeletal mesh triangle topology differs")
}

/// Result of checking the optional thin body against its heavy base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinTargetOutcome {
    /// Every mesh matched, and the target was attached.
    Attached,
    /// Mesh count, geometry, topology, or normals cannot support the target.
    Unavailable(&'static str),
}

/// Adds the thin composition to `heavy` as a `thin` morph target on every skeletal mesh.
///
/// Skinning, materials and texture sets stay on `heavy`, as the game keeps them at weight 100.
/// The target holds `thin - heavy` position and normal deltas, so morph weight 1 yields the thin
/// body and 0 yields today's heavy body. Nothing is changed when any mesh fails the topology
/// check.
pub fn attach_thin_target(
    heavy: &mut ComposedBody,
    thin: &ComposedBody,
) -> Result<ThinTargetOutcome> {
    if thin.model.skeletal_meshes.len() != heavy.model.skeletal_meshes.len() {
        return Ok(ThinTargetOutcome::Unavailable(
            "skeletal mesh counts differ",
        ));
    }

    let mut targets = Vec::with_capacity(thin.model.skeletal_meshes.len());
    for index in 0..thin.model.skeletal_meshes.len() {
        let thin_mesh = &thin.model.skeletal_meshes[index];
        let heavy_mesh = &heavy.model.skeletal_meshes[index];
        if let Some(reason) = check_same_topology(thin_mesh, heavy_mesh) {
            return Ok(ThinTargetOutcome::Unavailable(reason));
        }
        let (Some(thin_geometry), Some(heavy_geometry)) = (&thin_mesh.mesh, &heavy_mesh.mesh)
        else {
            return Ok(ThinTargetOutcome::Unavailable(
                "skeletal mesh geometry is missing",
            ));
        };
        if thin_geometry.normals.len() != thin_geometry.positions.len()
            || heavy_geometry.normals.len() != heavy_geometry.positions.len()
        {
            return Ok(ThinTargetOutcome::Unavailable(
                "per-vertex normals are missing or incomplete",
            ));
        }
        targets.push(MorphTarget {
            name: "thin".to_owned(),
            positions: thin_geometry
                .positions
                .iter()
                .zip(&heavy_geometry.positions)
                .map(|(thin, heavy)| *thin - *heavy)
                .collect(),
            normals: thin_geometry
                .normals
                .iter()
                .zip(&heavy_geometry.normals)
                .map(|(thin, heavy)| *thin - *heavy)
                .collect(),
        });
    }

    for (mesh, target) in heavy.model.skeletal_meshes.iter_mut().zip(targets) {
        mesh.morph_targets = vec![target];
    }
    heavy
        .model
        .validate()
        .map_err(|error| eyre!("invalid body with a thin morph target: {error}"))?;
    Ok(ThinTargetOutcome::Attached)
}

/// Writes a composed model as a GLB using the same exporter, material publication and atomic
/// publish step as every other converted NIF. Returns whether the file changed.
pub fn write_composed_body(body: &ComposedBody, output: &Path) -> Result<bool> {
    let bytes = composed_body_glb(body, output)?;
    if fs::read(output).is_ok_and(|existing| existing == bytes) {
        return Ok(false);
    }
    write_glb_atomic(output, &bytes)?;
    Ok(true)
}

/// VFS-relative output path for a composed actor body. The shared humanoid set
/// uses `default` as its race segment; race-specific templates are additive
/// later.
pub fn composed_body_path(race: &str, sex: &str) -> PathBuf {
    PathBuf::from("meshes/actors/character/composed")
        .join(race)
        .join(sex)
        .join("body.glb")
}

/// Whether a staging-relative, `/`-separated path is a composed actor body.
///
/// Composed bodies have no source NIF at their own path, so the staging
/// cleanups that drop outputs without a matching source must leave them alone.
pub fn is_composed_output(relative: &str) -> bool {
    relative.starts_with("meshes/actors/character/composed/") && relative.ends_with(".glb")
}
