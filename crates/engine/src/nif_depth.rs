//! Authored NIF depth state for surfaces drawn after opaque geometry.
use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    render::render_resource::{
        AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, CompareFunction,
        DepthStencilState, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    },
};

// nif.xml AlphaFunction: SRC_ALPHA (6), ONE (0).
pub const SOURCE_ALPHA_ADDITIVE: [u8; 2] = [6, 0];

pub const DEPTH_TEST: u32 = 1 << 31;
pub const DEPTH_WRITE: u32 = 1;
pub const DECAL: u32 = 1 << 26;
pub const DYNAMIC_DECAL: u32 = 1 << 27;
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
    pub blend_factors: Option<[u8; 2]>,
    pub uses_effect_emittance: bool,
    /// Drawn in the opaque or cutout phase, with the base material's alpha mode, so that the fog
    /// composite runs after it ([`Self::retail_first_stage`]).
    pub first_stage: bool,
}

impl NifDepthState {
    pub fn from_flags(flags_1: u32, flags_2: u32, alpha_mode: AlphaMode) -> Option<Self> {
        Self::from_material(flags_1, flags_2, alpha_mode, None)
    }

    pub fn from_material(
        flags_1: u32,
        flags_2: u32,
        alpha_mode: AlphaMode,
        blend_factors: Option<[u8; 2]>,
    ) -> Option<Self> {
        let depth_test = flags_1 & DEPTH_TEST != 0;
        let depth_write = depth_test && flags_2 & DEPTH_WRITE != 0;
        let decal = flags_1 & (DECAL | DYNAMIC_DECAL) != 0;
        // Stock blending already avoids depth writes. Ordinary depth-writing surfaces keep
        // their existing opaque/cutout/prepass path.
        if !decal && depth_test && (depth_write || matches!(alpha_mode, AlphaMode::Blend)) {
            return None;
        }
        Some(Self {
            depth_test,
            depth_write,
            decal,
            alpha_mask: matches!(alpha_mode, AlphaMode::Mask(_)),
            cast_shadows: flags_1 & CAST_SHADOWS != 0,
            blend_factors: if matches!(alpha_mode, AlphaMode::Mask(_)) {
                None
            } else {
                blend_factors
            },
            uses_effect_emittance: false,
            first_stage: false,
        })
    }

    /// The retail stage of a lit decal that does not blend (SPEC-1 section 9.3,
    /// [near-lighting.md](../../../docs/specs/engine/near-lighting.md)): retail draws it in the
    /// first stage, before the composite, and it writes depth. A blended surface, a surface that
    /// is not a decal and a surface without a depth test keep the sorted draw.
    /// STAND-IN(#362): blended decals and other surfaces without depth write draw after the composite.
    #[must_use]
    pub fn retail_first_stage(mut self, alpha_mode: AlphaMode) -> Self {
        if self.decal
            && self.depth_test
            && matches!(alpha_mode, AlphaMode::Opaque | AlphaMode::Mask(_))
        {
            self.first_stage = true;
            self.depth_write = true;
        }
        self
    }

    /// [`Self::from_material`] under the `skyrim` profile: the blend pair is retail's
    /// ([`retail_blend_factors`]), and a blended surface whose pair is not the plain alpha blend
    /// takes the authored state even when its depth state alone would not need one.
    pub fn retail_from_material(
        flags_1: u32,
        flags_2: u32,
        alpha_mode: AlphaMode,
        blend_factors: Option<[u8; 2]>,
    ) -> Option<Self> {
        let blend_factors = blend_factors.map(retail_blend_factors);
        let state = Self::from_material(flags_1, flags_2, alpha_mode, blend_factors);
        if state.is_some()
            || !matches!(alpha_mode, AlphaMode::Blend)
            || blend_factors.is_none_or(|factors| factors == [6, 7])
        {
            return state;
        }
        let depth_test = flags_1 & DEPTH_TEST != 0;
        Some(Self {
            depth_test,
            // A sorted blended draw of this path writes no depth, as before.
            depth_write: false,
            decal: false,
            alpha_mask: false,
            cast_shadows: flags_1 & CAST_SHADOWS != 0,
            blend_factors,
            uses_effect_emittance: false,
            first_stage: false,
        })
    }

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

    pub fn excludes_shadow(&self) -> bool {
        !self.cast_shadows
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
#[bind_group_data(NifDepthState)]
pub struct NifDepthExtension {
    pub state: NifDepthState,
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

    fn instance_alpha_mode(&self) -> Option<AlphaMode> {
        if self.state.first_stage {
            return None;
        }
        Self::alpha_mode()
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

pub(super) fn specialize_nif_depth_descriptor(
    descriptor: &mut RenderPipelineDescriptor,
    state: NifDepthState,
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
    if let Some([source, destination]) = state.blend_factors
        && let Some(color) = blend_component(source, destination)
        && let Some(fragment) = descriptor.fragment.as_mut()
    {
        for target in fragment.targets.iter_mut().flatten() {
            // Keep Bevy's alpha equation. A separate retail alpha equation is unverified.
            let alpha = target.blend.unwrap_or(BlendState::ALPHA_BLENDING).alpha;
            target.blend = Some(BlendState { color, alpha });
        }
    }
    if color_pass && let Some(depth) = descriptor.depth_stencil.as_mut() {
        state.apply(depth);
    }
}

/// The factors retail draws a serialized blend pair with (SPEC-2 section 4.2 and SPEC-1 section
/// 9.2, [near-lighting.md](../../../docs/specs/engine/near-lighting.md)):
///
/// | Authored (source, destination) | Retail mode | Drawn as |
/// |---|---|---|
/// | (6, 7) | 1 | SrcAlpha, InvSrcAlpha |
/// | (6, 0), (0, 0), (6, 9) | 2 | SrcAlpha, One |
/// | (4, 7) | 3 | DestColor, InvSrcAlpha |
/// | (1, 2), (4, 1) | 4 | DestColor, Zero |
///
/// STAND-IN(#362): for any other pair retail keeps the mode of the previous draw, which has no
/// equivalent here; it draws with SrcAlpha, InvSrcAlpha.
pub fn retail_blend_factors([source, destination]: [u8; 2]) -> [u8; 2] {
    match (source, destination) {
        (6, 0) | (0, 0) | (6, 9) => SOURCE_ALPHA_ADDITIVE,
        (4, 7) => [4, 7],
        (1, 2) | (4, 1) => [4, 1],
        _ => [6, 7],
    }
}

// nif.xml AlphaFunction, documented in vendor/project-wormhole-nif/src/nif_enum.rs.
fn blend_factor(value: u8) -> Option<BlendFactor> {
    use BlendFactor::*;
    Some(match value {
        0 => One,
        1 => Zero,
        2 => Src,
        3 => OneMinusSrc,
        4 => Dst,
        5 => OneMinusDst,
        6 => SrcAlpha,
        7 => OneMinusSrcAlpha,
        8 => DstAlpha,
        9 => OneMinusDstAlpha,
        10 => SrcAlphaSaturated,
        _ => return None,
    })
}

pub fn valid_blend_factors([source, destination]: [u8; 2]) -> bool {
    // SRC_ALPHA_SATURATE is valid only as a source factor.
    source <= 10 && destination <= 9
}

fn blend_component(source: u8, destination: u8) -> Option<BlendComponent> {
    if !valid_blend_factors([source, destination]) {
        return None;
    }
    Some(BlendComponent {
        src_factor: blend_factor(source)?,
        dst_factor: blend_factor(destination)?,
        operation: BlendOperation::Add,
    })
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
            blend: Some(BlendState::ALPHA_BLENDING),
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
    fn a_retail_decal_that_does_not_blend_draws_in_the_first_stage_and_writes_depth() {
        // The Whiterun wall-top cap: a decal with an alpha test and no depth-write flag.
        for alpha_mode in [AlphaMode::Mask(0.5), AlphaMode::Opaque] {
            let authored = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, alpha_mode).unwrap();
            assert!(!authored.first_stage && !authored.depth_write);
            let state = authored.retail_first_stage(alpha_mode);
            assert!(state.first_stage && state.depth_write && state.decal);
            let base = StandardMaterial {
                alpha_mode,
                ..default()
            };
            let first = depth_material(base.clone(), state);
            assert_eq!(Material::alpha_mode(&first), alpha_mode);
            // The sorted draw of the same surface stays after the composite.
            let sorted = depth_material(base, authored);
            assert_eq!(Material::alpha_mode(&sorted), AlphaMode::Blend);
        }
    }

    #[test]
    fn other_authored_surfaces_keep_the_sorted_draw_under_retail() {
        for (flags_1, alpha_mode) in [
            (DEPTH_TEST | DECAL, AlphaMode::Blend),
            (DEPTH_TEST, AlphaMode::Mask(0.5)),
            (DECAL, AlphaMode::Mask(0.5)),
        ] {
            let authored = NifDepthState::from_flags(flags_1, 0, alpha_mode).unwrap();
            assert_eq!(authored.retail_first_stage(alpha_mode), authored);
        }
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

        specialize_nif_depth_descriptor(&mut shadow, state);

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

        specialize_nif_depth_descriptor(&mut color, state);

        assert!(has_may_discard(&color));
        let depth = color.depth_stencil.unwrap();
        assert_eq!(depth.depth_compare, Some(CompareFunction::GreaterEqual));
        assert_eq!(depth.depth_write_enabled, Some(true));
        assert_eq!(depth.bias.constant, DECAL_DEPTH_BIAS);
    }

    #[test]
    fn additive_decal_uses_authored_blend_factors() {
        let mut state = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Blend).unwrap();
        state.blend_factors = Some(SOURCE_ALPHA_ADDITIVE);
        let mut color = descriptor(vec![color_target()]);

        specialize_nif_depth_descriptor(&mut color, state);

        let fragment = color.fragment.unwrap();
        let blend = fragment.targets[0].as_ref().unwrap().blend.unwrap();
        assert_eq!(blend.color.src_factor, BlendFactor::SrcAlpha);
        assert_eq!(blend.color.dst_factor, BlendFactor::One);
        assert_eq!(blend.alpha, BlendState::ALPHA_BLENDING.alpha);
        assert_eq!(
            color.depth_stencil.unwrap().depth_write_enabled,
            Some(false)
        );
    }

    #[test]
    fn authored_blend_preserves_cutout_and_ordinary_blend_routing() {
        for mode in [AlphaMode::Mask(0.4), AlphaMode::Blend] {
            assert!(
                NifDepthState::from_material(
                    DEPTH_TEST,
                    DEPTH_WRITE,
                    mode,
                    Some(SOURCE_ALPHA_ADDITIVE)
                )
                .is_none()
            );
        }
        let mask = NifDepthState::from_material(
            DEPTH_TEST | DECAL,
            DEPTH_WRITE,
            AlphaMode::Mask(0.4),
            Some(SOURCE_ALPHA_ADDITIVE),
        )
        .unwrap();
        assert!(mask.alpha_mask);
        assert_eq!(mask.blend_factors, None);
    }

    #[test]
    fn authored_source_alpha_blend_keeps_bevys_alpha_component() {
        let state =
            NifDepthState::from_material(DEPTH_TEST | DECAL, 0, AlphaMode::Blend, Some([6, 7]))
                .unwrap();
        let mut pipeline = descriptor(vec![color_target()]);
        specialize_nif_depth_descriptor(&mut pipeline, state);
        assert_eq!(
            pipeline.fragment.unwrap().targets[0]
                .as_ref()
                .unwrap()
                .blend,
            Some(BlendState::ALPHA_BLENDING)
        );
    }

    #[test]
    fn nonmask_colorless_and_fragmentless_variants_keep_pipeline_state() {
        let nonmask = NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Opaque).unwrap();
        let mut shadow = descriptor(vec![None]);
        let light_depth = shadow.depth_stencil.clone();
        specialize_nif_depth_descriptor(&mut shadow, nonmask);
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
        specialize_nif_depth_descriptor(&mut no_fragment, mask);
        assert!(!has_may_discard(&no_fragment));
        assert_eq!(no_fragment.depth_stencil, light_depth);
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

    /// SPEC-2 example E4: destination (0.5, 0.4, 0.3), source (0.8, 0.6, 0.2), alpha 0.25 through
    /// the factors each authored pair is drawn with.
    #[test]
    fn retail_blend_pairs_give_the_specification_example() {
        let (destination, source, alpha) = ([0.5_f32, 0.4, 0.3], [0.8_f32, 0.6, 0.2], 0.25_f32);
        let drawn = |pair| {
            let component =
                blend_component(retail_blend_factors(pair)[0], retail_blend_factors(pair)[1])
                    .unwrap();
            let factor = |factor: BlendFactor, channel: usize| match factor {
                BlendFactor::One => 1.0,
                BlendFactor::Zero => 0.0,
                BlendFactor::SrcAlpha => alpha,
                BlendFactor::OneMinusSrcAlpha => 1.0 - alpha,
                BlendFactor::Dst => destination[channel],
                other => panic!("unexpected factor {other:?}"),
            };
            [0, 1, 2].map(|channel| {
                source[channel] * factor(component.src_factor, channel)
                    + destination[channel] * factor(component.dst_factor, channel)
            })
        };
        for (pairs, expected) in [
            (&[[6, 7]][..], [0.575, 0.45, 0.275]),
            (&[[6, 0], [0, 0], [6, 9]][..], [0.70, 0.55, 0.35]),
            (&[[4, 7]][..], [0.775, 0.54, 0.285]),
            (&[[1, 2], [4, 1]][..], [0.40, 0.24, 0.06]),
        ] {
            for &pair in pairs {
                for (actual, expected) in drawn(pair).into_iter().zip(expected) {
                    assert!(
                        (actual - expected).abs() < 1.0e-6,
                        "{pair:?}: {actual} {expected}"
                    );
                }
            }
        }
    }

    /// An additive surface with an ordinary depth state takes the authored blend under `skyrim`
    /// only; a plain alpha blend keeps its route.
    #[test]
    fn retail_blend_state_covers_a_non_decal_additive_surface() {
        let flags = (DEPTH_TEST, 0);
        let mode = AlphaMode::Blend;
        assert!(NifDepthState::from_material(flags.0, flags.1, mode, Some([6, 0])).is_none());
        let state =
            NifDepthState::retail_from_material(flags.0, flags.1, mode, Some([0, 0])).unwrap();
        assert_eq!(state.blend_factors, Some(SOURCE_ALPHA_ADDITIVE));
        assert!(!state.depth_write && !state.decal);
        assert!(
            NifDepthState::retail_from_material(flags.0, flags.1, mode, Some([6, 7])).is_none()
        );
    }
}
