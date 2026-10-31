//! Authored NIF depth state for surfaces drawn after opaque geometry.
use bevy::{
    ecs::system::SystemParamItem,
    mesh::MeshVertexBufferLayoutRef,
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    render::{
        render_resource::{
            AsBindGroup, AsBindGroupError, BindGroupLayout, BindGroupLayoutEntry, BindingResources,
            BindlessDescriptor, BindlessSlabResourceLimit, CompareFunction, DepthStencilState,
            RenderPipelineDescriptor, SpecializedMeshPipelineError, UnpreparedBindGroup,
        },
        renderer::RenderDevice,
    },
};

pub use shared::material_depth::{DECAL, DEPTH_TEST, DEPTH_WRITE, DYNAMIC_DECAL};
// SkyrimShaderPropertyFlags1, vendor/project-wormhole-nif/src/nif_flags.rs.
pub const CAST_SHADOWS: u32 = 1 << 9;

// A small raster offset in reversed-Z depth units, scoped to authored decals. This is a
// renderer policy tested on the GPU, not a recovered Skyrim numeric bias or a mesh offset.
pub const DECAL_DEPTH_BIAS: i32 = 4;
// Skyrim SE 1.7.104.0 raster modes 6/8 use -0.65 in forward Z. Reverse the sign
// for Bevy's reversed Z. Native viewport offsets and constant units depend on
// its camera/depth convention; those are not copied as numeric parity here.
// The native -100 clamp is not ported either: preserve the pipeline's clamp
// (normally 0, unclamped). Grazing-angle occlusion remains a validation gate.
pub const DECAL_SLOPE_BIAS: f32 = 0.65;

pub type NifDepthMaterial = ExtendedMaterial<StandardMaterial, NifDepthExtension>;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Reflect)]
pub struct NifDepthState {
    pub depth_test: bool,
    pub depth_write: bool,
    pub decal: bool,
    pub alpha_mask: bool,
    pub cast_shadows: bool,
}

impl NifDepthState {
    pub fn from_flags(flags_1: u32, flags_2: u32, alpha_mode: AlphaMode) -> Option<Self> {
        let depth_test = flags_1 & DEPTH_TEST != 0;
        let depth_write = depth_test && flags_2 & DEPTH_WRITE != 0;
        let decal = flags_1 & (DECAL | DYNAMIC_DECAL) != 0;
        // Stock blending already avoids depth writes. Ordinary depth-writing surfaces keep
        // their existing opaque/cutout/prepass path.
        if !shared::material_depth::requires_sorted_depth_pass(
            flags_1,
            flags_2,
            matches!(alpha_mode, AlphaMode::Blend),
        ) {
            return None;
        }
        Some(Self {
            depth_test,
            depth_write,
            decal,
            alpha_mask: matches!(alpha_mode, AlphaMode::Mask(_)),
            cast_shadows: flags_1 & CAST_SHADOWS != 0,
        })
    }

    pub fn apply(&self, depth: &mut DepthStencilState) {
        NifDepthRasterKey::from(*self).apply(depth);
    }

    pub fn excludes_shadow(&self) -> bool {
        !self.cast_shadows
    }
}

/// Camera raster specialization, independent of authored caster eligibility.
///
/// `NotShadowCaster` owns per-entity shadow inclusion. That flag does not alter
/// the material pipeline and must not split otherwise identical sorted runs.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Reflect)]
pub struct NifDepthRasterKey {
    pub depth_test: bool,
    pub depth_write: bool,
    pub decal: bool,
    pub alpha_mask: bool,
}

impl From<NifDepthState> for NifDepthRasterKey {
    fn from(state: NifDepthState) -> Self {
        Self {
            depth_test: state.depth_test,
            depth_write: state.depth_write,
            decal: state.decal,
            alpha_mask: state.alpha_mask,
        }
    }
}

impl NifDepthRasterKey {
    pub fn apply(&self, depth: &mut DepthStencilState) {
        depth.depth_write_enabled = Some(self.depth_write);
        depth.depth_compare = Some(if self.depth_test {
            CompareFunction::GreaterEqual
        } else {
            CompareFunction::Always
        });
        if self.decal {
            depth.bias.constant = DECAL_DEPTH_BIAS;
            depth.bias.slope_scale = DECAL_SLOPE_BIAS;
        }
    }
}

#[derive(Asset, Reflect, Debug, Clone)]
pub struct NifDepthExtension {
    pub state: NifDepthState,
}

// This extension owns only a pipeline key and deliberately preserves the original
// non-bindless material contract. It introduces no additional GPU bindings.
impl AsBindGroup for NifDepthExtension {
    type Data = NifDepthRasterKey;
    type Param = ();

    fn label() -> &'static str {
        "NifDepthExtension"
    }

    fn bind_group_data(&self) -> Self::Data {
        self.state.into()
    }

    fn bindless_slot_count() -> Option<BindlessSlabResourceLimit> {
        None
    }

    fn bindless_descriptor() -> Option<BindlessDescriptor> {
        None
    }

    fn unprepared_bind_group(
        &self,
        _layout: &BindGroupLayout,
        _render_device: &RenderDevice,
        _param: &mut SystemParamItem<'_, '_, Self::Param>,
        _force_no_bindless: bool,
    ) -> Result<UnpreparedBindGroup, AsBindGroupError> {
        Ok(UnpreparedBindGroup {
            bindings: BindingResources(Vec::new()),
        })
    }

    fn bind_group_layout_entries(
        _render_device: &RenderDevice,
        _force_no_bindless: bool,
    ) -> Vec<BindGroupLayoutEntry> {
        Vec::new()
    }
}

impl From<&NifDepthExtension> for NifDepthState {
    fn from(extension: &NifDepthExtension) -> Self {
        extension.state
    }
}

impl MaterialExtension for NifDepthExtension {
    fn alpha_mode() -> Option<AlphaMode> {
        // The sorted color pass runs after opaque and cutout geometry. The base material's
        // GPU flags still control opaque alpha, mask discard, and genuine alpha blending.
        Some(AlphaMode::Blend)
    }

    fn enable_prepass() -> bool {
        // These surfaces must not populate the depth pyramid or occlude their receivers.
        false
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        specialize_nif_depth_descriptor(descriptor, key.bind_group_data);
        Ok(())
    }
}

fn specialize_nif_depth_descriptor(
    descriptor: &mut RenderPipelineDescriptor,
    state: NifDepthRasterKey,
) {
    // Shadow depth belongs to the light, not the authored camera depth state.
    // Spawned mesh entities exclude casters only when Cast_Shadows is unset.
    let color_pass = descriptor
        .fragment
        .as_ref()
        .is_some_and(|fragment| fragment.targets.iter().any(Option::is_some));
    if state.alpha_mask
        && let Some(fragment) = descriptor.fragment.as_mut()
    {
        let may_discard = fragment
            .shader_defs
            .iter_mut()
            .find(|def| shader_def_name(def) == "MAY_DISCARD");
        if let Some(def) = may_discard {
            *def = "MAY_DISCARD".into();
        } else {
            fragment.shader_defs.push("MAY_DISCARD".into());
        }
    }
    if color_pass && let Some(depth) = descriptor.depth_stencil.as_mut() {
        state.apply(depth);
    }
}

fn shader_def_name(def: &bevy::shader::ShaderDefVal) -> &str {
    match def {
        bevy::shader::ShaderDefVal::Bool(name, _)
        | bevy::shader::ShaderDefVal::Int(name, _)
        | bevy::shader::ShaderDefVal::UInt(name, _) => name,
    }
}

/// Wraps `base` in the authored depth state. The extension has no shaders of its own, so the
/// base keeps drawing with its own; effect palettes use this too (`crate::nif_material`).
pub fn depth_material<B: Material>(
    base: B,
    state: NifDepthState,
) -> ExtendedMaterial<B, NifDepthExtension> {
    ExtendedMaterial {
        base,
        extension: NifDepthExtension { state },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::render_resource::{
        ColorTargetState, ColorWrites, FragmentState, TextureFormat,
    };

    fn descriptor(targets: Vec<Option<ColorTargetState>>) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            depth_stencil: Some(DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(CompareFunction::Greater),
                stencil: default(),
                bias: default(),
            }),
            fragment: Some(FragmentState {
                targets,
                ..default()
            }),
            ..default()
        }
    }

    fn color_target() -> Option<ColorTargetState> {
        Some(ColorTargetState {
            format: TextureFormat::Rgba8UnormSrgb,
            blend: None,
            write_mask: ColorWrites::ALL,
        })
    }

    fn has_may_discard(descriptor: &RenderPipelineDescriptor) -> bool {
        descriptor.fragment.as_ref().is_some_and(|fragment| {
            fragment.shader_defs.iter().any(|def| {
                shader_def_name(def) == "MAY_DISCARD"
                    && matches!(def, bevy::shader::ShaderDefVal::Bool(_, true))
            })
        })
    }

    #[test]
    fn masked_palette_specialization_discards_in_shadow_without_overriding_light_depth() {
        let state =
            NifDepthState::from_flags(DEPTH_TEST | DECAL, DEPTH_WRITE, AlphaMode::Mask(0.5))
                .unwrap();
        let mut shadow = descriptor(vec![None]);
        shadow
            .fragment
            .as_mut()
            .unwrap()
            .shader_defs
            .push(bevy::shader::ShaderDefVal::Bool(
                "MAY_DISCARD".into(),
                false,
            ));
        let light_depth = shadow.depth_stencil.clone().unwrap();

        specialize_nif_depth_descriptor(&mut shadow, state.into());

        assert!(has_may_discard(&shadow));
        assert_eq!(shadow.fragment.unwrap().shader_defs.len(), 1);
        assert_eq!(shadow.depth_stencil, Some(light_depth));
    }

    #[test]
    fn color_pass_keeps_authored_depth_override_and_alpha_mask_definition() {
        let state =
            NifDepthState::from_flags(DEPTH_TEST | DECAL, DEPTH_WRITE, AlphaMode::Mask(0.5))
                .unwrap();
        let mut color = descriptor(vec![color_target()]);

        specialize_nif_depth_descriptor(&mut color, state.into());

        assert!(has_may_discard(&color));
        let depth = color.depth_stencil.unwrap();
        assert_eq!(depth.depth_compare, Some(CompareFunction::GreaterEqual));
        assert_eq!(depth.depth_write_enabled, Some(true));
        assert_eq!(depth.bias.constant, DECAL_DEPTH_BIAS);
    }

    #[test]
    fn nonmask_colorless_and_fragmentless_variants_keep_pipeline_state() {
        let nonmask = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Opaque).unwrap();
        let mut shadow = descriptor(vec![None]);
        let light_depth = shadow.depth_stencil.clone();
        specialize_nif_depth_descriptor(&mut shadow, nonmask.into());
        assert!(!has_may_discard(&shadow));
        assert_eq!(shadow.depth_stencil, light_depth);

        let mask = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Mask(0.5)).unwrap();
        let mut no_fragment = RenderPipelineDescriptor {
            depth_stencil: Some(DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(CompareFunction::Greater),
                stencil: default(),
                bias: default(),
            }),
            ..default()
        };
        let light_depth = no_fragment.depth_stencil.clone();
        specialize_nif_depth_descriptor(&mut no_fragment, mask.into());
        assert!(!has_may_discard(&no_fragment));
        assert_eq!(no_fragment.depth_stencil, light_depth);
    }

    #[test]
    fn caster_eligibility_does_not_split_identical_raster_keys() {
        let mut state = NifDepthState {
            depth_test: true,
            depth_write: false,
            decal: true,
            alpha_mask: true,
            cast_shadows: false,
        };
        let no_shadow = depth_material(StandardMaterial::default(), state);
        state.cast_shadows = true;
        let shadow = depth_material(StandardMaterial::default(), state);
        assert!(no_shadow.extension.state.excludes_shadow());
        assert!(!shadow.extension.state.excludes_shadow());
        assert_eq!(
            no_shadow.extension.bind_group_data(),
            shadow.extension.bind_group_data()
        );
        assert!(no_shadow.bind_group_data() == shadow.bind_group_data());
        assert_ne!(
            NifDepthState::from(&no_shadow.extension),
            NifDepthState::from(&shadow.extension)
        );
    }

    #[test]
    fn state_only_extension_keeps_the_legacy_non_bindless_slot_limit() {
        assert!(NifDepthMaterial::bindless_slot_count().is_none());
    }

    #[test]
    fn state_only_extension_declares_no_bindless_resources_or_index_tables() {
        assert!(NifDepthExtension::bindless_descriptor().is_none());
        assert!(NifDepthMaterial::bindless_descriptor().is_none());
        assert!(crate::nif_material::EffectPaletteDepthMaterial::bindless_descriptor().is_none());
        assert!(crate::skin_material::SkyrimSkinDepthMaterial::bindless_descriptor().is_none());
    }

    #[test]
    fn all_authored_states_preserve_each_raster_bit_without_caster_splits() {
        let mut keys = std::collections::HashSet::new();
        for bits in 0..32 {
            let state = NifDepthState {
                depth_test: bits & 1 != 0,
                depth_write: bits & 2 != 0,
                decal: bits & 4 != 0,
                alpha_mask: bits & 8 != 0,
                cast_shadows: bits & 16 != 0,
            };
            let material = depth_material(StandardMaterial::default(), state);
            let key = material.bind_group_data();
            let extension_key = key.extension;
            assert_eq!(material.extension.state, state);
            assert_eq!(extension_key.depth_test, state.depth_test);
            assert_eq!(extension_key.depth_write, state.depth_write);
            assert_eq!(extension_key.decal, state.decal);
            assert_eq!(extension_key.alpha_mask, state.alpha_mask);
            assert_eq!(extension_key, NifDepthRasterKey::from(state));
            assert_eq!(material.extension.bind_group_data(), extension_key);
            assert_eq!(NifDepthState::from(&material.extension), state);
            assert_eq!(
                material.extension.state.excludes_shadow(),
                !state.cast_shadows
            );
            assert_eq!(keys.insert(key), bits < 16);
        }
        assert_eq!(keys.len(), 16);
    }

    #[test]
    fn sorted_color_pass_keeps_authored_alpha_uniform_flags() {
        use bevy::{
            pbr::{Material, StandardMaterialFlags, StandardMaterialUniform},
            render::{render_asset::RenderAssets, render_resource::AsBindGroupShaderType},
        };
        for (alpha_mode, expected) in [
            (AlphaMode::Opaque, StandardMaterialFlags::ALPHA_MODE_OPAQUE),
            (AlphaMode::Mask(0.4), StandardMaterialFlags::ALPHA_MODE_MASK),
            (AlphaMode::Blend, StandardMaterialFlags::ALPHA_MODE_BLEND),
        ] {
            let state = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, alpha_mode).unwrap();
            let material = depth_material(
                StandardMaterial {
                    base_color: Color::linear_rgba(0.0, 1.0, 0.0, 0.5),
                    alpha_mode,
                    ..default()
                },
                state,
            );
            assert_eq!(Material::alpha_mode(&material), AlphaMode::Blend);
            let uniform: StandardMaterialUniform = material
                .base
                .as_bind_group_shader_type(&RenderAssets::default());
            assert_eq!(uniform.base_color.w, 0.5);
            assert_eq!(
                uniform.flags & StandardMaterialFlags::ALPHA_MODE_RESERVED_BITS.bits(),
                expected.bits()
            );
            if matches!(alpha_mode, AlphaMode::Mask(_)) {
                assert_eq!(uniform.alpha_cutoff, 0.4);
                assert!(state.alpha_mask);
            }
        }
    }

    #[test]
    fn shadow_casting_uses_authored_flag_independently_of_camera_depth() {
        // SkyrimShaderPropertyFlags1::Cast_Shadows is bit 9 in the vendored NIF table.
        for decal in [0, DECAL, DYNAMIC_DECAL, DECAL | DYNAMIC_DECAL] {
            for depth_test in [0, DEPTH_TEST] {
                for depth_write in [0, DEPTH_WRITE] {
                    for casts in [false, true] {
                        let flags = decal | depth_test | if casts { 0x200 } else { 0 };
                        if let Some(state) =
                            NifDepthState::from_flags(flags, depth_write, AlphaMode::Opaque)
                        {
                            assert_eq!(state.excludes_shadow(), !casts, "{flags:#x}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nondecal_depth_override_preserves_existing_raster_bias() {
        let state = NifDepthState::from_flags(DEPTH_TEST, 0, AlphaMode::Opaque).unwrap();
        let mut depth = DepthStencilState {
            format: bevy::render::render_resource::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(CompareFunction::Greater),
            stencil: default(),
            bias: bevy::render::render_resource::DepthBiasState {
                constant: 7,
                slope_scale: 0.25,
                clamp: 0.125,
            },
        };
        state.apply(&mut depth);
        assert_eq!(depth.depth_write_enabled, Some(false));
        assert_eq!(depth.depth_compare, Some(CompareFunction::GreaterEqual));
        assert_eq!(depth.bias.constant, 7);
        assert_eq!(depth.bias.slope_scale, 0.25);
        assert_eq!(depth.bias.clamp, 0.125);
    }

    #[test]
    fn ordinary_depth_and_blending_keep_their_original_material_path() {
        for alpha in [AlphaMode::Opaque, AlphaMode::Mask(0.4), AlphaMode::Blend] {
            assert!(NifDepthState::from_flags(DEPTH_TEST, DEPTH_WRITE, alpha).is_none());
        }
        assert!(NifDepthState::from_flags(DEPTH_TEST, 0, AlphaMode::Blend).is_none());
    }

    #[test]
    fn disabled_depth_test_also_disables_writes() {
        for flags_2 in [0, DEPTH_WRITE] {
            let state = NifDepthState::from_flags(0, flags_2, AlphaMode::Opaque).unwrap();
            assert!(!state.depth_test);
            assert!(!state.depth_write);
            assert!(state.excludes_shadow());
        }
    }

    #[test]
    fn camera_depth_state_preserves_testing_without_writing() {
        use bevy::render::render_resource::{DepthBiasState, StencilState, TextureFormat};
        for (flags_1, flags_2, compare, writes, bias) in [
            (DEPTH_TEST, 0, CompareFunction::GreaterEqual, false, 0),
            (0, DEPTH_WRITE, CompareFunction::Always, false, 0),
            (
                DEPTH_TEST | DECAL,
                0,
                CompareFunction::GreaterEqual,
                false,
                DECAL_DEPTH_BIAS,
            ),
            (
                DEPTH_TEST | DECAL,
                DEPTH_WRITE,
                CompareFunction::GreaterEqual,
                true,
                DECAL_DEPTH_BIAS,
            ),
        ] {
            let state = NifDepthState::from_flags(flags_1, flags_2, AlphaMode::Opaque).unwrap();
            let mut depth = DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::Never),
                stencil: StencilState::default(),
                bias: DepthBiasState::default(),
            };
            state.apply(&mut depth);
            assert_eq!(depth.depth_compare, Some(compare));
            assert_eq!(depth.depth_write_enabled, Some(writes));
            assert_eq!(depth.bias.constant, bias);
            assert_eq!(
                depth.bias.slope_scale,
                if state.decal { DECAL_SLOPE_BIAS } else { 0.0 }
            );
        }
    }

    #[test]
    fn only_authored_decals_receive_the_reversed_z_bias() {
        for decal in [DECAL, DYNAMIC_DECAL, DECAL | DYNAMIC_DECAL] {
            let state =
                NifDepthState::from_flags(DEPTH_TEST | decal, 0, AlphaMode::Mask(0.4)).unwrap();
            assert!(state.decal && state.alpha_mask && state.depth_test);
            assert!(!state.depth_write);
            assert!(state.excludes_shadow());
            let base = StandardMaterial {
                alpha_mode: AlphaMode::Mask(0.4),
                depth_bias: 2.0,
                ..default()
            };
            let material = depth_material(base, state);
            assert_eq!(material.base.alpha_mode, AlphaMode::Mask(0.4));
            assert_eq!(material.base.depth_bias, 2.0);
        }
        let state = NifDepthState::from_flags(DEPTH_TEST, 0, AlphaMode::Opaque).unwrap();
        assert!(!state.decal);
        assert!(state.excludes_shadow());
        assert_eq!(
            depth_material(StandardMaterial::default(), state)
                .base
                .depth_bias,
            0.0
        );
    }

    #[test]
    fn authored_depth_extensions_keep_the_legacy_non_bindless_contract() {
        assert!(NifDepthExtension::bindless_slot_count().is_none());
        assert!(NifDepthMaterial::bindless_slot_count().is_none());
        assert!(crate::nif_material::EffectPaletteDepthMaterial::bindless_slot_count().is_none());
        assert!(crate::skin_material::SkyrimSkinDepthMaterial::bindless_slot_count().is_none());
    }
}
