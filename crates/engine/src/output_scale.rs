//! The retail renderer-wide output scale `1 / FramebufferRange`.
//!
//! Registry sources: RF-SHADER-001, RF-SHADER-004 and RF-GRASS-024. The shared global holds
//! `1 / FramebufferRange`; its initial value is 0.8333333 (range 1.2), and the setup can also
//! take a 1.0 fallback. The global is mutable: the console command `SetFramebufferRange` stores
//! `1 / argument` into it, so the active runtime value is unverified. Retail water and the grass
//! pixel shader read the global; whether the Lighting, Sky, Effect, DistantTree, Particle and
//! image-space shader setups also read it is unverified. The grass pixel shader multiplies its
//! output by it. When the current fog object's near and far distances are both 0, the grass
//! constant is 1.0 instead. This module is the single source for the value; other passes must use
//! it and not a copy.
//!
//! Engine status: only grass applies the scale. Terrain, sky and water do not, so grass uses the
//! global's initial 0.8333 of terrain strength relative to retail if retail applies it to those
//! passes too (unverified). The retail fog condition is replaced by a proxy: a `DistanceFog`
//! component on the camera (exterior cell, Fog setting on). The proxy is unverified. Turning the
//! Fog setting off makes grass about 20 percent brighter (1.0 against 0.8333) while terrain does
//! not change.

/// Retail default `FramebufferRange` (`1 / 0.8333333`); RF-SHADER-001.
pub const FRAMEBUFFER_RANGE: f32 = 1.2;

/// The output scale for a pass. `fog_active` stands for the retail fog object having non-zero
/// distances (RF-SHADER-004). The engine does not read retail fog distances, so the caller passes
/// whether a `DistanceFog` component is on the camera: a proxy, unverified.
pub fn framebuffer_output_scale(fog_active: bool) -> f32 {
    if fog_active {
        1.0 / FRAMEBUFFER_RANGE
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The static default is 0.8333333 and fog off gives 1.0 (RF-SHADER-001, RF-SHADER-004).
    #[test]
    fn output_scale_is_one_over_the_framebuffer_range_with_fog() {
        assert_eq!(framebuffer_output_scale(true), 0.833_333_3);
        assert_eq!(framebuffer_output_scale(false), 1.0);
    }
}
