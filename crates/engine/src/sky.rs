//! A gradient sky dome drawn around the world camera.
//!
//! Skyrim draws its daytime sky as one dome (`meshes/sky/atmosphere.nif`) whose vertex colours are
//! weights rather than colours: red selects the weather's Horizon colour, green its Sky-Lower
//! colour and blue its Sky-Upper colour, and the vertex alpha fades the lowest 2.1 degrees out so
//! the fog colour behind the dome shows through at the horizon. The converted dome carries no
//! vertex colours, so the engine builds an equivalent dome here from the measured weight table.
//!
//! The dome follows the camera's position (not its rotation) and its vertex shader pins every
//! fragment to the far plane, so it never hides geometry however far that geometry is. It is
//! alpha-blended with no depth write, which also keeps it out of the depth prepass. The camera's
//! clear colour is the weather's Fog Far colour, which fills the dome's transparent band.
//!
//! The same Fog Far colour is the colour of the distance fog every world camera carries, fitted
//! to the weather's `FNAM` fog distances, so terrain fades into the band the dome leaves open at
//! the horizon.

use crate::world::database::CellKey;
use bevy::{
    asset::{RenderAssetUsages, embedded_asset},
    camera::visibility::NoFrustumCulling,
    light::NotShadowCaster,
    mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology},
    pbr::{
        DistanceFog, FogFalloff, Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin,
    },
    prelude::*,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
    transform::TransformSystems,
};

const SKY_SHADER: &str = "embedded://engine/shaders/sky.wgsl";

/// Radius of the dome mesh in world units. The vertex shader places the dome on the far plane,
/// so the radius does not decide what the dome covers for the camera it follows; it only needs
/// to be large enough that other cameras near that one (a mirrored water-reflection camera) are
/// still inside it.
pub const DOME_RADIUS: f32 = 32_768.0;
const DOME_SEGMENTS: u32 = 48;
const DOME_RINGS: u32 = 24;

/// Sort offset that places the dome behind every other transparent draw. The dome is centred on
/// the camera, so without it the dome would sort as the nearest transparent object and be drawn
/// over blended geometry that has only sky behind it.
const DOME_SORT_BIAS: f32 = -1.0e9;

/// One ring of the vanilla sky dome: the elevation seen from the dome centre and the weights of
/// the weather's Horizon, Sky-Lower and Sky-Upper colours there, plus the dome's opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DomeMaskRow {
    pub elevation_degrees: f32,
    pub horizon: f32,
    pub lower: f32,
    pub upper: f32,
    pub alpha: f32,
}

const fn mask_row(
    elevation_degrees: f32,
    horizon: f32,
    lower: f32,
    upper: f32,
    alpha: f32,
) -> DomeMaskRow {
    DomeMaskRow {
        elevation_degrees,
        horizon,
        lower,
        upper,
        alpha,
    }
}

/// The vertex colours of `meshes/sky/atmosphere.nif` (shape `AtmosphereDome:0`, 24 vertices a
/// ring, no variation inside a ring), read ring by ring: red = Horizon, green = Sky-Lower,
/// blue = Sky-Upper, alpha = opacity. Between rows the dome interpolates linearly, as the
/// rasteriser does across the original triangles.
pub const DOME_MASKS: [DomeMaskRow; 16] = [
    mask_row(0.0, 1.0, 0.0, 0.0, 0.0),
    mask_row(0.5, 1.0, 0.0, 0.0, 0.22),
    mask_row(1.5, 1.0, 0.0, 0.0, 0.80),
    mask_row(2.1, 1.0, 0.0, 0.0, 1.0),
    mask_row(3.6, 0.933, 0.071, 0.0, 1.0),
    mask_row(5.6, 0.804, 0.220, 0.0, 1.0),
    mask_row(9.7, 0.490, 0.573, 0.0, 1.0),
    mask_row(14.5, 0.157, 0.910, 0.0, 1.0),
    mask_row(18.0, 0.020, 1.0, 0.016, 1.0),
    mask_row(19.4, 0.008, 1.0, 0.020, 1.0),
    mask_row(26.8, 0.0, 0.769, 0.306, 1.0),
    mask_row(34.9, 0.0, 0.471, 0.584, 1.0),
    mask_row(47.0, 0.0, 0.184, 0.831, 1.0),
    mask_row(59.1, 0.0, 0.063, 0.941, 1.0),
    mask_row(73.9, 0.0, 0.012, 0.984, 1.0),
    mask_row(90.0, 0.0, 0.0, 1.0, 1.0),
];

// The shader packs the row elevations four to a vector.
const _: () = assert!(DOME_MASKS.len() == 16);

/// The dome weights at an elevation in degrees, interpolated linearly between the measured rows
/// and held at the first and last row outside them.
pub fn dome_mask(elevation_degrees: f32) -> DomeMaskRow {
    let first = DOME_MASKS[0];
    let last = DOME_MASKS[DOME_MASKS.len() - 1];
    if elevation_degrees.is_nan() || elevation_degrees <= first.elevation_degrees {
        return first;
    }
    if elevation_degrees >= last.elevation_degrees {
        return last;
    }
    for pair in DOME_MASKS.windows(2) {
        let (below, above) = (pair[0], pair[1]);
        if elevation_degrees <= above.elevation_degrees {
            let t = (elevation_degrees - below.elevation_degrees)
                / (above.elevation_degrees - below.elevation_degrees);
            // Exact at both rows.
            let lerp = |a: f32, b: f32| a * (1.0 - t) + b * t;
            return DomeMaskRow {
                elevation_degrees,
                horizon: lerp(below.horizon, above.horizon),
                lower: lerp(below.lower, above.lower),
                upper: lerp(below.upper, above.upper),
                alpha: lerp(below.alpha, above.alpha),
            };
        }
    }
    last
}

/// The colours one weather gives the sky at one time of day.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct SkyPalette {
    pub upper: Color,
    pub lower: Color,
    pub horizon: Color,
    pub fog_far: Color,
    /// Linear scale on the dome and clear colour, for calibrating the sky against lit surfaces.
    pub brightness: f32,
}

impl SkyPalette {
    /// `SkyrimClear` (`WTHR` 0x0000081A, as left by Update.esm): the day column of its `NAM0`
    /// Sky-Upper, Sky-Lower and Horizon colours and of its Fog Far colour. Weather colours are
    /// 8-bit sRGB values.
    pub const SKYRIM_CLEAR_DAY: Self = Self {
        upper: Color::srgb_u8(21, 77, 117),
        lower: Color::srgb_u8(60, 135, 183),
        horizon: Color::srgb_u8(125, 163, 183),
        fog_far: Color::srgb_u8(116, 168, 203),
        brightness: 1.0,
    };

    /// The dome colour at an elevation: `R * Horizon + G * Sky-Lower + B * Sky-Upper` over the
    /// sRGB-encoded colours, as vanilla mixes them, with the dome's alpha.
    pub fn dome_colour(&self, elevation_degrees: f32) -> Srgba {
        let mask = dome_mask(elevation_degrees);
        mix_row(self, mask)
    }

    /// The colour the camera clears to: Fog Far outside, black inside an interior cell.
    pub fn clear_colour(&self, space: CameraSpace) -> Color {
        match space {
            CameraSpace::Exterior => {
                let fog = self.fog_far.to_linear();
                Color::LinearRgba(LinearRgba::rgb(
                    fog.red * self.brightness,
                    fog.green * self.brightness,
                    fog.blue * self.brightness,
                ))
            }
            CameraSpace::Interior => Color::BLACK,
        }
    }
}

impl Default for SkyPalette {
    fn default() -> Self {
        Self::SKYRIM_CLEAR_DAY
    }
}

fn mix_row(palette: &SkyPalette, mask: DomeMaskRow) -> Srgba {
    let horizon = palette.horizon.to_srgba();
    let lower = palette.lower.to_srgba();
    let upper = palette.upper.to_srgba();
    let mix = |h: f32, l: f32, u: f32| mask.horizon * h + mask.lower * l + mask.upper * u;
    Srgba::new(
        mix(horizon.red, lower.red, upper.red),
        mix(horizon.green, lower.green, upper.green),
        mix(horizon.blue, lower.blue, upper.blue),
        mask.alpha,
    )
}

/// Whether the camera stands in an exterior or an interior cell. The dome is drawn only outside.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CameraSpace {
    #[default]
    Exterior,
    Interior,
}

impl CameraSpace {
    pub fn of(cell: CellKey) -> Self {
        match cell {
            CellKey::Exterior { .. } => Self::Exterior,
            CellKey::Interior(_) => Self::Interior,
        }
    }
}

/// A weather's fog distance: the four `FNAM` numbers of the weather record.
///
/// Vanilla's fog amount at a distance `d` is `min(max, t ^ power)`, where
/// `t = clamp((d - near) / (far - near), 0, 1)`, and its fog colour is the Near-to-Far colour
/// lerp taken at that amount. The amount caps at `max` instead of reaching 1, and it follows a
/// power of the distance rather than an exponential, so the engine fits Bevy's
/// [`FogFalloff::Exponential`] to the curve as well as one number allows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VanillaFog {
    pub near: f32,
    pub far: f32,
    pub power: f32,
    pub max: f32,
}

impl VanillaFog {
    /// The vanilla fog amount at a distance: `min(max, t ^ power)`.
    pub fn amount(&self, distance: f32) -> f32 {
        let span = self.far - self.near;
        if span <= 0.0 {
            return self.max;
        }
        let t = ((distance - self.near) / span).clamp(0.0, 1.0);
        t.powf(self.power).min(self.max)
    }

    /// The distance at which vanilla's fog reaches its full strength: the amount is `max` where
    /// `t = max ^ (1 / power)`. Everything past it is fogged at `max`, never at 1.
    pub fn full_strength_distance(&self) -> f32 {
        self.near + (self.far - self.near) * self.max.powf(self.power.recip())
    }
}

/// `SkyrimClear`'s fog distance (`WTHR` 0x0000081A, `FNAM`, as left by Update.esm), day column:
/// the fog starts at the camera, follows the 0.4 power of distance and reaches its full strength
/// of 0.85 at 53_289 units. The night column is the same curve over 40_000 units; the engine's one
/// time of day is the day.
pub const SKYRIM_CLEAR_FOG: VanillaFog = VanillaFog {
    near: 0.0,
    far: 80_000.0,
    power: 0.4,
    max: 0.85,
};

/// Density of the engine's distance fog, in inverse world units: the least-squares fit of
/// `1 - exp(-density * d)` to [`SKYRIM_CLEAR_FOG`]'s curve over 0 to 120_000 units, 6.2e-5 with an
/// RMSE of 0.034. The fit runs thin close in and thick far out: 0.10 against vanilla's 0.23 at
/// 2_048 units, and 0.54 against 0.53 at 16_384 units.
///
/// Vanilla stops at 0.85 and the exponential does not, so the cap rides on the fog colour's alpha
/// instead (see [`fog_for`]). A weather record will bring its own density once weathers are
/// loaded.
pub const FOG_DENSITY: f32 = 6.2e-5;

/// The distance fog for one weather: vanilla's fitted density in the Fog Far colour.
pub fn fog_for(palette: &SkyPalette) -> DistanceFog {
    // The fog colour is the clear colour - the Fog Far colour the dome's transparent band lets
    // through - carrying vanilla's maximum fog strength as its alpha, because Bevy multiplies the
    // falloff by the fog colour's alpha. The camera's own clear colour stays opaque. Vanilla lerps
    // Fog Near to Fog Far by the fog amount and Bevy's fog has no such lerp, so the far colour is
    // the one the fog takes throughout.
    let color = palette
        .clear_colour(CameraSpace::Exterior)
        .with_alpha(SKYRIM_CLEAR_FOG.max);
    DistanceFog {
        color,
        // The sun's glow in the fog needs a sun in the sky. Until there is one, the fog is the
        // plain Fog Far colour in every direction.
        directional_light_color: color,
        falloff: FogFalloff::Exponential {
            density: FOG_DENSITY,
        },
        ..default()
    }
}

/// Marks a camera that gets a sky dome and whose clear colour the sky controls.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SkyCamera;

/// Marks a camera that renders the world and therefore sees its distance fog: the streaming
/// world camera and the fixture cameras that draw converted meshes, terrain and water.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct FogCamera;

/// The dome drawn for one [`SkyCamera`].
#[derive(Component, Debug, Clone, Copy)]
pub struct SkyDome {
    pub camera: Entity,
}

/// The dome's uniform: the colour table the fragment shader interpolates by elevation.
#[derive(ShaderType, Debug, Clone, PartialEq)]
pub struct SkyUniform {
    /// Row elevations in degrees, four rows to a vector.
    pub elevations: [Vec4; 4],
    /// Per row: the sRGB-encoded dome colour and the row's alpha.
    pub colours: [Vec4; 16],
    /// `x`: the palette's linear brightness scale.
    pub brightness: Vec4,
}

impl SkyUniform {
    pub fn new(palette: &SkyPalette) -> Self {
        let mut elevations = [Vec4::ZERO; 4];
        let mut colours = [Vec4::ZERO; 16];
        for (index, row) in DOME_MASKS.iter().enumerate() {
            elevations[index / 4][index % 4] = row.elevation_degrees;
            let colour = mix_row(palette, *row);
            colours[index] = Vec4::new(colour.red, colour.green, colour.blue, colour.alpha);
        }
        Self {
            elevations,
            colours,
            brightness: Vec4::new(palette.brightness, 0.0, 0.0, 0.0),
        }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct SkyMaterial {
    #[uniform(0)]
    pub sky: SkyUniform,
}

impl SkyMaterial {
    pub fn new(palette: &SkyPalette) -> Self {
        Self {
            sky: SkyUniform::new(palette),
        }
    }
}

impl Material for SkyMaterial {
    fn vertex_shader() -> ShaderRef {
        SKY_SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        SKY_SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn depth_bias(&self) -> f32 {
        DOME_SORT_BIAS
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Seen from inside, and by mirrored reflection cameras from either side.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/sky.wgsl");
        app.add_plugins(MaterialPlugin::<SkyMaterial>::default())
            .init_resource::<SkyPalette>()
            .init_resource::<CameraSpace>()
            .add_systems(
                PostUpdate,
                (spawn_sky_domes, follow_sky_cameras, apply_sky, apply_fog)
                    .chain()
                    .before(TransformSystems::Propagate),
            );
    }
}

/// A hemisphere of [`DOME_RADIUS`] from the horizon to the zenith, positions only.
pub fn dome_mesh() -> Mesh {
    let mut positions = Vec::with_capacity((DOME_SEGMENTS * DOME_RINGS + 1) as usize);
    for ring in 0..DOME_RINGS {
        let elevation = ring as f32 / DOME_RINGS as f32 * std::f32::consts::FRAC_PI_2;
        let (sin_elevation, cos_elevation) = elevation.sin_cos();
        for segment in 0..DOME_SEGMENTS {
            let azimuth = segment as f32 / DOME_SEGMENTS as f32 * std::f32::consts::TAU;
            let (sin_azimuth, cos_azimuth) = azimuth.sin_cos();
            positions.push([
                DOME_RADIUS * cos_elevation * cos_azimuth,
                DOME_RADIUS * sin_elevation,
                DOME_RADIUS * cos_elevation * sin_azimuth,
            ]);
        }
    }
    let apex = positions.len() as u32;
    positions.push([0.0, DOME_RADIUS, 0.0]);

    let mut indices = Vec::with_capacity((DOME_SEGMENTS * DOME_RINGS * 6) as usize);
    for ring in 0..DOME_RINGS {
        for segment in 0..DOME_SEGMENTS {
            let next = (segment + 1) % DOME_SEGMENTS;
            let a = ring * DOME_SEGMENTS + segment;
            let b = ring * DOME_SEGMENTS + next;
            if ring + 1 == DOME_RINGS {
                indices.extend([a, apex, b]);
            } else {
                let c = (ring + 1) * DOME_SEGMENTS + segment;
                let d = (ring + 1) * DOME_SEGMENTS + next;
                indices.extend([a, c, b, b, c, d]);
            }
        }
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

fn spawn_sky_domes(
    mut commands: Commands,
    cameras: Query<(Entity, &Transform), Added<SkyCamera>>,
    palette: Res<SkyPalette>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
) {
    for (camera, transform) in &cameras {
        commands.spawn((
            Name::new("Sky dome"),
            SkyDome { camera },
            Mesh3d(meshes.add(dome_mesh())),
            MeshMaterial3d(materials.add(SkyMaterial::new(&palette))),
            Transform::from_translation(transform.translation),
            Visibility::default(),
            NotShadowCaster,
            // The dome always surrounds its camera.
            NoFrustumCulling,
        ));
    }
}

/// Keeps each dome centred on its camera's position; the dome never rotates. The world camera is
/// a root entity, so its `Transform` is its world position.
fn follow_sky_cameras(
    mut commands: Commands,
    cameras: Query<&Transform, (With<SkyCamera>, Without<SkyDome>)>,
    mut domes: Query<(Entity, &SkyDome, &mut Transform)>,
) {
    for (entity, dome, mut transform) in &mut domes {
        match cameras.get(dome.camera) {
            Ok(camera) => {
                if transform.translation != camera.translation {
                    transform.translation = camera.translation;
                }
            }
            Err(_) => commands.entity(entity).despawn(),
        }
    }
}

fn apply_sky(
    palette: Res<SkyPalette>,
    space: Res<CameraSpace>,
    mut cameras: Query<&mut Camera, With<SkyCamera>>,
    mut domes: Query<(&MeshMaterial3d<SkyMaterial>, &mut Visibility), With<SkyDome>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
) {
    let clear = palette.clear_colour(*space);
    for mut camera in &mut cameras {
        let current = match camera.clear_color {
            ClearColorConfig::Custom(colour) => Some(colour),
            _ => None,
        };
        if current != Some(clear) {
            camera.clear_color = ClearColorConfig::Custom(clear);
        }
    }
    let visibility = match *space {
        CameraSpace::Exterior => Visibility::Inherited,
        CameraSpace::Interior => Visibility::Hidden,
    };
    for (material, mut dome_visibility) in &mut domes {
        dome_visibility.set_if_neq(visibility);
        if palette.is_changed()
            && let Some(mut sky_material) = materials.get_mut(&material.0)
        {
            sky_material.sky = SkyUniform::new(&palette);
        }
    }
}

/// Gives every [`FogCamera`] the weather's distance fog, and takes it away inside an interior,
/// where there is no weather and nothing far enough away to hide. The fog's colour and its
/// density only change when the palette does, so the component is written then and left alone
/// otherwise.
fn apply_fog(
    mut commands: Commands,
    palette: Res<SkyPalette>,
    space: Res<CameraSpace>,
    cameras: Query<(Entity, Option<&DistanceFog>), With<FogCamera>>,
) {
    let wanted = match *space {
        CameraSpace::Exterior => Some(fog_for(&palette)),
        CameraSpace::Interior => None,
    };
    for (camera, current) in &cameras {
        let up_to_date = match (&wanted, current) {
            (Some(fog), Some(current)) => same_fog(current, fog),
            (None, None) => true,
            _ => false,
        };
        if up_to_date {
            continue;
        }
        match &wanted {
            Some(fog) => commands.entity(camera).insert(fog.clone()),
            None => commands.entity(camera).remove::<DistanceFog>(),
        };
    }
}

/// Whether a camera already carries the fog the sky asks for. `DistanceFog` is not `PartialEq`,
/// so the fields the engine sets are compared one by one.
fn same_fog(current: &DistanceFog, wanted: &DistanceFog) -> bool {
    if current.color != wanted.color
        || current.directional_light_color != wanted.directional_light_color
    {
        return false;
    }
    match (&current.falloff, &wanted.falloff) {
        (
            FogFalloff::Exponential {
                density: current_density,
            },
            FogFalloff::Exponential {
                density: wanted_density,
            },
        ) => current_density == wanted_density,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1.0e-5;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    fn assert_mask(elevation: f32, horizon: f32, lower: f32, upper: f32, alpha: f32) {
        let mask = dome_mask(elevation);
        assert_close(mask.horizon, horizon);
        assert_close(mask.lower, lower);
        assert_close(mask.upper, upper);
        assert_close(mask.alpha, alpha);
    }

    #[test]
    fn dome_mask_returns_every_measured_row_at_its_elevation() {
        for row in DOME_MASKS {
            assert_mask(
                row.elevation_degrees,
                row.horizon,
                row.lower,
                row.upper,
                row.alpha,
            );
        }
        // The fade and the zenith, spelled out.
        assert_mask(0.0, 1.0, 0.0, 0.0, 0.0);
        assert_mask(2.1, 1.0, 0.0, 0.0, 1.0);
        assert_mask(90.0, 0.0, 0.0, 1.0, 1.0);
    }

    #[test]
    fn dome_mask_rows_rise_in_elevation() {
        for pair in DOME_MASKS.windows(2) {
            assert!(pair[0].elevation_degrees < pair[1].elevation_degrees);
        }
    }

    #[test]
    fn dome_mask_interpolates_linearly_between_rows() {
        // Halfway through the fade from 0.5 to 1.5 degrees.
        assert_mask(1.0, 1.0, 0.0, 0.0, 0.51);
        // Halfway from 26.8 to 34.9 degrees.
        assert_mask(30.85, 0.0, 0.62, 0.445, 1.0);
    }

    #[test]
    fn dome_mask_holds_the_end_rows_outside_the_table() {
        assert_mask(-10.0, 1.0, 0.0, 0.0, 0.0);
        assert_mask(f32::NAN, 1.0, 0.0, 0.0, 0.0);
        assert_mask(120.0, 0.0, 0.0, 1.0, 1.0);
    }

    fn assert_srgb_u8(colour: Srgba, expected: [f32; 3], alpha: f32) {
        assert!(
            (colour.red * 255.0 - expected[0]).abs() < 1.0e-3
                && (colour.green * 255.0 - expected[1]).abs() < 1.0e-3
                && (colour.blue * 255.0 - expected[2]).abs() < 1.0e-3,
            "expected {expected:?}, got {:?}",
            [
                colour.red * 255.0,
                colour.green * 255.0,
                colour.blue * 255.0
            ]
        );
        assert_close(colour.alpha, alpha);
    }

    #[test]
    fn skyrim_clear_day_matches_the_weather_record() {
        let palette = SkyPalette::SKYRIM_CLEAR_DAY;
        assert_eq!(palette.upper, Color::srgb_u8(21, 77, 117));
        assert_eq!(palette.lower, Color::srgb_u8(60, 135, 183));
        assert_eq!(palette.horizon, Color::srgb_u8(125, 163, 183));
        assert_eq!(palette.fog_far, Color::srgb_u8(116, 168, 203));
        assert_eq!(palette.brightness, 1.0);
        assert_eq!(SkyPalette::default(), palette);
    }

    #[test]
    fn skyrim_clear_day_mixes_horizon_lower_and_upper_by_elevation() {
        let palette = SkyPalette::SKYRIM_CLEAR_DAY;
        // Pure Horizon, transparent at the horizon line and opaque from 2.1 degrees.
        assert_srgb_u8(palette.dome_colour(0.0), [125.0, 163.0, 183.0], 0.0);
        assert_srgb_u8(palette.dome_colour(2.1), [125.0, 163.0, 183.0], 1.0);
        // 0.49 Horizon + 0.573 Sky-Lower.
        assert_srgb_u8(
            palette.dome_colour(9.7),
            [
                0.49 * 125.0 + 0.573 * 60.0,
                0.49 * 163.0 + 0.573 * 135.0,
                0.49 * 183.0 + 0.573 * 183.0,
            ],
            1.0,
        );
        // 0.02 Horizon + Sky-Lower + 0.016 Sky-Upper.
        assert_srgb_u8(
            palette.dome_colour(18.0),
            [
                0.02 * 125.0 + 60.0 + 0.016 * 21.0,
                0.02 * 163.0 + 135.0 + 0.016 * 77.0,
                0.02 * 183.0 + 183.0 + 0.016 * 117.0,
            ],
            1.0,
        );
        // Pure Sky-Upper at the zenith.
        assert_srgb_u8(palette.dome_colour(90.0), [21.0, 77.0, 117.0], 1.0);
    }

    #[test]
    fn shader_table_holds_the_mixed_rows_in_order() {
        let palette = SkyPalette {
            brightness: 0.5,
            ..SkyPalette::SKYRIM_CLEAR_DAY
        };
        let uniform = SkyUniform::new(&palette);
        for (index, row) in DOME_MASKS.iter().enumerate() {
            assert_eq!(
                uniform.elevations[index / 4][index % 4],
                row.elevation_degrees
            );
            let colour = palette.dome_colour(row.elevation_degrees);
            assert_eq!(
                uniform.colours[index],
                Vec4::new(colour.red, colour.green, colour.blue, colour.alpha)
            );
        }
        assert_eq!(uniform.brightness.x, 0.5);
    }

    #[test]
    fn clear_colour_is_fog_far_outside_and_black_inside() {
        let palette = SkyPalette::SKYRIM_CLEAR_DAY;
        let outside = palette.clear_colour(CameraSpace::Exterior).to_srgba();
        assert_srgb_u8(outside, [116.0, 168.0, 203.0], 1.0);
        assert_eq!(palette.clear_colour(CameraSpace::Interior), Color::BLACK);
        assert_eq!(CameraSpace::of(CellKey::Interior(7)), CameraSpace::Interior);
        assert_eq!(
            CameraSpace::of(CellKey::Exterior {
                worldspace_id: 0x3c,
                grid_x: 1,
                grid_y: -2,
            }),
            CameraSpace::Exterior
        );
    }

    #[test]
    fn dome_mesh_is_a_hemisphere_from_the_horizon_to_the_zenith() {
        let mesh = dome_mesh();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("dome mesh has no positions");
        };
        for position in positions {
            let length = Vec3::from_array(*position).length();
            assert!((length - DOME_RADIUS).abs() < DOME_RADIUS * 1.0e-5);
            assert!(position[1] >= 0.0);
        }
        assert!(
            positions
                .iter()
                .any(|position| position[1].abs() < f32::EPSILON)
        );
        assert!(positions.contains(&[0.0, DOME_RADIUS, 0.0]));
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("dome mesh has no indices");
        };
        assert_eq!(indices.len() % 3, 0);
        assert!(
            indices
                .iter()
                .all(|&index| (index as usize) < positions.len())
        );
    }

    fn sky_app() -> App {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<SkyMaterial>>()
            .init_resource::<SkyPalette>()
            .init_resource::<CameraSpace>()
            .add_systems(
                Update,
                (spawn_sky_domes, follow_sky_cameras, apply_sky, apply_fog).chain(),
            );
        app
    }

    fn only_dome(app: &mut App) -> (Entity, SkyDome, Transform, Visibility) {
        let mut domes = app
            .world_mut()
            .query::<(Entity, &SkyDome, &Transform, &Visibility)>();
        let found: Vec<_> = domes
            .iter(app.world())
            .map(|(entity, dome, transform, visibility)| (entity, *dome, *transform, *visibility))
            .collect();
        assert_eq!(found.len(), 1, "expected one sky dome");
        found[0]
    }

    #[test]
    fn dome_follows_the_camera_position_but_not_its_rotation() {
        let mut app = sky_app();
        let camera = app
            .world_mut()
            .spawn((
                Camera::default(),
                Transform::from_xyz(100.0, 250.0, -40.0)
                    .looking_to(Vec3::new(1.0, -0.4, 0.3), Vec3::Y),
                SkyCamera,
            ))
            .id();
        app.update();
        let (_, dome, transform, _) = only_dome(&mut app);
        assert_eq!(dome.camera, camera);
        assert_eq!(transform.translation, Vec3::new(100.0, 250.0, -40.0));
        assert_eq!(transform.rotation, Quat::IDENTITY);

        {
            let mut camera_transform = app.world_mut().get_mut::<Transform>(camera).unwrap();
            camera_transform.translation = Vec3::new(-8_000.0, 1_200.0, 4_096.0);
            camera_transform.rotation = Quat::from_rotation_y(2.0);
        }
        app.update();
        let (_, _, transform, _) = only_dome(&mut app);
        assert_eq!(transform.translation, Vec3::new(-8_000.0, 1_200.0, 4_096.0));
        assert_eq!(transform.rotation, Quat::IDENTITY);
    }

    #[test]
    fn cameras_without_the_marker_get_no_dome() {
        let mut app = sky_app();
        app.world_mut()
            .spawn((Camera::default(), Transform::default()));
        app.update();
        let mut domes = app.world_mut().query::<&SkyDome>();
        assert_eq!(domes.iter(app.world()).count(), 0);
    }

    #[test]
    fn dome_is_removed_with_its_camera() {
        let mut app = sky_app();
        let camera = app
            .world_mut()
            .spawn((Camera::default(), Transform::default(), SkyCamera))
            .id();
        app.update();
        only_dome(&mut app);
        app.world_mut().despawn(camera);
        app.update();
        let mut domes = app.world_mut().query::<&SkyDome>();
        assert_eq!(domes.iter(app.world()).count(), 0);
    }

    #[test]
    fn interior_hides_the_dome_and_clears_to_black() {
        let mut app = sky_app();
        let camera = app
            .world_mut()
            .spawn((Camera::default(), Transform::default(), SkyCamera))
            .id();
        app.update();
        let clear = |app: &App| match app.world().get::<Camera>(camera).unwrap().clear_color {
            ClearColorConfig::Custom(colour) => colour,
            other => panic!("sky camera clear colour is {other:?}"),
        };
        let (_, _, _, visibility) = only_dome(&mut app);
        assert_eq!(visibility, Visibility::Inherited);
        assert_eq!(
            clear(&app),
            SkyPalette::SKYRIM_CLEAR_DAY.clear_colour(CameraSpace::Exterior)
        );

        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Interior;
        app.update();
        let (_, _, _, visibility) = only_dome(&mut app);
        assert_eq!(visibility, Visibility::Hidden);
        assert_eq!(clear(&app), Color::BLACK);

        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Exterior;
        app.update();
        let (_, _, _, visibility) = only_dome(&mut app);
        assert_eq!(visibility, Visibility::Inherited);
    }

    #[test]
    fn palette_changes_reach_the_dome_material() {
        let mut app = sky_app();
        app.world_mut()
            .spawn((Camera::default(), Transform::default(), SkyCamera));
        app.update();
        let dimmer = SkyPalette {
            brightness: 0.25,
            ..SkyPalette::SKYRIM_CLEAR_DAY
        };
        *app.world_mut().resource_mut::<SkyPalette>() = dimmer;
        app.update();
        let (entity, ..) = only_dome(&mut app);
        let handle = app
            .world()
            .get::<MeshMaterial3d<SkyMaterial>>(entity)
            .unwrap()
            .0
            .clone();
        let materials = app.world().resource::<Assets<SkyMaterial>>();
        assert_eq!(
            materials.get(&handle).unwrap().sky,
            SkyUniform::new(&dimmer)
        );
    }

    /// Relative check for the fit's very small numbers, which [`assert_close`] cannot see.
    fn assert_relative(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= expected.abs() * 1.0e-6,
            "expected {expected}, got {actual}"
        );
    }

    /// The `FNAM` fog amounts of `SkyrimClear`'s day column, sampled where the design notes
    /// measured the fit, to three decimals.
    const MEASURED_FOG: [(f32, f32); 5] = [
        (2_048.0, 0.231),
        (4_096.0, 0.305),
        (16_384.0, 0.530),
        (36_864.0, 0.734),
        (65_536.0, 0.850),
    ];

    /// The engine's fog strength at a distance, as the fog shader computes it: Bevy's exponential
    /// falloff times the fog colour's alpha, which carries vanilla's cap.
    fn engine_fog(distance: f32) -> f32 {
        SKYRIM_CLEAR_FOG.max * (1.0 - (-FOG_DENSITY * distance).exp())
    }

    #[test]
    fn vanilla_fog_follows_the_measured_clear_day_curve() {
        let fog = SKYRIM_CLEAR_FOG;
        assert_eq!(
            fog,
            VanillaFog {
                near: 0.0,
                far: 80_000.0,
                power: 0.4,
                max: 0.85,
            }
        );
        // `min(Max, t ^ Power)` with `t = (d - Near) / (Far - Near)`, at the measured samples.
        assert_close(fog.amount(0.0), 0.0);
        assert_close(fog.amount(-1_000.0), 0.0);
        for (distance, amount) in MEASURED_FOG {
            assert!(
                (fog.amount(distance) - amount).abs() <= 1.0e-3,
                "vanilla is {} at {distance} units, not {amount}",
                fog.amount(distance)
            );
        }
        // The cap holds from 53_289 units on.
        let full = fog.full_strength_distance();
        assert!((full - 53_289.0).abs() < 1.0, "full strength at {full}");
        assert_close(fog.amount(53_289.0), 0.85);
        assert_close(fog.amount(1_000_000.0), 0.85);
    }

    #[test]
    fn the_fog_fit_tracks_the_measured_clear_day_curve() {
        // The fitted density, pinned, and the error table it was chosen by: the exponential runs
        // thin near the camera and within 0.035 of vanilla from the ring's far edge outward.
        assert_relative(FOG_DENSITY, 6.2e-5);
        for (distance, fitted) in [
            (2_048.0, 0.101),
            (4_096.0, 0.191),
            (16_384.0, 0.542),
            (36_864.0, 0.764),
            (65_536.0, 0.835),
        ] {
            let engine = engine_fog(distance);
            assert!(
                (engine - fitted).abs() <= 1.0e-3,
                "the fit is {engine} at {distance} units, not {fitted}"
            );
            let error = (engine - SKYRIM_CLEAR_FOG.amount(distance)).abs();
            assert!(error <= 0.14, "the fit is {error} off at {distance} units");
            if distance >= 16_384.0 {
                assert!(error <= 0.035, "the fit is {error} off at {distance} units");
            }
        }
        // At the far edge of the default streamed ring, three cells of 4_096 units away, the fit
        // is at its closest to vanilla: 0.45 against vanilla's 0.47, and 0.74 at the default far
        // plane against vanilla's 0.70. Whatever the ring's edge looks like, it is the haze
        // vanilla would draw at that distance, not the fit falling short.
        assert!((engine_fog(12_288.0) - 0.453).abs() <= 1.0e-3);
        assert!((engine_fog(32_768.0) - 0.739).abs() <= 1.0e-3);
    }

    #[test]
    fn fog_cameras_get_the_weathers_fog_and_lose_it_inside() {
        let mut app = sky_app();
        let palette = SkyPalette::SKYRIM_CLEAR_DAY;
        let unfogged = app
            .world_mut()
            .spawn((Camera::default(), Transform::default()))
            .id();
        let camera = app
            .world_mut()
            .spawn((Camera::default(), Transform::default(), FogCamera))
            .id();
        app.update();

        let fog = app
            .world()
            .get::<DistanceFog>(camera)
            .cloned()
            .expect("a fog camera carries fog outside");
        assert_eq!(
            fog.color.with_alpha(1.0),
            palette.clear_colour(CameraSpace::Exterior),
            "the fog colour is the colour the sky clears to"
        );
        assert_eq!(
            fog.color.alpha(),
            SKYRIM_CLEAR_FOG.max,
            "the fog colour's alpha is the cap vanilla puts on its fog"
        );
        assert_eq!(fog.directional_light_color, fog.color);
        match &fog.falloff {
            FogFalloff::Exponential { density } => assert_relative(*density, FOG_DENSITY),
            other => panic!("expected an exponential falloff, got {other:?}"),
        }
        assert!(app.world().get::<DistanceFog>(unfogged).is_none());

        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Interior;
        app.update();
        assert!(app.world().get::<DistanceFog>(camera).is_none());

        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Exterior;
        app.update();
        assert!(app.world().get::<DistanceFog>(camera).is_some());
    }
}
