//! The one terrain material every merged cell shares (#215).
//!
//! It binds the game-wide land texture arrays as static `texture_2d_array` bindings (one per
//! class the table lists, up to the contract's nine per usage), one sampler, the table that maps
//! a texture id to its class slot and layer, and one tiling constant. The shader is specialised
//! to exactly the listed classes: [`TerrainClassKey`] is the material's bind group data, so the
//! pipeline key carries it and `specialize` turns it into shader defs.
//!
//! The group-4 binding-array path is the later step. Its seam is this file's bindings and the
//! `sample_diffuse` / `sample_normal` helpers in `terrain_merged.wgsl`: the per-vertex ids, the
//! lookup table and the blend maths do not change.

use super::{
    classes::{MAX_CLASSES_PER_USAGE, TerrainClassKey},
    mesh::{
        SLOT_IDS_FIRST_LOCATION, SLOT_WEIGHTS_FIRST_LOCATION, TERRAIN_SLOT_IDS,
        TERRAIN_SLOT_WEIGHTS,
    },
};
use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    render::{
        render_resource::{
            AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
        },
        storage::ShaderBuffer,
    },
    shader::{ShaderDefVal, ShaderRef},
};
use shared::terrain_mesh::TERRAIN_UV_REPEATS_PER_CELL;

pub type MergedTerrainMaterial = ExtendedMaterial<StandardMaterial, MergedTerrainExtension>;

/// Where the shader is embedded (`embedded_asset!` in the plugin).
pub const MERGED_TERRAIN_SHADER: &str = "embedded://engine/terrain_merge/terrain_merged.wgsl";

#[derive(ShaderType, Reflect, Debug, Clone)]
pub struct MergedTerrainSettings {
    /// xy = land texture repeats per cell on each axis, from [`TERRAIN_UV_REPEATS_PER_CELL`].
    pub tiling: Vec4,
}

impl Default for MergedTerrainSettings {
    fn default() -> Self {
        Self {
            tiling: Vec4::new(
                TERRAIN_UV_REPEATS_PER_CELL,
                TERRAIN_UV_REPEATS_PER_CELL,
                0.0,
                0.0,
            ),
        }
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
#[bind_group_data(TerrainClassKey)]
pub struct MergedTerrainExtension {
    // Diffuse classes. The sampler is tied to slot 0, which a valid table always fills; every
    // image carries the same repeating sampler (`terrain_layer_sampler`).
    #[texture(100, dimension = "2d_array", visibility(fragment))]
    #[sampler(101, visibility(fragment))]
    pub diffuse_0: Option<Handle<Image>>,
    #[texture(102, dimension = "2d_array", visibility(fragment))]
    pub diffuse_1: Option<Handle<Image>>,
    #[texture(103, dimension = "2d_array", visibility(fragment))]
    pub diffuse_2: Option<Handle<Image>>,
    #[texture(104, dimension = "2d_array", visibility(fragment))]
    pub diffuse_3: Option<Handle<Image>>,
    #[texture(105, dimension = "2d_array", visibility(fragment))]
    pub diffuse_4: Option<Handle<Image>>,
    #[texture(106, dimension = "2d_array", visibility(fragment))]
    pub diffuse_5: Option<Handle<Image>>,
    #[texture(107, dimension = "2d_array", visibility(fragment))]
    pub diffuse_6: Option<Handle<Image>>,
    #[texture(108, dimension = "2d_array", visibility(fragment))]
    pub diffuse_7: Option<Handle<Image>>,
    #[texture(109, dimension = "2d_array", visibility(fragment))]
    pub diffuse_8: Option<Handle<Image>>,
    // Normal classes, sampled through the diffuse sampler.
    #[texture(110, dimension = "2d_array", visibility(fragment))]
    pub normal_0: Option<Handle<Image>>,
    #[texture(111, dimension = "2d_array", visibility(fragment))]
    pub normal_1: Option<Handle<Image>>,
    #[texture(112, dimension = "2d_array", visibility(fragment))]
    pub normal_2: Option<Handle<Image>>,
    #[texture(113, dimension = "2d_array", visibility(fragment))]
    pub normal_3: Option<Handle<Image>>,
    #[texture(114, dimension = "2d_array", visibility(fragment))]
    pub normal_4: Option<Handle<Image>>,
    #[texture(115, dimension = "2d_array", visibility(fragment))]
    pub normal_5: Option<Handle<Image>>,
    #[texture(116, dimension = "2d_array", visibility(fragment))]
    pub normal_6: Option<Handle<Image>>,
    #[texture(117, dimension = "2d_array", visibility(fragment))]
    pub normal_7: Option<Handle<Image>>,
    #[texture(118, dimension = "2d_array", visibility(fragment))]
    pub normal_8: Option<Handle<Image>>,
    /// The id to (diffuse word, normal word) table: two `u32`s per land texture id.
    #[storage(119, read_only, visibility(fragment))]
    pub entries: Handle<ShaderBuffer>,
    #[uniform(120, visibility(fragment))]
    pub settings: MergedTerrainSettings,
}

impl MergedTerrainExtension {
    /// The material of a table whose classes are `diffuse` and `normal` (at most
    /// [`MAX_CLASSES_PER_USAGE`] each, in slot order) and whose lookup table is `entries`. More
    /// classes than the material has slots is an error, not a panic: the counts come from a file.
    pub fn new(
        diffuse: &[Handle<Image>],
        normal: &[Handle<Image>],
        entries: Handle<ShaderBuffer>,
    ) -> Result<Self, String> {
        if diffuse.len() > MAX_CLASSES_PER_USAGE || normal.len() > MAX_CLASSES_PER_USAGE {
            return Err(format!(
                "{} diffuse and {} normal land texture classes, more than the {MAX_CLASSES_PER_USAGE} the terrain material binds per usage",
                diffuse.len(),
                normal.len()
            ));
        }
        let slot = |handles: &[Handle<Image>], index: usize| handles.get(index).cloned();
        Ok(Self {
            diffuse_0: slot(diffuse, 0),
            diffuse_1: slot(diffuse, 1),
            diffuse_2: slot(diffuse, 2),
            diffuse_3: slot(diffuse, 3),
            diffuse_4: slot(diffuse, 4),
            diffuse_5: slot(diffuse, 5),
            diffuse_6: slot(diffuse, 6),
            diffuse_7: slot(diffuse, 7),
            diffuse_8: slot(diffuse, 8),
            normal_0: slot(normal, 0),
            normal_1: slot(normal, 1),
            normal_2: slot(normal, 2),
            normal_3: slot(normal, 3),
            normal_4: slot(normal, 4),
            normal_5: slot(normal, 5),
            normal_6: slot(normal, 6),
            normal_7: slot(normal, 7),
            normal_8: slot(normal, 8),
            entries,
            settings: MergedTerrainSettings::default(),
        })
    }

    fn diffuse_slots(&self) -> [&Option<Handle<Image>>; MAX_CLASSES_PER_USAGE] {
        [
            &self.diffuse_0,
            &self.diffuse_1,
            &self.diffuse_2,
            &self.diffuse_3,
            &self.diffuse_4,
            &self.diffuse_5,
            &self.diffuse_6,
            &self.diffuse_7,
            &self.diffuse_8,
        ]
    }

    fn normal_slots(&self) -> [&Option<Handle<Image>>; MAX_CLASSES_PER_USAGE] {
        [
            &self.normal_0,
            &self.normal_1,
            &self.normal_2,
            &self.normal_3,
            &self.normal_4,
            &self.normal_5,
            &self.normal_6,
            &self.normal_7,
            &self.normal_8,
        ]
    }
}

impl From<&MergedTerrainExtension> for TerrainClassKey {
    fn from(extension: &MergedTerrainExtension) -> Self {
        let bound = |slots: [&Option<Handle<Image>>; MAX_CLASSES_PER_USAGE]| {
            slots.iter().filter(|slot| slot.is_some()).count() as u8
        };
        Self {
            diffuse_classes: bound(extension.diffuse_slots()),
            normal_classes: bound(extension.normal_slots()),
        }
    }
}

/// The shader defs that specialise `terrain_merged.wgsl`: one per bound class, and the slot
/// stride of the mesh being drawn. Pure, so the specialisation is testable without a device.
pub fn shader_defs(key: TerrainClassKey, wide_slots: bool) -> Vec<ShaderDefVal> {
    let mut defs: Vec<ShaderDefVal> = Vec::new();
    for class in 0..usize::from(key.diffuse_classes) {
        defs.push(format!("TERRAIN_DIFFUSE_CLASS_{class}").into());
    }
    for class in 0..usize::from(key.normal_classes) {
        defs.push(format!("TERRAIN_NORMAL_CLASS_{class}").into());
    }
    if key.normal_classes > 0 {
        defs.push("TERRAIN_HAS_NORMAL_CLASSES".into());
    }
    if wide_slots {
        defs.push("TERRAIN_SLOT_STRIDE_16".into());
    }
    defs
}

impl MaterialExtension for MergedTerrainExtension {
    /// The merged terrain stays out of the depth prepass. With it in, GPU occlusion culling (on the
    /// world camera) hid whole houses and stalls over merged cells that the quadrant path draws:
    /// the same Whiterun view, the same entities, but the buildings never reached the screen, and
    /// they came back with occlusion culling off or the terrain out of the prepass. The cause is
    /// not found yet; until it is, the terrain does not act as an occluder. Check a before/after
    /// shot with `--no-fog` before changing this.
    fn enable_prepass() -> bool {
        false
    }

    fn vertex_shader() -> ShaderRef {
        MERGED_TERRAIN_SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        MERGED_TERRAIN_SHADER.into()
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // A stride of 16 slots is a mesh with a third and fourth attribute of each family: two
        // vertex layouts, so two pipelines per pass, which is the accepted cost of the format.
        let wide_slots = layout.0.contains(TERRAIN_SLOT_IDS[2].id);
        let groups = if wide_slots { 4 } else { 2 };
        let mut wanted = Vec::with_capacity(2 * groups);
        for group in 0..groups {
            wanted.push(
                TERRAIN_SLOT_IDS[group].at_shader_location(SLOT_IDS_FIRST_LOCATION + group as u32),
            );
            wanted.push(
                TERRAIN_SLOT_WEIGHTS[group]
                    .at_shader_location(SLOT_WEIGHTS_FIRST_LOCATION + group as u32),
            );
        }
        // Bevy's own attributes are already in the buffer; the extra ones share its interleaved
        // layout and stride, so they are appended to it.
        let extra = layout.0.get_layout(&wanted)?;
        if let Some(buffer) = descriptor.vertex.buffers.first_mut() {
            buffer.attributes.extend(extra.attributes);
        }
        let defs = shader_defs(key.bind_group_data, wide_slots);
        descriptor.vertex.shader_defs.extend(defs.iter().cloned());
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.extend(defs);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_terrain_stays_out_of_the_depth_prepass() {
        assert!(
            !MergedTerrainExtension::enable_prepass(),
            "occlusion culling hid objects over merged cells while the terrain wrote the prepass"
        );
    }

    #[test]
    fn the_key_counts_the_bound_classes_per_usage() {
        let none = MergedTerrainExtension::new(&[], &[], Handle::default()).unwrap();
        assert_eq!(
            TerrainClassKey::from(&none),
            TerrainClassKey {
                diffuse_classes: 0,
                normal_classes: 0
            }
        );
        let images: Vec<Handle<Image>> = (0..4).map(|_| Handle::default()).collect();
        let some = MergedTerrainExtension::new(&images, &images[..2], Handle::default()).unwrap();
        assert_eq!(
            TerrainClassKey::from(&some),
            TerrainClassKey {
                diffuse_classes: 4,
                normal_classes: 2
            }
        );
    }

    #[test]
    fn too_many_classes_are_an_error_not_a_panic() {
        let images: Vec<Handle<Image>> = (0..=MAX_CLASSES_PER_USAGE)
            .map(|_| Handle::default())
            .collect();
        let error = MergedTerrainExtension::new(&images, &[], Handle::default()).unwrap_err();
        assert!(error.contains("10 diffuse and 0 normal"), "{error}");
        assert!(MergedTerrainExtension::new(&[], &images, Handle::default()).is_err());
        MergedTerrainExtension::new(&images[..MAX_CLASSES_PER_USAGE], &[], Handle::default())
            .unwrap();
    }

    #[test]
    fn shader_defs_name_exactly_the_bound_classes_and_the_stride() {
        let key = TerrainClassKey {
            diffuse_classes: 3,
            normal_classes: 1,
        };
        let names = |defs: Vec<ShaderDefVal>| -> Vec<String> {
            defs.into_iter()
                .map(|def| match def {
                    ShaderDefVal::Bool(name, true) => name,
                    other => panic!("unexpected def {other:?}"),
                })
                .collect()
        };
        assert_eq!(
            names(shader_defs(key, false)),
            [
                "TERRAIN_DIFFUSE_CLASS_0",
                "TERRAIN_DIFFUSE_CLASS_1",
                "TERRAIN_DIFFUSE_CLASS_2",
                "TERRAIN_NORMAL_CLASS_0",
                "TERRAIN_HAS_NORMAL_CLASSES"
            ]
        );
        assert!(names(shader_defs(key, true)).contains(&"TERRAIN_SLOT_STRIDE_16".to_owned()));
        let no_normals = TerrainClassKey {
            diffuse_classes: 1,
            normal_classes: 0,
        };
        assert_eq!(
            names(shader_defs(no_normals, false)),
            ["TERRAIN_DIFFUSE_CLASS_0"]
        );
    }
}
