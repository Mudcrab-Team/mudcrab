use crate::streaming::near_terrain_blend::{NearTerrainBlend, NearTerrainBlendSettings};
use crate::{
    color_pipeline::{DEFAULT_SCENE_EV100, SceneColorPipeline},
    profiling::ProfilingState,
    world::{
        cache::TerrainSnapshot,
        database::{AssetCatalog, WaterFog},
    },
};
use bevy::{
    app::{HierarchyPropagatePlugin, PropagateOver, PropagateSet},
    asset::embedded_asset,
    camera::{
        Exposure, RenderTarget,
        primitives::{Aabb, Frustum},
        visibility::{Layer, RenderLayers, VisibilitySystems},
    },
    core_pipeline::{mip_generation::experimental::depth::ViewDepthPyramid, prepass::DepthPrepass},
    image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor},
    pbr::{
        ExtendedMaterial, ExtractedDirectionalLight, LightEntity, MaterialExtension, ShadowView,
        prepare_lights,
    },
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        batching::gpu_preprocessing::{
            GpuPreprocessingMode, GpuPreprocessingSupport, IndirectParametersBuffers,
        },
        camera::ExtractedCamera,
        extract_component::ExtractComponentPlugin,
        occlusion_culling::OcclusionCulling,
        render_resource::{AsBindGroup, ShaderType},
        renderer::RenderAdapterInfo,
        view::NoIndirectDrawing,
    },
    shader::ShaderRef,
};
use serde::Serialize;
use shared::LAND_TEXTURE_REPEATS_PER_CELL;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainExtension>;

/// The retail surface of near landscape under the `skyrim` profile, whose colour textures decode
/// raw (SPEC-1 sections 6.1 and 9.2).
///
/// STAND-IN(#547): retail's landscape specular is not drawn. The converter publishes no `LTEX`
/// specular exponent and the specification gives no land specular colour.
pub(crate) fn retail_terrain_surface(
    decodes_colour_srgb: bool,
) -> Option<bevy::pbr::RetailSurface> {
    (!decodes_colour_srgb).then_some(bevy::pbr::RetailSurface::MATTE)
}
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

/// The previous procedural waves' tilt, kept only as the neutral point of the user's "Water wave
/// strength" setting: the setting divided by this is a multiplier on the amplitudes the water's own
/// WATR record gives, so the default setting changes nothing. It never reaches the shader as a
/// value of its own.
pub(crate) const WAVE_STRENGTH: f32 = 0.05;

/// The neutral point of the "Water wave scale" setting, in the same way: the setting over this is a
/// multiplier on the record's noise UV scales.
pub(crate) const DEFAULT_WAVE_SCALE: f32 = 0.006;

/// The neutral point of the "Water wave speed" setting: the setting over this is a multiplier on
/// the record's wind speeds.
pub(crate) const DEFAULT_WAVE_SPEED: f32 = 0.15;

/// DefaultWater's `noiseFalloff` (`DNAM` +0x60), from the retail records read for this PR; used for a
/// water whose record bytes cannot be read.
pub(crate) const DEFAULT_WATER_NOISE_FALLOFF: f32 = 4007.0;

/// The weight of the retail near-camera blend of the reflection and the mix (RF-SHADER-008):
/// `distT = sat(lerp(flag, 1, (D - 8192) / (falloff - 8192)))`, `D` the eye distance, `falloff` the
/// record's `noiseFalloff` clamped to `[1, 8191]` (RF-SHADER-007). The frame flag `w` of the main
/// pass is unverified (#698); `1` is the value for which `distT` is `1` at every distance, which is
/// what the arithmetic also gives for any flag at distances up to 4007 units.
#[cfg(test)]
pub(crate) const WATER_FRAME_FLAG: f32 = 1.0;

/// The retail literal in the `distT` ratio (RF-SHADER-008, RF-WATER-009).
#[cfg(test)]
pub(crate) const WATER_DISTANCE_LITERAL: f32 = 8192.0;

/// `fWaterSSRIntensity:Display`: 0.5 in this install's `Skyrim.ini`; the executable's own default
/// is 1.3. The install value is followed (RF-WATER-009, `verify-report.md` apply item 1).
#[cfg(test)]
pub(crate) const WATER_SSR_INTENSITY: f32 = 0.5;

/// The retail distance term of the reflection mix (RF-SHADER-008).
#[cfg(test)]
pub(crate) fn water_dist_t(eye_distance: f32, noise_falloff: f32, flag: f32) -> f32 {
    let falloff = noise_falloff.clamp(1.0, 8191.0);
    let ratio = (eye_distance - WATER_DISTANCE_LITERAL) / (falloff - WATER_DISTANCE_LITERAL);
    (flag + (1.0 - flag) * ratio).clamp(0.0, 1.0)
}

/// The share of the sampled reflection image in the environment, as the retail pixel shader weights
/// its screen-space reflection (RF-WATER-009): `sat(distT * fWaterSSRIntensity * ssr.a)`.
#[cfg(test)]
pub(crate) fn water_reflection_share(dist_t: f32, coverage: f32) -> f32 {
    (dist_t * WATER_SSR_INTENSITY * coverage).clamp(0.0, 1.0)
}

/// The reflection the retail cube-map programs write (RF-WATER-050): `lerp(ReflectionColor, env,
/// DNAM 0x14)` with `env = lerp(cube * FBR, ssr, share)`, `share` being [`water_reflection_share`].
/// The flat colour is only the `1 - DNAM 0x14` part; the cube is the rest less the SSR share.
#[cfg(test)]
pub(crate) fn water_reflection_colour(
    flat: f32,
    cube: f32,
    ssr: f32,
    share: f32,
    reflection_amount: f32,
) -> f32 {
    let environment = cube + (ssr - cube) * share;
    flat + (environment - flat) * reflection_amount
}

/// The world-XY offset of the refracted point (RF-WATER-049): `N.xy * DNAM 0x98 * lerp(1, sat(L / F),
/// DNAM 0xD4)`, `along` being `sat(L / F)` and `depth_refraction` the DNAM 0xD4 depth property.
#[cfg(test)]
pub(crate) fn water_refraction_offset(
    normal_xy: [f32; 2],
    magnitude: f32,
    depth_refraction: f32,
    along: f32,
) -> [f32; 2] {
    let factor = 1.0 + depth_refraction * (along.clamp(0.0, 1.0) - 1.0);
    normal_xy.map(|component| component * magnitude * factor)
}

/// The leak test of the refraction (RF-WATER-049, medium-high on the sign): the scene point seen
/// through the offset pixel is in front of the water when it is on the same side of the water plane
/// as the camera, and the unperturbed pixel is used then.
#[cfg(test)]
pub(crate) fn water_refraction_leaks(
    camera_height: f32,
    surface_height: f32,
    scene_height: f32,
) -> bool {
    (camera_height - surface_height) * (scene_height - surface_height) > 0.0
}

/// The cells whose flow maps the flow program's shifted taps read past a cell's east and south
/// edges: east (x + 1, y), south (x, y - 1) and south-east (x + 1, y - 1). Retail's flow atlas puts
/// geographic neighbours next to each other and samples it with wrap addressing, so a tap shifted
/// half a texel east or south at the edge reads the neighbour's first texel (RF-WATER-063,
/// RF-WATER-036). The cell's V axis runs south, toward smaller Y.
pub(crate) fn water_flow_neighbour_cells(grid: IVec2) -> [IVec2; 3] {
    [
        grid + IVec2::new(1, 0),
        grid + IVec2::new(0, -1),
        grid + IVec2::new(1, -1),
    ]
}

/// Which flow map a texel read lands in.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaterFlowTile {
    Own,
    East,
    South,
    SouthEast,
}

/// Where the flow program's point read of texel `texel` (which may be 64, one past the cell's last
/// texel, for a tap shifted past the east or south edge) lands: the cell's own map, or a bound
/// neighbour's first row or column; `bound` is the bit set of `settings.flow.w`. A side with no
/// neighbour bound clamps to the cell's own edge texel, as the flow program did before neighbours.
/// Mirrors `flow_texel` in `water.wgsl`.
#[cfg(test)]
pub(crate) fn water_flow_texel_source(texel: IVec2, bound: u8) -> (WaterFlowTile, IVec2) {
    let past_east = texel.x >= 64;
    let past_south = texel.y >= 64;
    match (past_east, past_south) {
        (false, false) => (WaterFlowTile::Own, texel),
        (true, false) if bound & 1 != 0 => (WaterFlowTile::East, IVec2::new(0, texel.y)),
        (false, true) if bound & 2 != 0 => (WaterFlowTile::South, IVec2::new(texel.x, 0)),
        (true, true) if bound & 4 != 0 => (WaterFlowTile::SouthEast, IVec2::ZERO),
        _ => (WaterFlowTile::Own, texel.min(IVec2::splat(63))),
    }
}

/// The bilinear weight of the flow-map program along one axis (RF-WATER-044): `1 - |2 * fract(64 *
/// uv) - 1|`, 1 at a texel centre and 0 at a texel edge.
#[cfg(test)]
pub(crate) fn water_flow_weight(cell_uv: f32) -> f32 {
    let scaled = 64.0 * cell_uv;
    1.0 - (2.0 * (scaled - scaled.floor()) - 1.0).abs()
}

/// The renormalising divisor of the flow blend (RF-WATER-044): `sqrt((1 - w)^2 + w^2)` per axis,
/// multiplied.
#[cfg(test)]
pub(crate) fn water_flow_divisor(weight_x: f32, weight_y: f32) -> f32 {
    let axis = |w: f32| ((1.0 - w) * (1.0 - w) + w * w).sqrt();
    axis(weight_x) * axis(weight_y)
}

/// The flow clock in game seconds after `seconds` of real time: the retail clock is the game hour
/// times 3600 and wraps at midnight (RF-WATER-038); the engine starts it at hour 12 and runs it at
/// the retail time scale of 20. `speed` is the player's wave-speed multiplier, 1 at its default and
/// 0 for a frozen phase.
#[cfg(test)]
pub(crate) fn water_flow_clock(seconds: f32, speed: f32) -> f32 {
    (43200.0 + seconds * speed * 20.0) % 86400.0
}

/// The flow program's normal xy after the player's wave settings: the retail value times the
/// strength multiplier (0 with waves off, 1 at the default).
#[cfg(test)]
pub(crate) fn water_flow_strength(retail_xy: [f32; 2], strength: f32) -> [f32; 2] {
    retail_xy.map(|component| component * strength)
}

/// The two sun terms of the retail water pixel shader, before the sun colour (RF-WATER-010):
/// `sat(R.sun)^specularPower * specularMagnitude + sat(N.(-0.099, -0.099, 0.99))^sparklePower *
/// sparkleMagnitude`. `terms` is (specular power, specular magnitude, sparkle power, sparkle
/// magnitude). A base of zero is floored so a power of zero gives the magnitude rather than NaN.
#[cfg(test)]
pub(crate) fn water_sun_terms(r_dot_sun: f32, n_dot_sparkle: f32, terms: [f32; 4]) -> f32 {
    let floor = 1.0e-30_f32;
    let term = |base: f32, power: f32, magnitude: f32| {
        (power * base.clamp(0.0, 1.0).max(floor).log2()).exp2() * magnitude
    };
    term(r_dot_sun, terms[0], terms[1]) + term(n_dot_sparkle, terms[2], terms[3])
}

/// The body colour of the retail variants that have no depth (RF-WATER-017): `lerp(Shallow, Deep, F)`
/// times `sat(sunDir.N)` for the exterior no-depth program, and without that factor for the
/// interior program, which has no sun and no light.
#[cfg(test)]
pub(crate) fn water_variant_body(
    shallow: f32,
    deep: f32,
    fresnel: f32,
    sun_dot_n: Option<f32>,
) -> f32 {
    (shallow + (deep - shallow) * fresnel) * sun_dot_n.map_or(1.0, |dot| dot.clamp(0.0, 1.0))
}

/// The depth terms of the retail water programs that have depth (RF-WATER-048, RF-WATER-059), as
/// `water_body` in `water.wgsl` computes them: `x = sat(L / F)`, `y` = the body's share over the bed
/// `1 - sat((F - min(L, F)) / range)^exponent`, `z = sat(h / F)`. `body` is the uniform's
/// `(opacity, range, F, exponent)`; `path` is L and `vertical` is h, both in world units.
#[cfg(test)]
pub(crate) fn water_depth_terms(path: f32, vertical: f32, body: [f32; 4]) -> [f32; 3] {
    let [_, range, far, exponent] = body;
    [
        (path / far).clamp(0.0, 1.0),
        1.0 - ((far - path.min(far)) / range)
            .clamp(0.0, 1.0)
            .powf(exponent),
        (vertical / far).clamp(0.0, 1.0),
    ]
}

/// A DepthControl factor: `lerp(1, t, D)` for the term `t` in 0..1 and the record's property `D`
/// (RF-WATER-048); at the waterline `t = 0`, so it is `1 - D` (RF-WATER-059).
#[cfg(test)]
pub(crate) fn water_depth_factor(depth_property: f32, term: f32) -> f32 {
    depth_property * (term - 1.0) + 1.0
}

/// A phase below zero makes the shader run the waves on the frame clock instead of a phase the CPU
/// uploaded, so an interactive run does not rewrite every water material every frame.
pub(crate) const ANIMATED_WAVE_PHASE: f32 = -1.0;

/// Degrees to radians as the retail engine does it for the noise wind directions: a float
/// constant, `0.017453292` (`SkyrimSE.exe` 1.7.104.0, VA `0x1418121c8`, read in the loop at
/// `0x1405290ac`; `evidence/rea-20261114/q1-water.md`).
pub(crate) const WATER_DEGREES_TO_RADIANS: f32 = 0.017453292;

/// Where the pattern of a noise layer sits at water clock `seconds`, in tiles. Skyrim wraps
/// `(sin(theta) * speed * T, cos(theta) * speed * T)` into `[0, 1)`, `theta` being the layer's wind
/// direction in radians and `speed` its wind speed in tiles per second (`SkyrimSE.exe`
/// `0x1405290ac` to `0x140529356`; `evidence/rea-20261114/q1-water.md`). The shader adds it to
/// the layer's UV. `water.wgsl` does the same; a test keeps them equal.
#[cfg(test)]
pub(crate) fn noise_scroll(direction_degrees: f32, speed: f32, seconds: f32) -> Vec2 {
    let theta = direction_degrees * WATER_DEGREES_TO_RADIANS;
    let wrap = |value: f32| value - value.floor();
    Vec2::new(
        wrap(theta.sin() * speed * seconds),
        wrap(theta.cos() * speed * seconds),
    )
}

/// How long the shared shader clock (`globals.time`) runs before it wraps to zero. Bevy wraps it
/// after one hour, and the waves would jump to a different pattern at that instant, because the
/// wave phase is the clock times the wave speed and is not periodic in an hour. A day keeps `f32`
/// resolution near 8 ms, which is far finer than the wave motion needs.
pub(crate) const SHADER_CLOCK_WRAP: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Lengthens the shader clock's wrap period so the water waves do not jump once per hour.
fn extend_shader_clock(time: Option<ResMut<Time<Virtual>>>) {
    if let Some(mut time) = time {
        time.set_wrap_period(SHADER_CLOCK_WRAP);
    }
}

/// Skyrim's DefaultWater after Update.esm: Fresnel Amount 0.10 and a Reflectivity Amount of 0.8
/// (WATR `DNAM`). Used when a water has no decoded colours yet, e.g. a database converted before
/// `crates/converter` started reading them.
pub const DEFAULT_WATER_FRESNEL: f32 = 0.10;
pub const DEFAULT_WATER_REFLECTIVITY: f32 = 0.8;
/// The `specular_transmission` of the water's `StandardMaterial`. Only its being above zero matters:
/// it makes Bevy draw the water in the transmissive phase and bind the scene copy the shader
/// refracts (an engineering flag; the shader reads neither the value nor any transmission term).
pub const WATER_TRANSMISSION_FLAG: f32 = 1.0;

/// DefaultWater's refraction magnitude after Update.esm (WATR DNAM +0x98 = 40; RF-WATER-056), used
/// when a water's record cannot be read.
pub const DEFAULT_WATER_REFRACTION_MAGNITUDE: f32 = 40.0;
/// DefaultWater's Reflection Color after Update.esm (`#778C9D`): what the water reflects where the
/// reflection image holds nothing, and what a water with no decoded colours falls back to.
pub const DEFAULT_WATER_REFLECTION_COLOR: [u8; 3] = [0x77, 0x8C, 0x9D];
/// DefaultWater's Deep Color (WATR DNAM): the material base colour, and the fallback deep colour
/// for a water with no decoded colours.
pub const DEFAULT_WATER_DEEP_COLOR: [u8; 3] = [0x05, 0x0E, 0x12];
/// DefaultWater's Shallow Color (WATR DNAM): the fallback for a water with no decoded colours.
pub const DEFAULT_WATER_SHALLOW_COLOR: [u8; 3] = [0x26, 0x27, 0x18];
/// DefaultWater's opacity (WATR ANAM, 0-100).
pub const DEFAULT_WATER_OPACITY: u8 = 50;
/// A WATR colour (stored sRGB bytes) times the weather's Water Multiplier (also bytes), as the
/// retail water shader multiplies them (RF-SHADER-007, `evidence/rea-20261114/s1-water-mix.md`):
/// the product is taken on the 0..1 values and then read as an sRGB colour, as in retail.
fn water_srgb(rgb: [u8; 3], multiplier: [f32; 3]) -> [f32; 3] {
    [
        f32::from(rgb[0]) / 255.0 * multiplier[0],
        f32::from(rgb[1]) / 255.0 * multiplier[1],
        f32::from(rgb[2]) / 255.0 * multiplier[2],
    ]
}

fn linear_rgb(rgb: [f32; 3]) -> Vec4 {
    let linear = Color::srgb(rgb[0], rgb[1], rgb[2]).to_linear();
    Vec4::new(linear.red, linear.green, linear.blue, 0.0)
}

/// A water's Deep Color times the weather's Water Multiplier, as the material base colour.
pub fn water_deep_color(deep: [u8; 3], multiplier: [u8; 3]) -> Color {
    let [r, g, b] = water_srgb(deep, multiplier.map(|byte| f32::from(byte) / 255.0));
    Color::srgb(r, g, b)
}

/// The above-water fog terms as the retail material copy writes them (RF-SHADER-006,
/// `SkyrimSE.exe` `0x140307460`): x = opacity (ANAM / 100, used only where the depth is unknown),
/// y = the fog range, far minus near (`1e-5` when that is zero), z = max(far, 1), w = the curve
/// exponent max(4 * fog amount, 1). The shader's opacity is
/// `1 - saturate((z - min(L, z)) / y) ^ w` for a view path `L` through the water
/// (RF-SHADER-010).
fn water_body(opacity: u8, fog: WaterFog) -> Vec4 {
    let range = fog.far - fog.near;
    Vec4::new(
        f32::from(opacity.min(100)) / 100.0,
        if range == 0.0 { 1.0e-5 } else { range },
        fog.far.max(1.0),
        (4.0 * fog.amount).max(1.0),
    )
}

fn default_water_body() -> Vec4 {
    water_body(DEFAULT_WATER_OPACITY, WaterFog::default())
}

/// One rendering layer per role, so no view list is a bare literal.
///
/// Layer 0 is Bevy's default and holds the world itself: terrain today, and - when they land - the
/// distant LOD land, LOD objects, LOD trees and the sky. Water surfaces take their own layer so a
/// water plane can never reflect another one. Full-detail placed objects take a third layer so the
/// reflection pass can leave them out; see [`REFLECTION_VIEW_LAYERS`] for why.
pub const WORLD_LAYER: Layer = 0;
pub const WATER_LAYER: Layer = 1;
pub const PLACED_OBJECT_LAYER: Layer = 2;

/// Everything the player's own cameras draw: terrain, water and the placed objects.
pub const MAIN_VIEW_LAYERS: &[Layer] = &[WORLD_LAYER, WATER_LAYER, PLACED_OBJECT_LAYER];

/// What the water reflection camera draws.
///
/// Vanilla Skyrim builds its water reflection from the LOD world - LOD land, LOD objects and LOD
/// trees - plus the sky, selected by the `[Water]` settings `bReflectLODLand`, `bReflectLODObjects`,
/// `bReflectLODTrees` and `bReflectSky`; full-detail placed objects, actors and grass are not in it
/// (SE adds screen-space reflections for near geometry, which this engine does not have). This
/// engine has no LOD and no sky yet, so what vanilla reflects is the terrain: the world layer only.
/// LOD and the sky join [`WORLD_LAYER`] when they land, and a full-detail object stays out of the
/// reflection because it is on [`PLACED_OBJECT_LAYER`].
pub const REFLECTION_VIEW_LAYERS: &[Layer] = &[WORLD_LAYER];

/// What every scene light belongs to: the sun today, and any point light a cell adds later.
///
/// A light must intersect a view's layers to light it, so this has to keep [`WORLD_LAYER`] - both
/// cameras render it. It also has to include [`PLACED_OBJECT_LAYER`], because a light only collects
/// shadow casters that share a layer with it (`check_dir_light_mesh_visibility`,
/// `bevy_light-0.19.0/src/lib.rs:423`). Dropping the placed-object layer would light the objects
/// while silently removing them from the sun's shadow cascades.
pub const LIGHT_LAYERS: &[Layer] = &[WORLD_LAYER, PLACED_OBJECT_LAYER];

/// The render layers for a light spawned below a placed-object reference.
///
/// The reference's [`Propagate`](bevy::app::Propagate) would otherwise copy
/// [`PLACED_OBJECT_RENDER_LAYERS`] onto the light and drop it from [`WORLD_LAYER`], breaking the
/// [`LIGHT_LAYERS`] contract. [`PropagateOver`] keeps the propagated value off this entity only
/// (`PropagateStop` would still write it here and just stop the walk below).
pub fn light_render_layers() -> (RenderLayers, PropagateOver<RenderLayers>) {
    (
        RenderLayers::from_layers(LIGHT_LAYERS),
        PropagateOver::default(),
    )
}

/// The value [`Propagate`](bevy::app::Propagate) copies onto the meshes below a placed-object
/// reference, which is what keeps those meshes out of the reflection pass.
pub const PLACED_OBJECT_RENDER_LAYERS: RenderLayers = RenderLayers::layer(PLACED_OBJECT_LAYER);

/// Registers the propagation of [`PLACED_OBJECT_RENDER_LAYERS`] down reference hierarchies.
///
/// A reference entity is spawned in `streaming::spawn_cell` and the meshes it draws arrive later as
/// descendants from the converted glb, so `RenderLayers` on the reference alone would not reach
/// them. The propagation runs in `PostUpdate` before [`VisibilitySystems::CheckVisibility`]:
/// visibility compares render layers after that, and the glTF scene children are spawned in
/// `SpawnScene` between `Update` and `PostUpdate`, so the layers are in place in the first frame a
/// mesh exists - including for descendants spawned after the reference.
pub fn add_placed_object_layer_propagation(app: &mut App) {
    app.add_plugins(HierarchyPropagatePlugin::<RenderLayers>::new(PostUpdate))
        .configure_sets(
            PostUpdate,
            PropagateSet::<RenderLayers>::default().before(VisibilitySystems::CheckVisibility),
        );
}

pub struct VercidiumRendererPlugin;

impl Plugin for VercidiumRendererPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/terrain.wgsl");
        embedded_asset!(app, "shaders/water.wgsl");
        app.add_plugins((
            MaterialPlugin::<TerrainMaterial>::default(),
            crate::terrain_lod_noise::TerrainLodNoisePlugin,
            MaterialPlugin::<WaterMaterial>::default(),
            crate::prepass_vertex_alpha::PrepassVertexAlphaPlugin,
            crate::water_depth::WaterDepthPlugin,
            crate::underwater_pass::UnderwaterPassPlugin,
            crate::retail_fog::RetailFogPlugin,
            crate::retail_hdr::RetailHdrPlugin,
            ExtractComponentPlugin::<crate::world::components::StreamingCamera>::default(),
        ))
        .init_resource::<RendererMetrics>()
        .add_systems(Startup, (setup_water_reflection, extend_shader_clock))
        .add_systems(PostUpdate, animate_water_materials)
        .add_systems(
            Update,
            (update_water_reflection_camera, sync_renderer_metrics)
                .after(crate::settings_ui::SettingsUiSet),
        );

        add_placed_object_layer_propagation(app);

        let bridge = RendererProofBridge::default();
        app.insert_resource(bridge.clone());
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(bridge)
                .add_systems(
                    Render,
                    sample_renderer_path.after(RenderSystems::PrepareResourcesCollectPhaseBuffers),
                )
                .add_systems(
                    Render,
                    give_shadow_views_their_light_layers
                        .in_set(RenderSystems::CreateViews)
                        .after(prepare_lights),
                );
        }
    }
}

/// A shadow view Bevy created without render layers.
type UnlayeredShadowView = (With<ShadowView>, Without<RenderLayers>);

/// Gives each directional shadow cascade the render layers of the light that owns it.
///
/// Bevy 0.19 builds a cascade's shadow view without a `RenderLayers` component, and
/// `queue_shadows` tests every caster's layers against that view's, defaulting to layer 0 when it
/// has none. The CPU side (`check_dir_light_mesh_visibility`) does use the light's layers, so a
/// caster on [`PLACED_OBJECT_LAYER`] is collected for the cascade and then dropped before it is
/// queued: none of the placed objects - buildings, trees, rocks - would cast a shadow. Copying the
/// light's layers onto its views makes the queue test agree with the collection test.
fn give_shadow_views_their_light_layers(
    mut commands: Commands,
    views: Query<(Entity, &LightEntity), UnlayeredShadowView>,
    lights: Query<&ExtractedDirectionalLight>,
) {
    for (view, owner) in &views {
        let LightEntity::Directional { light_entity, .. } = owner else {
            continue;
        };
        if let Ok(light) = lights.get(*light_entity) {
            commands.entity(view).insert(light.render_layers.clone());
        }
    }
}

#[derive(Resource, Debug, Clone, Default, Serialize)]
pub struct RendererMetrics {
    pub metal_backend: bool,
    pub direct_drawing_views: u64,
    pub gpu_preprocessing_active: bool,
    pub gpu_culling_active: bool,
    pub indirect_drawing_active: bool,
    pub occlusion_culling_views: u64,
    pub hzb_views: u64,
    pub indirect_phase_buffers: u64,
    pub indirect_batch_sets: u64,
    pub proof_frames: u64,
    pub renderer_fixture_validated: bool,
    pub renderer_validation_failures: u64,
}

impl RendererMetrics {
    pub fn final_path_active(&self) -> bool {
        let direct_metal = self.metal_backend && self.direct_drawing_views > 0;
        (direct_metal
            || (self.gpu_preprocessing_active
                && self.gpu_culling_active
                && self.indirect_drawing_active
                && self.occlusion_culling_views > 0
                && self.hzb_views > 0
                && self.indirect_phase_buffers > 0
                && self.indirect_batch_sets > 0))
            && self.proof_frames > 0
            && self.renderer_validation_failures == 0
    }
}

#[derive(Default)]
struct RendererProofState {
    gpu_preprocessing: AtomicBool,
    gpu_culling: AtomicBool,
    indirect_drawing: AtomicBool,
    occlusion_views: AtomicU64,
    hzb_views: AtomicU64,
    indirect_phases: AtomicU64,
    indirect_batch_sets: AtomicU64,
    direct_views: AtomicU64,
    metal_backend: AtomicBool,
    frames: AtomicU64,
}

#[derive(Resource, Clone, Default)]
struct RendererProofBridge(Arc<RendererProofState>);

type DirectWorldViewFilter = (
    With<ExtractedCamera>,
    With<crate::world::components::StreamingCamera>,
    With<NoIndirectDrawing>,
);

fn sample_renderer_path(
    bridge: Res<RendererProofBridge>,
    support: Option<Res<GpuPreprocessingSupport>>,
    indirect: Option<Res<IndirectParametersBuffers>>,
    adapter: Option<Res<RenderAdapterInfo>>,
    views: Query<Option<&ViewDepthPyramid>, With<OcclusionCulling>>,
    direct_views: Query<(), DirectWorldViewFilter>,
) {
    let preprocessing = support
        .as_deref()
        .is_some_and(GpuPreprocessingSupport::is_available);
    let culling = support
        .as_deref()
        .is_some_and(|value| value.max_supported_mode == GpuPreprocessingMode::Culling);
    let (phase_count, batch_sets, indirect_active) =
        indirect.as_deref().map_or((0, 0, false), |buffers| {
            let phases = buffers.len() as u64;
            let batch_sets = buffers
                .values()
                .map(|phase| {
                    phase.batch_set_count(true) as u64 + phase.batch_set_count(false) as u64
                })
                .sum::<u64>();
            let active = buffers.values().any(|phase| {
                phase.indexed.data_buffer().is_some() || phase.non_indexed.data_buffer().is_some()
            });
            (phases, batch_sets, active)
        });
    let occlusion_views = views.iter().count() as u64;
    let hzb_views = views.iter().filter(|pyramid| pyramid.is_some()).count() as u64;
    let direct_views = direct_views.iter().count() as u64;
    let metal_backend = adapter.is_some_and(|value| value.backend == wgpu::Backend::Metal);
    bridge
        .0
        .gpu_preprocessing
        .fetch_or(preprocessing, Ordering::Relaxed);
    bridge.0.gpu_culling.fetch_or(culling, Ordering::Relaxed);
    bridge
        .0
        .indirect_drawing
        .fetch_or(indirect_active, Ordering::Relaxed);
    bridge
        .0
        .occlusion_views
        .fetch_max(occlusion_views, Ordering::Relaxed);
    bridge.0.hzb_views.fetch_max(hzb_views, Ordering::Relaxed);
    bridge
        .0
        .indirect_phases
        .fetch_max(phase_count, Ordering::Relaxed);
    bridge
        .0
        .indirect_batch_sets
        .fetch_max(batch_sets, Ordering::Relaxed);
    bridge
        .0
        .direct_views
        .fetch_max(direct_views, Ordering::Relaxed);
    bridge
        .0
        .metal_backend
        .fetch_or(metal_backend, Ordering::Relaxed);
    bridge.0.frames.fetch_add(1, Ordering::Relaxed);
}

fn sync_renderer_metrics(bridge: Res<RendererProofBridge>, mut metrics: ResMut<RendererMetrics>) {
    metrics.gpu_preprocessing_active = bridge.0.gpu_preprocessing.load(Ordering::Relaxed);
    metrics.gpu_culling_active = bridge.0.gpu_culling.load(Ordering::Relaxed);
    metrics.indirect_drawing_active = bridge.0.indirect_drawing.load(Ordering::Relaxed);
    metrics.occlusion_culling_views = bridge.0.occlusion_views.load(Ordering::Relaxed);
    metrics.hzb_views = bridge.0.hzb_views.load(Ordering::Relaxed);
    metrics.indirect_phase_buffers = bridge.0.indirect_phases.load(Ordering::Relaxed);
    metrics.indirect_batch_sets = bridge.0.indirect_batch_sets.load(Ordering::Relaxed);
    metrics.proof_frames = bridge.0.frames.load(Ordering::Relaxed);
    metrics.direct_drawing_views = bridge.0.direct_views.load(Ordering::Relaxed);
    metrics.metal_backend = bridge.0.metal_backend.load(Ordering::Relaxed);
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct TerrainExtension {
    #[uniform(121)]
    seam_settings: NearTerrainBlendSettings,
    #[texture(122, visibility(fragment))]
    #[sampler(123, visibility(fragment))]
    seam_overlay: Option<Handle<Image>>,
    #[texture(124, visibility(fragment))]
    #[sampler(125, visibility(fragment))]
    seam_noise: Option<Handle<Image>>,
    #[texture(100)]
    layer_0: Option<Handle<Image>>,
    #[texture(102)]
    layer_1: Option<Handle<Image>>,
    #[texture(104)]
    layer_2: Option<Handle<Image>>,
    #[texture(106)]
    layer_3: Option<Handle<Image>>,
    #[texture(108)]
    layer_4: Option<Handle<Image>>,
    #[texture(110)]
    layer_5: Option<Handle<Image>>,
    // LAND images share one repeating sampler. Use the first available color layer so an
    // untextured base does not select Bevy's clamping fallback sampler. AsBindGroup requires a
    // texture on the sampler's field; binding 119 supplies it without another image allocation.
    #[texture(119)]
    #[sampler(101)]
    sampler_source: Option<Handle<Image>>,
    #[uniform(112)]
    settings: TerrainSettings,
    // Each layer's normal map, sampled through `sampler_source`: every terrain layer image is
    // loaded with the same repeating sampler (`terrain_layer_sampler`), so the six need no sampler
    // bindings of their own. Which layers have one is in `TerrainSettings::normal_layers_*`, since
    // Bevy's stand-in for a missing texture is white, which decodes to a steep tilt.
    #[texture(113)]
    normal_0: Option<Handle<Image>>,
    #[texture(114)]
    normal_1: Option<Handle<Image>>,
    #[texture(115)]
    normal_2: Option<Handle<Image>>,
    #[texture(116)]
    normal_3: Option<Handle<Image>>,
    #[texture(117)]
    normal_4: Option<Handle<Image>>,
    #[texture(118)]
    normal_5: Option<Handle<Image>>,
}

#[derive(ShaderType, Reflect, Debug, Clone)]
struct TerrainSettings {
    tiling_and_layer_count: Vec4,
    quadrant_origin: Vec4,
    fallback_weights_0: Vec4,
    fallback_weights_1: Vec4,
    weight_source: Vec4,
    /// 1.0 in component `i` when layer `i` (0-3) has a normal map bound, else 0.0.
    normal_layers_0: Vec4,
    /// Layers 4 and 5 in x and y.
    normal_layers_1: Vec4,
    weights: [Vec4; WEIGHT_FIELD_WORDS],
}

/// The sample grid of one terrain quadrant: `17x17` opacities on the same `128`-unit spacing as the
/// quadrant's mesh columns, which is what LAND's `VTXT` entries index.
pub(crate) const QUADRANT_WEIGHT_SAMPLES: usize = 17;

/// Overlay layers a quadrant can carry beside its base: the runtime's cap, since
/// [`crate::streaming::quadrant_layers`] rejects a quadrant with more than six layers in total.
pub(crate) const OVERLAY_WEIGHT_SLOTS: usize = 5;

/// `Vec4`s per overlay in the weight field: one [`QUADRANT_WEIGHT_SAMPLES`]-square grid packed four
/// samples to a `Vec4` and rounded up to whole `Vec4`s, so every overlay starts on a 16-byte
/// boundary. The shader declares the same number (`WEIGHT_GRID_WORDS` in `terrain.wgsl`).
pub(crate) const OVERLAY_WEIGHT_WORDS: usize =
    (QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES).div_ceil(4);

/// The whole field one material carries: [`OVERLAY_WEIGHT_SLOTS`] overlays of
/// [`OVERLAY_WEIGHT_WORDS`] words each. The shader declares the same number as the length of its
/// `weights` array.
pub(crate) const WEIGHT_FIELD_WORDS: usize = OVERLAY_WEIGHT_SLOTS * OVERLAY_WEIGHT_WORDS;

/// The sampler a terrain layer texture needs: the shader tiles every layer `tiling` times across a
/// cell (`uv * LAND_TEXTURE_REPEATS_PER_CELL`), so the address mode must repeat.
/// Bevy's default sampler clamps to the edge,
/// which stretches a texture's last texel column, row and corner across everything past the first
/// tile - long streaks where the edge column is stretched, and one flat colour where the corner
/// texel covers the rest. A terrain texture that is not loaded through the asset server - the
/// synthetic fixtures build theirs in memory - has to be given the same sampler by hand.
pub(crate) fn terrain_layer_sampler() -> ImageSamplerDescriptor {
    ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        anisotropy_clamp: 16,
        ..ImageSamplerDescriptor::linear()
    }
}

/// Packs a quadrant's overlay weight grids into the field the material uniform carries: sample `s`
/// of overlay `o` lands in component `s % 4` of word `o * OVERLAY_WEIGHT_WORDS + s / 4`, which is
/// where `grid_weight` in `terrain.wgsl` reads it. Grids beyond [`OVERLAY_WEIGHT_SLOTS`] and
/// samples beyond one grid are dropped, so a malformed grid cannot write outside the field.
fn weight_field(overlay_weights: &[Vec<f32>]) -> [Vec4; WEIGHT_FIELD_WORDS] {
    let mut field = [Vec4::ZERO; WEIGHT_FIELD_WORDS];
    for (overlay, grid) in overlay_weights
        .iter()
        .take(OVERLAY_WEIGHT_SLOTS)
        .enumerate()
    {
        for (sample, weight) in grid
            .iter()
            .enumerate()
            .take(QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES)
        {
            field[overlay * OVERLAY_WEIGHT_WORDS + sample / 4][sample % 4] = *weight;
        }
    }
    field
}

impl TerrainSettings {
    /// Marks which layers have a normal map bound, for the shader to blend only those.
    fn set_normal_layers(&mut self, normals: &[Option<Handle<Image>>; 6]) {
        let present = |index: usize| if normals[index].is_some() { 1.0 } else { 0.0 };
        self.normal_layers_0 = Vec4::new(present(0), present(1), present(2), present(3));
        self.normal_layers_1 = Vec4::new(present(4), present(5), 0.0, 0.0);
    }

    /// The settings of one quadrant of `terrain`: the tiling the shader repeats every layer by, the
    /// quadrant's origin inside the cell, and its overlay weight field. The shader interpolates the
    /// field itself, so a weight reaches the fragment stage exactly as LAND records it instead of
    /// through a vertex attribute Bevy re-normalizes.
    fn for_quadrant(quadrant: u8, layers: usize, overlay_weights: &[Vec<f32>]) -> Self {
        Self {
            tiling_and_layer_count: Vec4::new(
                LAND_TEXTURE_REPEATS_PER_CELL,
                LAND_TEXTURE_REPEATS_PER_CELL,
                layers as f32,
                0.0,
            ),
            quadrant_origin: Vec4::new(f32::from(quadrant % 2), f32::from(quadrant / 2), 0.0, 0.0),
            fallback_weights_0: Vec4::X,
            fallback_weights_1: Vec4::ZERO,
            // A quadrant with no overlay has nothing in the field to read, so the shader is told to
            // skip it: the packed attributes carry the same (empty) overlays.
            normal_layers_0: Vec4::ZERO,
            normal_layers_1: Vec4::ZERO,
            weight_source: if overlay_weights.is_empty() {
                Vec4::ZERO
            } else {
                Vec4::X
            },
            weights: weight_field(overlay_weights),
        }
    }

    /// The settings of a material that carries no weight field, so the shader reads the weights the
    /// mesh packs into its vertex attributes instead. Only the stand-in terrain a synthetic scene
    /// draws without a LAND snapshot - the streaming and benchmark fixtures - uses it.
    fn vertex_weights_only(layers: f32) -> Self {
        Self {
            tiling_and_layer_count: Vec4::new(
                LAND_TEXTURE_REPEATS_PER_CELL,
                LAND_TEXTURE_REPEATS_PER_CELL,
                layers,
                0.0,
            ),
            quadrant_origin: Vec4::ZERO,
            fallback_weights_0: Vec4::X,
            fallback_weights_1: Vec4::ZERO,
            normal_layers_0: Vec4::ZERO,
            normal_layers_1: Vec4::ZERO,
            weight_source: Vec4::ZERO,
            weights: [Vec4::ZERO; WEIGHT_FIELD_WORDS],
        }
    }
}

/// The images a terrain quadrant's material waits on, by the colour space each
/// must decode to.
pub struct TerrainImages {
    /// The layers' diffuse images, sRGB.
    pub color: Vec<Handle<Image>>,
    /// The layers' normal maps, linear.
    pub normal: Vec<Handle<Image>>,
}

impl TerrainExtension {
    pub(crate) fn blend_differs(&self, blend: Option<&NearTerrainBlend>) -> bool {
        match blend {
            Some((settings, overlay, noise)) => {
                self.seam_settings != *settings
                    || self.seam_overlay.as_ref() != Some(overlay)
                    || self.seam_noise.as_ref() != Some(noise)
            }
            None => self.seam_overlay.is_some(),
        }
    }

    pub(crate) fn set_blend(&mut self, blend: Option<NearTerrainBlend>) {
        if let Some((settings, overlay, noise)) = blend {
            self.seam_settings = settings;
            self.seam_overlay = Some(overlay);
            self.seam_noise = Some(noise);
        } else {
            self.seam_settings = NearTerrainBlendSettings::default();
            self.seam_overlay = None;
            self.seam_noise = None;
        }
    }

    /// The layer and normal images, plus the atlas and noise while seam blending is bound.
    pub(crate) fn image_ids(&self) -> impl Iterator<Item = bevy::asset::AssetId<Image>> + '_ {
        [
            self.layer_0.as_ref(),
            self.layer_1.as_ref(),
            self.layer_2.as_ref(),
            self.layer_3.as_ref(),
            self.layer_4.as_ref(),
            self.layer_5.as_ref(),
            self.normal_0.as_ref(),
            self.normal_1.as_ref(),
            self.normal_2.as_ref(),
            self.normal_3.as_ref(),
            self.normal_4.as_ref(),
            self.normal_5.as_ref(),
            self.seam_overlay.as_ref(),
            self.seam_noise.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(Handle::id)
    }

    /// Binds the pack's configured default for a zero base ID. Zero overlays retain
    /// their slots and weights without images. Nonzero IDs with a missing diffuse retain
    /// their slots and weights without requesting either a diffuse or an orphan normal.
    pub fn from_quadrant(
        terrain: &TerrainSnapshot,
        quadrant: u8,
        catalog: &AssetCatalog,
        asset_server: &AssetServer,
        colour_srgb: bool,
    ) -> Result<(Self, TerrainImages), String> {
        let mut textures: [Option<Handle<Image>>; 6] = std::array::from_fn(|_| None);
        let mut normals: [Option<Handle<Image>>; 6] = std::array::from_fn(|_| None);
        let layers = crate::streaming::quadrant_layers(terrain, quadrant)?;
        let mut handles = Vec::with_capacity(layers.len());
        let mut normal_handles = Vec::new();
        for ((target, normal), layer) in textures.iter_mut().zip(normals.iter_mut()).zip(&layers) {
            if layer.texture_form_id == 0 && !layer.is_base {
                // A preserved unresolved overlay keeps its slot and weights with no image bound.
                continue;
            }
            if layer.texture_form_id == 0 && catalog.landscape_diffuse(0).is_none() {
                // An explicit white default (or an old pack) needs no image or normal.
                continue;
            }
            let Some(path) = catalog.landscape_diffuse(layer.texture_form_id) else {
                // Keep the unresolved layer's weight without loading an orphan normal map.
                continue;
            };
            if let Some(path) = catalog.landscape_normal(layer.texture_form_id) {
                // A normal map holds directions, not colours: it must not be decoded as sRGB.
                let handle = asset_server
                    .load_builder()
                    .with_settings(|settings: &mut ImageLoaderSettings| {
                        settings.sampler = ImageSampler::Descriptor(terrain_layer_sampler());
                        settings.is_srgb = false;
                    })
                    .load(path.to_owned());
                *normal = Some(handle.clone());
                normal_handles.push(handle);
            }
            let handle = asset_server
                .load_builder()
                .with_settings(move |settings: &mut ImageLoaderSettings| {
                    settings.sampler = ImageSampler::Descriptor(terrain_layer_sampler());
                    settings.is_srgb = colour_srgb;
                })
                .load(path.to_owned());
            *target = Some(handle.clone());
            handles.push(handle);
        }
        let overlay_weights = crate::streaming::quadrant_overlay_weights(terrain, quadrant)?;
        let mut settings = TerrainSettings::for_quadrant(quadrant, layers.len(), &overlay_weights);
        settings.set_normal_layers(&normals);
        Ok((
            Self {
                seam_settings: NearTerrainBlendSettings::default(),
                seam_overlay: None,
                seam_noise: None,
                layer_0: textures[0].clone(),
                layer_1: textures[1].clone(),
                layer_2: textures[2].clone(),
                layer_3: textures[3].clone(),
                layer_4: textures[4].clone(),
                layer_5: textures[5].clone(),
                sampler_source: textures.iter().flatten().next().cloned(),
                settings,
                normal_0: normals[0].clone(),
                normal_1: normals[1].clone(),
                normal_2: normals[2].clone(),
                normal_3: normals[3].clone(),
                normal_4: normals[4].clone(),
                normal_5: normals[5].clone(),
            },
            TerrainImages {
                color: handles,
                normal: normal_handles,
            },
        ))
    }

    /// The material of one quadrant of a synthetic `terrain`, drawn with textures a fixture already
    /// holds in memory. It carries the same settings a streamed quadrant does, so a fixture with
    /// overlay layers shows the weight field the shader interpolates rather than only the packed
    /// vertex attributes it falls back on.
    pub(crate) fn fixture(
        terrain: &TerrainSnapshot,
        quadrant: u8,
        textures: [Handle<Image>; 6],
    ) -> Result<Self, String> {
        let layers = crate::streaming::quadrant_layers(terrain, quadrant)?;
        let overlay_weights = crate::streaming::quadrant_overlay_weights(terrain, quadrant)?;
        Ok(Self {
            seam_settings: NearTerrainBlendSettings::default(),
            seam_overlay: None,
            seam_noise: None,
            layer_0: Some(textures[0].clone()),
            layer_1: Some(textures[1].clone()),
            layer_2: Some(textures[2].clone()),
            layer_3: Some(textures[3].clone()),
            layer_4: Some(textures[4].clone()),
            layer_5: Some(textures[5].clone()),
            sampler_source: Some(textures[0].clone()),
            settings: TerrainSettings::for_quadrant(quadrant, layers.len(), &overlay_weights),
            normal_0: None,
            normal_1: None,
            normal_2: None,
            normal_3: None,
            normal_4: None,
            normal_5: None,
        })
    }

    /// Whether the shader reads this material's overlay weights from its uniform weight field rather
    /// than from the weights the mesh packs into its vertex attributes. False only for a material
    /// with no overlays to read: a quadrant whose sole layer is its base, or a stand-in terrain.
    pub(crate) fn reads_weight_field(&self) -> bool {
        self.settings.weight_source.x > 0.5
    }

    /// Unbinds every layer's normal map, so the quadrant is lit by its geometric normal as a
    /// quadrant without normal maps is. Used when a normal map fails to load: a normal map is
    /// optional detail, and losing one must not hide the terrain its diffuse layers can draw.
    pub(crate) fn drop_normal_maps(&mut self) {
        let none: [Option<Handle<Image>>; 6] = Default::default();
        self.settings.set_normal_layers(&none);
        [
            self.normal_0,
            self.normal_1,
            self.normal_2,
            self.normal_3,
            self.normal_4,
            self.normal_5,
        ] = none;
    }

    /// Whether any layer has a normal map bound.
    #[cfg(test)]
    pub(crate) fn has_normal_maps(&self) -> bool {
        self.settings.normal_layers_0 != Vec4::ZERO || self.settings.normal_layers_1 != Vec4::ZERO
    }
}

impl Default for TerrainExtension {
    fn default() -> Self {
        Self {
            seam_settings: NearTerrainBlendSettings::default(),
            seam_overlay: None,
            seam_noise: None,
            layer_0: None,
            layer_1: None,
            layer_2: None,
            layer_3: None,
            layer_4: None,
            layer_5: None,
            sampler_source: None,
            settings: TerrainSettings::vertex_weights_only(0.0),
            normal_0: None,
            normal_1: None,
            normal_2: None,
            normal_3: None,
            normal_4: None,
            normal_5: None,
        }
    }
}

impl MaterialExtension for TerrainExtension {
    fn vertex_shader() -> ShaderRef {
        "embedded://engine/shaders/terrain.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "embedded://engine/shaders/terrain.wgsl".into()
    }
}

/// The WATR `DNAM` values that shape and move a water's noise layers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterNoiseValues {
    /// World units per tile of each layer (`uvScaleA`).
    pub uv_scale: [f32; 3],
    /// How much each layer tilts the normal (`amplitudeA`).
    pub amplitude: [f32; 3],
    /// Reflections, refraction, normals and specular lighting depth properties.
    pub depth_properties: [f32; 4],
    /// Compass bearing each layer's wind comes from, degrees (`noiseWindDirectionA`).
    pub wind_direction: [f32; 3],
    /// Tiles per second (`noiseWindSpeedA`).
    pub wind_speed: [f32; 3],
    /// World units (`noiseFalloff`, `DNAM` +0x60).
    pub noise_falloff: f32,
    /// Sun specular power, sun specular magnitude, sparkle power, sparkle magnitude (`DNAM` +0x10,
    /// +0xCC, +0xE0, +0xC8; RF-WATER-010).
    pub sun: [f32; 4],
    /// World units the wave normal moves the refracted point by (`DNAM` +0x98; RF-WATER-049).
    pub refraction_magnitude: f32,
}

impl WaterNoiseValues {
    /// No layers: a flat surface.
    pub fn flat() -> Self {
        Self {
            uv_scale: [1.0; 3],
            amplitude: [0.0; 3],
            depth_properties: [0.0; 4],
            wind_direction: [0.0; 3],
            wind_speed: [0.0; 3],
            noise_falloff: DEFAULT_WATER_NOISE_FALLOFF,
            sun: [0.0; 4],
            refraction_magnitude: DEFAULT_WATER_REFRACTION_MAGNITUDE,
        }
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct WaterExtension {
    #[uniform(100)]
    settings: WaterSettings,
    #[texture(101)]
    #[sampler(102)]
    reflection: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    flow_normal: Option<Handle<Image>>,
    #[texture(105, filterable = false)]
    bed_depth: Option<Handle<Image>>,
    /// The three noise normal maps (WATR `NAM2` to `NAM4`), read with the sampler of `flow_normal`.
    #[texture(106)]
    noise_normal_0: Option<Handle<Image>>,
    #[texture(107)]
    noise_normal_1: Option<Handle<Image>>,
    #[texture(108)]
    noise_normal_2: Option<Handle<Image>>,
    /// The cell's 64 by 64 flow map (RF-WATER-036): per texel the flow direction (rg), the scale (b)
    /// and the phase speed (a). Read by texel with `textureLoad`, so it is not filtered.
    #[texture(110, filterable = false)]
    flow_map: Option<Handle<Image>>,
    /// The flow maps of the three cells whose first texels the program's shifted taps read past
    /// this cell's east and south edges: east (x + 1, y), south (x, y - 1) and south-east
    /// (x + 1, y - 1). Retail reads them from its flow atlas, where geographic neighbours are
    /// adjacent (RF-WATER-063). `settings.flow.w` says which are bound.
    #[texture(111, filterable = false)]
    flow_map_east: Option<Handle<Image>>,
    #[texture(112, filterable = false)]
    flow_map_south: Option<Handle<Image>>,
    #[texture(113, filterable = false)]
    flow_map_south_east: Option<Handle<Image>>,
    /// The weather's Water Multiplier as 0..1 values, applied to the colours set after it.
    multiplier: [f32; 3],
}

#[derive(ShaderType, Reflect, Debug, Clone, PartialEq)]
struct WaterSettings {
    /// x = multiplier on the noise UV scales, y = multiplier on the wind speeds, z = multiplier on
    /// the amplitudes (all neutral at 1 and set from the user's settings; z is 0 when waves are
    /// off), w = pinned wave time in seconds, or negative to run on the frame clock.
    wave_scale_speed_strength: Vec4,
    /// xyz = the three noise layers' wind directions in radians (WATR DNAM `noiseWindDirectionA`,
    /// compass bearing the wind comes from); w unused.
    noise_wind_direction: Vec4,
    /// xyz = the three noise layers' wind speeds in tiles per second (DNAM `noiseWindSpeedA`).
    noise_wind_speed: Vec4,
    /// x = Fresnel Amount (Schlick F0), y = Reflectivity Amount, z = the user's reflection
    /// multiplier, w = Reflection Magnitude (all WATR DNAM except z).
    fresnel_reflectivity: Vec4,
    /// The water's Reflection Color (WATR DNAM), linear. What the reflection fades to where the
    /// reflection image has no coverage; alpha unused.
    reflection_color: Vec4,
    /// The water's Shallow Color (WATR DNAM), linear; alpha unused. The deep colour is the
    /// StandardMaterial base colour.
    shallow_color: Vec4,
    /// x = opacity 0..1 (WATR ANAM / 100), used where the depth is unknown; y = the above-water fog
    /// range (far - near, 1e-5 when zero), z = max(far, 1) in world units, w = the curve exponent
    /// max(4 * fog amount, 1) (WATR DNAM +0x20, +0x24, +0x84; RF-SHADER-006).
    body: Vec4,
    /// x = 1 when a snapshot texture is bound to `bed_depth`, 0 when none is and the body falls
    /// back to its ANAM opacity. It says nothing about whether this frame's depth is valid: a
    /// frame whose resolve was skipped leaves the texture bound but cleared to a negative sentinel,
    /// which the shader reads as unknown too. y = the record's noise falloff, clamped to
    /// `[1, 8191]` (RF-SHADER-007). z = 1 for a water in an interior cell, which uses retail's
    /// interior program (RF-WATER-017). w = 1 when the sky gradient below is set, 0 when the flat
    /// Reflection Color stands in for it.
    depth_params: Vec4,
    /// xyz = the three noise layers' UV scales in world units per texture tile (WATR DNAM
    /// `uvScaleA`); w = 1 when the noise textures are bound, 0 for a flat surface.
    noise_uv_scale: Vec4,
    /// xyz = the three noise layers' amplitudes (WATR DNAM `amplitudeA`).
    noise_amplitude: Vec4,
    /// x, y, z, w = the WATR DNAM depth properties: reflections, refraction, normals and specular
    /// lighting. How much of each effect is left at zero depth is `1 - value` (RF-WATER-048,
    /// RF-WATER-059); all four are used.
    depth_control: Vec4,
    /// x = sun specular power, y = its magnitude, z = sparkle power, w = its magnitude (WATR DNAM
    /// +0x10, +0xCC, +0xE0, +0xC8; RF-WATER-010).
    sun_terms: Vec4,
    /// rgb = the sun colour `S` the terms are multiplied by (linear); the weather's sunlight
    /// colour. The sun intensity `SunDir.w` is not traced and taken as 1 (#698). w unused.
    sun_color: Vec4,
    /// x = the refraction magnitude in world units (WATR DNAM +0x98; RF-WATER-049); yzw unused.
    refraction: Vec4,
    /// xy = the cell's grid coordinates, z = 1 when the water runs retail's flow-map program
    /// (FNAM bit 3 with a flow map and a flow normal texture bound), w = the bound neighbour flow
    /// maps as a bit set (1 east, 2 south, 4 south-east; RF-WATER-044, RF-WATER-063).
    flow: Vec4,
    /// The sky dome's colour table (`SkyUniform`): row elevations in degrees, four rows to a
    /// vector. The water reflects it where retail reflects its sky cube (see `with_sky`).
    sky_elevations: [Vec4; 4],
    /// Per row: the sRGB-encoded dome colour and the row's alpha.
    sky_colours: [Vec4; 16],
    /// x = the dome's linear brightness scale.
    sky_brightness: Vec4,
}

impl Default for WaterExtension {
    fn default() -> Self {
        Self {
            settings: WaterSettings {
                wave_scale_speed_strength: Vec4::new(1.0, 1.0, 1.0, ANIMATED_WAVE_PHASE),
                noise_wind_direction: Vec4::ZERO,
                noise_wind_speed: Vec4::ZERO,
                fresnel_reflectivity: Vec4::new(
                    DEFAULT_WATER_FRESNEL,
                    DEFAULT_WATER_REFLECTIVITY,
                    1.0,
                    crate::world::database::DEFAULT_WATER_REFLECTION_MAGNITUDE,
                ),
                reflection_color: linear_rgb(water_srgb(DEFAULT_WATER_REFLECTION_COLOR, [1.0; 3])),
                shallow_color: linear_rgb(water_srgb(DEFAULT_WATER_SHALLOW_COLOR, [1.0; 3])),
                body: default_water_body(),
                depth_params: Vec4::new(0.0, DEFAULT_WATER_NOISE_FALLOFF, 0.0, 0.0),
                noise_uv_scale: Vec4::ZERO,
                noise_amplitude: Vec4::ZERO,
                depth_control: Vec4::ZERO,
                sun_terms: Vec4::ZERO,
                sun_color: Vec4::ZERO,
                refraction: Vec4::new(DEFAULT_WATER_REFRACTION_MAGNITUDE, 0.0, 0.0, 0.0),
                flow: Vec4::ZERO,
                sky_elevations: [Vec4::ZERO; 4],
                sky_colours: [Vec4::ZERO; 16],
                sky_brightness: Vec4::ZERO,
            },
            reflection: None,
            flow_normal: None,
            bed_depth: None,
            noise_normal_0: None,
            noise_normal_1: None,
            noise_normal_2: None,
            flow_map: None,
            flow_map_east: None,
            flow_map_south: None,
            flow_map_south_east: None,
            multiplier: [1.0; 3],
        }
    }
}

impl WaterExtension {
    /// Per-cell dependencies. The shared reflection target belongs to the
    /// renderer baseline and must not keep a retired water cell reserved.
    pub(crate) fn image_ids(&self) -> impl Iterator<Item = bevy::asset::AssetId<Image>> + '_ {
        self.flow_normal
            .iter()
            .chain(self.noise_normal_0.iter())
            .chain(self.noise_normal_1.iter())
            .chain(self.noise_normal_2.iter())
            .chain(self.flow_map.iter())
            .chain(self.flow_map_east.iter())
            .chain(self.flow_map_south.iter())
            .chain(self.flow_map_south_east.iter())
            .map(Handle::id)
    }

    /// Builds a water material with Skyrim's DefaultWater fresnel and reflectivity. Callers that
    /// know a water's own factors (from [`crate::world::database::AssetCatalog::water_colors`])
    /// should use [`Self::with_reflection_and_factors`] instead.
    pub fn with_reflection(reflection: Handle<Image>, flow_normal: Option<Handle<Image>>) -> Self {
        Self::with_reflection_and_factors(
            reflection,
            flow_normal,
            DEFAULT_WATER_FRESNEL,
            DEFAULT_WATER_REFLECTIVITY,
        )
    }

    pub fn with_reflection_and_factors(
        reflection: Handle<Image>,
        flow_normal: Option<Handle<Image>>,
        fresnel: f32,
        reflectivity: f32,
    ) -> Self {
        Self {
            reflection: Some(reflection),
            flow_normal,
            settings: WaterSettings {
                wave_scale_speed_strength: Vec4::new(1.0, 1.0, 1.0, ANIMATED_WAVE_PHASE),
                noise_wind_direction: Vec4::ZERO,
                noise_wind_speed: Vec4::ZERO,
                // z gates the reflection strength; 1.0 keeps reflections on until settings set it.
                fresnel_reflectivity: Vec4::new(
                    fresnel,
                    reflectivity,
                    1.0,
                    crate::world::database::DEFAULT_WATER_REFLECTION_MAGNITUDE,
                ),
                reflection_color: linear_rgb(water_srgb(DEFAULT_WATER_REFLECTION_COLOR, [1.0; 3])),
                shallow_color: linear_rgb(water_srgb(DEFAULT_WATER_SHALLOW_COLOR, [1.0; 3])),
                body: default_water_body(),
                depth_params: Vec4::new(0.0, DEFAULT_WATER_NOISE_FALLOFF, 0.0, 0.0),
                noise_uv_scale: Vec4::ZERO,
                noise_amplitude: Vec4::ZERO,
                depth_control: Vec4::ZERO,
                sun_terms: Vec4::ZERO,
                sun_color: Vec4::ZERO,
                refraction: Vec4::new(DEFAULT_WATER_REFRACTION_MAGNITUDE, 0.0, 0.0, 0.0),
                flow: Vec4::ZERO,
                sky_elevations: [Vec4::ZERO; 4],
                sky_colours: [Vec4::ZERO; 16],
                sky_brightness: Vec4::ZERO,
            },
            bed_depth: None,
            noise_normal_0: None,
            noise_normal_1: None,
            noise_normal_2: None,
            flow_map: None,
            flow_map_east: None,
            flow_map_south: None,
            flow_map_south_east: None,
            multiplier: [1.0; 3],
        }
    }

    /// x = opacity 0..1, y/z = above-water fog near and far, w = fog amount, as the shader reads them.
    #[cfg(test)]
    pub(crate) fn body(&self) -> Vec4 {
        self.settings.body
    }

    /// The depth snapshot this material reads the bed from, when it has one.
    pub(crate) fn bed_depth(&self) -> Option<&Handle<Image>> {
        self.bed_depth.as_ref()
    }

    /// Points the material at a new bed-depth snapshot, or at the constant fallback when `None`.
    /// Binding a texture only enables the lookup; per-frame validity is carried by the texels.
    pub(crate) fn set_bed_depth(&mut self, handle: Option<Handle<Image>>) {
        self.settings.depth_params.x = if handle.is_some() { 1.0 } else { 0.0 };
        self.bed_depth = handle;
    }

    /// Runs retail's flow-map program (RF-WATER-044) for the cell `grid`, over its flow map, or
    /// stops it with `None`. It needs the flow normal texture (WATR NAM5) too, so call it after
    /// the water has one; without both the water keeps its noise layers.
    pub fn with_flow(mut self, grid: IVec2, flow_map: Option<Handle<Image>>) -> Self {
        let active = flow_map.is_some() && self.flow_normal.is_some();
        self.settings.flow = Vec4::new(
            grid.x as f32,
            grid.y as f32,
            f32::from(u8::from(active)),
            0.0,
        );
        self.flow_map = flow_map;
        // Neighbours belong to a flow map: dropping it drops them, and `with_flow_neighbours`
        // adds them back.
        self.flow_map_east = None;
        self.flow_map_south = None;
        self.flow_map_south_east = None;
        self.settings.flow.w = 0.0;
        self
    }

    /// Binds the flow maps of the east, south and south-east neighbour cells (see
    /// [`water_flow_neighbour_cells`]); a `None` leaves the program clamping to this cell's own
    /// edge texels on that side. Call it after [`Self::with_flow`].
    pub fn with_flow_neighbours(mut self, neighbours: [Option<Handle<Image>>; 3]) -> Self {
        let [east, south, south_east] = neighbours;
        let mask = u8::from(east.is_some())
            | (u8::from(south.is_some()) << 1)
            | (u8::from(south_east.is_some()) << 2);
        self.settings.flow.w = f32::from(mask);
        self.flow_map_east = east;
        self.flow_map_south = south;
        self.flow_map_south_east = south_east;
        self
    }

    /// Gives the water the sky it reflects, or none (the flat Reflection Color then stands in).
    /// Retail's exterior water reflects a sky cube: the worldspace names one, and when that name is
    /// not a cube (Tamriel's is a 2D path) the constructor falls back to a rendered six-face cube of
    /// the sky (RF-WATER-007; `Cubemaps\QuickSky_e.dds` only when no name exists). The engine
    /// renders no cube, so the reflection is the sky dome's own colour for the reflected direction:
    /// the dome the camera draws, as a rendered cube of it would show, without clouds or sun disc.
    /// The 32 by 32 `quicksky_e` cube was tried first and read as coarse rectangular blocks (its
    /// BC1 texels, magnified), so it is not used (#698).
    pub fn with_sky(mut self, sky: Option<&crate::sky::SkyUniform>) -> Self {
        self.settings.depth_params.w = f32::from(u8::from(sky.is_some()));
        if let Some(sky) = sky {
            self.settings.sky_elevations = sky.elevations;
            self.settings.sky_colours = sky.colours;
            self.settings.sky_brightness = sky.brightness;
        }
        self
    }

    /// Whether the water reflects a sky (an exterior water; an interior one has none).
    pub(crate) fn reflects_sky(&self) -> bool {
        self.settings.depth_params.w == 1.0
    }

    /// Whether the water already holds exactly this sky (the sync system writes only on a change).
    pub(crate) fn sky_matches(&self, sky: &crate::sky::SkyUniform) -> bool {
        self.settings.depth_params.w == 1.0
            && self.settings.sky_elevations == sky.elevations
            && self.settings.sky_colours == sky.colours
            && self.settings.sky_brightness == sky.brightness
    }

    /// Gives the water its three noise normal maps and the DNAM values that shape and move them.
    /// A layer without a texture adds no tilt; with none, the surface is flat.
    pub fn with_noise(
        mut self,
        textures: [Option<Handle<Image>>; 3],
        values: WaterNoiseValues,
    ) -> Self {
        let has = |texture: &Option<Handle<Image>>| texture.is_some() as u8 as f32;
        // A missing layer is a zero amplitude, so the shader can sample all three unconditionally.
        let amplitude = [
            values.amplitude[0] * has(&textures[0]),
            values.amplitude[1] * has(&textures[1]),
            values.amplitude[2] * has(&textures[2]),
        ];
        let bound = textures.iter().any(Option::is_some);
        let scale = values.uv_scale;
        self.settings.noise_uv_scale = Vec4::new(scale[0], scale[1], scale[2], bound as u8 as f32);
        self.settings.noise_amplitude = Vec4::new(amplitude[0], amplitude[1], amplitude[2], 0.0);
        self.settings.depth_control = Vec4::from_array(values.depth_properties);
        let radians = values
            .wind_direction
            .map(|degrees| degrees * WATER_DEGREES_TO_RADIANS);
        self.settings.noise_wind_direction = Vec4::new(radians[0], radians[1], radians[2], 0.0);
        let speed = values.wind_speed;
        self.settings.noise_wind_speed = Vec4::new(speed[0], speed[1], speed[2], 0.0);
        // The retail clamp of the noise falloff (RF-SHADER-007).
        self.settings.depth_params.y = values.noise_falloff.clamp(1.0, 8191.0);
        self.settings.sun_terms = Vec4::from_array(values.sun);
        self.settings.refraction.x = values.refraction_magnitude;
        let [first, second, third] = textures;
        self.noise_normal_0 = first;
        self.noise_normal_1 = second;
        self.noise_normal_2 = third;
        self
    }

    /// Shallow colour, Reflection Color, sun colour (linear) and the interior flag, for tests.
    #[cfg(test)]
    pub(crate) fn colours(&self) -> (Vec4, Vec4, Vec4, f32) {
        (
            self.settings.shallow_color,
            self.settings.reflection_color,
            self.settings.sun_color,
            self.settings.depth_params.z,
        )
    }

    /// The noise layers' UV scales and amplitudes as the shader reads them, for tests.
    #[cfg(test)]
    pub(crate) fn noise(&self) -> (Vec4, Vec4) {
        (self.settings.noise_uv_scale, self.settings.noise_amplitude)
    }

    /// Sets the sun colour the sun terms are multiplied by: the weather's sunlight colour
    /// (RF-WATER-010, `S = SunDir.w * SunColor`, with `SunDir.w` taken as 1).
    pub fn with_sun_color(mut self, color: Color) -> Self {
        let linear = color.to_linear();
        self.settings.sun_color = Vec4::new(linear.red, linear.green, linear.blue, 0.0);
        self
    }

    /// Marks the water as one in an interior cell: retail draws it with its interior program, which
    /// has no sun, no depth, no refraction, no reflection image and no light (RF-WATER-017).
    pub fn with_interior(mut self, interior: bool) -> Self {
        self.settings.depth_params.z = f32::from(u8::from(interior));
        self
    }

    /// Sets the weather's Water Multiplier (bytes). Call it before the colour setters: they apply it.
    pub fn with_water_multiplier(mut self, multiplier: [u8; 3]) -> Self {
        self.multiplier = multiplier.map(|byte| f32::from(byte) / 255.0);
        self
    }

    /// Sets the water's Reflection Magnitude (WATR DNAM): how much of the wave normal reaches the
    /// reflection.
    pub fn with_reflection_magnitude(mut self, magnitude: f32) -> Self {
        self.settings.fresnel_reflectivity.w = magnitude;
        self
    }

    /// Sets the water's own Reflection Color (WATR DNAM, as stored bytes).
    pub fn with_reflection_color(mut self, rgb: [u8; 3]) -> Self {
        self.settings.reflection_color = linear_rgb(water_srgb(rgb, self.multiplier));
        self
    }

    /// Sets the water's Shallow Color (stored sRGB bytes), opacity (0-100) and above-water fog.
    pub fn with_body(mut self, shallow: [u8; 3], opacity: u8, fog: WaterFog) -> Self {
        self.settings.shallow_color = linear_rgb(water_srgb(shallow, self.multiplier));
        self.settings.body = water_body(opacity, fog);
        self
    }
}

impl MaterialExtension for WaterExtension {
    fn fragment_shader() -> ShaderRef {
        "embedded://engine/shaders/water.wgsl".into()
    }
}

/// The wave phase a capture pins, in seconds: `--water-time` when the run sets it, and the capture
/// default for a terrain/water fixture or a shot run. `None` leaves an interactive run's waves on
/// the shader clock.
fn water_phase(config: Option<&crate::config::EngineConfig>) -> Option<f32> {
    config.and_then(|config| {
        config
            .water_time
            .or_else(|| (config.terrain_water_fixture || config.shots.is_some()).then_some(1.0))
    })
}

/// The settings a water material should carry: the resolved phase and the live rendering settings,
/// with every other field left as the material set it.
fn wanted_water_settings(
    current: &WaterExtension,
    phase: f32,
    settings: Option<&crate::settings::RenderingSettings>,
) -> WaterSettings {
    let mut wanted = current.settings.clone();
    wanted.wave_scale_speed_strength.w = phase;
    if let Some(settings) = settings {
        // The user's wave scale and strength are multipliers on the water's own values, neutral at
        // their defaults.
        wanted.wave_scale_speed_strength.x = settings.wave_scale / DEFAULT_WAVE_SCALE;
        wanted.wave_scale_speed_strength.y = settings.wave_speed / DEFAULT_WAVE_SPEED;
        wanted.wave_scale_speed_strength.z = if settings.water_waves {
            settings.wave_strength / WAVE_STRENGTH
        } else {
            0.0
        };
        wanted.fresnel_reflectivity.z = if settings.water_reflections {
            settings.water_reflection_strength
        } else {
            0.0
        };
    }
    wanted
}

/// Writes the phase and the live water settings into each water material, touching a material only
/// when a value actually changes: the asset is re-extracted and re-uploaded on every write, and an
/// interactive frame keeps the phase at the sentinel the shader runs on its own clock.
fn animate_water_materials(
    config: Option<Res<crate::config::EngineConfig>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut profiler: ResMut<ProfilingState>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
    mut ids: Local<Vec<AssetId<WaterMaterial>>>,
) {
    let started = std::time::Instant::now();
    let phase = water_phase(config.as_deref()).unwrap_or(ANIMATED_WAVE_PHASE);
    // Reading the assets by id keeps an unchanged material from being marked changed; the ids sit
    // in a buffer the system keeps, so a steady frame allocates nothing.
    ids.clear();
    ids.extend(materials.ids());
    for &id in ids.iter() {
        let Some(current) = materials.get(id) else {
            continue;
        };
        let wanted = wanted_water_settings(&current.extension, phase, settings.as_deref());
        if wanted == current.extension.settings {
            continue;
        }
        if let Some(mut material) = materials.get_mut(id) {
            material.extension.settings = wanted;
        }
    }
    profiler.record_elapsed("render/water_animation", started);
}

#[derive(Resource, Clone)]
pub struct WaterReflectionTexture(pub Handle<Image>);

#[derive(Component)]
pub(crate) struct WaterReflectionCamera;

/// Slack added to a water plane's bounds before the main camera frustum test, in world units (4096
/// to a cell).
///
/// The frustum carried by the main camera was built at the end of the previous frame, so a fast
/// turn brings water into a view the gate has already closed for, and the frame that renders it
/// shows the reflection of the frame before. A plane this far outside the frustum still keeps the
/// reflection camera on: seen from one cell away, it buys about seven degrees of extra turn per
/// frame - a flick past 400 degrees a second - or, when the camera moves instead of turning, 512
/// units of travel between two frames. A plane that far off the edge of a 45 degree view is a
/// sliver of the frame, so little of the saving is given back.
const WATER_REFLECTION_FRUSTUM_MARGIN: f32 = 512.0;

/// Spawns the camera that fills [`WaterReflectionTexture`], and the texture it renders into.
///
/// It is the flipped copy of the streaming camera, drawn before the main view, and it renders
/// [`REFLECTION_VIEW_LAYERS`]: the world layer only. What Skyrim puts in a water reflection, and
/// why full-detail placed objects are not in it, is documented on that constant.
fn setup_water_reflection(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = images.add(Image::new_target_texture(
        1024,
        576,
        bevy::render::render_resource::TextureFormat::Rgba16Float,
        None,
    ));
    commands.insert_resource(WaterReflectionTexture(image.clone()));
    commands.spawn((
        Camera3d::default(),
        SceneColorPipeline::reflection(),
        Camera {
            order: -1,
            // The camera is placed below the water looking up, not mirrored, so its triangles keep
            // their winding: inverting the culling drew the back faces.
            invert_culling: false,
            is_active: false,
            // Nothing drawn means alpha 0, so the water can tell "no reflected geometry" from
            // reflected geometry and fall back to its Reflection Color there.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(image.into()),
        Transform::default(),
        DepthPrepass,
        OcclusionCulling,
        RenderLayers::from_layers(REFLECTION_VIEW_LAYERS),
        // The mirrored terrain carries the same scene fog as the direct view. Retail reads the
        // already-fogged frame for its screen-space reflection (RF-WATER-009, RF-WATER-011: the
        // fog is the scene fog). That the reflection should be fogged is REASONED from the SSR
        // source, not traced: no reflection-fog setting was found (medium confidence).
        crate::sky::FogCamera,
        WaterReflectionCamera,
    ));
}

/// The streamed world camera, never the reflection camera.
type WaterReflectionObserver = (
    With<crate::world::components::StreamingCamera>,
    Without<WaterReflectionCamera>,
);

/// What the gate drives on the reflection camera: its entity, pose, switch, exposure and lens.
type WaterReflectionCameraParts = (
    Entity,
    &'static mut Transform,
    &'static mut Camera,
    Option<&'static mut Exposure>,
    Option<&'static mut Projection>,
);

/// What the gate reads off the world camera: its pose, the [`Frustum`] Bevy keeps in step with it,
/// and the projection whose far distance the frustum itself does not carry.
type WaterReflectionObserverView = (
    &'static GlobalTransform,
    Option<&'static Frustum>,
    Option<&'static Projection>,
    Option<&'static Exposure>,
);

fn update_water_reflection_camera(
    mut commands: Commands,
    main_camera: Query<WaterReflectionObserverView, WaterReflectionObserver>,
    water: Query<(&GlobalTransform, Option<&Aabb>), With<crate::world::components::WaterSurface>>,
    mut reflection_camera: Query<WaterReflectionCameraParts, With<WaterReflectionCamera>>,
    mut profiler: ResMut<ProfilingState>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
) {
    let started = std::time::Instant::now();
    let (
        Ok((main, frustum, projection, exposure)),
        Ok((entity, mut reflection, mut camera, reflection_exposure, reflection_projection)),
    ) = (main_camera.single(), reflection_camera.single_mut())
    else {
        return;
    };
    // PBR lighting is already exposure-scaled when the reflection is sampled by water.
    // Keep both views in the same domain, including explicit diagnostic exposure changes.
    let ev100 = exposure.map_or(DEFAULT_SCENE_EV100, |value| value.ev100);
    if let Some(mut reflection_exposure) = reflection_exposure {
        if reflection_exposure.ev100 != ev100 {
            reflection_exposure.ev100 = ev100;
        }
    } else {
        commands.entity(entity).insert(Exposure { ev100 });
    }
    // The surface nearest the main camera fixes the mirror plane; a further surface would put the
    // reflection at the wrong height when more than one water level is streamed in.
    let mut mirror_surface = None;
    let mut mirror_distance = f32::INFINITY;
    let mut surface_in_view = false;
    for (surface, bounds) in &water {
        let distance = (surface.translation().y - main.translation().y).abs();
        if distance < mirror_distance {
            mirror_distance = distance;
            mirror_surface = Some(surface);
        }
        surface_in_view |= water_plane_in_view(
            main,
            frustum,
            projection.map(|projection| projection.far()),
            surface,
            bounds,
        );
    }
    let Some(surface) = mirror_surface else {
        if camera.is_active {
            camera.is_active = false;
        }
        return;
    };
    // The mirror plane is refreshed even while the view is gated off, so the frame the gate reopens
    // reflects the camera as it stands then rather than the last frame water was on screen.
    reflection.set_if_neq(reflected_camera_transform(main, surface.translation().y));
    // The reflection image is sampled with the main view's screen coordinates, so it has to be
    // rendered with the main camera's lens, and with everything below the water out of it.
    if let (Some(Projection::Perspective(main_lens)), Some(mut reflection_projection)) =
        (projection, reflection_projection)
    {
        *reflection_projection = Projection::Perspective(reflection_lens(
            main_lens,
            &reflection,
            surface.translation().y,
        ));
    }
    // The Graphics setting switches the reflection off without touching the mirror plane above.
    let active = surface_in_view && settings.is_none_or(|settings| settings.water_reflections);
    if camera.is_active != active {
        camera.is_active = active;
    }
    profiler.record_elapsed("render/water_reflection_camera", started);
}

/// Whether a water plane's bounds reach the main camera's view, its bounds grown by
/// [`WATER_REFLECTION_FRUSTUM_MARGIN`] first.
///
/// The frustum half is Bevy's own test against the plane's [`Aabb`]. Bevy's perspective projection
/// is infinite reverse-z, so the [`Frustum`] carries no far plane at all (`from_clip_from_world`
/// leaves its last half space at `(NaN, NaN, NaN, inf)`), and the far distance of the camera's
/// [`Projection`] is applied here by hand: the plane's nearest reach along the camera's forward
/// axis, from its bounds projected onto that axis.
///
/// Missing pieces mean the test cannot run - a plane whose mesh has not produced an [`Aabb`] yet,
/// or a main camera without a [`Frustum`] - and the plane then counts as visible, keeping the
/// reflection camera on rather than risk a stale reflection.
fn water_plane_in_view(
    main: &GlobalTransform,
    frustum: Option<&Frustum>,
    far: Option<f32>,
    surface: &GlobalTransform,
    bounds: Option<&Aabb>,
) -> bool {
    let (Some(frustum), Some(far), Some(bounds)) = (frustum, far, bounds) else {
        return true;
    };
    let margin = Vec3::splat(WATER_REFLECTION_FRUSTUM_MARGIN);
    let grown = Aabb::from_min_max(
        Vec3::from(bounds.min()) - margin,
        Vec3::from(bounds.max()) + margin,
    );
    let surface_to_world = surface.affine();
    let centre = Vec3::from(surface_to_world.transform_point3a(grown.center));
    let radius = grown.relative_radius(&main.forward().as_vec3().into(), &surface_to_world.matrix3);
    if (centre - main.translation()).dot(*main.forward()) - radius > far {
        return false;
    }
    frustum.intersects_obb(&grown, &surface_to_world, true, true)
}

/// The reflection camera's lens: the main camera's, clipped at the water plane.
///
/// The reflection camera sits below the surface looking up, so the river bed and the banks lie
/// between it and the sky and would fill the image (the white sheet). Clipping at the water plane
/// keeps only what is above it, as a real mirror shows. Bevy's own mirror example does the same with
/// [`PerspectiveProjection::near_clip_plane`]: the plane's normal in the camera's view space, and
/// the negative distance to it as the fourth component.
fn reflection_lens(
    main_lens: &PerspectiveProjection,
    reflection: &Transform,
    water_y: f32,
) -> PerspectiveProjection {
    // Keep the side the main camera looks at the water from: above it, unless the main camera is
    // underwater (its mirrored camera then sits above the surface).
    let height_above_camera = water_y - reflection.translation.y;
    let normal_world = if height_above_camera >= 0.0 {
        Vec3::Y
    } else {
        Vec3::NEG_Y
    };
    let normal_view = reflection.rotation.inverse() * normal_world;
    PerspectiveProjection {
        near_clip_plane: normal_view.extend(-height_above_camera.abs()),
        ..*main_lens
    }
}

fn reflected_camera_transform(main: &GlobalTransform, water_y: f32) -> Transform {
    let mut position = main.translation();
    position.y = water_y * 2.0 - position.y;
    let mut forward = main.forward().as_vec3();
    forward.y = -forward.y;
    Transform::from_translation(position).looking_to(forward, Vec3::Y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_reservation_observes_layers_and_bound_seam_images() {
        let mut images = Assets::<Image>::default();
        let handles: [Handle<Image>; 12] = std::array::from_fn(|_| images.add(Image::default()));
        let mut extension = TerrainExtension {
            layer_0: Some(handles[0].clone()),
            layer_1: Some(handles[1].clone()),
            layer_2: Some(handles[2].clone()),
            layer_3: Some(handles[3].clone()),
            layer_4: Some(handles[4].clone()),
            layer_5: Some(handles[5].clone()),
            normal_0: Some(handles[6].clone()),
            normal_1: Some(handles[7].clone()),
            normal_2: Some(handles[8].clone()),
            normal_3: Some(handles[9].clone()),
            normal_4: Some(handles[10].clone()),
            normal_5: Some(handles[11].clone()),
            ..default()
        };
        let layer_ids: Vec<_> = handles.iter().map(Handle::id).collect();
        assert_eq!(extension.image_ids().collect::<Vec<_>>(), layer_ids);

        let overlay = images.add(Image::default());
        let noise = images.add(Image::default());
        let mut blended_ids = layer_ids.clone();
        blended_ids.extend([overlay.id(), noise.id()]);
        extension.set_blend(Some((NearTerrainBlendSettings::default(), overlay, noise)));
        assert_eq!(extension.image_ids().collect::<Vec<_>>(), blended_ids);

        extension.set_blend(None);
        assert_eq!(extension.image_ids().collect::<Vec<_>>(), layer_ids);
    }
    use crate::world::components::{StreamingCamera, WaterSurface};

    const MAIN_CAMERA_FAR: f32 = 1000.0;
    /// The mesh bounds of the water plane streaming.rs spawns: a `Plane3d` quad a cell wide with no
    /// thickness at all.
    const CELL_WATER_HALF_EXTENTS: Vec3 = Vec3::new(2048.0, 0.0, 2048.0);

    /// The main camera and the reflection camera the gate drives, with no plugins. The gate reads
    /// the camera's pose, frustum and far distance and writes `Camera::is_active` on the reflection
    /// camera, so a headless `App` is enough to run its frames.
    struct ReflectionHarness {
        app: App,
        main_camera: Entity,
        reflection_camera: Entity,
    }

    impl ReflectionHarness {
        fn new(main: Transform) -> Self {
            let mut app = App::new();
            app.init_resource::<ProfilingState>()
                .add_systems(Update, update_water_reflection_camera);
            let main_camera = app
                .world_mut()
                .spawn((
                    GlobalTransform::from(main),
                    main_camera_frustum(&main),
                    main_camera_projection(),
                    StreamingCamera,
                ))
                .id();
            let reflection_camera = app
                .world_mut()
                .spawn((
                    Camera {
                        order: -1,
                        is_active: false,
                        ..default()
                    },
                    Transform::default(),
                    Projection::Perspective(PerspectiveProjection::default()),
                    SceneColorPipeline::reflection(),
                    WaterReflectionCamera,
                ))
                .id();
            Self {
                app,
                main_camera,
                reflection_camera,
            }
        }

        /// Turns the main camera and the frustum Bevy keeps in step with it.
        fn aim(&mut self, main: Transform) {
            self.app
                .world_mut()
                .entity_mut(self.main_camera)
                .insert((GlobalTransform::from(main), main_camera_frustum(&main)));
        }

        fn spawn_water(&mut self, translation: Vec3, half_extents: Vec3) {
            self.app.world_mut().spawn((
                GlobalTransform::from(Transform::from_translation(translation)),
                water_bounds(half_extents),
                WaterSurface,
            ));
        }

        fn frame(&mut self) -> ReflectionFrame {
            self.app.update();
            let world = self.app.world();
            let camera = world.get::<Camera>(self.reflection_camera).unwrap();
            let transform = world.get::<Transform>(self.reflection_camera).unwrap();
            ReflectionFrame {
                active: camera.is_active,
                transform: *transform,
            }
        }
    }

    struct ReflectionFrame {
        active: bool,
        transform: Transform,
    }

    fn main_camera_projection() -> Projection {
        Projection::Perspective(PerspectiveProjection {
            far: MAIN_CAMERA_FAR,
            ..default()
        })
    }

    /// The `Frustum` `update_frusta` derives from a camera's projection and pose.
    fn main_camera_frustum(main: &Transform) -> Frustum {
        let projection = main_camera_projection();
        let global = GlobalTransform::from(*main);
        let clip_from_world = projection.get_clip_from_view() * global.to_matrix().inverse();
        Frustum(ViewFrustum::from_clip_from_world(&clip_from_world))
    }

    /// A water plane's bounds in its own space, as `calculate_bounds` leaves them on the entity.
    fn water_bounds(half_extents: Vec3) -> Aabb {
        Aabb::from_min_max(-half_extents, half_extents)
    }

    #[test]
    fn a_spawned_camera_carries_the_components_the_gate_reads() {
        let mut app = App::new();
        let camera = app
            .world_mut()
            .spawn((Camera3d::default(), StreamingCamera))
            .id();
        let world = app.world();
        assert!(world.get::<Frustum>(camera).is_some());
        assert!(world.get::<Projection>(camera).is_some());
    }

    #[test]
    fn reflection_target_preserves_linear_hdr_until_the_main_view() {
        use bevy::camera::Hdr;
        use bevy::core_pipeline::tonemapping::Tonemapping;
        use bevy::render::render_resource::TextureFormat;

        let mut app = App::new();
        app.init_resource::<Assets<Image>>()
            .add_systems(Startup, setup_water_reflection);
        app.update();
        let target = &app.world().resource::<WaterReflectionTexture>().0;
        let image = app.world().resource::<Assets<Image>>().get(target).unwrap();
        assert_eq!(image.texture_descriptor.format, TextureFormat::Rgba16Float);
        assert!(image.texture_view_descriptor.is_none());
        let mut cameras = app
            .world_mut()
            .query_filtered::<(&Hdr, &Tonemapping, &Exposure), With<WaterReflectionCamera>>();
        let (_, tonemapping, exposure) = cameras.single(app.world()).unwrap();
        assert_eq!(*tonemapping, Tonemapping::None);
        assert_eq!(exposure.ev100, 9.7);
    }

    #[test]
    fn reflection_tracks_exposure_changes_even_while_not_visible() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        for ev100 in [7.0, 12.0, 9.7] {
            harness
                .app
                .world_mut()
                .entity_mut(harness.main_camera)
                .insert(Exposure { ev100 });
            assert!(!harness.frame().active);
            assert_eq!(
                harness
                    .app
                    .world()
                    .get::<Exposure>(harness.reflection_camera)
                    .unwrap()
                    .ev100,
                ev100
            );
        }
        harness
            .app
            .world_mut()
            .entity_mut(harness.main_camera)
            .remove::<Exposure>();
        harness.frame();
        assert_eq!(
            harness
                .app
                .world()
                .get::<Exposure>(harness.reflection_camera)
                .unwrap()
                .ev100,
            9.7
        );
    }

    #[test]
    fn stationary_reflection_does_not_mark_camera_state_changed() {
        #[derive(Resource, Default)]
        struct Changes {
            transform: bool,
            camera: bool,
            exposure: bool,
        }

        type ReflectionChanges<'w> = (Ref<'w, Transform>, Ref<'w, Camera>, Ref<'w, Exposure>);

        fn observe_changes(
            reflection: Query<ReflectionChanges<'_>, With<WaterReflectionCamera>>,
            mut changes: ResMut<Changes>,
        ) {
            let (transform, camera, exposure) = reflection.single().unwrap();
            *changes = Changes {
                transform: transform.is_changed(),
                camera: camera.is_changed(),
                exposure: exposure.is_changed(),
            };
        }

        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness
            .app
            .init_resource::<Changes>()
            .add_systems(Last, observe_changes);
        harness.spawn_water(Vec3::new(0.0, 40.0, -800.0), Vec3::new(200.0, 0.0, 200.0));
        assert!(harness.frame().active);
        assert!(harness.frame().active);
        let changes = harness.app.world().resource::<Changes>();
        assert!(!changes.transform && !changes.camera && !changes.exposure);

        harness.aim(Transform::from_xyz(0.0, 160.0, 0.0));
        let frame = harness.frame();
        assert_eq!(frame.transform.translation.y, -80.0);
        let changes = harness.app.world().resource::<Changes>();
        assert!(changes.transform && !changes.camera && !changes.exposure);

        harness
            .app
            .world_mut()
            .entity_mut(harness.main_camera)
            .insert(Exposure { ev100: 7.0 });
        harness.frame();
        let changes = harness.app.world().resource::<Changes>();
        assert!(!changes.transform && !changes.camera && changes.exposure);

        harness.aim(Transform::from_xyz(0.0, 160.0, 0.0).looking_to(Vec3::Z, Vec3::Y));
        assert!(!harness.frame().active);
        assert!(harness.app.world().resource::<Changes>().camera);
        assert!(!harness.frame().active);
        let changes = harness.app.world().resource::<Changes>();
        assert!(!changes.transform && !changes.camera && !changes.exposure);
    }

    #[test]
    fn v2_reflection_restores_missing_exposure_and_updates_its_pose() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -800.0), CELL_WATER_HALF_EXTENTS);
        for exposure in [Some(Exposure { ev100: 7.0 }), None] {
            harness
                .app
                .world_mut()
                .entity_mut(harness.reflection_camera)
                .remove::<Exposure>();
            if let Some(exposure) = exposure {
                harness
                    .app
                    .world_mut()
                    .entity_mut(harness.main_camera)
                    .insert(exposure);
            } else {
                harness
                    .app
                    .world_mut()
                    .entity_mut(harness.main_camera)
                    .remove::<Exposure>();
            }
            let frame = harness.frame();
            assert!(frame.active);
            assert_eq!(frame.transform.translation.y, -40.0);
            assert_eq!(
                harness
                    .app
                    .world()
                    .get::<Exposure>(harness.reflection_camera)
                    .unwrap()
                    .ev100,
                exposure.map_or(9.7, |value| value.ev100)
            );
        }
    }

    #[test]
    fn reflection_camera_renders_while_a_water_plane_is_in_view() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -800.0), CELL_WATER_HALF_EXTENTS);
        assert!(harness.frame().active);
    }

    #[test]
    fn reflection_camera_skips_water_behind_the_main_camera() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, 5_000.0), Vec3::new(200.0, 0.0, 200.0));
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_skips_water_past_the_far_plane() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -4_000.0), Vec3::new(200.0, 0.0, 200.0));
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_skips_a_cell_sized_plane_past_the_far_plane() {
        // The plane's bounding sphere reaches inside the far distance, but no part of the plane
        // does: its nearest edge, grown by the margin, is still 1,940 units ahead.
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -4_500.0), CELL_WATER_HALF_EXTENTS);
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_renders_when_only_a_plane_edge_reaches_the_view() {
        // A cell-sized plane a full cell to the side: its centre is far outside the view, but the
        // near corner of the plane crosses into the frustum.
        let main = Transform::from_xyz(0.0, 0.0, 0.0);
        let mut harness = ReflectionHarness::new(main);
        let centre = Vec3::new(2048.0, 0.0, -700.0);
        harness.spawn_water(centre, CELL_WATER_HALF_EXTENTS);
        assert!(harness.frame().active);

        // Counted as a point at its centre, the same plane stays outside the frustum even with the
        // gate's margin, so only the plane's bounds can carry the test.
        assert!(!water_plane_in_view(
            &GlobalTransform::from(main),
            Some(&main_camera_frustum(&main)),
            Some(MAIN_CAMERA_FAR),
            &GlobalTransform::from_translation(centre),
            Some(&water_bounds(Vec3::ZERO)),
        ));
    }

    #[test]
    fn reflection_camera_stays_off_without_water() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_is_mirrored_in_the_frame_it_becomes_active() {
        let facing_away = Transform::from_xyz(0.0, 300.0, 0.0).looking_to(Vec3::Z, Vec3::Y);
        let mut harness = ReflectionHarness::new(facing_away);
        harness.spawn_water(Vec3::new(0.0, 40.0, -800.0), Vec3::new(200.0, 0.0, 200.0));
        assert!(!harness.frame().active);

        // Facing the water reopens the gate, and the mirror of that frame reflects the camera of
        // that frame, not the pose the gate closed at.
        harness.aim(Transform::from_xyz(0.0, 120.0, 0.0));
        let frame = harness.frame();
        assert!(frame.active);
        assert!((frame.transform.translation.y + 40.0).abs() < 1.0e-4);
        assert!(frame.transform.forward().z < 0.0);
    }

    #[test]
    fn reflection_camera_renders_until_a_plane_reports_its_bounds() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.app.world_mut().spawn((
            GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 40.0, -800.0))),
            WaterSurface,
        ));
        assert!(harness.frame().active);
    }

    #[test]
    fn terrain_sampler_repeats_with_linear_mips_and_16x_anisotropy() {
        let sampler = terrain_layer_sampler();
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
        assert_eq!(sampler.mag_filter, bevy::image::ImageFilterMode::Linear);
        assert_eq!(sampler.min_filter, bevy::image::ImageFilterMode::Linear);
        assert_eq!(sampler.mipmap_filter, bevy::image::ImageFilterMode::Linear);
        assert_eq!(sampler.anisotropy_clamp, 16);
    }

    use crate::world::cache::TerrainLayerSnapshot;
    use bevy::app::Propagate;
    use bevy::asset::AssetApp;
    use bevy::{
        image::ImageFilterMode,
        mesh::VertexAttributeValues,
        reflect::{PartialReflect, ReflectRef},
    };

    /// A cell whose four quadrants each carry a base layer plus the given overlays, in the order the
    /// `VTXT` lists are given, so a weight field built from it can be read back against the lists
    /// that produced it.
    fn terrain_fixture_with_overlays(
        cell_id: u32,
        overlays: &[Vec<(u16, f32)>],
    ) -> TerrainSnapshot {
        let mut layers = Vec::new();
        for quadrant in 0..4u8 {
            layers.push(TerrainLayerSnapshot {
                texture_form_id: 1,
                quadrant,
                layer: 0,
                is_base: true,
                weights: Vec::new(),
            });
            for (slot, weights) in overlays.iter().enumerate() {
                let layer = u16::try_from(slot + 1).expect("fixture overlay count fits a u16");
                layers.push(TerrainLayerSnapshot {
                    texture_form_id: u32::from(layer) + 6,
                    quadrant,
                    layer,
                    is_base: false,
                    weights: weights.clone(),
                });
            }
        }
        TerrainSnapshot {
            cell_id,
            width: 33,
            height: 33,
            heights: vec![0.0; 33 * 33],
            normals: [0, 0, 127].repeat(33 * 33),
            vertex_colors: vec![255; 33 * 33 * 3],
            layers,
            water_height: None,
            water_type_form_id: None,
        }
    }

    /// The samples of one overlay's grid, in `VTXT` vertex order.
    fn overlay_grid(sample: impl Fn(usize, usize) -> f32) -> Vec<(u16, f32)> {
        let mut grid = Vec::with_capacity(QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES);
        for y in 0..QUADRANT_WEIGHT_SAMPLES {
            for x in 0..QUADRANT_WEIGHT_SAMPLES {
                let index = y * QUADRANT_WEIGHT_SAMPLES + x;
                grid.push((
                    u16::try_from(index).expect("a sample index fits a u16"),
                    sample(x, y),
                ));
            }
        }
        grid
    }

    /// The weight a fragment receives from `grid_point` and `grid_weight` in `terrain.wgsl`: the
    /// quadrant-local coordinate clamped onto the `17x17` sample grid, then read bilinearly, clamped
    /// at the far edge. A model of the shader, not of the field: which sample of which word a
    /// coordinate reads is pinned against the shader's own expressions by
    /// `shader_weight_index_expressions_address_the_packed_field`.
    fn shader_weight(settings: &TerrainSettings, overlay: usize, coordinate: [f32; 2]) -> f32 {
        let last = (QUADRANT_WEIGHT_SAMPLES - 1) as f32;
        let sample = |x: usize, y: usize| -> f32 {
            let index = y * QUADRANT_WEIGHT_SAMPLES + x;
            settings.weights[overlay * OVERLAY_WEIGHT_WORDS + index / 4][index % 4]
        };
        // `grid_point`: clamping before the fraction is taken keeps the edge sample of a coordinate
        // an ulp outside the quadrant from blending its neighbour in.
        let column = coordinate[0].clamp(0.0, last);
        let row = coordinate[1].clamp(0.0, last);
        let blend = [column - column.floor(), row - row.floor()];
        let axis = |value: f32| -> (usize, usize) {
            let base = value.floor() as usize;
            (base, (base + 1).min(last as usize))
        };
        let (west, east) = axis(column);
        let (north, south) = axis(row);
        let top = sample(west, north) * (1.0 - blend[0]) + sample(east, north) * blend[0];
        let bottom = sample(west, south) * (1.0 - blend[0]) + sample(east, south) * blend[0];
        top * (1.0 - blend[1]) + bottom * blend[1]
    }

    /// The value of a `const NAME: u32 = <n>u;` declaration in `terrain.wgsl`.
    fn shader_constant(source: &str, name: &str) -> usize {
        let declaration = format!("const {name}: u32 = ");
        let rest = source
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `{declaration}`"))
            .1;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("`{declaration}` is not followed by a number: {rest}"))
    }

    /// The element count of a `field: array<vec4<f32>, <n>>` declaration in `terrain.wgsl`.
    fn shader_array_length(source: &str, field: &str) -> usize {
        let declaration = format!("{field}: array<vec4<f32>, ");
        let rest = source
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `{declaration}`"))
            .1;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("`{declaration}` is not followed by a number: {rest}"))
    }

    /// The field names of `struct NAME { .. }` in `terrain.wgsl`, in declaration order, so the
    /// uniform's layout can be compared with the Rust struct that is written into it.
    fn shader_struct_fields(source: &str, name: &str) -> Vec<String> {
        let body = source
            .split_once(&format!("struct {name} {{"))
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `struct {name}`"))
            .1
            .split_once('}')
            .expect("the struct declaration must be closed")
            .0;
        body.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .map(|line| {
                line.split_once(':')
                    .unwrap_or_else(|| panic!("`{line}` is not a field declaration"))
                    .0
                    .to_owned()
            })
            .collect()
    }

    /// The expression a `const NAME: u32 = <expression>;` declaration in `terrain.wgsl` holds, so a
    /// constant derived from another is read as the shader writes it.
    fn shader_constant_expression<'a>(source: &'a str, name: &str) -> &'a str {
        let declaration = format!("const {name}: u32 = ");
        source
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `{declaration}`"))
            .1
            .split_once(';')
            .expect("a const declaration must end with a semicolon")
            .0
            .trim()
    }

    /// The body of `fn NAME(..) { <body> }` in `terrain.wgsl`, for the helpers whose whole body is
    /// the arithmetic under test.
    fn shader_function_body<'a>(source: &'a str, name: &str) -> &'a str {
        source
            .split_once(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `fn {name}`"))
            .1
            .split_once('{')
            .expect("a function declaration must open its body")
            .1
            .split_once('}')
            .expect("a function body must be closed")
            .0
    }

    /// `source` without its whitespace, so a pinned expression survives a reformat of the file.
    fn without_whitespace(source: &str) -> String {
        source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    /// The value of one of `terrain.wgsl`'s `u32` expressions over the named `terms`: `+`, `-`, `*`,
    /// `/`, `%`, parentheses, literals with their `u` suffix, and identifiers. This runs the
    /// shader's own arithmetic rather than a copy of it, which is the only way a test can notice a
    /// transposed index - a copy of the expression transposes with it.
    fn evaluate_u32(expression: &str, terms: &[(&str, u32)]) -> u32 {
        /// The expression left to read, and what the shader's identifiers stand for.
        struct Parser<'a> {
            characters: Vec<char>,
            position: usize,
            terms: &'a [(&'a str, u32)],
        }

        impl<'a> Parser<'a> {
            fn skip_whitespace(&mut self) {
                while self
                    .characters
                    .get(self.position)
                    .is_some_and(|character| character.is_whitespace())
                {
                    self.position += 1;
                }
            }

            /// Reads `expected` if it is next, whitespace aside.
            fn eat(&mut self, expected: char) -> bool {
                self.skip_whitespace();
                if self.characters.get(self.position) == Some(&expected) {
                    self.position += 1;
                    return true;
                }
                false
            }

            /// `<operand> (('*' | '/' | '%') <operand>)*`, left to right as WGSL binds it.
            fn product(&mut self) -> u32 {
                let mut value = self.operand();
                loop {
                    if self.eat('*') {
                        value *= self.operand();
                    } else if self.eat('/') {
                        value /= self.operand();
                    } else if self.eat('%') {
                        value %= self.operand();
                    } else {
                        return value;
                    }
                }
            }

            /// `<product> (('+' | '-') <product>)*`.
            fn sum(&mut self) -> u32 {
                let mut value = self.product();
                loop {
                    if self.eat('+') {
                        value += self.product();
                    } else if self.eat('-') {
                        value -= self.product();
                    } else {
                        return value;
                    }
                }
            }

            /// The whole expression, for the messages of the assertions below.
            fn text(&self) -> String {
                self.characters.iter().collect()
            }

            /// A literal, one of the named terms, or a parenthesised sum.
            fn operand(&mut self) -> u32 {
                self.skip_whitespace();
                if self.eat('(') {
                    let value = self.sum();
                    assert!(
                        self.eat(')'),
                        "`{}` must close its parentheses",
                        self.text()
                    );
                    return value;
                }
                let mut name = String::new();
                while let Some(&character) = self.characters.get(self.position) {
                    if !character.is_ascii_alphanumeric() && character != '_' {
                        break;
                    }
                    name.push(character);
                    self.position += 1;
                }
                // A literal may carry WGSL's `u` suffix, which says nothing about its value.
                if let Ok(literal) = name.trim_end_matches('u').parse::<u32>() {
                    return literal;
                }
                let (_, value) = self
                    .terms
                    .iter()
                    .find(|(term, _)| *term == name.as_str())
                    .unwrap_or_else(|| {
                        panic!(
                            "`{}` names `{name}`, which the test does not define",
                            self.text()
                        )
                    });
                *value
            }
        }

        let mut parser = Parser {
            characters: expression.chars().collect(),
            position: 0,
            terms,
        };
        let value = parser.sum();
        parser.skip_whitespace();
        assert_eq!(
            parser.position,
            parser.characters.len(),
            "`{expression}` has a suffix the test cannot evaluate"
        );
        value
    }

    #[test]
    fn reflects_camera_above_and_below_the_water_plane() {
        let above = GlobalTransform::from(
            Transform::from_xyz(2.0, 10.0, 4.0).looking_to(Vec3::new(0.0, -0.5, -1.0), Vec3::Y),
        );
        let reflected = reflected_camera_transform(&above, 3.0);
        assert!((reflected.translation.y + 4.0).abs() < 1.0e-5);
        assert!(reflected.forward().y > 0.0);

        let below = GlobalTransform::from(
            Transform::from_xyz(2.0, -4.0, 4.0).looking_to(Vec3::new(0.0, 0.5, -1.0), Vec3::Y),
        );
        let reflected = reflected_camera_transform(&below, 3.0);
        assert!((reflected.translation.y - 10.0).abs() < 1.0e-5);
        assert!(reflected.forward().y < 0.0);
    }

    /// Whether a world point is inside the reflection camera's clipped view (reverse-z, so a point
    /// in front of the near or oblique clip plane has `z > w`).
    fn in_reflection_view(lens: &PerspectiveProjection, camera: &Transform, point: Vec3) -> bool {
        use bevy::camera::CameraProjection;
        let view = camera.compute_affine().inverse().transform_point3(point);
        let clip = lens.get_clip_from_view() * view.extend(1.0);
        clip.w > 0.0 && clip.z >= 0.0 && clip.z <= clip.w
    }

    #[test]
    fn the_reflection_lens_keeps_what_is_above_the_water_and_drops_what_is_below() {
        let main = GlobalTransform::from(
            Transform::from_xyz(0.0, 10.0, 0.0).looking_to(Vec3::NEG_Z, Vec3::Y),
        );
        let water_y = 3.0;
        let reflection = reflected_camera_transform(&main, water_y);
        let lens = reflection_lens(&PerspectiveProjection::default(), &reflection, water_y);
        // The mirrored camera sits at y = -4 looking level; the points are straight ahead of it.
        assert!(in_reflection_view(
            &lens,
            &reflection,
            Vec3::new(0.0, 5.0, -20.0)
        ));
        assert!(!in_reflection_view(
            &lens,
            &reflection,
            Vec3::new(0.0, 1.0, -20.0)
        ));
        assert!(!in_reflection_view(
            &lens,
            &reflection,
            Vec3::new(0.0, -2.0, -20.0)
        ));
    }

    #[test]
    fn the_reflection_lens_for_a_camera_below_the_water_keeps_what_is_below_it() {
        let main = GlobalTransform::from(
            Transform::from_xyz(0.0, -4.0, 0.0).looking_to(Vec3::NEG_Z, Vec3::Y),
        );
        let water_y = 3.0;
        let reflection = reflected_camera_transform(&main, water_y);
        let lens = reflection_lens(&PerspectiveProjection::default(), &reflection, water_y);
        // The mirrored camera sits at y = 10 looking level: the world under the surface is kept
        // and the air above it is dropped.
        assert!(in_reflection_view(
            &lens,
            &reflection,
            Vec3::new(0.0, -2.0, -20.0)
        ));
        assert!(!in_reflection_view(
            &lens,
            &reflection,
            Vec3::new(0.0, 5.0, -20.0)
        ));
    }

    #[test]
    fn the_reflection_system_gives_the_reflection_camera_the_clipped_main_lens() {
        use bevy::camera::CameraProjection;
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -800.0), CELL_WATER_HALF_EXTENTS);
        assert!(harness.frame().active);
        let Projection::Perspective(lens) = harness
            .app
            .world()
            .get::<Projection>(harness.reflection_camera)
            .unwrap()
        else {
            panic!("the reflection camera lost its perspective projection");
        };
        let Projection::Perspective(main_lens) = main_camera_projection() else {
            unreachable!();
        };
        assert_eq!(lens.far, main_lens.far);
        assert!(lens.near_clip_plane.xyz().length() > 0.99);
        assert!(lens.near_clip_plane.w < 0.0);
        let _ = lens.get_clip_from_view();
    }

    #[test]
    fn the_reflection_lens_keeps_the_main_cameras_field_of_view() {
        let main_lens = PerspectiveProjection {
            fov: 1.3,
            aspect_ratio: 4.0 / 3.0,
            near: 0.5,
            far: 5000.0,
            ..default()
        };
        let reflection = Transform::from_xyz(0.0, -4.0, 0.0);
        let lens = reflection_lens(&main_lens, &reflection, 3.0);
        assert_eq!(lens.fov, main_lens.fov);
        assert_eq!(lens.aspect_ratio, main_lens.aspect_ratio);
        assert_eq!(lens.far, main_lens.far);
    }

    /// The reflection pass is what costs 1.72 ms of a 4.41 ms frame at the rural cell, because it
    /// redraws every placed object; the camera must be spawned on the world layer alone. Asserting
    /// on the spawned camera rather than on the constant keeps the two from drifting apart.
    #[test]
    fn the_reflection_camera_renders_the_world_layer_only() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .add_systems(Startup, setup_water_reflection);
        app.update();

        let mut cameras = app
            .world_mut()
            .query_filtered::<&RenderLayers, With<WaterReflectionCamera>>();
        let layers = cameras
            .single(app.world())
            .expect("the reflection camera must be spawned");
        assert!(layers.intersects(&RenderLayers::layer(WORLD_LAYER)));
        assert!(!layers.intersects(&RenderLayers::layer(WATER_LAYER)));
        assert!(!layers.intersects(&PLACED_OBJECT_RENDER_LAYERS));
    }

    /// The other half of the contract: the main camera is what still draws the placed objects and
    /// the water, so a new layer has to be added to its list and not only taken out of the
    /// reflection camera's.
    #[test]
    fn the_main_view_renders_every_layer() {
        let main_view = RenderLayers::from_layers(MAIN_VIEW_LAYERS);
        assert!(main_view.intersects(&RenderLayers::layer(WORLD_LAYER)));
        assert!(main_view.intersects(&RenderLayers::layer(WATER_LAYER)));
        assert!(main_view.intersects(&PLACED_OBJECT_RENDER_LAYERS));
        assert!(
            !RenderLayers::from_layers(REFLECTION_VIEW_LAYERS)
                .intersects(&PLACED_OBJECT_RENDER_LAYERS),
            "a placed object must never be drawn into the reflection texture"
        );
    }

    /// A light needs the world layer for both cameras and the placed-object layer for its shadow
    /// cascades: `check_dir_light_mesh_visibility` only collects casters that share a layer with
    /// the light.
    #[test]
    fn lights_cover_both_cameras_and_the_placed_objects() {
        let light = RenderLayers::from_layers(LIGHT_LAYERS);
        for view in [MAIN_VIEW_LAYERS, REFLECTION_VIEW_LAYERS] {
            assert!(
                light.intersects(&RenderLayers::from_layers(view)),
                "a light must reach {view:?}"
            );
        }
        assert!(light.intersects(&PLACED_OBJECT_RENDER_LAYERS));
    }

    /// A point light is a child of its placed-object reference, whose layer propagates down the
    /// hierarchy; the light must keep [`LIGHT_LAYERS`] anyway, or it stops lighting the terrain
    /// and the reflection view.
    #[test]
    fn a_light_below_a_reference_keeps_the_light_layers() {
        let mut app = App::new();
        add_placed_object_layer_propagation(&mut app);
        let reference = app
            .world_mut()
            .spawn(Propagate(PLACED_OBJECT_RENDER_LAYERS))
            .id();
        let light = app
            .world_mut()
            .spawn((light_render_layers(), ChildOf(reference)))
            .id();
        app.update();
        app.update();
        assert_eq!(
            app.world().entity(reference).get::<RenderLayers>(),
            Some(&PLACED_OBJECT_RENDER_LAYERS)
        );
        assert_eq!(
            app.world().entity(light).get::<RenderLayers>(),
            Some(&RenderLayers::from_layers(LIGHT_LAYERS)),
            "the propagated placed-object layer must not replace the light's layers"
        );
    }

    /// The propagation has to be in place before visibility compares layers, and the meshes it
    /// carries arrive from the glb spawner in `SpawnScene` - between `Update` and `PostUpdate`. A
    /// descendant spawned after its reference must still inherit the layer in that same frame.
    #[test]
    fn a_descendant_spawned_after_its_reference_inherits_the_placed_object_layer() {
        let mut app = App::new();
        add_placed_object_layer_propagation(&mut app);
        let reference = app
            .world_mut()
            .spawn(Propagate(PLACED_OBJECT_RENDER_LAYERS))
            .id();
        app.update();
        // The glb is spawned frames after the reference: a scene root with the mesh primitives
        // below it.
        let scene_root = app.world_mut().spawn(ChildOf(reference)).id();
        let mesh = app
            .world_mut()
            .spawn((Mesh3d(Handle::default()), ChildOf(scene_root)))
            .id();

        app.update();

        for entity in [reference, scene_root, mesh] {
            assert_eq!(
                app.world().entity(entity).get::<RenderLayers>(),
                Some(&PLACED_OBJECT_RENDER_LAYERS),
                "a reference and everything below it must carry the placed-object layer"
            );
        }
    }

    fn water_app(config: Option<crate::config::EngineConfig>) -> App {
        let mut app = App::new();
        app.init_resource::<Assets<WaterMaterial>>()
            .init_resource::<ProfilingState>()
            .add_systems(PostUpdate, animate_water_materials);
        if let Some(config) = config {
            app.insert_resource(config);
        }
        app
    }

    fn add_water(app: &mut App) -> Handle<WaterMaterial> {
        app.world_mut()
            .resource_mut::<Assets<WaterMaterial>>()
            .add(WaterMaterial {
                base: StandardMaterial::default(),
                extension: WaterExtension::default(),
            })
    }

    fn water_phase_of(app: &App, handle: &Handle<WaterMaterial>) -> f32 {
        app.world()
            .resource::<Assets<WaterMaterial>>()
            .get(handle)
            .expect("water material")
            .extension
            .settings
            .wave_scale_speed_strength
            .w
    }

    #[test]
    fn an_interactive_run_leaves_the_waves_on_the_shader_clock() {
        let mut app = water_app(None);
        let handle = add_water(&mut app);
        app.update();
        assert_eq!(water_phase_of(&app, &handle), ANIMATED_WAVE_PHASE);

        let current = app
            .world()
            .resource::<Assets<WaterMaterial>>()
            .get(&handle)
            .unwrap()
            .extension
            .clone();
        assert_eq!(
            wanted_water_settings(&current, ANIMATED_WAVE_PHASE, None),
            current.settings,
            "a steady interactive frame must not rewrite the material"
        );
        let settled = app
            .world()
            .get_resource_change_ticks::<Assets<WaterMaterial>>()
            .unwrap()
            .changed;
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(
            app.world()
                .get_resource_change_ticks::<Assets<WaterMaterial>>()
                .unwrap()
                .changed,
            settled,
            "steady interactive frames must not take the materials mutably"
        );
    }

    #[test]
    fn a_capture_pins_every_water_material_including_late_ones() {
        let mut app = water_app(Some(crate::config::EngineConfig {
            shots: Some(std::path::PathBuf::from("shots.toml")),
            ..default()
        }));
        let first = add_water(&mut app);
        app.update();
        assert_eq!(water_phase_of(&app, &first), 1.0);

        let late = add_water(&mut app);
        app.update();
        assert_eq!(water_phase_of(&app, &late), 1.0);
    }

    #[test]
    fn an_explicit_water_time_outranks_the_capture_default() {
        let mut app = water_app(Some(crate::config::EngineConfig {
            terrain_water_fixture: true,
            water_time: Some(0.0),
            ..default()
        }));
        let handle = add_water(&mut app);
        app.update();
        assert_eq!(water_phase_of(&app, &handle), 0.0);
    }

    #[test]
    fn water_phase_pins_only_when_asked() {
        use crate::config::EngineConfig;
        assert_eq!(water_phase(None), None);
        assert_eq!(water_phase(Some(&EngineConfig::default())), None);

        let fixture = EngineConfig {
            terrain_water_fixture: true,
            ..default()
        };
        assert_eq!(water_phase(Some(&fixture)), Some(1.0));

        let shot = EngineConfig {
            shots: Some(std::path::PathBuf::from("shots.toml")),
            ..default()
        };
        assert_eq!(water_phase(Some(&shot)), Some(1.0));

        let pinned = EngineConfig {
            water_time: Some(3.25),
            ..default()
        };
        assert_eq!(water_phase(Some(&pinned)), Some(3.25));

        let pinned_zero = EngineConfig {
            water_time: Some(0.0),
            ..default()
        };
        assert_eq!(water_phase(Some(&pinned_zero)), Some(0.0));

        // An explicit capture time outranks the capture default.
        let pinned_fixture = EngineConfig {
            terrain_water_fixture: true,
            water_time: Some(3.25),
            ..default()
        };
        assert_eq!(water_phase(Some(&pinned_fixture)), Some(3.25));
        // An explicit capture time outranks the shot default just as it outranks the fixture.
        let pinned_shots = EngineConfig {
            shots: Some("poses.json".into()),
            water_time: Some(3.25),
            ..default()
        };
        assert_eq!(water_phase(Some(&pinned_shots)), Some(3.25));
    }

    #[test]
    fn the_shader_clock_does_not_wrap_at_the_hour() {
        let two_hours = std::time::Duration::from_secs(2 * 3600);
        // Bevy's default wrap period would restart the clock, and with it the waves, at one hour.
        let mut default_clock = Time::<Virtual>::default();
        default_clock.advance_by(two_hours);
        assert_eq!(default_clock.elapsed_secs_wrapped(), 0.0);

        let mut clock = Time::<Virtual>::default();
        clock.set_wrap_period(SHADER_CLOCK_WRAP);
        clock.advance_by(two_hours);
        assert_eq!(clock.elapsed_secs_wrapped(), 7200.0);
    }

    #[test]
    fn the_renderer_extends_the_clock_the_shaders_read() {
        let mut app = App::new();
        app.add_plugins(bevy::time::TimePlugin)
            .add_systems(Startup, extend_shader_clock);
        // The first frame runs Startup; the next copies the virtual clock into the generic one
        // that the render world extracts as `globals.time`.
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<Time<Virtual>>().wrap_period(),
            SHADER_CLOCK_WRAP
        );
        assert_eq!(
            app.world().resource::<Time>().wrap_period(),
            SHADER_CLOCK_WRAP
        );
    }

    #[test]
    fn the_fog_terms_are_the_retail_material_copy() {
        // RF-SHADER-006 / 010: range = far - near (1e-5 when zero), F = max(far, 1), exponent =
        // max(4 * amount, 1).
        let body = |near, far, amount| water_body(100, WaterFog { near, far, amount });
        // DefaultWater: near -10, far 150, amount 0.7. The exponent floors at 1.
        assert_eq!(
            body(-10.0, 150.0, 0.7),
            Vec4::new(1.0, 160.0, 150.0, 2.8_f32.max(1.0))
        );
        // A larger amount raises the exponent.
        assert_eq!(body(-10.0, 150.0, 1.0).w, 4.0);
        // No fog amount does not make the water clear: the exponent stays 1.
        assert_eq!(body(0.0, 10.0, 0.0), Vec4::new(1.0, 10.0, 10.0, 1.0));
        // The retail records whose far is not above zero: lava (near 0, far 0) has a zero range
        // that becomes 1e-5, and Apocrypha water (-10000, -9200) keeps its range of 800 while
        // the far used for the depth ratio floors at 1.
        assert_eq!(body(0.0, 0.0, 0.95), Vec4::new(1.0, 1.0e-5, 1.0, 3.8));
        assert_eq!(
            body(-10000.0, -9200.0, 1.0),
            Vec4::new(1.0, 800.0, 1.0, 4.0)
        );
        // Opacity is ANAM over 100.
        assert_eq!(water_body(35, WaterFog::default()).x, 0.35);
    }

    #[test]
    fn the_water_multiplier_scales_the_colours_in_srgb_before_they_become_linear() {
        // RF-SHADER-007: Shallow, Deep and Reflection colours times the weather's Water Multiplier.
        // SkyrimClear's day value is (175, 237, 214); a colour of (200, 100, 50) becomes
        // (200 * 175, 100 * 237, 50 * 214) / 255 / 255 as sRGB values.
        let multiplier = crate::sky::SkyPalette::SKYRIM_CLEAR_DAY_WATER_MULTIPLIER;
        let water = WaterExtension::default()
            .with_water_multiplier(multiplier)
            .with_reflection_color([200, 100, 50]);
        let expected = Color::srgb(
            200.0 / 255.0 * 175.0 / 255.0,
            100.0 / 255.0 * 237.0 / 255.0,
            50.0 / 255.0 * 214.0 / 255.0,
        )
        .to_linear();
        let linear = water.settings.reflection_color;
        assert!((linear.x - expected.red).abs() < 1.0e-6);
        assert!((linear.y - expected.green).abs() < 1.0e-6);
        assert!((linear.z - expected.blue).abs() < 1.0e-6);
        // The deep colour takes the same product as the material base colour.
        let deep = water_deep_color([5, 14, 18], multiplier).to_linear();
        let expected = Color::srgb(
            5.0 / 255.0 * 175.0 / 255.0,
            14.0 / 255.0 * 237.0 / 255.0,
            18.0 / 255.0 * 214.0 / 255.0,
        )
        .to_linear();
        assert!((deep.red - expected.red).abs() < 1.0e-6);
        // Without a multiplier the colours are unchanged.
        let plain = WaterExtension::default().with_reflection_color([200, 100, 50]);
        assert!(
            (plain.settings.reflection_color.x - Color::srgb_u8(200, 100, 50).to_linear().red)
                .abs()
                < 1.0e-6
        );
    }

    #[test]
    fn the_shader_holds_the_retail_framebuffer_range_and_mix() {
        // RF-SHADER-001: FramebufferRange 1.2. RF-SHADER-008: color = lerp(lerp(refr, body, a),
        // reflection, m), written opaque over the refracted scene copy.
        let source = include_str!("shaders/water.wgsl");
        assert!(source.contains("const FRAMEBUFFER_RANGE: f32 = 1.2;"));
        assert!(!source.contains("coverage = a + m - a * m"));
        assert!(source.contains(
            "let m = mix(1.0, fresnel * depth_reflections * water.fresnel_reflectivity.z, dist_t);"
        ));
        assert!(source.contains(
            "let reflection_planar = mix(flat_reflection, environment, water.fresnel_reflectivity.y);"
        ));
        // The Fresnel reflection no longer raises the alpha and no lit body colour remains.
        assert!(!source.contains("max(body"));
        assert!(!source.contains("apply_pbr_lighting"));
    }

    /// DefaultWater's own values (WATR 0x18), as the retail Skyrim.esm and Update.esm write them.
    fn default_water_noise() -> WaterNoiseValues {
        WaterNoiseValues {
            uv_scale: [1920.0, 6703.0, 488.0],
            amplitude: [0.6957, 0.6304, 0.4746],
            depth_properties: [0.3, 0.95, 0.1, 0.9],
            wind_direction: [270.0, 210.0, 225.0],
            wind_speed: [0.019, 0.013, 0.096],
            noise_falloff: 4007.0,
            sun: [1021.0, 4.65, 3521.0, 2.89],
            refraction_magnitude: 40.0,
        }
    }

    #[test]
    fn the_noise_layers_carry_the_records_values() {
        let layer = || Some(Handle::<Image>::default());
        let water = WaterExtension::default()
            .with_noise([layer(), layer(), layer()], default_water_noise());
        let (scale, amplitude) = water.noise();
        assert_eq!(scale, Vec4::new(1920.0, 6703.0, 488.0, 1.0));
        assert_eq!(amplitude, Vec4::new(0.6957, 0.6304, 0.4746, 0.0));
        assert_eq!(water.settings.depth_control, Vec4::new(0.3, 0.95, 0.1, 0.9));
        // The wind directions go to the shader in radians, with the retail conversion constant.
        assert_eq!(
            water.settings.noise_wind_direction.x,
            270.0 * WATER_DEGREES_TO_RADIANS
        );
        assert_eq!(
            water.settings.noise_wind_speed,
            Vec4::new(0.019, 0.013, 0.096, 0.0)
        );
        assert_eq!(water.image_ids().count(), 3);
        // A layer without a texture adds no tilt, and with none the surface is flat.
        let two =
            WaterExtension::default().with_noise([layer(), None, layer()], default_water_noise());
        assert_eq!(two.noise().1, Vec4::new(0.6957, 0.0, 0.4746, 0.0));
        let flat =
            WaterExtension::default().with_noise([None, None, None], WaterNoiseValues::flat());
        assert_eq!(flat.noise().0.w, 0.0);
        assert_eq!(flat.noise().1, Vec4::ZERO);
        assert_eq!(flat.image_ids().count(), 0);
    }

    /// The compass bearing (0 north, 90 east, clockwise) a pattern moves toward when its UV offset
    /// follows `noise_scroll`: U runs east and V north, and the offset is added to the UV, so the
    /// pattern moves against it.
    fn pattern_bearing(direction_degrees: f32) -> f32 {
        let before = noise_scroll(direction_degrees, 0.05, 1.0);
        let after = noise_scroll(direction_degrees, 0.05, 1.01);
        // Fractions wrap at tile edges; a step this small does not cross one for these directions.
        let toward = -(after - before);
        toward.x.atan2(toward.y).to_degrees().rem_euclid(360.0)
    }

    #[test]
    fn the_reflection_camera_carries_the_scene_fog() {
        // RF-WATER-009 / RF-WATER-011 (reasoned, medium confidence): the mirrored terrain is fogged
        // like the direct view, so the reflection camera is a FogCamera.
        let mut app = App::new();
        app.init_resource::<Assets<Image>>()
            .add_systems(Startup, setup_water_reflection);
        app.update();
        let mut cameras = app
            .world_mut()
            .query_filtered::<Entity, (With<WaterReflectionCamera>, With<crate::sky::FogCamera>)>();
        assert_eq!(cameras.iter(app.world()).count(), 1);
    }

    #[test]
    fn the_variants_without_depth_follow_retail() {
        // RF-WATER-017: the exterior no-depth program is lerp(Shallow, Deep, F) * sat(sunDir.N), the
        // interior program the same without the sun factor, and both use the ANAM opacity
        // (RF-WATER-026: ANAM is read only in programs without depth).
        assert_eq!(water_variant_body(0.2, 0.0, 0.25, Some(1.0)), 0.15);
        assert_eq!(water_variant_body(0.2, 0.0, 0.25, Some(0.5)), 0.075);
        assert_eq!(water_variant_body(0.2, 0.0, 0.25, Some(-1.0)), 0.0);
        assert_eq!(water_variant_body(0.2, 0.0, 0.25, None), 0.15);
        let interior = WaterExtension::default().with_interior(true);
        assert_eq!(interior.settings.depth_params.z, 1.0);
        assert_eq!(WaterExtension::default().settings.depth_params.z, 0.0);
        let source = include_str!("shaders/water.wgsl");
        assert!(source.contains("let interior = water.depth_params.z > 0.5;"));
        assert!(source.contains("mix(water.shallow_color.rgb, deep, fresnel)"));
        assert!(source.contains("let a = select(body.y, water.body.x, no_depth);"));
        // Retail's reflected vector has no clamp; the engine's only guards the planar lookup.
        assert!(source.contains("engineering guard"));
    }

    #[test]
    fn the_sun_terms_are_the_retail_specular_and_sparkle() {
        // RF-WATER-010 with DefaultWater's fields: 1021 x 4.65 and 3521 x 2.89.
        let terms = [1021.0, 4.65, 3521.0, 2.89];
        // Perfect alignment gives each magnitude.
        assert!((water_sun_terms(1.0, 1.0, terms) - (4.65 + 2.89)).abs() < 1.0e-5);
        // The glint is a narrow lobe: a mirror direction ten degrees off the sun is dark.
        let ten_degrees = 10.0_f32.to_radians().cos();
        assert!(water_sun_terms(ten_degrees, 0.0, terms) < 1.0e-3);
        // The power falls with the angle and is monotonic.
        let wide = water_sun_terms(0.9999, 0.0, terms);
        let narrow = water_sun_terms(0.9995, 0.0, terms);
        assert!(wide > narrow && narrow > 0.0);
        // Pointing away from the sun adds nothing; an unlit water (power and magnitude 0, as in
        // `LavaSettingsDUPLICATE001`) adds nothing and is not NaN.
        assert_eq!(water_sun_terms(-0.5, -0.5, terms), 0.0);
        let lava = water_sun_terms(0.3, 0.3, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(lava, 0.0);
        // The sparkle direction is the retail literal in the engine's shader.
        let source = include_str!("shaders/water.wgsl");
        assert!(source.contains("vec3<f32>(-0.099, -0.099, 0.99)"));
        assert!(source.contains("exp2(water.sun_terms.x * log2("));
        assert!(source.contains("exp2(water.sun_terms.z * log2("));
        // The material carries the record's four fields and the sun colour.
        let water = WaterExtension::default()
            .with_noise(
                [None, None, None],
                WaterNoiseValues {
                    sun: terms,
                    ..WaterNoiseValues::flat()
                },
            )
            .with_sun_color(Color::srgb_u8(190, 181, 171));
        assert_eq!(water.settings.sun_terms, Vec4::from_array(terms));
        let expected = Color::srgb_u8(190, 181, 171).to_linear();
        assert_eq!(water.settings.sun_color.x, expected.red);
    }

    #[test]
    fn the_reflection_weight_follows_the_retail_ssr_weighting() {
        // RF-WATER-009: sat(distT * fWaterSSRIntensity * coverage), 0.5 in this install.
        assert_eq!(WATER_SSR_INTENSITY, 0.5);
        assert_eq!(water_reflection_share(1.0, 1.0), 0.5);
        assert_eq!(water_reflection_share(1.0, 0.0), 0.0);
        assert_eq!(water_reflection_share(0.5, 1.0), 0.25);
        // The share never exceeds one, even with the executable's own intensity of 1.3 scaled up.
        assert_eq!(water_reflection_share(10.0, 1.0), 1.0);
        // Coverage below one, as at the edge of a mirrored mountain, lowers it in proportion.
        assert!((water_reflection_share(1.0, 0.4) - 0.2).abs() < 1.0e-6);
    }

    #[test]
    fn the_flat_colour_is_only_the_part_the_reflection_amount_leaves() {
        // RF-WATER-050: refl = lerp(ReflectionColor, env, DNAM 0x14); env = lerp(sky, ssr, share).
        // A river (DNAM 0x14 = 0.8) at distT 1 with full image coverage: share = 0.5. The flat
        // colour's weight is 1 - 0.8 = 0.2 and nothing else; before the sky it was
        // 0.2 + 0.8 * 0.5 = 0.6 (RF-WATER-050 correction, WATER5 section 1).
        let share = water_reflection_share(1.0, 1.0);
        let only_flat = water_reflection_colour(1.0, 0.0, 0.0, share, 0.8);
        assert!((only_flat - 0.2).abs() < 1.0e-6);
        let only_sky = water_reflection_colour(0.0, 1.0, 0.0, share, 0.8);
        assert!((only_sky - 0.8 * 0.5).abs() < 1.0e-6);
        let only_ssr = water_reflection_colour(0.0, 0.0, 1.0, share, 0.8);
        assert!((only_ssr - 0.8 * 0.5).abs() < 1.0e-6);
        // Where the image has no coverage the sky carries all of the 0.8.
        let no_image =
            water_reflection_colour(0.0, 1.0, 0.0, water_reflection_share(1.0, 0.0), 0.8);
        assert!((no_image - 0.8).abs() < 1.0e-6);
        // A creek (DNAM 0x14 = 1.0) has no flat share at all.
        assert_eq!(water_reflection_colour(1.0, 0.0, 0.0, share, 1.0), 0.0);
        // The shader holds the same mix over the sky dome's colour for the reflected direction,
        // in Bevy's axes (the dome shader's elevation of the ray).
        let source = include_str!("shaders/water.wgsl");
        assert!(source.contains("fn sky_dome_colour(elevation: f32) -> vec3<f32>"));
        assert!(source.contains("let sky_elevation = degrees(asin(clamp(sky_ray.y, -1.0, 1.0)));"));
        assert!(source.contains("let reflection_planar = mix(flat_reflection, environment, water.fresnel_reflectivity.y);"));
    }

    #[test]
    fn the_refraction_offset_and_leak_test_follow_retail() {
        // RF-WATER-049: a river (DNAM 0x98 = 40, 0xD4 = 0.95) at a normal tilt of 0.1 moves the
        // point 4 units where the water is deep and 0.05 of that at zero depth.
        let deep = water_refraction_offset([0.1, -0.05], 40.0, 0.95, 1.0);
        assert!((deep[0] - 4.0).abs() < 1.0e-5 && (deep[1] + 2.0).abs() < 1.0e-5);
        let shallow = water_refraction_offset([0.1, 0.0], 40.0, 0.95, 0.0);
        assert!((shallow[0] - 4.0 * 0.05).abs() < 1.0e-5);
        // A creek's magnitude is 8, a flat normal moves nothing.
        assert_eq!(
            water_refraction_offset([0.0, 0.0], 8.0, 0.95, 0.5),
            [0.0, 0.0]
        );
        // The bed below a camera above the water is not a leak; a bank above the water is.
        assert!(!water_refraction_leaks(100.0, 0.0, -50.0));
        assert!(water_refraction_leaks(100.0, 0.0, 30.0));
        // A camera under the surface sees the surface from below: the ground below is on its side.
        assert!(water_refraction_leaks(-100.0, 0.0, -50.0));
        assert!(!water_refraction_leaks(-100.0, 0.0, 30.0));
        // The shader holds the formula: the offset is in the water plane, the factor uses DNAM 0xD4,
        // the scene copy is Bevy's transmission texture and the colour is written opaque.
        let source = include_str!("shaders/water.wgsl");
        assert!(source.contains("let factor = water.depth_control.y * (along - 1.0) + 1.0;"));
        assert!(source.contains(
            "vec3<f32>(normal_skyrim.x, 0.0, -normal_skyrim.y) * water.refraction.x * factor"
        ));
        assert!(source.contains("if camera_side * scene_side <= 0.0 {"));
        assert!(
            source.contains(
                "textureSampleLevel(view_transmission_texture, view_transmission_sampler"
            )
        );
        assert!(source.contains("refracted * (1.0 - a) * (1.0 - m) + water_colour * (1.0 - m) * a + reflection * m + sun, 1.0)"));
    }

    #[test]
    fn the_flow_program_follows_the_retail_arithmetic() {
        // RF-WATER-044: weight 1 at a texel centre, 0 at an edge, and the divisor that keeps the
        // blend's spread when the weights are one half.
        assert!((water_flow_weight(0.5 / 64.0) - 1.0).abs() < 1.0e-6);
        assert!(water_flow_weight(1.0 / 64.0).abs() < 1.0e-6);
        assert!((water_flow_weight(0.25 / 64.0) - 0.5).abs() < 1.0e-6);
        assert!((water_flow_divisor(1.0, 1.0) - 1.0).abs() < 1.0e-6);
        assert!((water_flow_divisor(0.5, 0.5) - 0.5).abs() < 1.0e-6);
        // RF-WATER-038: hour 12 at the start, 20 game seconds per real second, wrapping at midnight.
        assert_eq!(water_flow_clock(0.0, 1.0), 43200.0);
        assert_eq!(water_flow_clock(10.0, 1.0), 43400.0);
        assert_eq!(water_flow_clock(2160.0, 1.0), 0.0);
        // The player's wave settings apply to the flow path and are neutral at their defaults:
        // speed 0 freezes the phase, waves off (strength 0) leaves no ripple tilt, strength scales it.
        assert_eq!(water_flow_clock(10.0, 0.0), 43200.0);
        assert_eq!(water_flow_clock(10.0, 2.0), 43600.0);
        assert_eq!(water_flow_strength([0.2, -0.1], 1.0), [0.2, -0.1]);
        assert_eq!(water_flow_strength([0.2, -0.1], 0.0), [0.0, 0.0]);
        assert_eq!(water_flow_strength([0.2, -0.1], 2.0), [0.4, -0.2]);
        // The shader holds the corrected taps (RF-WATER-044): A at (0, h), B at (h, h), C at the
        // pixel, D at (h, 0); rates 10.64, 9.92, 8, 8.48 with offsets 0.27, 0, 0, 0.62.
        let source = include_str!("shaders/water.wgsl");
        for needle in [
            "let flow_a = flow_texel(cell_uv + vec2<f32>(0.0, h));",
            "let flow_b = flow_texel(cell_uv + vec2<f32>(h, h));",
            "let flow_c = flow_texel(cell_uv);",
            "let flow_d = flow_texel(cell_uv + vec2<f32>(h, 0.0));",
            "return textureLoad(water_flow_map, min(texel, vec2<i32>(last)), 0);",
            "return textureLoad(water_flow_map_east, vec2<i32>(0, texel.y), 0);",
            "return textureLoad(water_flow_map_south, vec2<i32>(texel.x, 0), 0);",
            "return textureLoad(water_flow_map_south_east, vec2<i32>(0, 0), 0);",
            "const FLOW_RATES: vec4<f32> = vec4<f32>(10.64, 9.92, 8.0, 8.48);",
            "const FLOW_OFFSETS: vec4<f32> = vec4<f32>(0.27, 0.0, 0.0, 0.62);",
            "const FLOW_SCALE_BASE: f32 = 1.01;",
            "const FLOW_CLOCK_FACTOR: f32 = 0.001;",
            "const FLOW_DEPTH_FLOOR: f32 = 0.4;",
            "let w = 1.0 - abs(2.0 * fract(FLOW_MAP_TEXELS * cell_uv) - 1.0);",
            "w.y * (w.x * n_c + (1.0 - w.x) * n_d) + (1.0 - w.y) * (w.x * n_a + (1.0 - w.x) * n_b)",
            "normal_skyrim = normalize(mix(flow_tiled_normal(position, time), flow, dist_t));",
            "let ripple = (cell_uv + vec2<f32>(water.flow.x, -water.flow.y)) * wave_scale;",
            "let clock = (FLOW_CLOCK_START + seconds * wave_speed * FLOW_CLOCK_SPEED) % FLOW_CLOCK_DAY;",
            "return renormalised * max(normals_depth, FLOW_DEPTH_FLOOR) * wave_strength;",
            "let wave_strength = water.wave_scale_speed_strength.z;",
        ] {
            assert!(source.contains(needle), "missing {needle}");
        }
        // The flow map is point-sampled: filtering a direction and a phase speed smears the ripples
        // into flat patches.
        assert!(!source.contains("textureSampleLevel(water_flow_map"));
        // A material that runs the flow program takes the player's wave settings through the same
        // uniform as every water: waves off gives strength 0, speed 0 gives a frozen clock, and the
        // flow program stays on.
        let handle = || Some(Handle::<Image>::default());
        let flowing = WaterExtension::with_reflection(Handle::default(), handle())
            .with_flow(IVec2::new(-3, 12), handle());
        let defaults = crate::settings::RenderingSettings::default();
        let neutral = wanted_water_settings(&flowing, 0.0, Some(&defaults));
        assert_eq!(neutral.wave_scale_speed_strength.xyz(), Vec3::ONE);
        assert_eq!(neutral.flow.z, 1.0);
        let still = crate::settings::RenderingSettings {
            water_waves: false,
            wave_speed: 0.0,
            ..defaults
        };
        let wanted = wanted_water_settings(&flowing, 0.0, Some(&still));
        assert_eq!(wanted.wave_scale_speed_strength.y, 0.0);
        assert_eq!(wanted.wave_scale_speed_strength.z, 0.0);
        assert_eq!(wanted.flow.z, 1.0);
        // The program runs only with a flow map and a flow normal texture bound.
        let both = WaterExtension::with_reflection(Handle::default(), handle())
            .with_flow(IVec2::new(-3, 12), handle());
        assert_eq!(both.settings.flow, Vec4::new(-3.0, 12.0, 1.0, 0.0));
        assert_eq!(both.image_ids().count(), 2);
        // The neighbours' maps are bound after the cell's own and set the bit mask of the uniform
        // (1 east, 2 south, 4 south-east); a side left out keeps its bit clear, and dropping the
        // cell's own map drops them all.
        let bound = both
            .clone()
            .with_flow_neighbours([handle(), None, handle()]);
        assert_eq!(bound.settings.flow, Vec4::new(-3.0, 12.0, 1.0, 5.0));
        assert_eq!(bound.image_ids().count(), 4);
        let dropped = bound.with_flow(IVec2::new(-3, 12), None);
        assert_eq!(
            (dropped.settings.flow.w, dropped.image_ids().count()),
            (0.0, 1)
        );
        let no_normal = WaterExtension::default().with_flow(IVec2::new(1, 2), handle());
        assert_eq!(no_normal.settings.flow.z, 0.0);
        assert_eq!(
            both.with_flow(IVec2::new(-3, 12), None).settings.flow.z,
            0.0
        );
    }

    /// The flow program's blend of four point taps (RF-WATER-044), on one scalar per texel instead
    /// of a normal: taps C at `t`, D half a texel east, A half a texel south, B both, weighted
    /// `w = 1 - |2 fract(t) - 1|` per axis and renormalised. `t` is in texels; `texel` reads one.
    fn flow_blend_model(t: Vec2, texel: impl Fn(IVec2) -> f32) -> f32 {
        let weight = |component: f32| water_flow_weight(component / 64.0);
        let (wx, wy) = (weight(t.x), weight(t.y));
        let read = |shift: Vec2| texel((t + shift).floor().as_ivec2());
        let (c, d) = (read(Vec2::ZERO), read(Vec2::new(0.5, 0.0)));
        let (a, b) = (read(Vec2::new(0.0, 0.5)), read(Vec2::new(0.5, 0.5)));
        let blended = wy * (wx * c + (1.0 - wx) * d) + (1.0 - wy) * (wx * a + (1.0 - wx) * b);
        (blended - 0.5) / water_flow_divisor(wx, wy)
    }

    /// Flow maps whose neighbouring tiles differ (a different value per tile, and a gradient inside
    /// each), so a cell border that does not read its neighbour shows a step. Tile contents are
    /// scalars standing for the 64 by 64 texels.
    fn flow_model_tile(tile: WaterFlowTile, texel: IVec2) -> f32 {
        let base = match tile {
            WaterFlowTile::Own => 0.25,
            WaterFlowTile::East => 0.75,
            WaterFlowTile::South => 0.60,
            WaterFlowTile::SouthEast => 0.90,
        };
        base + 0.001 * texel.x as f32 + 0.0005 * texel.y as f32
    }

    /// Retail is continuous across every cell border (RF-WATER-063): just before a cell's east or
    /// south edge the shifted taps read the neighbour's first texel, so the blend there equals the
    /// neighbour cell's blend just after the border. With the neighbours unbound, the clamp the
    /// engine used to apply steps by far more (the numeric model of RF-WATER-063 gave 0.19 against 0).
    #[test]
    fn the_flow_blend_is_continuous_across_cell_borders_when_neighbours_are_read() {
        let eps = 0.001;
        let sample = |own: WaterFlowTile, t: Vec2, bound: u8| {
            flow_blend_model(t, |raw| {
                let (tile, at) = water_flow_texel_source(raw, bound);
                // A cell standing in `own` reads its own tile as `Own`, so a tile that is the
                // cell's neighbour reads its data under that neighbour's name.
                let named = if tile == WaterFlowTile::Own {
                    own
                } else {
                    tile
                };
                flow_model_tile(named, at)
            })
        };
        // The east border of (5, -10), against (6, -10): the left side is cell (5, -10) at u = 1 - eps,
        // the right side cell (6, -10) at u = eps, on the same row.
        for row in [3.5, 31.25, 62.75] {
            let left = |bound| sample(WaterFlowTile::Own, Vec2::new(64.0 - eps, row), bound);
            let right = sample(WaterFlowTile::East, Vec2::new(eps, row), 0);
            assert!((left(0b001) - right).abs() < 0.01, "east border, row {row}");
            assert!(
                (left(0) - right).abs() > 0.1,
                "clamped east border, row {row}"
            );
        }
        // The south border of (5, -10), against (5, -11), the cell below it (V runs south).
        for column in [3.5, 31.25, 62.75] {
            let above = |bound| sample(WaterFlowTile::Own, Vec2::new(column, 64.0 - eps), bound);
            let below = sample(WaterFlowTile::South, Vec2::new(column, eps), 0);
            assert!(
                (above(0b010) - below).abs() < 0.01,
                "south border, column {column}"
            );
            assert!(
                (above(0) - below).abs() > 0.1,
                "clamped south border, column {column}"
            );
        }
        // The corner reads the south-east cell.
        let corner = |bound| sample(WaterFlowTile::Own, Vec2::new(64.0 - eps, 64.0 - eps), bound);
        let beyond = sample(WaterFlowTile::SouthEast, Vec2::new(eps, eps), 0);
        assert!((corner(0b111) - beyond).abs() < 0.01);
        assert!((corner(0) - beyond).abs() > 0.1);
    }

    /// The tap's texel lands in the neighbour's first column or row, or clamps when the neighbour
    /// is not bound, and never reads outside the 64 by 64 map.
    #[test]
    fn a_flow_tap_past_the_edge_reads_the_neighbours_first_texel() {
        use WaterFlowTile::*;
        let source = |x, y, bound| water_flow_texel_source(IVec2::new(x, y), bound);
        assert_eq!(source(10, 20, 0b111), (Own, IVec2::new(10, 20)));
        assert_eq!(source(63, 63, 0b111), (Own, IVec2::new(63, 63)));
        assert_eq!(source(64, 20, 0b111), (East, IVec2::new(0, 20)));
        assert_eq!(source(10, 64, 0b111), (South, IVec2::new(10, 0)));
        assert_eq!(source(64, 64, 0b111), (SouthEast, IVec2::new(0, 0)));
        assert_eq!(source(64, 20, 0b110), (Own, IVec2::new(63, 20)));
        assert_eq!(source(10, 64, 0b101), (Own, IVec2::new(10, 63)));
        assert_eq!(source(64, 64, 0b011), (Own, IVec2::new(63, 63)));
        // The neighbours of Taylor's bridge cell (5, -10): east (6, -10), south (5, -11).
        assert_eq!(
            water_flow_neighbour_cells(IVec2::new(5, -10)),
            [IVec2::new(6, -10), IVec2::new(5, -11), IVec2::new(6, -11)]
        );
    }

    /// At the waterline (L = h = 0) retail shows the bed through the body, the shallow colour, a
    /// reduced reflection, almost no refraction offset and a muted sun (RF-WATER-059); the engine's
    /// depth terms reproduce those numbers for RiverWaterFlow and CreekWaterFlow, and the water
    /// shader computes them with the same lines.
    #[test]
    fn the_waterline_matches_the_retail_depth_terms() {
        // RiverWaterFlow: fog -10 / 150 / 0.70, so (opacity, range, F, exponent) = (_, 160, 150, 2.8).
        let river = [0.5, 160.0, 150.0, 2.8];
        let bed_share = |path: f32| 1.0 - water_depth_terms(path, 0.0, river)[1];
        for (path, share) in [
            (0.0, 0.835),
            (5.0, 0.759),
            (20.0, 0.559),
            (50.0, 0.268),
            (100.0, 0.039),
        ] {
            assert!((bed_share(path) - share).abs() < 0.001, "L = {path}");
        }
        // The terms are zero at the waterline, so the colour is the shallow one (lerp x = 0).
        assert_eq!(water_depth_terms(0.0, 0.0, river)[0], 0.0);
        // RiverWaterFlow DepthControl (0.3, 0.95, 0.25, 0.9): reflection x0.70, refraction x0.05,
        // normals x0.75, sun x0.10.
        let at_waterline = |d: [f32; 4]| d.map(|property| water_depth_factor(property, 0.0));
        let river_factors = at_waterline([0.3, 0.95, 0.25, 0.9]);
        for (factor, expected) in river_factors.into_iter().zip([0.70, 0.05, 0.75, 0.10]) {
            assert!((factor - expected).abs() < 1e-6);
        }
        // The refraction offset at the bank is 2 of the river's 40 units, not the full magnitude.
        let offset = water_refraction_offset([1.0, 0.0], 40.0, 0.95, 0.0);
        assert!((offset[0] - 2.0).abs() < 1e-4);
        // CreekWaterFlow: fog 20 / 160 / 0.87 (range 140, exponent 3.48): the bed is the whole
        // body for L <= 20; DepthControl (0.7, 0.1, 0.05, 0.05).
        let creek = [0.5, 140.0, 160.0, 3.48];
        assert_eq!(water_depth_terms(20.0, 0.0, creek)[1], 0.0);
        assert!((1.0 - water_depth_terms(50.0, 0.0, creek)[1] - 0.432).abs() < 0.001);
        let creek_factors = at_waterline([0.7, 0.1, 0.05, 0.05]);
        for (factor, expected) in creek_factors.into_iter().zip([0.30, 0.90, 0.95, 0.95]) {
            assert!((factor - expected).abs() < 1e-6);
        }
        // The shader holds the same lines.
        let source = include_str!("shaders/water.wgsl");
        for needle in [
            "return vec3<f32>(along, 1.0 - pow(clear, exponent), vertical);",
            "let clear = clamp((far - min(path, far)) / range, 0.0, 1.0);",
            "let depth_reflections = water.depth_control.x * (body.x - 1.0) + 1.0;",
            "let specular_depth = water.depth_control.w * (body.z - 1.0) + 1.0;",
        ] {
            assert!(source.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn the_sky_is_set_only_when_there_is_one() {
        let sky = crate::sky::SkyUniform::new(&crate::sky::SkyPalette::default());
        let without = WaterExtension::default();
        assert_eq!(without.settings.depth_params.w, 0.0);
        assert!(!without.sky_matches(&sky));
        let with = WaterExtension::default().with_sky(Some(&sky));
        assert_eq!(with.settings.depth_params.w, 1.0);
        assert!(with.sky_matches(&sky));
        assert_eq!(with.with_sky(None).settings.depth_params.w, 0.0);
    }

    #[test]
    fn the_distance_term_is_the_retail_one() {
        // RF-SHADER-008: distT = sat(lerp(flag, 1, (D - 8192) / (falloff - 8192))), the falloff
        // clamped to [1, 8191]. With the flag at 1 it is 1 everywhere.
        for distance in [0.0, 100.0, 4000.0, 8192.0, 20000.0] {
            assert_eq!(water_dist_t(distance, 4007.0, WATER_FRAME_FLAG), 1.0);
        }
        // With the flag at 0 the term follows the ratio: 1 up to the falloff (the ratio is above 1
        // there), and a record whose falloff is outside the range is clamped first.
        assert_eq!(water_dist_t(4007.0, 4007.0, 0.0), 1.0);
        assert!(
            (water_dist_t(6000.0, 4007.0, 0.0) - (6000.0 - 8192.0) / (4007.0 - 8192.0)).abs()
                < 1.0e-6
        );
        assert_eq!(water_dist_t(9000.0, 4007.0, 0.0), 0.0);
        assert_eq!(water_dist_t(100.0, 0.0, 0.0), water_dist_t(100.0, 1.0, 0.0));
        assert_eq!(
            water_dist_t(100.0, 90000.0, 0.0),
            water_dist_t(100.0, 8191.0, 0.0)
        );
        // The noise falloff reaches the shader through the material, clamped as in retail.
        let flat = |falloff| {
            WaterExtension::default()
                .with_noise(
                    [None, None, None],
                    WaterNoiseValues {
                        noise_falloff: falloff,
                        ..WaterNoiseValues::flat()
                    },
                )
                .settings
                .depth_params
                .y
        };
        assert_eq!(flat(4007.0), 4007.0);
        assert_eq!(flat(0.0), 1.0);
        assert_eq!(flat(50000.0), 8191.0);
        // The shader holds the same terms.
        let source = include_str!("shaders/water.wgsl");
        assert!(source.contains("const WATER_DISTANCE_LITERAL: f32 = 8192.0;"));
        assert!(source.contains("const WATER_SSR_INTENSITY: f32 = 0.5;"));
        assert!(source.contains("const WATER_FRAME_FLAG: f32 = 1.0;"));
        assert!(
            source.contains("clamp(dist_t * WATER_SSR_INTENSITY * reflection_sample.a, 0.0, 1.0)")
        );
        assert!(source.contains(
            "let m = mix(1.0, fresnel * depth_reflections * water.fresnel_reflectivity.z, dist_t);"
        ));
    }

    #[test]
    fn the_wind_direction_is_the_bearing_the_wind_comes_from() {
        // The retail scroll (`SkyrimSE.exe` 0x1405290ac; `evidence/rea-20261114/q1-water.md`) puts
        // the pattern toward the wind direction plus 180 degrees. The five directional retail
        // records agree with their names under it: first-layer direction, then the bearing the
        // water flows toward.
        for (record, direction, toward) in [
            ("RiverWaterFlowNE", 233.0, 53.0),
            ("RiverWaterFlowNW", 141.0, 321.0),
            ("RiverWaterFlowSE", 340.0, 160.0),
            ("CreekWaterFlowSW", 33.0, 213.0),
            ("CreekWaterFlowSE", 360.0, 180.0),
        ] {
            let bearing = pattern_bearing(direction);
            let off = (bearing - toward + 540.0).rem_euclid(360.0) - 180.0;
            assert!(
                off.abs() < 0.5,
                "{record}: {direction} degrees moves toward {bearing}, not {toward}"
            );
        }
    }

    #[test]
    fn the_noise_scroll_is_the_retail_formula() {
        // frac(sin(theta) * speed * T) in U and frac(cos(theta) * speed * T) in V.
        let scroll = noise_scroll(90.0, 0.5, 3.0);
        assert!((scroll.x - 0.5).abs() < 1.0e-6, "{scroll}");
        assert!(
            scroll.y.abs() < 1.0e-4 || (scroll.y - 1.0).abs() < 1.0e-4,
            "{scroll}"
        );
        // Negative products wrap upward, as the retail `fmodf(x, 1) + 1` does.
        let scroll = noise_scroll(270.0, 0.25, 1.0);
        assert!((scroll.x - 0.75).abs() < 1.0e-6, "{scroll}");
        // The shader holds the same formula.
        let source = include_str!("shaders/water.wgsl");
        assert!(
            source.contains("return fract(vec2<f32>(sin(theta), cos(theta)) * speed * seconds);")
        );
        assert_eq!(WATER_DEGREES_TO_RADIANS, 0.017453292);
    }

    #[test]
    fn the_users_wave_settings_are_multipliers_that_are_neutral_at_their_defaults() {
        let water = WaterExtension::default();
        let defaults = crate::settings::RenderingSettings::default();
        let wanted = wanted_water_settings(&water, 0.0, Some(&defaults));
        assert_eq!(wanted.wave_scale_speed_strength.x, 1.0);
        assert_eq!(wanted.wave_scale_speed_strength.y, 1.0);
        assert_eq!(wanted.wave_scale_speed_strength.z, 1.0);
        let doubled = crate::settings::RenderingSettings {
            wave_scale: defaults.wave_scale * 2.0,
            wave_speed: defaults.wave_speed * 2.0,
            wave_strength: defaults.wave_strength * 2.0,
            ..defaults.clone()
        };
        let wanted = wanted_water_settings(&water, 0.0, Some(&doubled));
        assert_eq!(wanted.wave_scale_speed_strength.x, 2.0);
        assert_eq!(wanted.wave_scale_speed_strength.y, 2.0);
        assert_eq!(wanted.wave_scale_speed_strength.z, 2.0);
        let off = crate::settings::RenderingSettings {
            water_waves: false,
            ..defaults
        };
        assert_eq!(
            wanted_water_settings(&water, 0.0, Some(&off))
                .wave_scale_speed_strength
                .z,
            0.0
        );
    }

    #[test]
    fn the_shader_has_no_tuned_or_legacy_constants_left() {
        // Every number in the water shader is a WATR field, a number from a cited source, or an
        // engineering guard. The names below were tuned by eye or kept from the old waves, and are
        // gone.
        let source = include_str!("shaders/water.wgsl");
        for gone in [
            "REFLECTION_SHARE",
            "REFLECTION_DISTORTION",
            "MAX_REFLECTION_OFFSET",
            "NORMAL_FADE_START",
            "NORMAL_FADE_END",
            "SECOND_OCTAVE",
            "MAX_WATER_PATH",
            "LEGACY",
            "flow_direction",
            "0.006",
        ] {
            assert!(!source.contains(gone), "{gone} is back in water.wgsl");
        }
    }

    #[test]
    fn with_body_sets_the_shallow_colour_and_clamps_opacity() {
        let water = WaterExtension::default().with_body(
            DEFAULT_WATER_SHALLOW_COLOR,
            50,
            WaterFog::default(),
        );
        assert_eq!(water.settings.body.x, 0.5);
        // DefaultWater's fog as the retail copy writes it: range 160, F 150, exponent 2.8.
        assert_eq!(water.settings.body.yzw(), Vec3::new(160.0, 150.0, 2.8));
        assert_eq!(
            water.settings.shallow_color,
            linear_rgb(water_srgb(DEFAULT_WATER_SHALLOW_COLOR, [1.0; 3]))
        );
        // A WATR ANAM above 100 must not push the body past fully opaque.
        let clamped = WaterExtension::default().with_body(
            DEFAULT_WATER_SHALLOW_COLOR,
            250,
            WaterFog::default(),
        );
        assert_eq!(clamped.settings.body.x, 1.0);
    }

    #[test]
    fn with_body_takes_the_records_fog_terms() {
        let puddle = WaterFog {
            near: 0.0,
            far: 10.0,
            amount: 0.0,
        };
        let water = WaterExtension::default().with_body([1, 2, 3], 25, puddle);
        // Range 10, F 10, and the exponent floors at 1: no fog amount does not clear the body.
        assert_eq!(water.settings.body, Vec4::new(0.25, 10.0, 10.0, 1.0));
    }

    #[test]
    fn final_renderer_requires_every_gpu_path_signal() {
        let complete = RendererMetrics {
            gpu_preprocessing_active: true,
            gpu_culling_active: true,
            indirect_drawing_active: true,
            occlusion_culling_views: 1,
            hzb_views: 1,
            indirect_phase_buffers: 1,
            indirect_batch_sets: 1,
            proof_frames: 1,
            ..default()
        };
        assert!(complete.final_path_active());
        assert!(
            !RendererMetrics {
                hzb_views: 0,
                ..complete.clone()
            }
            .final_path_active()
        );
        assert!(
            !RendererMetrics {
                renderer_validation_failures: 1,
                ..complete
            }
            .final_path_active()
        );
    }

    #[test]
    fn reflection_alone_cannot_prove_direct_world_drawing() {
        use crate::world::components::StreamingCamera;
        use bevy::render::camera::CameraRenderGraph;
        let extracted = || {
            let camera = Camera::default();
            ExtractedCamera {
                target: None,
                physical_viewport_size: None,
                physical_target_size: None,
                viewport: None,
                schedule: CameraRenderGraph::new(bevy::core_pipeline::schedule::Core3d).0,
                order: 0,
                output_mode: camera.output_mode,
                msaa_writeback: camera.msaa_writeback,
                clear_color: camera.clear_color,
                sorted_camera_index_for_target: 0,
                exposure: 1.0,
                hdr: false,
                compositing_space: None,
            }
        };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(RendererProofBridge::default())
            .add_systems(Update, sample_renderer_path);
        let world = app.world_mut().spawn((extracted(), StreamingCamera)).id();
        app.world_mut().spawn((extracted(), NoIndirectDrawing));
        app.update();
        assert_eq!(
            app.world()
                .resource::<RendererProofBridge>()
                .0
                .direct_views
                .load(Ordering::Relaxed),
            0
        );
        app.world_mut().entity_mut(world).insert(NoIndirectDrawing);
        app.update();
        assert_eq!(
            app.world()
                .resource::<RendererProofBridge>()
                .0
                .direct_views
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn direct_path_requires_metal_and_a_proven_direct_view() {
        let direct = RendererMetrics {
            metal_backend: true,
            direct_drawing_views: 2,
            proof_frames: 1,
            ..default()
        };
        assert!(direct.final_path_active());
        assert!(
            !RendererMetrics {
                direct_drawing_views: 0,
                ..direct.clone()
            }
            .final_path_active()
        );
        assert!(
            !RendererMetrics {
                metal_backend: false,
                ..direct.clone()
            }
            .final_path_active()
        );
        assert!(
            !RendererMetrics {
                proof_frames: 0,
                ..direct.clone()
            }
            .final_path_active()
        );
        assert!(!RendererMetrics::default().final_path_active());
    }

    /// The shader tiles every layer by the shared LAND repeats per cell, so the layer textures
    /// must be sampled with a repeating address mode. Bevy's default clamps to the edge, which
    /// stretched the textures in the frames: everything past the first tile read the edge texels.
    #[test]
    fn dropping_normal_maps_unbinds_them_and_clears_their_flags() {
        let mut extension = TerrainExtension::default();
        let normals: [Option<Handle<Image>>; 6] =
            std::array::from_fn(|index| (index % 2 == 0).then(Handle::default));
        extension.settings.set_normal_layers(&normals);
        [extension.normal_0, extension.normal_2, extension.normal_4] =
            [normals[0].clone(), normals[2].clone(), normals[4].clone()];
        assert!(extension.has_normal_maps());

        extension.drop_normal_maps();

        assert!(!extension.has_normal_maps());
        assert!(
            [
                &extension.normal_0,
                &extension.normal_1,
                &extension.normal_2,
                &extension.normal_3,
                &extension.normal_4,
                &extension.normal_5,
            ]
            .iter()
            .all(|normal| normal.is_none())
        );
    }

    #[test]
    fn tiled_terrain_layers_are_sampled_with_a_repeating_sampler() {
        let sampler = terrain_layer_sampler();
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_w, ImageAddressMode::Repeat);
        // Repeat and 16x anisotropy retain linear filtering and the default mip range.
        let default_sampler = ImageSamplerDescriptor::linear();
        assert_eq!(sampler.mag_filter, default_sampler.mag_filter);
        assert_eq!(sampler.min_filter, default_sampler.min_filter);
        assert_eq!(sampler.mipmap_filter, default_sampler.mipmap_filter);
        assert_eq!(sampler.lod_min_clamp, default_sampler.lod_min_clamp);
        assert_eq!(sampler.lod_max_clamp, default_sampler.lod_max_clamp);
        assert_eq!(sampler.anisotropy_clamp, 16);
        assert_eq!(sampler.compare, default_sampler.compare);
        assert_eq!(sampler.mag_filter, ImageFilterMode::Linear);
        assert_eq!(
            default_sampler.address_mode_u,
            ImageAddressMode::ClampToEdge
        );
        assert_eq!(
            default_sampler.address_mode_v,
            ImageAddressMode::ClampToEdge
        );
        assert!(
            TerrainSettings::vertex_weights_only(1.0)
                .tiling_and_layer_count
                .x
                > 1.0,
            "layers only need a repeating sampler because the shader tiles them"
        );
    }

    /// Configured defaults affect only null bases, including absent bases in all quadrants.
    #[test]
    fn terrain_configured_default_land_is_scoped_to_null_bases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT,normal_path TEXT);
            CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER);
            CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT);
            INSERT INTO texture_sets VALUES(1,'textures/poison.dds','textures/poison_n.dds');
            INSERT INTO landscape_textures VALUES(0,1),(10,1);").unwrap();
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>();
        let asset_server = app.world().resource::<AssetServer>();
        let mut terrain = terrain_fixture_with_overlays(91, &[vec![(3, 0.625)]]);
        for layer in &mut terrain.layers {
            layer.texture_form_id = 0;
        }
        let old = AssetCatalog::open(&path).unwrap();
        assert!(
            old.landscape_diffuse(0).is_none(),
            "legacy packs without default metadata keep a white null base"
        );
        let (material, images) =
            TerrainExtension::from_quadrant(&terrain, 0, &old, asset_server, true).unwrap();
        assert!(images.color.is_empty() && images.normal.is_empty());
        assert!(material.layer_0.is_none());
        connection.execute_batch("CREATE TABLE terrain_default_texture(id INTEGER PRIMARY KEY,diffuse_path TEXT,normal_path TEXT);
            INSERT INTO terrain_default_texture VALUES(0,NULL,NULL);").unwrap();
        for (diffuse, normal) in [
            (None, None),
            (
                Some("terrain/custom.ktx2"),
                Some("terrain/custom-normal.ktx2"),
            ),
            (Some("terrain/custom.ktx2"), None),
            (Some("../escape.ktx2"), Some("terrain/orphan.ktx2")),
        ] {
            connection
                .execute(
                    "UPDATE terrain_default_texture SET diffuse_path=?1,normal_path=?2",
                    rusqlite::params![diffuse, normal],
                )
                .unwrap();
            let catalog = AssetCatalog::open(&path).unwrap();
            let expected = diffuse.filter(|path| !path.starts_with(".."));
            let (material, images) =
                TerrainExtension::from_quadrant(&terrain, 0, &catalog, asset_server, true).unwrap();
            assert_eq!(images.color.len(), usize::from(expected.is_some()));
            assert_eq!(
                images.normal.len(),
                usize::from(expected.is_some() && normal.is_some())
            );
            if let Some(path) = expected {
                assert_eq!(
                    material.layer_0.as_ref().unwrap().path().unwrap().path(),
                    std::path::Path::new(path)
                );
            }
            assert!(
                material.layer_1.is_none(),
                "zero overlay never receives the default"
            );
            assert_eq!(material.settings.weights[0][3], 0.625);
            let mut unpainted = terrain.clone();
            unpainted.layers.clear();
            for quadrant in 0..4 {
                let (empty, _) = TerrainExtension::from_quadrant(
                    &unpainted,
                    quadrant,
                    &catalog,
                    asset_server,
                    true,
                )
                .unwrap();
                assert_eq!(empty.layer_0, material.layer_0);
            }
            let mut painted = terrain.clone();
            painted
                .layers
                .iter_mut()
                .find(|layer| layer.is_base)
                .unwrap()
                .texture_form_id = 10;
            let (painted, _) =
                TerrainExtension::from_quadrant(&painted, 0, &catalog, asset_server, true).unwrap();
            assert_eq!(
                painted.layer_0.unwrap().path().unwrap().path(),
                std::path::Path::new("textures/poison.ktx2")
            );
        }
    }

    fn bc1_ktx2() -> Vec<u8> {
        use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};

        let mut dds = Dds::new_dxgi(NewDxgiParams {
            height: 8,
            width: 8,
            depth: None,
            format: DxgiFormat::BC1_UNorm,
            mipmap_levels: Some(1),
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Straight,
        })
        .unwrap();
        for (index, byte) in dds.data.iter_mut().enumerate() {
            *byte = (index * 7 + 3) as u8;
        }
        let mut bytes = Vec::new();
        dds.write(&mut bytes).unwrap();
        converter::texture::TextureConverter::convert_uncompressed(
            &bytes,
            converter::texture::TextureEncoding::ColorSrgb,
        )
        .unwrap()
    }

    #[test]
    fn terrain_diffuse_loads_in_the_profile_colour_space() {
        use bevy::asset::{AssetPlugin, LoadState};
        use bevy::image::{CompressedImageFormats, ImageLoader, ImagePlugin};

        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("textures")).unwrap();
        std::fs::write(directory.path().join("textures/diffuse.ktx2"), bc1_ktx2()).unwrap();
        let database = directory.path().join("catalogue.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT);
                 CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER);
                 CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT);
                 INSERT INTO texture_sets VALUES(1,'textures/diffuse.ktx2');
                 INSERT INTO landscape_textures VALUES(1,1);",
            )
            .unwrap();
        drop(connection);
        let catalog = AssetCatalog::open(&database).unwrap();
        for (colour_srgb, expected) in [
            (
                true,
                bevy::render::render_resource::TextureFormat::Bc1RgbaUnormSrgb,
            ),
            (
                false,
                bevy::render::render_resource::TextureFormat::Bc1RgbaUnorm,
            ),
        ] {
            let mut app = App::new();
            app.add_plugins((
                MinimalPlugins,
                AssetPlugin {
                    file_path: directory.path().to_string_lossy().into_owned(),
                    ..default()
                },
                ImagePlugin::default(),
            ))
            .init_asset::<Image>()
            .register_asset_loader(ImageLoader::new(CompressedImageFormats::all()));
            app.finish();
            app.cleanup();
            let server = app.world().resource::<AssetServer>().clone();
            let terrain = terrain_fixture_with_overlays(7, &[]);
            let (_extension, images) =
                TerrainExtension::from_quadrant(&terrain, 0, &catalog, &server, colour_srgb)
                    .unwrap();
            assert_eq!(images.color.len(), 1);
            let handle = images.color[0].clone();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                app.update();
                let server = app.world().resource::<AssetServer>();
                if let LoadState::Failed(error) = server.load_state(handle.id()) {
                    panic!("diffuse load failed: {error}");
                }
                if app
                    .world()
                    .resource::<Assets<Image>>()
                    .get(&handle)
                    .is_some()
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "diffuse load timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let image = app
                .world()
                .resource::<Assets<Image>>()
                .get(&handle)
                .unwrap();
            assert_eq!(
                image.texture_descriptor.format, expected,
                "colour_srgb={colour_srgb}"
            );
        }
    }

    /// A null base binds Dirt02 while zero overlays keep slots and weights without requests.
    /// A missing nonzero diffuse retains its slot and weights without requesting an orphan normal.
    #[test]
    fn terrain_zero_texture_overlay_material_preserves_slots_without_image_requests() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("placeholder-catalogue.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT,normal_path TEXT);
                 CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER);
                 CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT);
                 CREATE TABLE terrain_default_texture(id INTEGER PRIMARY KEY,diffuse_path TEXT,normal_path TEXT);
                 INSERT INTO terrain_default_texture VALUES(0,'textures/landscape/dirt02.ktx2','textures/landscape/dirt02_n.ktx2');
                 INSERT INTO texture_sets VALUES(1,'textures/zero-must-not-load.dds',NULL);
                 INSERT INTO texture_sets VALUES(2,'textures/painted.dds',NULL);
                 INSERT INTO texture_sets VALUES(3,NULL,'textures/orphan-normal-must-not-load.dds');
                 INSERT INTO landscape_textures VALUES(0,1),(2116,3),(2117,2);",
            )
            .unwrap();
        drop(connection);
        let catalog = AssetCatalog::open(&path).unwrap();
        assert!(
            catalog.landscape_diffuse(0).is_some(),
            "the default exists independently of the poison database row"
        );
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>();
        let asset_server = app.world().resource::<AssetServer>();
        let mut terrain =
            terrain_fixture_with_overlays(91, &[vec![(0, 0.25), (18, 0.75)], vec![(3, 0.625)]]);
        for layer in &mut terrain.layers {
            layer.texture_form_id = 0;
            if !layer.is_base && layer.layer == 2 {
                layer.layer = 4;
            }
        }
        let (extension, images) =
            TerrainExtension::from_quadrant(&terrain, 0, &catalog, asset_server, true)
                .expect("zero overlays keep material slots and do not request a texture");
        assert_eq!(images.color.len(), 1);
        assert_eq!(images.normal.len(), 1);
        let diffuse = extension
            .layer_0
            .as_ref()
            .expect("null base must bind diffuse");
        let normal = extension
            .normal_0
            .as_ref()
            .expect("null base must bind normal");
        assert_eq!(
            diffuse.path().unwrap().path(),
            std::path::Path::new("textures/landscape/dirt02.ktx2")
        );
        assert_eq!(
            normal.path().unwrap().path(),
            std::path::Path::new("textures/landscape/dirt02_n.ktx2")
        );
        assert_eq!(extension.sampler_source.as_ref(), Some(diffuse));
        assert!(extension.layer_1.is_none() && extension.layer_2.is_none());
        assert!(
            extension.layer_3.is_none()
                && extension.layer_4.is_none()
                && extension.layer_5.is_none()
        );
        assert_eq!(extension.settings.tiling_and_layer_count.z, 3.0);
        assert_eq!(extension.settings.normal_layers_0, Vec4::X);
        assert_eq!(extension.settings.normal_layers_1, Vec4::ZERO);
        let sample = |overlay: usize, vertex: usize| {
            extension.settings.weights[overlay * OVERLAY_WEIGHT_WORDS + vertex / 4][vertex % 4]
        };
        assert_eq!(sample(0, 0).to_bits(), 0.25f32.to_bits());
        assert_eq!(sample(0, 18).to_bits(), 0.75f32.to_bits());
        assert_eq!(sample(1, 3).to_bits(), 0.625f32.to_bits());
        assert_eq!(sample(1, 0), 0.0);

        terrain.layers[2].texture_form_id = 0x844;
        assert!(catalog.landscape_normal(0x844).is_some());
        let (fallback, images) =
            TerrainExtension::from_quadrant(&terrain, 0, &catalog, asset_server, true)
                .expect("a missing diffuse retains the layer without requesting either image");
        assert_eq!(images.color.len(), 1);
        assert_eq!(images.normal.len(), 1);
        assert_eq!(fallback.layer_0, extension.layer_0);
        assert_eq!(fallback.normal_0, extension.normal_0);
        assert!(fallback.layer_2.is_none() && fallback.normal_2.is_none());
        assert_eq!(fallback.settings.weights, extension.settings.weights);
        assert_eq!(fallback.settings.tiling_and_layer_count.z, 3.0);
        assert_eq!(fallback.settings.normal_layers_0, Vec4::X);
        assert_eq!(fallback.settings.normal_layers_1, Vec4::ZERO);

        // A white configured default preserves #500's no-image fallback for every layer.
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE terrain_default_texture SET diffuse_path=NULL,normal_path=NULL",
                [],
            )
            .unwrap();
        let white = AssetCatalog::open(&path).unwrap();
        let (fallback, images) =
            TerrainExtension::from_quadrant(&terrain, 0, &white, asset_server, true).unwrap();
        assert!(images.color.is_empty() && images.normal.is_empty());
        assert!(fallback.image_ids().next().is_none());
        assert_eq!(fallback.settings.weights, extension.settings.weights);
        assert_eq!(fallback.settings.tiling_and_layer_count.z, 3.0);
        assert_eq!(fallback.settings.normal_layers_0, Vec4::ZERO);
        assert_eq!(fallback.settings.normal_layers_1, Vec4::ZERO);
        terrain.layers[2].texture_form_id = 0;
        assert!(TerrainExtension::from_quadrant(&terrain, 0, &catalog, asset_server, true).is_ok());
        terrain.layers[2].texture_form_id = 0x845;
        let (painted, images) =
            TerrainExtension::from_quadrant(&terrain, 0, &catalog, asset_server, true).unwrap();
        assert_eq!(images.color.len(), 2);
        assert_eq!(
            painted.layer_2.as_ref().unwrap().path().unwrap().path(),
            std::path::Path::new("textures/painted.ktx2")
        );
        assert_eq!(painted.settings.weights, extension.settings.weights);
        terrain.layers.clear();
        let (unpainted, images) =
            TerrainExtension::from_quadrant(&terrain, 0, &catalog, asset_server, true).unwrap();
        assert_eq!(images.color.len(), 1);
        assert_eq!(images.normal.len(), 1);
        assert_eq!(unpainted.layer_0, extension.layer_0);
        assert_eq!(unpainted.normal_0, extension.normal_0);
    }

    /// The weight field is the quadrant's own `17x17` grids, one per overlay, in the order
    /// `quadrant_layers` returns them: an overlay's `VTXT` list reaches the fragment stage at the
    /// samples it names, and every sample it does not name reads as opacity 0.
    #[test]
    fn weight_field_carries_every_overlay_grid_from_the_layer_list() {
        let terrain = terrain_fixture_with_overlays(
            0x0001_2345,
            &[
                vec![(0, 1.0), (17 + 1, 0.5), (16 * 17 + 16, 0.25)],
                vec![(17 * 8 + 8, 0.75)],
            ],
        );
        let grids = crate::streaming::quadrant_overlay_weights(&terrain, 0).unwrap();
        assert_eq!(grids.len(), 2, "one grid per overlay, base excluded");
        assert_eq!(
            grids[0].len(),
            QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES,
            "a grid covers the quadrant's whole sample square"
        );
        let settings = TerrainSettings::for_quadrant(0, 3, &grids);
        assert_eq!(
            settings.tiling_and_layer_count,
            Vec4::new(24.0, 24.0, 3.0, 0.0)
        );
        assert_eq!(settings.quadrant_origin.xy(), Vec2::ZERO);
        assert_eq!(
            settings.weight_source.x, 1.0,
            "the material carries the weight field"
        );

        let sample = |overlay: usize, x: usize, y: usize| -> f32 {
            let index = y * QUADRANT_WEIGHT_SAMPLES + x;
            settings.weights[overlay * OVERLAY_WEIGHT_WORDS + index / 4][index % 4]
        };
        assert_eq!(sample(0, 0, 0), 1.0);
        assert_eq!(sample(0, 1, 1), 0.5);
        assert_eq!(sample(0, 16, 16), 0.25);
        assert_eq!(sample(0, 5, 5), 0.0, "an unnamed sample is opacity 0");
        assert_eq!(sample(1, 8, 8), 0.75, "the second overlay's own grid");
        assert_eq!(
            sample(1, 0, 0),
            0.0,
            "the two grids do not bleed into each other"
        );
        for overlay in 2..OVERLAY_WEIGHT_SLOTS {
            assert_eq!(
                sample(overlay, 8, 8),
                0.0,
                "an overlay the quadrant does not use leaves its slot empty"
            );
        }
        assert_eq!(
            settings.weights[OVERLAY_WEIGHT_WORDS - 1].w,
            0.0,
            "the padding word after the last sample stays zero"
        );
    }

    /// Each quadrant reads its own origin out of the cell, so the same cell uv addresses the same
    /// ground in every quadrant.
    #[test]
    fn quadrant_origin_places_each_quadrant_inside_the_cell() {
        let origins: Vec<[f32; 2]> = (0..4)
            .map(|quadrant| {
                let settings = TerrainSettings::for_quadrant(quadrant, 2, &[]);
                assert_eq!(
                    settings.quadrant_origin.z, 0.0,
                    "only xy carries the origin"
                );
                [settings.quadrant_origin.x, settings.quadrant_origin.y]
            })
            .collect();
        assert_eq!(
            origins,
            [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            "the mesh's quadrants in the same order: west/east then south/north"
        );
    }

    /// The weight field is only worth reading where there are overlays to read. A quadrant whose only
    /// layer is its base has none, and the packed vertex attributes carry none either, so the shader
    /// is told to skip the field's five lookups altogether.
    #[test]
    fn weight_source_is_set_only_for_quadrants_with_overlays() {
        let grid = vec![1.0; QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES];
        assert_eq!(
            TerrainSettings::for_quadrant(0, 2, &[grid]).weight_source,
            Vec4::X
        );
        assert_eq!(
            TerrainSettings::for_quadrant(0, 1, &[]).weight_source,
            Vec4::ZERO,
            "a base-only quadrant has no overlay to read"
        );
        assert_eq!(
            TerrainSettings::vertex_weights_only(6.0).weight_source,
            Vec4::ZERO,
            "a material with no weight field reads the packed attributes"
        );
    }

    /// A fixture material carries the quadrant's own weight field, so the no-game-data scene draws
    /// its overlays through the interpolation the streamed path uses instead of the packed tangent.
    #[test]
    fn fixture_materials_carry_the_quadrants_weight_field() {
        let sample = QUADRANT_WEIGHT_SAMPLES * 8 + 8;
        let terrain =
            terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0), (sample as u16, 0.5)]]);
        for quadrant in 0..4u8 {
            let extension = TerrainExtension::fixture(
                &terrain,
                quadrant,
                std::array::from_fn(|_| Handle::<Image>::default()),
            )
            .expect("the fixture material must build");
            assert!(
                extension.reads_weight_field(),
                "quadrant {quadrant} must render its overlay through the weight field"
            );
            assert_eq!(
                extension.settings.quadrant_origin.xy(),
                Vec2::new(f32::from(quadrant % 2), f32::from(quadrant / 2)),
                "each quadrant reads its own origin"
            );
            assert_eq!(extension.settings.weights[0][0], 1.0);
            assert_eq!(
                extension.settings.weights[sample / 4][sample % 4],
                0.5,
                "the fixture's own VTXT opacity reaches the field"
            );
        }
    }

    /// The overlay weight field is the quadrant's own sample grid, interpolated bilinearly. A layer
    /// that is full strength on one sample and absent on the next must reach the fragment as the
    /// linear interpolation of the two - `0.5` halfway between them - not as the sharpened ramp the
    /// packed vertex attributes produce.
    #[test]
    fn weight_field_interpolates_linearly_between_samples() {
        let terrain = terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0)]]);
        let grids = crate::streaming::quadrant_overlay_weights(&terrain, 0).unwrap();
        let settings = TerrainSettings::for_quadrant(0, 2, &grids);

        assert_eq!(shader_weight(&settings, 0, [0.0, 0.0]), 1.0);
        // Sample 0 is the quadrant's (x = 0, y = 0) corner and its x neighbour is empty, so one
        // sample step in x crosses an opacity of 1.0 down to 0.0.
        let halfway = shader_weight(&settings, 0, [0.5, 0.0]);
        assert!(
            (halfway - 0.5).abs() < 1.0e-5,
            "the midpoint of a 1.0 -> 0.0 edge must read 0.5, not the packed tangent's 0.25: {halfway}"
        );
        assert!((shader_weight(&settings, 0, [0.25, 0.0]) - 0.75).abs() < 1.0e-5);
        assert!((shader_weight(&settings, 0, [0.75, 0.0]) - 0.25).abs() < 1.0e-5);
        // Both axes: the field is bilinear, so the centre of the quad whose only full sample is the
        // corner reads 0.25 - what Skyrim's interpolation gives, and not what the triangle split
        // would if the weight stayed per vertex.
        assert!(
            (shader_weight(&settings, 0, [0.5, 0.5]) - 0.25).abs() < 1.0e-5,
            "the quad centre is the bilinear blend of its four samples"
        );
        // At the quadrant's far edge the field clamps to the last sample instead of wrapping.
        assert_eq!(shader_weight(&settings, 0, [16.0, 16.0]), 0.0);
        assert_eq!(shader_weight(&settings, 0, [16.0, 0.0]), 0.0);
        assert_eq!(shader_weight(&settings, 0, [0.0, 16.0]), 0.0);
        for overlay in 1..OVERLAY_WEIGHT_SLOTS {
            assert_eq!(shader_weight(&settings, overlay, [0.0, 0.0]), 0.0);
        }
    }

    #[test]
    fn landscape_uvs_repeat_twelve_times_per_quadrant_without_scaling_weights() {
        let terrain = terrain_fixture_with_overlays(0x12345, &[vec![(0, 1.0)]]);
        for quadrant in 0..4 {
            let mesh = crate::streaming::build_terrain_quadrant_mesh(&terrain, quadrant).unwrap();
            let VertexAttributeValues::Float32x2(uvs) =
                mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap()
            else {
                panic!("UVs missing")
            };
            let grids = crate::streaming::quadrant_overlay_weights(&terrain, quadrant).unwrap();
            let settings = TerrainSettings::for_quadrant(quadrant, 2, &grids);
            let tiling = settings.tiling_and_layer_count.xy();
            let origin = Vec2::from(uvs[0]);
            assert_eq!(
                (Vec2::from(uvs[16]) - origin) * tiling,
                Vec2::new(12.0, 0.0)
            );
            assert_eq!(
                (Vec2::from(uvs[272]) - origin) * tiling,
                Vec2::new(0.0, 12.0)
            );
            // Blend coordinates still address the original 17x17 LAND samples.
            let last = (Vec2::from(uvs[288]) * 2.0 - settings.quadrant_origin.xy()) * 16.0;
            assert_eq!(last, Vec2::splat(16.0));
        }
    }

    /// A coordinate outside the sample square reads the sample it is clamped onto. `grid_point`
    /// clamps before `grid_weight` takes its blend fraction, so a coordinate an ulp below the
    /// quadrant's west or south edge reads that edge sample rather than blending its neighbour in.
    #[test]
    fn weight_field_clamps_coordinates_onto_the_sample_square() {
        let last = (QUADRANT_WEIGHT_SAMPLES - 1) as f32;
        let edge = QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES - 1;
        let terrain =
            terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0), (edge as u16, 0.25)]]);
        let grids = crate::streaming::quadrant_overlay_weights(&terrain, 0).unwrap();
        let settings = TerrainSettings::for_quadrant(0, 2, &grids);

        assert_eq!(shader_weight(&settings, 0, [-1.0e-7, 0.0]), 1.0);
        assert_eq!(shader_weight(&settings, 0, [0.0, -1.0e-7]), 1.0);
        assert_eq!(shader_weight(&settings, 0, [last + 1.0e-7, last]), 0.25);
        assert_eq!(shader_weight(&settings, 0, [last, last + 1.0e-7]), 0.25);
    }

    /// The mechanism the weight field replaces. `build_terrain_quadrant_mesh` still packs overlays
    /// into `ATTRIBUTE_TANGENT` for materials without a weight field, and Bevy re-normalizes the
    /// tangent's direction in the vertex shader (`mesh_tangent_local_to_world`), so the direction
    /// interpolates at full length while the magnitude it is multiplied by does not: halfway across
    /// the same 1.0 -> 0.0 edge the product reads 0.25 where the opacity is 0.5. Streamed quadrants
    /// read the weight field instead.
    #[test]
    fn packed_vertex_weights_sharpen_the_midpoint_to_a_quarter() {
        let terrain = terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0)]]);
        let mesh = crate::streaming::build_terrain_quadrant_mesh(&terrain, 0).unwrap();
        let VertexAttributeValues::Float32x4(tangents) =
            mesh.attribute(Mesh::ATTRIBUTE_TANGENT).unwrap()
        else {
            panic!("terrain tangents must be Float32x4");
        };
        assert_eq!(
            tangents[0],
            [1.0, 0.0, 0.0, 1.0],
            "full weight, unit direction"
        );
        assert_eq!(tangents[1], [0.0; 4], "no weight at all");
        // The vertex shader normalizes xyz per vertex and the rasterizer interpolates the result;
        // the fragment shader multiplies the interpolated xyz by the interpolated magnitude.
        let midpoint: [f32; 4] =
            std::array::from_fn(|axis| (tangents[0][axis] + tangents[1][axis]) * 0.5);
        let reconstructed = midpoint[0] * midpoint[3].abs();
        assert!(
            (reconstructed - 0.25).abs() < 1.0e-5,
            "the packed path reads {reconstructed} where the interpolated opacity is 0.5"
        );
    }

    /// The mesh's cell UVs and the weight field agree sample for sample: `uv * 2 - quadrant_origin`
    /// scaled onto the grid lands exactly on the sample the shader must read for that vertex, with
    /// no scaling or orientation drift between the two.
    #[test]
    fn quadrant_uvs_address_the_weight_grid_sample_for_sample() {
        let samples = QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES;
        // A `VTXT` list that names every sample with its own index, so the weight read at a vertex
        // says which sample the shader looked up.
        let terrain = terrain_fixture_with_overlays(
            0x0001_2345,
            &[overlay_grid(|x, y| {
                (y * QUADRANT_WEIGHT_SAMPLES + x) as f32 / samples as f32
            })],
        );
        let last = (QUADRANT_WEIGHT_SAMPLES - 1) as f32;
        for quadrant in 0..4u8 {
            let mesh = crate::streaming::build_terrain_quadrant_mesh(&terrain, quadrant).unwrap();
            let grids = crate::streaming::quadrant_overlay_weights(&terrain, quadrant).unwrap();
            let settings = TerrainSettings::for_quadrant(quadrant, 2, &grids);
            let VertexAttributeValues::Float32x2(uvs) =
                mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap()
            else {
                panic!("terrain UVs must be Float32x2");
            };
            assert_eq!(uvs.len(), samples, "one vertex per grid sample");
            for (sample, uv) in uvs.iter().enumerate() {
                let coordinate = [
                    (uv[0] * 2.0 - settings.quadrant_origin.x) * last,
                    (uv[1] * 2.0 - settings.quadrant_origin.y) * last,
                ];
                let expected = sample as f32 / samples as f32;
                assert!(
                    (shader_weight(&settings, 0, coordinate) - expected).abs() < 1.0e-5,
                    "quadrant {quadrant} vertex {sample} reads {coordinate:?}, not its own sample"
                );
            }
        }
    }

    /// The uniform's Rust layout and the shader's must say the same numbers, and list the same
    /// fields in the same order - an insertion on one side alone shifts every field after it, and
    /// the shader then reads its neighbour's bytes.
    #[test]
    fn terrain_uniform_layout_matches_the_shader_source() {
        let source = include_str!("shaders/terrain.wgsl");
        assert_eq!(
            shader_constant(source, "WEIGHT_GRID_SIDE"),
            QUADRANT_WEIGHT_SAMPLES
        );
        assert_eq!(
            shader_constant(source, "WEIGHT_GRID_WORDS"),
            OVERLAY_WEIGHT_WORDS
        );
        assert_eq!(shader_array_length(source, "weights"), WEIGHT_FIELD_WORDS);
        assert_eq!(
            OVERLAY_WEIGHT_WORDS, 73,
            "289 samples four to a vec4, rounded up"
        );
        assert_eq!(WEIGHT_FIELD_WORDS, 365, "five overlays of 73 vec4s");

        let settings = TerrainSettings::for_quadrant(
            0,
            2,
            &[vec![1.0; QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES]],
        );
        let ReflectRef::Struct(fields) = settings.reflect_ref() else {
            panic!("TerrainSettings must reflect as a struct");
        };
        let rust_fields: Vec<String> = (0..fields.field_len())
            .map(|index| {
                fields
                    .name_at(index)
                    .expect("every field is named")
                    .to_owned()
            })
            .collect();
        assert_eq!(rust_fields, shader_struct_fields(source, "TerrainSettings"));
    }

    /// The shader's own index arithmetic, against the field the Rust side packs: which word and
    /// component a sample occupies, and which sample each corner of a grid point names. The
    /// expressions are read out of `terrain.wgsl` and evaluated, so a transposed index fails here -
    /// the model in `shader_weight` would transpose with it and keep passing.
    #[test]
    fn shader_weight_index_expressions_address_the_packed_field() {
        let source = include_str!("shaders/terrain.wgsl");
        let side = shader_constant(source, "WEIGHT_GRID_SIDE") as u32;
        let words = shader_constant(source, "WEIGHT_GRID_WORDS") as u32;
        // Every sample carries a value of its own, so an index that lands anywhere but on its own
        // sample reads a different opacity rather than a plausible one.
        let grid = |overlay: u32| -> Vec<f32> {
            (0..side * side)
                .map(|sample| (overlay * side * side + sample) as f32)
                .collect()
        };
        let grids: Vec<Vec<f32>> = (0..OVERLAY_WEIGHT_SLOTS as u32).map(grid).collect();
        let settings = TerrainSettings::for_quadrant(0, OVERLAY_WEIGHT_SLOTS + 1, &grids);

        // How `packed_weight` splits a sample between the word that holds it and the component of
        // that word.
        let packed = shader_function_body(source, "packed_weight");
        let field = packed
            .split_once("terrain.weights[")
            .expect("`packed_weight` must read the weight field")
            .1;
        let (word_of, rest) = field.split_once(']').expect("the word index must close");
        let (_, rest) = rest.split_once('[').expect("the component index must open");
        let component_of = rest.split_once(']').expect("the component must close").0;
        let (word_of, component_of) = (word_of.trim(), component_of.trim());

        // How `grid_weight` names the sample each corner of the coordinate reads, in the order the
        // bilinear blend takes them: west and east of the north row, then of the south row.
        let grid_weight = shader_function_body(source, "grid_weight");
        let corners: Vec<&str> = grid_weight
            .split("packed_weight(overlay, ")
            .skip(1)
            .map(|call| call.split(')').next().expect("a call must close"))
            .collect();
        assert_eq!(
            corners.len(),
            4,
            "the blend reads the four samples around the coordinate"
        );

        // The last row and column have no next sample - the shader clamps `next` there - so the
        // corners are evaluated only where all four neighbours are on the grid.
        for row in [0u32, 1, 7, 15] {
            for column in [0u32, 1, 7, 15] {
                let terms = [
                    ("west", column),
                    ("east", column + 1),
                    ("north", row),
                    ("south", row + 1),
                    ("WEIGHT_GRID_SIDE", side),
                ];
                let samples: Vec<u32> = corners
                    .iter()
                    .map(|corner| evaluate_u32(corner, &terms))
                    .collect();
                assert_eq!(
                    samples,
                    [
                        row * side + column,
                        row * side + column + 1,
                        (row + 1) * side + column,
                        (row + 1) * side + column + 1,
                    ],
                    "the corners of grid point ({column}, {row}) must be its own row and column"
                );
                // Three of the four corners, over three overlays: the field's first, middle and last
                // slots, so an index that reads another overlay's word fails too.
                let probes = [(0u32, samples[0]), (2, samples[3]), (4, samples[2])];
                for (overlay, sample) in probes {
                    let word_terms = [
                        ("overlay", overlay),
                        ("sample", sample),
                        ("WEIGHT_GRID_WORDS", words),
                    ];
                    let word = evaluate_u32(word_of, &word_terms) as usize;
                    let component = evaluate_u32(component_of, &[("sample", sample)]) as usize;
                    assert_eq!(
                        settings.weights[word][component],
                        (overlay * side * side + sample) as f32,
                        "overlay {overlay} sample {sample} must read the Rust side's value"
                    );
                }
            }
        }
    }

    /// The parts of `terrain.wgsl`'s weight arithmetic the evaluator cannot run, pinned as the
    /// shader writes them: the two places that bound the sample grid, where a bound of
    /// `WEIGHT_GRID_SIDE` instead of the last sample would read the next quadrant's edge texels
    /// along this one, and the fragment's quadrant-local coordinate.
    #[test]
    fn shader_weight_field_bounds_its_grid_at_the_last_sample() {
        let source = include_str!("shaders/terrain.wgsl");
        let side = evaluate_u32(shader_constant_expression(source, "WEIGHT_GRID_SIDE"), &[]);
        let last = evaluate_u32(
            shader_constant_expression(source, "GRID_LAST_SAMPLE"),
            &[("WEIGHT_GRID_SIDE", side)],
        );
        assert_eq!(
            last,
            side - 1,
            "the grid's far edge is its last sample index, not the grid's side"
        );

        let point = "clamp(coordinate, vec2<f32>(0.0), vec2<f32>(f32(GRID_LAST_SAMPLE)))";
        assert!(
            without_whitespace(shader_function_body(source, "grid_point"))
                .contains(&without_whitespace(point)),
            "a coordinate must be clamped onto the sample square before its blend fraction is taken"
        );
        let next = "min(base + vec2<u32>(1u), vec2<u32>(GRID_LAST_SAMPLE))";
        assert!(
            without_whitespace(shader_function_body(source, "grid_weight"))
                .contains(&without_whitespace(next)),
            "the blend must pair the far edge's last sample with itself"
        );
        let coordinate = "(in.uv * 2.0 - terrain.quadrant_origin.xy) * f32(GRID_LAST_SAMPLE)";
        assert!(
            without_whitespace(source).contains(&without_whitespace(coordinate)),
            "the fragment's coordinate must be quadrant-local, on the grid's own scale"
        );
    }
}

/// A CPU mirror of the body maths in `shaders/water.wgsl` (`water_path_length`, `water_body` and
/// the colour mix in `fragment`), fed from a real `WaterExtension`'s uniform. It is not a GPU
/// test: the WGSL is not executed, so a change to the shader equations must be repeated here.
#[cfg(test)]
mod water_body_tests {
    use super::*;

    const NEAR_PLANE: f32 = 0.1;
    /// The shader's `BOTTOMLESS_PATH`.
    const BOTTOMLESS_PATH: f32 = 1.0e7;
    /// What `water_depth.rs` clears the snapshot to when it cannot resolve the bed.
    const UNRESOLVED: f32 = -1.0;

    /// Camera at the origin looking down -Z, water surface 50 units below it and 100 ahead.
    const SURFACE_VIEW: Vec3 = Vec3::new(0.0, -50.0, -100.0);

    /// Returns (colour mix toward deep, body alpha) for one snapshot texel.
    fn shade(extension: &WaterExtension, texel: f32) -> (f32, f32) {
        let settings = &extension.settings;
        let path = if settings.depth_params.x < 0.5 || texel < 0.0 {
            -1.0
        } else if texel <= 0.0 {
            BOTTOMLESS_PATH
        } else {
            let bed_view_z = -NEAR_PLANE / texel;
            let past_surface = (bed_view_z / SURFACE_VIEW.z.min(-1e-4) - 1.0).max(0.0);
            (SURFACE_VIEW.length() * past_surface).min(BOTTOMLESS_PATH)
        };
        let vertical = path * SURFACE_VIEW.y.abs() / SURFACE_VIEW.length().max(1e-4);
        let body = settings.body;
        if path < 0.0 {
            return (1.0, body.x);
        }
        // The retail terms (RF-SHADER-006, RF-SHADER-010): body.y is the fog range, body.z the far
        // distance floored at 1 and body.w the curve exponent. The colour follows the path.
        let _ = vertical;
        let colour = (path / body.z).clamp(0.0, 1.0);
        let clear = ((body.z - path.min(body.z)) / body.y).clamp(0.0, 1.0);
        (colour, 1.0 - clear.powf(body.w))
    }

    /// The snapshot texel a bed at `bed_view_z` would leave behind.
    fn texel_for_bed(bed_view_z: f32) -> f32 {
        -NEAR_PLANE / bed_view_z
    }

    fn extension() -> WaterExtension {
        let mut extension = WaterExtension::default().with_body(
            [0x80, 0xA0, 0x60],
            50,
            WaterFog {
                near: 0.0,
                far: 150.0,
                amount: 0.7,
            },
        );
        extension.set_bed_depth(Some(Handle::default()));
        extension
    }

    #[test]
    fn deeper_beds_give_a_deeper_colour_and_a_more_opaque_body() {
        let extension = extension();
        let shallow = shade(&extension, texel_for_bed(-110.0));
        let middle = shade(&extension, texel_for_bed(-150.0));
        let deep = shade(&extension, texel_for_bed(-300.0));

        assert!(
            shallow.0 < middle.0 && middle.0 < deep.0,
            "colour: {shallow:?} {middle:?} {deep:?}"
        );
        assert!(
            shallow.1 < middle.1 && middle.1 < deep.1,
            "alpha: {shallow:?} {middle:?} {deep:?}"
        );
        assert!(
            shallow.0 < 0.1 && shallow.1 < 0.2,
            "a thin film stays shallow and clear"
        );
        assert!(
            deep.1 > 0.99,
            "past the far fog distance the body is opaque"
        );
    }

    #[test]
    fn an_unresolved_or_unbound_snapshot_draws_deep_at_the_anam_opacity() {
        let extension = extension();
        let anam = extension.body().x;
        assert_eq!(shade(&extension, UNRESOLVED), (1.0, anam));

        // A bound texture with a valid texel must not take the same branch.
        assert_ne!(shade(&extension, texel_for_bed(-110.0)), (1.0, anam));

        let mut unbound = extension.clone();
        unbound.set_bed_depth(None);
        assert_eq!(shade(&unbound, texel_for_bed(-300.0)), (1.0, anam));
    }

    #[test]
    fn nothing_behind_the_water_reads_as_bottomless() {
        let extension = extension();
        let open = shade(&extension, 0.0);
        assert_eq!(open, shade(&extension, texel_for_bed(-1.0e6)));
        assert!(open.1 > 0.99);
    }
}
