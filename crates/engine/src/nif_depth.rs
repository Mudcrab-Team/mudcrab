//! Authored NIF depth state for surfaces drawn after opaque geometry.
use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    render::render_resource::{
        AsBindGroup, CompareFunction, DepthStencilState, RenderPipelineDescriptor,
        SpecializedMeshPipelineError,
    },
};

pub const DEPTH_TEST: u32 = 1 << 31;
pub const DEPTH_WRITE: u32 = 1;
pub const DECAL: u32 = 1 << 26;
pub const DYNAMIC_DECAL: u32 = 1 << 27;

// A small raster offset in reversed-Z depth units, scoped to authored decals. This is a
// renderer policy tested on the GPU, not a recovered Skyrim numeric bias or a mesh offset.
pub const DECAL_DEPTH_BIAS: i32 = 4;
// Skyrim SE 1.7.104.0 raster modes 6/8 use -0.65 in forward Z. Reverse the sign
// for Bevy's reversed Z. Native viewport offsets and constant units depend on
// its camera/depth convention; those are not copied as numeric parity here.
pub const DECAL_SLOPE_BIAS: f32 = 0.65;

pub type NifDepthMaterial = ExtendedMaterial<StandardMaterial, NifDepthExtension>;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Reflect)]
pub struct NifDepthState {
    pub depth_test: bool,
    pub depth_write: bool,
    pub decal: bool,
    pub alpha_mask: bool,
}

impl NifDepthState {
    pub fn from_flags(flags_1: u32, flags_2: u32, alpha_mode: AlphaMode) -> Option<Self> {
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
        })
    }

    pub fn apply(&self, depth: &mut DepthStencilState) {
        depth.depth_write_enabled = Some(self.depth_write);
        depth.depth_compare = Some(if self.depth_test {
            CompareFunction::GreaterEqual
        } else {
            CompareFunction::Always
        });
        depth.bias.constant = if self.decal { DECAL_DEPTH_BIAS } else { 0 };
        depth.bias.slope_scale = if self.decal { DECAL_SLOPE_BIAS } else { 0.0 };
    }

    pub fn excludes_shadow(&self) -> bool {
        self.decal || !self.depth_test
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
        // Shadow depth belongs to the light, not the authored camera depth state. Decals and
        // depth-disabled effects are excluded as casters on their spawned mesh entities.
        let color_pass = descriptor
            .fragment
            .as_ref()
            .is_some_and(|fragment| fragment.targets.iter().any(Option::is_some));
        if color_pass {
            if let Some(depth) = descriptor.depth_stencil.as_mut() {
                key.bind_group_data.apply(depth);
            }
            if key.bind_group_data.alpha_mask
                && let Some(fragment) = descriptor.fragment.as_mut()
            {
                fragment.shader_defs.push("MAY_DISCARD".into());
            }
        }
        Ok(())
    }
}

pub fn depth_material(mut base: StandardMaterial, state: NifDepthState) -> NifDepthMaterial {
    if state.decal {
        base.depth_bias = DECAL_DEPTH_BIAS as f32;
    }
    NifDepthMaterial {
        base,
        extension: NifDepthExtension { state },
    }
}

/// The native base material retained for scene validation and collision classification.
#[derive(Component, Clone, Reflect)]
#[reflect(Component)]
pub(crate) struct NifDepthMaterialSource(pub Handle<StandardMaterial>);

#[cfg(test)]
mod tests {
    use super::*;

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
                ..default()
            };
            let material = depth_material(base, state);
            assert_eq!(material.base.alpha_mode, AlphaMode::Mask(0.4));
            assert_eq!(material.base.depth_bias, DECAL_DEPTH_BIAS as f32);
        }
        let state = NifDepthState::from_flags(DEPTH_TEST, 0, AlphaMode::Opaque).unwrap();
        assert!(!state.decal);
        assert!(!state.excludes_shadow());
        assert_eq!(
            depth_material(StandardMaterial::default(), state)
                .base
                .depth_bias,
            0.0
        );
    }
}
