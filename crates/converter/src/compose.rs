//! In-memory composition of the Skyrim body, hands and feet NIFs into one
//! skinned model that shares a single skeleton.

use crate::material::NifShapeMaterial;
use crate::mesh::{
    build_skinned_model_with_materials, rewrite_materials_and_texture_uris, write_glb_atomic,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use project_wormhole_nif::model::all::{BoneTree, Model};
use std::{
    fmt,
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
        f.write_str(match self {
            Self::Body => "body",
            Self::Hands => "hands",
            Self::Feet => "feet",
        })
    }
}

/// One NIF to merge into a composed actor body.
pub struct BodyPart<'a> {
    pub path: &'a Path,
    pub kind: BodyPartKind,
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

        for shape in build.material_contract {
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

/// Writes a composed model as a GLB using the same exporter, material publication and atomic
/// publish step as every other converted NIF.
pub fn write_composed_body(body: &ComposedBody, output: &Path) -> Result<()> {
    let name = body
        .model
        .name
        .clone()
        .unwrap_or_else(|| "composed_body".to_owned());
    let glb = body
        .model
        .to_glb(name)
        .map_err(|error| eyre!("composed body GLB export failed: {error}"))?;
    let glb = rewrite_materials_and_texture_uris(
        glb,
        &body.material_contract,
        &body.shape_blocks,
        output,
    )?;
    write_glb_atomic(output, &glb)
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
