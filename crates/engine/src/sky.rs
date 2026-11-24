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
//! The distance fog every world camera carries is the weather's power ramp over its fog near and far
//! distances, from the Fog Near to the Fog Far colour, so terrain fades into the band the dome leaves
//! open at the horizon. The palette, the distances and their blend by time of day come from the weather
//! record (`weather_sky.rs`).

use crate::world::database::CellKey;
use bevy::{
    asset::{RenderAssetUsages, embedded_asset},
    camera::visibility::{NoFrustumCulling, VisibilitySystems},
    ecs::{schedule::ScheduleConfigs, system::ScheduleSystem},
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

/// What one weather lights the world with at one time of day: the sun's colour and the six-axis
/// ambient cube of the weather's `DALC` record.
///
/// Weather colours are 8-bit sRGB values. Bevy has one ambient colour per view rather than an
/// ambient cube, so [`WeatherLighting::mean_ambient`] folds the cube to its linear mean.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct WeatherLighting {
    /// `NAM0`'s Sunlight colour.
    pub sunlight: Color,
    /// `DALC`'s six faces in stored order: +X, -X, +Y, -Y, +Z, -Z (the converter schema's
    /// `positive_x` ... `negative_z`).
    ///
    /// The labels name the stored slot, not the direction a face lights. The recovered ambient
    /// arithmetic (`docs/research/skyrim-environment-preparation-20261009.md`, "Six-face ambient
    /// matrix") is `ambient(N) = (Xminus - Xplus)/2 * N.x + (Yminus - Yplus)/2 * N.y +
    /// (Zminus - Zplus)/2 * N.z + mean`, so a stored positive face lights the opposite normal: the
    /// bright sixth face, "Z-", is what an upward normal reads. The record agrees - at sunset the
    /// sixth carries the sunset tint (146,132,131) while the fifth stays a neutral grey (80,83,90).
    pub ambient_cube: [Color; 6],
}

impl WeatherLighting {
    /// `SkyrimClear` (`WTHR` 0x0000081A, as left by Update.esm), day column: `NAM0` Sunlight
    /// 190,181,171 and `DALC` row 2 of 4 (sunrise, day, sunset, night).
    pub const SKYRIM_CLEAR_DAY: Self = Self {
        sunlight: Color::srgb_u8(190, 181, 171),
        ambient_cube: [
            Color::srgb_u8(108, 146, 164),
            Color::srgb_u8(94, 134, 142),
            Color::srgb_u8(101, 145, 154),
            Color::srgb_u8(100, 135, 155),
            Color::srgb_u8(77, 129, 136),
            Color::srgb_u8(191, 214, 221),
        ],
    };

    /// The ambient a surface with this normal reads, in linear light, with the normal in engine
    /// coordinates (`Creation(x, y, z) -> engine(x, z, -y)`).
    ///
    /// This is the recovered affine form with the unresolved global term `G` taken as zero; see
    /// [`WeatherLighting::ambient_cube`]. The native arithmetic runs on 8-bit values without an
    /// sRGB decode; the faces are decoded here because the engine composes in linear light.
    pub fn ambient_toward(&self, normal: Vec3) -> LinearRgba {
        let [x_plus, x_minus, y_plus, y_minus, z_plus, z_minus] =
            self.ambient_cube.map(|face| face.to_linear());
        let mean = (x_plus + x_minus + y_plus + y_minus + z_plus + z_minus) * (1.0 / 6.0);
        let dx = (x_minus - x_plus) * 0.5;
        let dy = (y_minus - y_plus) * 0.5;
        let dz = (z_minus - z_plus) * 0.5;
        // Creation (x, y, z) is engine (x, z, -y): the engine's up axis reads Creation's Z.
        mean + dx * normal.x + dz * normal.y - dy * normal.z
    }

    /// The ambient colour Bevy's single ambient takes: what a normal-less surface reads, which is
    /// the mean of the six faces. Bevy has one ambient colour per view, not a cube.
    pub fn mean_ambient(&self) -> Color {
        Color::LinearRgba(self.ambient_toward(Vec3::ZERO))
    }
}

impl Default for WeatherLighting {
    fn default() -> Self {
        Self::SKYRIM_CLEAR_DAY
    }
}

/// The colours one weather gives the sky at one time of day.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct SkyPalette {
    pub upper: Color,
    pub lower: Color,
    pub horizon: Color,
    /// `NAM0`'s Fog Near colour: the colour vanilla's fog starts from at the camera.
    pub fog_near: Color,
    pub fog_far: Color,
    /// The weather's fog distances. `None` when the record omitted a fog field: no fog is drawn.
    pub fog_ramp: Option<FogRamp>,
    /// Mudcrab uses this to scale the look: the fog distances are divided by it, so a larger value is a denser fog.
    /// A weather record's own palette always has 1.0; only the `cool-gray` look changes it.
    pub fog_density_scale: f32,
    /// Linear scale on the dome and clear colour, for calibrating the sky against lit surfaces.
    pub brightness: f32,
}

impl SkyPalette {
    /// `SkyrimClear` (`WTHR` 0x0000081A): the day column of its `NAM0` Water Multiplier colour, which
    /// the retail water shader multiplies into the shallow, deep and reflection colours
    /// (`evidence/rea-20261114/s1-water-mix.md`, RF-SHADER-007: the Sky `+0x150` colour, written once
    /// per frame). Read from the `weather_colors` table of the registered packs. The engine's sky is
    /// this static day palette, so this is the matching value; a weather and time-of-day driven
    /// multiplier is not implemented.
    pub const SKYRIM_CLEAR_DAY_WATER_MULTIPLIER: [u8; 3] = [175, 237, 214];

    /// `SkyrimClear` (`WTHR` 0x0000081A, as left by Update.esm): the day column of its `NAM0`
    /// Sky-Upper, Sky-Lower, Horizon and Fog colours and of its `FNAM` fog distances. Weather
    /// colours are 8-bit sRGB values. A world database run reads the record instead
    /// (`weather_sky.rs`); this constant serves the fixtures that have no database.
    pub const SKYRIM_CLEAR_DAY: Self = Self {
        upper: Color::srgb_u8(21, 77, 117),
        lower: Color::srgb_u8(60, 135, 183),
        horizon: Color::srgb_u8(125, 163, 183),
        fog_near: Color::srgb_u8(35, 98, 124),
        fog_far: Color::srgb_u8(116, 168, 203),
        // The day column of the record's `FNAM`: near 0, far 80,000, power 0.4, max 0.85.
        fog_ramp: Some(FogRamp {
            near: 0.0,
            far: 80_000.0,
            power: 0.4,
            max: 0.85,
        }),
        fog_density_scale: 1.0,
        brightness: 1.0,
    };

    /// The dome colour at an elevation: `R * Horizon + G * Sky-Lower + B * Sky-Upper` over the
    /// sRGB-encoded colours, as vanilla mixes them, with the dome's alpha.
    pub fn dome_colour(&self, elevation_degrees: f32) -> Srgba {
        let mask = dome_mask(elevation_degrees);
        mix_row(self, mask)
    }

    /// The Fog Near colour with the same brightness scale as the clear colour, linear.
    pub fn fog_near_colour(&self) -> Color {
        let near = self.fog_near.to_linear();
        Color::LinearRgba(LinearRgba::rgb(
            near.red * self.brightness,
            near.green * self.brightness,
            near.blue * self.brightness,
        ))
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

/// A weather's fog distances as the record states them (`FNAM`), in world units, blended by part
/// of the day (`weather_sky.rs`). `near` and `far` bound the ramp, `power` is its exponent and
/// `max` caps the fog amount.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FogRamp {
    pub near: f32,
    pub far: f32,
    pub power: f32,
    pub max: f32,
}

/// Warm tint for the sun's glow inside the fog, from Bevy's atmospheric fog example. Its alpha
/// is the glow strength. Mudcrab uses it for its own look; whether retail draws a sun glow in its
/// fog is unverified. The `skyrim` profile does not use it (`FogLook::Retail`).
pub const FOG_DIRECTIONAL_LIGHT_COLOR: Color = Color::srgba(1.0, 0.95, 0.85, 0.5);

/// Concentration of the sun's glow inside the fog, from Bevy's atmospheric fog example.
/// Mudcrab uses it with [`FOG_DIRECTIONAL_LIGHT_COLOR`].
pub const FOG_DIRECTIONAL_LIGHT_EXPONENT: f32 = 30.0;

/// Which fog look the exterior camera gets. The ramp is the weather's in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FogLook {
    /// The `mudcrab` profile: the weather's ramp plus a warm sun glow inside the fog
    /// (Mudcrab uses Bevy's example glow for its own look).
    #[default]
    Mudcrab,
    /// The `skyrim` profile: the weather's ramp and nothing else.
    Retail,
}

impl FogLook {
    pub fn of(profile: crate::visual_profile::VisualProfile) -> Self {
        match profile {
            crate::visual_profile::VisualProfile::Mudcrab => Self::Mudcrab,
            crate::visual_profile::VisualProfile::Skyrim => Self::Retail,
        }
    }
}

/// The exterior distance fog for one palette, from the weather's fog distances, power and max, in
/// the Fog Near and Fog Far colours. `None` when the palette has no ramp (the weather record
/// omitted a fog field), or the ramp draws nothing (`far <= near`, `max <= 0` or a power that is
/// not positive and finite): no fog is drawn and nothing is invented in its place.
///
/// The formula is retail's (verified, `skyrim_decomp#812`, a static reading of 1.7.104.0), with
/// `s = saturate((d - near) / (far - near))`, `F = min(s^power, max)` and the fog colour
/// `near colour + F * (far colour - near colour)`. A fragment then runs from its lit colour to the
/// fog colour by `F`. The drawing is the vendored `FogFalloff::PowerRamp` of `vendor/bevy_pbr`.
///
/// What differs from retail, all UNVERIFIED as to its effect on the picture:
/// - Retail's `d` is the length of the clip-space XYZ before the divide by W, so it depends on the
///   projection. Mudcrab uses the world distance from the camera, because that is what Bevy's fog
///   has.
/// - Retail evaluates the fog at the vertices and interpolates it. Bevy evaluates it per pixel.
/// - Retail's lit colour then goes through further steps whose runtime flags are not known. They
///   are not drawn.
pub fn fog_for(palette: &SkyPalette) -> Option<DistanceFog> {
    exterior_fog(palette, 1.0, FogLook::Mudcrab)
}

/// [`fog_for`] with the player's fog-density multiplier and the look. The density scales divide
/// the distances, so a larger scale draws a denser fog. The floor keeps a zero multiplier finite.
fn exterior_fog(palette: &SkyPalette, user_multiplier: f32, look: FogLook) -> Option<DistanceFog> {
    let ramp = palette.fog_ramp?;
    let scale = (palette.fog_density_scale * user_multiplier).max(1.0e-3);
    let (near, far) = (ramp.near / scale, ramp.far / scale);
    let drawn = near.is_finite()
        && far.is_finite()
        && far > near
        && ramp.max > 0.0
        && ramp.power.is_finite()
        && ramp.power > 0.0;
    if !drawn {
        return None;
    }
    // The far colour is the clear colour - the Fog Far colour the dome's transparent band lets
    // through - so fogged terrain runs into the band without a seam. The cap is the colour's alpha.
    let cap = ramp.max.clamp(0.0, 1.0);
    let (glow, exponent) = match look {
        FogLook::Mudcrab => (FOG_DIRECTIONAL_LIGHT_COLOR, FOG_DIRECTIONAL_LIGHT_EXPONENT),
        FogLook::Retail => (
            Color::NONE,
            DistanceFog::default().directional_light_exponent,
        ),
    };
    Some(DistanceFog {
        color: palette.clear_colour(CameraSpace::Exterior).with_alpha(cap),
        directional_light_color: glow,
        directional_light_exponent: exponent,
        falloff: FogFalloff::PowerRamp {
            start: near,
            end: far,
            power: ramp.power,
            near_color: palette.fog_near_colour(),
        },
    })
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
        app.add_plugins((
            MaterialPlugin::<SkyMaterial>::default(),
            crate::underwater::UnderwaterStatePlugin,
        ))
        .init_resource::<SkyPalette>()
        .init_resource::<CameraSpace>()
        .add_systems(
            PostUpdate,
            (spawn_sky_domes, follow_sky_cameras)
                .chain()
                .before(TransformSystems::Propagate),
        )
        .add_systems(
            PostUpdate,
            sky_systems()
                .after(crate::underwater::UnderwaterStateSet)
                .before(VisibilitySystems::VisibilityPropagate),
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

/// The palette with the player's sky-brightness multiplier applied.
fn scaled_palette(
    palette: &SkyPalette,
    settings: Option<&crate::settings::RenderingSettings>,
) -> SkyPalette {
    let mut scaled = *palette;
    if let Some(settings) = settings {
        scaled.brightness *= settings.sky_brightness_scale;
    }
    scaled
}

fn apply_sky(
    palette: Res<SkyPalette>,
    space: Res<CameraSpace>,
    mut cameras: Query<&mut Camera, With<SkyCamera>>,
    mut domes: Query<(&MeshMaterial3d<SkyMaterial>, &mut Visibility), With<SkyDome>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
    underwater: Res<crate::underwater::UnderwaterState>,
) {
    let adjusted_palette = scaled_palette(&palette, settings.as_deref());
    // Under the surface retail fogs every pixel, the sky included, to the water's fog colour
    // (RF-WATER-052). The underwater post-pass does that from the depth, but a pixel on the edge of
    // alpha-tested leaves mixes leaf and sky after the multisample resolve, and the pass can only
    // fog it by the nearest sample's depth: the sky share of that pixel would stay the bright sky
    // colour as a pale fringe. So the sky itself is already the fog colour: no dome, and the fog
    // colour as the clear colour.
    let below = settings
        .as_deref()
        .is_none_or(|settings| settings.fog)
        .then(|| underwater.fog())
        .flatten();
    let clear = below.map_or_else(|| adjusted_palette.clear_colour(*space), |fog| fog.color);
    for mut camera in &mut cameras {
        let current = match camera.clear_color {
            ClearColorConfig::Custom(colour) => Some(colour),
            _ => None,
        };
        if current != Some(clear) {
            camera.clear_color = ClearColorConfig::Custom(clear);
        }
    }
    let visibility = match (
        *space,
        settings.as_ref().is_none_or(|settings| settings.sky),
    ) {
        _ if below.is_some() => Visibility::Hidden,
        (CameraSpace::Exterior, true) => Visibility::Inherited,
        _ => Visibility::Hidden,
    };
    for (material, mut dome_visibility) in &mut domes {
        dome_visibility.set_if_neq(visibility);
        if (palette.is_changed() || settings.as_ref().is_some_and(|s| s.is_changed()))
            && let Some(mut sky_material) = materials.get_mut(&material.0)
        {
            sky_material.sky = SkyUniform::new(&adjusted_palette);
        }
    }
}

/// Follows the game clock: rebuilds the palette from the active weather at the clock's hour, so
/// the sky and fog colours and the fog distances change with the part of the day. Without a
/// weather record (a fixture run) the palette is a fixed look and nothing changes with the hour.
/// The palette is written only when it differs, so its change tick stays quiet at a frozen clock.
fn drive_sky_palette(
    clock: Option<Res<crate::sun::GameClock>>,
    source: Option<Res<crate::color_pipeline::SceneGradeSource>>,
    preset: Option<Res<crate::color_pipeline::SceneVisualPreset>>,
    mut palette: ResMut<SkyPalette>,
) {
    let (Some(clock), Some(source), Some(preset)) = (clock, source, preset) else {
        return;
    };
    if source.weather.is_none() {
        return;
    }
    palette.set_if_neq(source.sky_palette(*preset, clock.hour));
}

/// The sky systems that read the frame's underwater state, in order: sky, then the water's copy
/// of the sky, then fog. `apply_fog` reads `InteriorFog`, which `EnvironmentSet` writes in the
/// same schedule, so it runs after that set: a door or room crossing changes the fog in the same
/// frame as the light.
fn sky_systems() -> ScheduleConfigs<ScheduleSystem> {
    (
        drive_sky_palette,
        apply_sky,
        sync_water_sky,
        apply_fog.after(crate::world::environment::interior::EnvironmentSet),
    )
        .chain()
}

/// Points every exterior water material at the sky the camera sees, so the water reflects the dome
/// as it is now (the weather's colours and the player's brightness). Only a material whose sky
/// differs is written.
fn sync_water_sky(
    palette: Res<SkyPalette>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
    materials: Option<ResMut<Assets<crate::render::WaterMaterial>>>,
    mut ids: Local<Vec<AssetId<crate::render::WaterMaterial>>>,
) {
    let Some(mut materials) = materials else {
        return;
    };
    let sky = SkyUniform::new(&scaled_palette(&palette, settings.as_deref()));
    ids.clear();
    ids.extend(materials.ids());
    for &id in ids.iter() {
        let stale = materials.get(id).is_some_and(|material| {
            material.extension.reflects_sky() && !material.extension.sky_matches(&sky)
        });
        if stale && let Some(mut material) = materials.get_mut(id) {
            material.extension = material.extension.clone().with_sky(Some(&sky));
        }
    }
}

/// Gives every [`FogCamera`] the weather's distance fog outside. Inside an interior it gives the
/// cell fog of sky modes 1 and 2 (`InteriorFog`), or no fog when there is none. Under water it
/// gives neither (RF-WATER-031). The component is written only when the wanted fog differs from
/// the camera's.
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy system: each argument is a resource or query it reads"
)]
fn apply_fog(
    mut commands: Commands,
    palette: Res<SkyPalette>,
    space: Res<CameraSpace>,
    cameras: Query<(Entity, Option<&DistanceFog>), With<FogCamera>>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
    interior_fog: Option<Res<crate::world::environment::interior::InteriorFog>>,
    underwater: Res<crate::underwater::UnderwaterState>,
    profile: Option<Res<crate::visual_profile::VisualProfile>>,
) {
    let fog_on = settings.as_deref().is_none_or(|settings| settings.fog);
    let wanted = match (*space, fog_on) {
        // Below the surface retail replaces the view with the water's own fog (RF-WATER-031), which
        // the underwater post-pass draws over the whole frame; the exterior fog would fog it twice.
        _ if underwater.fog().is_some() => None,
        (CameraSpace::Exterior, true) => {
            // The fog is the clear colour, so it takes the same brightness scale as the sky.
            let scaled = scaled_palette(&palette, settings.as_deref());
            // The player's multiplier scales the authored weather density once.
            let user_multiplier = settings
                .as_deref()
                .map_or(1.0, |settings| settings.fog_density_scale);
            let look = profile
                .as_deref()
                .map_or(FogLook::Mudcrab, |profile| FogLook::of(*profile));
            exterior_fog(&scaled, user_multiplier, look)
        }
        // Sky modes 1 and 2 fog an interior with the cell's own fog (RF-LIGHT-CPU-013).
        (CameraSpace::Interior, true) => interior_fog
            .as_deref()
            .and_then(|fog| fog.0)
            .map(|fog| fog.distance_fog()),
        _ => None,
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
            FogFalloff::Linear {
                start: current_start,
                end: current_end,
            },
            FogFalloff::Linear {
                start: wanted_start,
                end: wanted_end,
            },
        ) => current_start == wanted_start && current_end == wanted_end,
        (
            FogFalloff::PowerRamp {
                start: current_start,
                end: current_end,
                power: current_power,
                near_color: current_near,
            },
            FogFalloff::PowerRamp {
                start: wanted_start,
                end: wanted_end,
                power: wanted_power,
                near_color: wanted_near,
            },
        ) => {
            current_start == wanted_start
                && current_end == wanted_end
                && current_power == wanted_power
                && current_near == wanted_near
        }
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
        assert_eq!(palette.fog_near, Color::srgb_u8(35, 98, 124));
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

    #[test]
    fn exterior_waters_follow_the_sky_and_interior_waters_have_none() {
        use crate::render::{WaterExtension, WaterMaterial};
        let mut app = App::new();
        app.init_resource::<Assets<WaterMaterial>>()
            .init_resource::<SkyPalette>()
            .add_systems(Update, sync_water_sky);
        let stale = SkyUniform::new(&SkyPalette {
            brightness: 0.25,
            ..SkyPalette::default()
        });
        let (outside, inside) = {
            let mut materials = app.world_mut().resource_mut::<Assets<WaterMaterial>>();
            let material = |extension| WaterMaterial {
                base: StandardMaterial::default(),
                extension,
            };
            (
                materials.add(material(WaterExtension::default().with_sky(Some(&stale)))),
                materials.add(material(WaterExtension::default())),
            )
        };
        app.update();
        let current = SkyUniform::new(&SkyPalette::default());
        let materials = app.world().resource::<Assets<WaterMaterial>>();
        assert!(
            materials
                .get(&outside)
                .unwrap()
                .extension
                .sky_matches(&current)
        );
        // An interior water has no sky to follow and keeps its flat reflection colour.
        assert!(!materials.get(&inside).unwrap().extension.reflects_sky());
    }

    #[test]
    fn below_the_surface_the_sky_is_the_fog_colour_and_the_dome_is_hidden() {
        use crate::underwater::{UnderwaterFog, UnderwaterState};
        let mut app = sky_app();
        let camera = app
            .world_mut()
            .spawn((Camera::default(), Transform::default(), SkyCamera))
            .id();
        app.update();
        let clear = |app: &App| match app.world().get::<Camera>(camera).unwrap().clear_color {
            ClearColorConfig::Custom(colour) => colour,
            _ => panic!("the sky camera clears to a colour"),
        };
        let above = clear(&app);
        let fog = UnderwaterFog {
            color: Color::srgb(0.1, 0.2, 0.05),
            near: -500.0,
            far: 1600.0,
            source: crate::underwater::UnderwaterFogSource::NoRecord,
        };
        app.insert_resource(UnderwaterState::below(fog));
        app.update();
        assert_eq!(clear(&app), fog.color);
        let (_, _, _, visibility) = only_dome(&mut app);
        assert_eq!(visibility, Visibility::Hidden);
        // Out of the water the sky comes back.
        app.insert_resource(UnderwaterState::default());
        app.update();
        assert_eq!(clear(&app), above);
    }

    /// Resurfacing restores the exterior sky and fog in the frame the camera moves.
    #[test]
    fn sky_plugin_restores_the_exterior_on_the_resurfacing_frame() {
        use crate::{
            settings::RenderingSettings,
            world::{
                cache::TerrainSnapshot,
                components::{StreamingCamera, WaterSurface},
                water::FlatWaterBody,
            },
        };
        use bevy::{
            asset::AssetPlugin, camera::visibility::VisibilityPlugin, mesh::MeshPlugin,
            transform::TransformPlugin,
        };

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            MeshPlugin,
            TransformPlugin,
            VisibilityPlugin,
            SkyPlugin,
        ))
        .insert_resource(RenderingSettings {
            sky: true,
            fog: true,
            ..default()
        });
        let water = FlatWaterBody::from_terrain(
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 0,
                grid_y: 0,
            },
            &TerrainSnapshot {
                cell_id: 1,
                width: 33,
                height: 33,
                heights: vec![-100.0; 33 * 33],
                normals: vec![],
                vertex_colors: vec![],
                layers: vec![],
                water_height: Some(0.0),
                water_type_form_id: None,
            },
        )
        .expect("complete flat water body");
        app.world_mut()
            .spawn((WaterSurface, water, Transform::default()));
        let camera = app
            .world_mut()
            .spawn((
                Camera::default(),
                Projection::Perspective(PerspectiveProjection::default()),
                Transform::from_xyz(0.0, -10.0, 0.0),
                StreamingCamera,
                SkyCamera,
                FogCamera,
            ))
            .id();

        // Settle the submerged state even with the previous frame's underwater state.
        app.update();
        app.update();
        let (dome, _, _, visibility) = only_dome(&mut app);
        assert_eq!(visibility, Visibility::Hidden);
        assert!(app.world().get::<DistanceFog>(camera).is_none());

        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation
            .y = 10.0;
        app.update();

        let world = app.world();
        let palette = world.resource::<SkyPalette>();
        let ClearColorConfig::Custom(clear) = world.get::<Camera>(camera).unwrap().clear_color
        else {
            panic!("the sky camera clears to a custom colour");
        };
        assert_eq!(
            clear,
            palette.clear_colour(CameraSpace::Exterior),
            "exterior clear colour on the resurfacing frame",
        );
        let fog = world
            .get::<DistanceFog>(camera)
            .expect("exterior fog on the resurfacing frame");
        assert!(same_fog(fog, &fog_for(palette).unwrap()));
        assert!(
            world.get::<InheritedVisibility>(dome).unwrap().get(),
            "the dome's visibility propagates on the resurfacing frame",
        );
    }

    fn sky_app() -> App {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<SkyMaterial>>()
            .init_resource::<SkyPalette>()
            .init_resource::<CameraSpace>()
            .init_resource::<crate::underwater::UnderwaterState>()
            .add_systems(
                Update,
                (spawn_sky_domes, follow_sky_cameras, apply_sky, apply_fog).chain(),
            );
        app
    }

    /// `apply_fog` reads `InteriorFog`, which `apply_interior_lighting` writes, so the two
    /// conflict. Only an ordering edge keeps the fog from lagging a frame behind the light, and a
    /// behavioural test cannot tell the edge from a lucky executor order. This test builds the
    /// schedule and fails if any pair of systems is unordered while both touch `InteriorFog`.
    /// (System names are hidden without Bevy's `debug` feature, so the resource identifies them.)
    #[test]
    fn systems_that_touch_interior_fog_are_ordered() {
        use crate::world::environment::interior::{InteriorEnvironmentPlugin, InteriorFog};
        let mut app = App::new();
        app.add_plugins(InteriorEnvironmentPlugin).add_systems(
            PostUpdate,
            sky_systems().before(TransformSystems::Propagate),
        );
        let fog = app.world().component_id::<InteriorFog>().unwrap();
        // `initialize` creates resources of its own, so the schedules leave the world first.
        let mut schedules = app
            .world_mut()
            .remove_resource::<bevy::ecs::schedule::Schedules>()
            .unwrap();
        let schedule = schedules.get_mut(PostUpdate).unwrap();
        schedule.initialize(app.world_mut()).unwrap();
        let unordered = schedule
            .graph()
            .conflicting_systems()
            .0
            .iter()
            .filter(|(_, _, resources)| resources.contains(&fog))
            .count();
        assert_eq!(unordered, 0, "systems that touch InteriorFog are unordered");
    }

    /// `apply_fog` reads the `InteriorFog` that `EnvironmentSet` writes in the same schedule:
    /// with both plugins' systems, one door crossing (exterior to a fogged interior) and one room
    /// crossing (into a room template with other fog colours) each fog the camera in the same
    /// frame.
    #[test]
    fn interior_fog_applies_in_the_frame_the_environment_changes() {
        interior_fog_crossing_frames();
    }

    fn interior_fog_crossing_frames() {
        use crate::streaming::ActiveSpace;
        use crate::world::components::StreamingCamera;
        use crate::world::environment::{EnvironmentCatalog, interior::InteriorEnvironmentPlugin};
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS room_bounds (
                     ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
                     pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
                     rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
                     half_x REAL NOT NULL, half_y REAL NOT NULL, half_z REAL NOT NULL,
                     lighting_template_id INTEGER, image_space_id INTEGER,
                     flags INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO cell_environment(cell_id,data_flags) VALUES(77,1);
                 INSERT INTO cell_lighting(cell_id,xcll_size,inherit_flags,fog_near_r,
                    fog_near_g,fog_near_b,fog_near,fog_far,fog_max)
                 VALUES(77,92,0,31,52,46,100.0,4000.0,1.0);
                 INSERT INTO lighting_templates(id,fog_near_r,fog_near_g,fog_near_b)
                 VALUES(88,7,8,9);
                 INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,
                    half_x,half_y,half_z,lighting_template_id)
                 VALUES(99,77,1000,0,0,0,0,0,100,100,100,88);",
            )
            .unwrap();
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<SkyMaterial>>()
            .init_resource::<SkyPalette>()
            .init_resource::<CameraSpace>()
            .init_resource::<crate::underwater::UnderwaterState>()
            .insert_resource(EnvironmentCatalog::from_connection(&connection))
            .init_resource::<ActiveSpace>();
        app.add_plugins(InteriorEnvironmentPlugin).add_systems(
            PostUpdate,
            sky_systems().before(TransformSystems::Propagate),
        );
        let camera = app
            .world_mut()
            .spawn((FogCamera, StreamingCamera, Transform::default()))
            .id();
        app.update();
        let linear = |app: &App| match app
            .world()
            .get::<DistanceFog>(camera)
            .map(|f| (f.color, &f.falloff))
        {
            Some((color, FogFalloff::Linear { start, end })) => Some((color, *start, *end)),
            _ => None,
        };
        assert_eq!(
            linear(&app),
            None,
            "the exterior has the weather's power ramp"
        );
        let exterior = app.world().get::<DistanceFog>(camera).unwrap();
        let FogFalloff::PowerRamp {
            start, end, power, ..
        } = exterior.falloff
        else {
            panic!("expected the power ramp, got {:?}", exterior.falloff);
        };
        assert_eq!(
            (start, end),
            (0.0, 80_000.0),
            "SkyrimClear's day near and far"
        );
        assert!((power - 0.4).abs() < 1e-6);
        // Door crossing, as `door_crossing::switch_space` writes it.
        app.world_mut().resource_mut::<ActiveSpace>().interior = Some(77);
        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Interior;
        app.update();
        assert_eq!(
            linear(&app),
            Some((Color::srgb_u8(31, 52, 46), 100.0, 4000.0)),
            "cell fog in the crossing frame"
        );
        // Room crossing: the camera enters room 99 (Creation x 1000, engine x 1000).
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = Vec3::new(1000.0, 0.0, 0.0);
        app.update();
        assert_eq!(
            linear(&app),
            Some((Color::srgb_u8(7, 8, 9), 100.0, 4000.0)),
            "room fog colour in the crossing frame"
        );
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

    /// `SkyrimClear`'s day ambient, read from the record: its mean in linear light, about
    /// (0.186, 0.324, 0.377), and its sky-facing face the brightest of the six.
    #[test]
    fn clear_day_ambient_is_the_mean_of_the_records_six_faces() {
        let lighting = WeatherLighting::SKYRIM_CLEAR_DAY;
        let mean = lighting.mean_ambient().to_linear();
        for (channel, expected) in [(mean.red, 0.186), (mean.green, 0.324), (mean.blue, 0.377)] {
            assert!(
                (channel - expected).abs() < 0.005,
                "mean ambient channel {channel}, expected about {expected}"
            );
        }
        let up = lighting.ambient_toward(Vec3::Y);
        let down = lighting.ambient_toward(Vec3::NEG_Y);
        assert!(
            up.green > down.green * 1.5,
            "an upward normal reads the sky-facing face: up {up:?}, down {down:?}"
        );
    }

    /// The sunlight is the record's warm white, not pure white: it is what tints sunlit surfaces.
    #[test]
    fn clear_day_sunlight_is_the_records_warm_white() {
        let sun = WeatherLighting::default().sunlight.to_srgba();
        assert_eq!(
            [sun.red, sun.green, sun.blue].map(|c| (c * 255.0).round() as u8),
            [190, 181, 171]
        );
    }

    /// The fog is the weather's power ramp: near, far and power from the record, the cap from its
    /// `Max` in the colour's alpha, and the near colour from the palette.
    #[test]
    fn fog_is_the_weathers_power_ramp_in_its_two_colours() {
        let palette = SkyPalette {
            fog_ramp: Some(FogRamp {
                near: 1_000.0,
                far: 81_000.0,
                power: 0.4,
                max: 0.5,
            }),
            ..SkyPalette::SKYRIM_CLEAR_DAY
        };
        let fog = fog_for(&palette).expect("a complete ramp draws fog");
        assert_eq!(fog.color.alpha(), 0.5, "the colour's alpha is the cap");
        assert_eq!(
            fog.color.with_alpha(1.0),
            palette.clear_colour(CameraSpace::Exterior),
            "the far colour is Fog Far, as the sky clears to it"
        );
        let FogFalloff::PowerRamp {
            start,
            end,
            power,
            near_color,
        } = fog.falloff
        else {
            panic!("expected a power ramp, got {:?}", fog.falloff);
        };
        assert_eq!((start, end, power), (1_000.0, 81_000.0, 0.4));
        assert_eq!(near_color, palette.fog_near_colour());
        // Retail's amount at a distance: min(saturate((d - near) / (far - near))^power, max).
        let amount = |distance: f32| {
            ((distance - start) / (end - start))
                .clamp(0.0, 1.0)
                .powf(power)
                .min(fog.color.alpha())
        };
        assert_eq!(amount(500.0), 0.0);
        assert!((amount(1_000.0 + 80_000.0 * 0.1) - 0.1_f32.powf(0.4)).abs() < 1.0e-6);
        assert!(
            (amount(1_000.0 + 80_000.0 * 0.5) - 0.5).abs() < 1.0e-6,
            "capped at Max"
        );
        assert!((amount(500_000.0) - 0.5).abs() < 1.0e-6, "capped at Max");
        // Power 1 is the linear ramp.
        let linear = SkyPalette {
            fog_ramp: Some(FogRamp {
                power: 1.0,
                max: 1.0,
                ..palette.fog_ramp.unwrap()
            }),
            ..palette
        };
        let FogFalloff::PowerRamp { power, .. } = fog_for(&linear).unwrap().falloff else {
            panic!("expected a power ramp");
        };
        assert_eq!(power, 1.0);
    }

    /// A palette without a ramp (the record omitted a fog field), or with an empty one, draws no
    /// fog. No distance is invented.
    #[test]
    fn no_ramp_or_an_empty_ramp_draws_no_fog() {
        let with = |fog_ramp| SkyPalette {
            fog_ramp,
            ..SkyPalette::SKYRIM_CLEAR_DAY
        };
        let ramp = |near, far, max| {
            Some(FogRamp {
                near,
                far,
                power: 0.4,
                max,
            })
        };
        assert!(fog_for(&with(None)).is_none());
        assert!(fog_for(&with(ramp(100.0, 100.0, 0.85))).is_none());
        assert!(fog_for(&with(ramp(200.0, 100.0, 0.85))).is_none());
        assert!(fog_for(&with(ramp(0.0, 100.0, 0.0))).is_none());
        assert!(fog_for(&with(ramp(0.0, f32::NAN, 0.85))).is_none());
        // A power that is not positive and finite has no unverified retail meaning: no fog.
        for power in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let bad = Some(FogRamp {
                near: 0.0,
                far: 100.0,
                power,
                max: 0.85,
            });
            assert!(fog_for(&with(bad)).is_none(), "power {power}");
        }
        assert!(fog_for(&with(ramp(0.0, 100.0, 0.85))).is_some());
    }

    /// The built-in `SkyrimClear` palette carries the record's day distances.
    #[test]
    fn skyrim_clear_day_carries_the_record_fog_distances() {
        assert_eq!(
            SkyPalette::SKYRIM_CLEAR_DAY.fog_ramp,
            Some(FogRamp {
                near: 0.0,
                far: 80_000.0,
                power: 0.4,
                max: 0.85
            })
        );
    }

    /// The `skyrim` profile draws the weather's ramp and no sun glow, because the glow is a Mudcrab look.
    #[test]
    fn the_retail_look_has_no_sun_glow_and_the_mudcrab_look_does() {
        let palette = SkyPalette::SKYRIM_CLEAR_DAY;
        let mudcrab = exterior_fog(&palette, 1.0, FogLook::Mudcrab).unwrap();
        let retail = exterior_fog(&palette, 1.0, FogLook::Retail).unwrap();
        assert_eq!(mudcrab.directional_light_color, FOG_DIRECTIONAL_LIGHT_COLOR);
        assert_eq!(
            mudcrab.directional_light_exponent,
            FOG_DIRECTIONAL_LIGHT_EXPONENT
        );
        assert_eq!(retail.directional_light_color, Color::NONE);
        assert_eq!(retail.color, mudcrab.color);
        assert!(same_falloff(&retail, &mudcrab));
    }

    fn same_falloff(a: &DistanceFog, b: &DistanceFog) -> bool {
        matches!(
            (&a.falloff, &b.falloff),
            (
                FogFalloff::PowerRamp { start: a0, end: a1, power: a2, .. },
                FogFalloff::PowerRamp { start: b0, end: b1, power: b2, .. },
            ) if a0 == b0 && a1 == b1 && a2 == b2
        )
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
            0.85,
            "the fog cap is the weather's Max, so distant terrain reaches the fog colour only at the cap"
        );
        assert_eq!(fog.directional_light_color, FOG_DIRECTIONAL_LIGHT_COLOR);
        assert_eq!(
            fog.directional_light_exponent,
            FOG_DIRECTIONAL_LIGHT_EXPONENT
        );
        // `SkyrimClear` by day: near 0, far 80,000, power 0.4, max 0.85.
        match &fog.falloff {
            FogFalloff::PowerRamp {
                start, end, power, ..
            } => {
                assert_eq!((*start, *end), (0.0, 80_000.0));
                assert!((*power - 0.4).abs() < 1e-6);
            }
            other => panic!("expected a power ramp, got {other:?}"),
        }
        assert!(app.world().get::<DistanceFog>(unfogged).is_none());

        // The `cool-gray` look keeps the weather's ramp and divides both distances by its
        // density scale 0.25 (Mudcrab uses its approved look): four times as far.
        *app.world_mut().resource_mut::<SkyPalette>() =
            crate::color_pipeline::SceneVisualPreset::CoolGray.sky_palette();
        app.update();
        let lighter = app.world().get::<DistanceFog>(camera).unwrap();
        assert_eq!(
            lighter.color.with_alpha(1.0),
            Color::srgb_u8(210, 222, 235).to_linear().into()
        );
        match (&fog.falloff, &lighter.falloff) {
            (
                FogFalloff::PowerRamp {
                    start, end, power, ..
                },
                FogFalloff::PowerRamp {
                    start: lighter_start,
                    end: lighter_end,
                    power: lighter_power,
                    ..
                },
            ) => {
                assert_eq!(*lighter_start, *start * 4.0);
                assert_eq!(*lighter_end, *end * 4.0);
                assert_eq!(
                    lighter_power, power,
                    "the density scale does not touch power"
                );
            }
            _ => panic!("preset fog must remain a power ramp"),
        }
        assert!(app.world().get::<DistanceFog>(unfogged).is_none());
        *app.world_mut().resource_mut::<SkyPalette>() = palette;
        app.update();
        assert!(same_fog(
            app.world().get::<DistanceFog>(camera).unwrap(),
            &fog
        ));

        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Interior;
        app.update();
        assert!(app.world().get::<DistanceFog>(camera).is_none());

        *app.world_mut().resource_mut::<CameraSpace>() = CameraSpace::Exterior;
        app.update();
        assert!(app.world().get::<DistanceFog>(camera).is_some());
    }

    /// The `skyrim` profile gets the weather's ramp without the sun glow, because the glow is a Mudcrab look.
    #[test]
    fn the_skyrim_profile_fog_has_no_sun_glow() {
        let mut app = sky_app();
        app.insert_resource(crate::visual_profile::VisualProfile::Skyrim);
        let camera = app
            .world_mut()
            .spawn((Camera::default(), Transform::default(), FogCamera))
            .id();
        app.update();
        let fog = app.world().get::<DistanceFog>(camera).unwrap();
        assert_eq!(fog.directional_light_color, Color::NONE);
        assert!(same_falloff(
            fog,
            &exterior_fog(&SkyPalette::SKYRIM_CLEAR_DAY, 1.0, FogLook::Retail).unwrap()
        ));
    }

    /// A palette with no ramp (a record that omitted a fog field) leaves the camera unfogged.
    #[test]
    fn a_palette_without_a_ramp_leaves_the_exterior_unfogged() {
        let mut app = sky_app();
        *app.world_mut().resource_mut::<SkyPalette>() = SkyPalette {
            fog_ramp: None,
            ..SkyPalette::SKYRIM_CLEAR_DAY
        };
        let camera = app
            .world_mut()
            .spawn((Camera::default(), Transform::default(), FogCamera))
            .id();
        app.update();
        assert!(app.world().get::<DistanceFog>(camera).is_none());
    }

    #[test]
    fn fog_density_multiplier_composes_with_palette_and_updates_live() {
        let mut app = sky_app();
        let palette = crate::color_pipeline::SceneVisualPreset::CoolGray.sky_palette();
        *app.world_mut().resource_mut::<SkyPalette>() = palette;
        app.insert_resource(crate::settings::RenderingSettings::default());
        let camera = app
            .world_mut()
            .spawn((Camera::default(), Transform::default(), FogCamera))
            .id();
        app.update();

        let authored = fog_for(&palette).unwrap();
        let current = app.world().get::<DistanceFog>(camera).unwrap();
        assert!(same_fog(current, &authored));

        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .fog_density_scale = 0.5;
        app.update();
        let half = app.world().get::<DistanceFog>(camera).unwrap().clone();
        assert_fog_density_factor(&half, &authored, 0.5);

        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .fog_density_scale = 2.0;
        app.update();
        let double = app.world().get::<DistanceFog>(camera).unwrap().clone();
        assert_fog_density_factor(&double, &authored, 2.0);

        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .fog_density_scale = 0.5;
        app.update();
        assert!(same_fog(
            app.world().get::<DistanceFog>(camera).unwrap(),
            &half
        ));

        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .fog = false;
        app.update();
        assert!(app.world().get::<DistanceFog>(camera).is_none());
        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .fog = true;
        app.update();
        assert!(same_fog(
            app.world().get::<DistanceFog>(camera).unwrap(),
            &half
        ));

        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .fog_density_scale = 0.0;
        app.update();
        let zero = app.world().get::<DistanceFog>(camera).unwrap();
        let FogFalloff::PowerRamp { start, end, .. } = &zero.falloff else {
            panic!("expected a power ramp");
        };
        assert!(start.is_finite() && end.is_finite());
    }

    /// A density factor divides both distances, so a larger factor draws a denser fog.
    fn assert_fog_density_factor(actual: &DistanceFog, authored: &DistanceFog, factor: f32) {
        assert_eq!(actual.color, authored.color);
        let (
            FogFalloff::PowerRamp {
                start: authored_start,
                end: authored_end,
                ..
            },
            FogFalloff::PowerRamp {
                start: actual_start,
                end: actual_end,
                ..
            },
        ) = (&authored.falloff, &actual.falloff)
        else {
            panic!("expected a power ramp");
        };
        assert!((*actual_start - *authored_start / factor).abs() < 1.0e-1);
        assert!((*actual_end - *authored_end / factor).abs() < 1.0);
    }

    /// The palette follows the game clock: the same weather at another hour draws another fog
    /// distance, and a frozen clock leaves the palette's change tick quiet.
    #[test]
    fn the_palette_follows_the_clock_through_the_weather() {
        use crate::color_pipeline::{SceneGradeSource, SceneVisualPreset};
        use crate::weather_sky::WeatherSky;
        use crate::world::environment::{TimeOfDay, Weather, WeatherFog};
        let mut weather = Weather {
            fog: WeatherFog {
                day_near: Some(0.0),
                day_far: Some(80_000.0),
                night_near: Some(0.0),
                night_far: Some(40_000.0),
                day_power: Some(0.4),
                night_power: Some(0.4),
                day_max: Some(0.85),
                night_max: Some(0.85),
            },
            ..Default::default()
        };
        for time in TimeOfDay::ALL {
            for name in ["sky_upper", "sky_lower", "horizon", "fog_near", "fog_far"] {
                weather
                    .colors
                    .insert((name.to_owned(), time), [90, 100, 110]);
            }
        }
        let timing = crate::sun::SunTiming([33, 60, 96, 123]);
        let source = SceneGradeSource::default().with_weather(
            crate::visual_profile::VisualProfile::Skyrim,
            Some(WeatherSky::from_weather(0x81A, &weather, timing).unwrap()),
        );
        let mut app = App::new();
        app.insert_resource(source)
            .insert_resource(SceneVisualPreset::Vivid)
            .insert_resource(crate::sun::GameClock {
                hour: 12.0,
                timescale: 0.0,
            })
            .init_resource::<SkyPalette>()
            .add_systems(Update, drive_sky_palette);
        app.update();
        assert_eq!(
            app.world().resource::<SkyPalette>().fog_ramp.unwrap().far,
            80_000.0
        );
        app.update();
        assert!(
            !app.world().resource_ref::<SkyPalette>().is_changed(),
            "a frozen clock leaves the palette alone"
        );
        app.world_mut().resource_mut::<crate::sun::GameClock>().hour = 0.0;
        app.update();
        assert_eq!(
            app.world().resource::<SkyPalette>().fog_ramp.unwrap().far,
            40_000.0,
            "at midnight the night distances apply"
        );
    }

    #[test]
    fn fog_follows_the_sky_brightness_scale_so_it_matches_the_clear_colour() {
        let mut app = sky_app();
        app.insert_resource(crate::settings::RenderingSettings {
            sky_brightness_scale: 2.0,
            ..default()
        });
        let sky = app
            .world_mut()
            .spawn((
                Camera::default(),
                Transform::default(),
                SkyCamera,
                FogCamera,
            ))
            .id();
        app.update();

        let ClearColorConfig::Custom(clear) = app.world().get::<Camera>(sky).unwrap().clear_color
        else {
            panic!("the sky camera clears to a custom colour");
        };
        let fog = app.world().get::<DistanceFog>(sky).expect("fog outside");
        assert_eq!(fog.color.with_alpha(1.0), clear);
        assert_ne!(
            clear,
            SkyPalette::SKYRIM_CLEAR_DAY.clear_colour(CameraSpace::Exterior),
            "the multiplier must change the clear colour for this test to mean anything"
        );
    }
}
