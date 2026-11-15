//! Authored interior lighting selection. The sun direction is applied (RF-LIGHT-CPU-004). Every
//! other resolved value is reported, not applied.
//! See docs/specs/engine/interior-environment.md for evidence and unresolved native semantics.
//! Sources: q5-interior.md (REA 2026-11-14) and RF-LIGHT-CPU-001 to RF-LIGHT-CPU-009, and
//! s3-image-space.md.

use super::{DirectionalAmbient, EnvironmentCatalog, Lighting};
use crate::streaming::{ActiveSpace, creation_to_bevy};
use bevy::{prelude::*, transform::TransformSystems};
use std::collections::BTreeMap;

const SHOW_SKY: u32 = 0x80;
const USE_SKY_LIGHTING: u32 = 0x100;

/// s3-image-space.md "Cell image space for interiors (XCIM)" (cell getter 0x1402bf340): an
/// interior without the use-sky-lighting flag, `CELL DATA flags & 0x101 == 0x001`, takes its
/// image space from the cell's XCIM. The same getter returns form 0x167 `DefaultImageSpace`
/// when XCIM is absent.
const IMAGE_SPACE_FLAGS_MASK: u32 = 0x101;
const IMAGE_SPACE_FLAGS_INTERIOR_OWN: u32 = 0x001;
const DEFAULT_IMAGE_SPACE_FORM: u32 = 0x167;

/// Where the base image space of the scene comes from (s3-image-space.md, "Cell image space for
/// interiors (XCIM)"). Reported only: no image space is applied by this selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSpaceSource {
    /// The cell's XCIM form.
    Xcim,
    /// XCIM is absent: form 0x167 `DefaultImageSpace`.
    DefaultImageSpace,
    /// An exterior, or an interior that uses sky lighting: the weather-driven global block. It
    /// has no form id.
    WeatherGlobal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ImageSpaceSelection {
    pub source: ImageSpaceSource,
    pub form_id: Option<u32>,
}

impl ImageSpaceSelection {
    /// s3-image-space.md "Cell image space for interiors (XCIM)". `xcim` is `None` when the
    /// database has no value for the cell, `Some(None)` when the cell has no XCIM. Missing data
    /// returns `None`: no source is invented.
    fn for_interior(cell_flags: Option<u32>, xcim: Option<Option<u32>>) -> Option<Self> {
        let flags = cell_flags?;
        if flags & IMAGE_SPACE_FLAGS_MASK != IMAGE_SPACE_FLAGS_INTERIOR_OWN {
            return Some(Self {
                source: ImageSpaceSource::WeatherGlobal,
                form_id: None,
            });
        }
        Some(match xcim? {
            Some(form_id) => Self {
                source: ImageSpaceSource::Xcim,
                form_id: Some(form_id),
            },
            None => Self {
                source: ImageSpaceSource::DefaultImageSpace,
                form_id: Some(DEFAULT_IMAGE_SPACE_FORM),
            },
        })
    }
}

// XCLL inheritance bits (the template supplies the field group when the bit is set).
// Retail trace, RF-LIGHT-CPU-001 (q5-interior.md "Inheritance", getters 0x1402bbe40 to
// 0x1402bc790): 0x1, 0x2, 0x4, 0x20, 0x40 and 0x400.
// Retail trace, RF-LIGHT-CPU-009 (high): 0x80 clip distance (+0x20), 0x100 fog power (+0x24),
// 0x200 fog max (+0x4C). These fields are reported, not applied.
const INHERIT_AMBIENT_AND_DALC: u32 = 0x1;
const INHERIT_DIRECTIONAL_COLOR: u32 = 0x2;
const INHERIT_FOG_COLORS: u32 = 0x4;
const INHERIT_ROTATION: u32 = 0x20;
const INHERIT_DIRECTIONAL_FADE: u32 = 0x40;
const INHERIT_LIGHT_FADE: u32 = 0x400;
const INHERIT_FOG_CLIP: u32 = 0x80;
const INHERIT_FOG_POWER: u32 = 0x100;
const INHERIT_FOG_MAX: u32 = 0x200;
// Not covered by RF-LIGHT-CPU-001 or RF-LIGHT-CPU-009: 0x8 and 0x10 (fog near and fog far
// distance) keep the xEdit definitions (docs/specs/engine/interior-environment.md "Evidence").
// Reported, never used.
const INHERIT_FOG_NEAR: u32 = 0x8;
const INHERIT_FOG_FAR: u32 = 0x10;

/// Colour bytes are RGBA8 x 1/255 (q5-interior.md "Inheritance": "Colours are RGBA8 bytes x
/// 1/255", high). This is the only conversion applied. It is a report, not a render value.
const BYTE_TO_UNIT: f32 = 1.0 / 255.0;

fn unit_rgb(bytes: [u8; 3]) -> [f32; 3] {
    bytes.map(|byte| f32::from(byte) * BYTE_TO_UNIT)
}

/// The ambient cube the retail Sky update builds from the six DALC colours (RF-LIGHT-CPU-002,
/// function 0x1414ec3b0, evaluated at 0x1414ec730). Per channel, with c[0..5] the six colours in
/// record order (bytes x 1/255):
/// `mean = (c0+c1+c2+c3+c4+c5)/6`, `half_difference_x = (c1-c0)/2`,
/// `half_difference_y = (c3-c2)/2`, `half_difference_z = (c5-c4)/2`.
/// The specular colour passes through (x 1/255). `scale = max(DALC scale, 1.0)`.
/// Confidence: high. The sign is settled by s3-image-space.md Q-B: record order, no negation, no
/// clamp; a +X normal leans to record colour 1, +Y to colour 3, +Z to colour 5 (xEdit labels X-,
/// Y-, Z- ... record colour 0 lights the -X face). This value is reported only. No ambient light
/// or shader reads it here.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct AmbientCube {
    pub mean: [f32; 3],
    pub half_difference_x: [f32; 3],
    pub half_difference_y: [f32; 3],
    pub half_difference_z: [f32; 3],
    /// Missing stays missing: no default colour.
    pub specular: Option<[f32; 3]>,
    /// `max(DALC scale, 1.0)`. Missing stays missing: no default scale.
    pub scale: Option<f32>,
}

impl AmbientCube {
    pub fn from_dalc(dalc: &DirectionalAmbient) -> Self {
        let mut cube = Self {
            mean: [0.0; 3],
            half_difference_x: [0.0; 3],
            half_difference_y: [0.0; 3],
            half_difference_z: [0.0; 3],
            specular: dalc.specular.map(unit_rgb),
            scale: dalc.scale.map(|scale| scale.max(1.0)),
        };
        for channel in 0..3 {
            let c = |face: usize| f32::from(dalc.colors[face][channel]);
            cube.mean[channel] = (c(0) + c(1) + c(2) + c(3) + c(4) + c(5)) / 6.0 * BYTE_TO_UNIT;
            cube.half_difference_x[channel] = (c(1) - c(0)) / 2.0 * BYTE_TO_UNIT;
            cube.half_difference_y[channel] = (c(3) - c(2)) / 2.0 * BYTE_TO_UNIT;
            cube.half_difference_z[channel] = (c(5) - c(4)) / 2.0 * BYTE_TO_UNIT;
        }
        cube
    }

    /// `ambient(n) = mean + n.x * half_difference_x + n.y * half_difference_y +
    /// n.z * half_difference_z` per channel (RF-LIGHT-CPU-002, 0x1414ec730).
    /// Sign: settled by s3-image-space.md Q-B: record order, no negation, no clamp; a +X normal
    /// leans to record colour 1, +Y to colour 3, +Z to colour 5 (xEdit labels X-, Y-, Z- ...
    /// record colour 0 lights the -X face). `normal` is the world-space normal (s3-image-space.md
    /// Q-A and Q-B: the evaluation is in world space, medium-high) in Creation axes. Reported
    /// only: nothing applies this to `GlobalAmbientLight` or a shader.
    pub fn ambient_at(&self, normal: Vec3) -> [f32; 3] {
        std::array::from_fn(|channel| {
            self.mean[channel]
                + normal.x * self.half_difference_x[channel]
                + normal.y * self.half_difference_y[channel]
                + normal.z * self.half_difference_z[channel]
        })
    }
}

/// The selected light fade distances and their squares (RF-LIGHT-CPU-001: inheritance bit 0x400
/// selects begin and end; RF-LIGHT-CPU-003: retail stores `begin^2` and `end^2` at 0x1433d3d34
/// and 0x1433d3d38). Present only when both distances are present. The raw values stay in
/// `Lighting::light_fade_begin` and `light_fade_end`. Reported only. Retail applies the fade only
/// to a light whose BSLight `+0x63` flag is set (RF-LIGHT-CPU-007, see
/// [`point_light_fade_factor`]).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct LightFade {
    pub begin: f32,
    pub end: f32,
    pub begin_squared: f32,
    pub end_squared: f32,
}

impl LightFade {
    pub fn new(begin: f32, end: f32) -> Self {
        Self {
            begin,
            end,
            begin_squared: begin * begin,
            end_squared: end * end,
        }
    }
}

/// The retail point-light fade factor (RF-LIGHT-CPU-003, function 0x141575d20) with
/// `q = |camera - light|^2 - r^2` (r: the light term at the same site): 1 if `q < begin^2`;
/// `1 - (q - begin^2) / (end^2 - begin^2)` up to `end^2`; 0 (light off) beyond.
/// Retail applies it only to a light whose per-light fade flag is set. That flag is BSLight
/// `+0x63` (RF-LIGHT-CPU-007, high). It is set from the creation parameter byte 5, which is NOT
/// (LIGH reference has REFR flag 0x00010000, "Never Fades"), and defaults to 1.
/// NOT APPLIED to any light here: this function is reported only. The local-light path belongs
/// to #488, which receives the resolved begin and end values and this flag rule.
pub fn point_light_fade_factor(q: f32, fade: &LightFade) -> f32 {
    if q < fade.begin_squared {
        1.0
    } else if q < fade.end_squared {
        1.0 - (q - fade.begin_squared) / (fade.end_squared - fade.begin_squared)
    } else {
        0.0
    }
}

/// The direction the interior directional light travels, in engine axes (RF-LIGHT-CPU-004,
/// retail 0x14041be6e to 0x14041bf8x, medium: sign/axis convention unverified). Retail builds
/// the light rotation as `R_z(rotation XY) x R_y(rotation Z)`, both in degrees x pi/180, and the
/// light shines along its local X axis (RF-SUN-002 convention). In Creation axes that is
/// `d = R_z(a) * R_y(b) * (1,0,0) = (cos a cos b, sin a cos b, -sin b)`. The engine's existing
/// Creation-to-engine mapping, `(x, y, z) -> (x, z, -y)` (`creation_to_bevy`), converts it.
pub(crate) fn interior_sun_direction(rotation_xy_degrees: i32, rotation_z_degrees: i32) -> Vec3 {
    let (sin_a, cos_a) = (rotation_xy_degrees as f32).to_radians().sin_cos();
    let (sin_b, cos_b) = (rotation_z_degrees as f32).to_radians().sin_cos();
    creation_to_bevy(Vec3::new(cos_a * cos_b, sin_a * cos_b, -sin_b))
}

/// The streamed world's sun: the one directional light the interior selection may turn.
#[derive(Component, Debug, Default, Clone, Copy)]
pub(crate) struct WorldSun;

/// The exterior rotation of the [`WorldSun`], kept while an interior sun direction is applied.
/// Nothing rewrites the sun's rotation each frame in the exterior, so it is restored from here.
#[derive(Component, Debug, Clone, Copy)]
struct ExteriorSunRotation(Quat);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum LightingSource {
    CellXcll,
    MalformedCellXcll(u32),
    TemplateData(u32),
    TemplateDalc(u32),
    MissingTemplate(Option<u32>),
    MissingCell,
}

/// Selected values remain optional: authored black/zero never means missing.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct InteriorLighting {
    pub cell_id: u32,
    pub xcll_size: Option<u32>,
    pub template_id: Option<u32>,
    pub inherit_flags: Option<u32>,
    pub template_only: bool,
    pub lighting: Lighting,
    pub sources: BTreeMap<&'static str, LightingSource>,
    pub missing: Vec<&'static str>,
    /// Authored ambient colour, bytes x 1/255 (q5-interior.md "Inheritance"). Reported only.
    pub ambient_color: Option<[f32; 3]>,
    /// Authored directional colour, bytes x 1/255 (q5-interior.md "Inheritance"). Reported
    /// only. Its unit conversion to a render value and the combine wait for S3.
    pub directional_color: Option<[f32; 3]>,
    /// RF-LIGHT-CPU-002, reported only.
    pub ambient_cube: Option<AmbientCube>,
    /// RF-LIGHT-CPU-001 and RF-LIGHT-CPU-003, reported only.
    pub light_fade: Option<LightFade>,
}

impl EnvironmentCatalog {
    pub(crate) fn resolve_interior(&self, cell_id: u32) -> InteriorLighting {
        let cell = self.cell_lighting(cell_id);
        let template_id = cell.and_then(|cell| cell.lighting_template);
        let template = template_id.and_then(|id| self.lighting_template(id));
        let local = cell.map_or(Lighting::default(), |cell| cell.lighting);
        let inherited = template.map_or(Lighting::default(), |template| template.lighting);
        let inherit_flags = cell.and_then(|cell| cell.inherit_flags);
        // LTMP-only cells have no local block to override the template. Short XCLL blocks
        // without a mask keep local values. This is a compatibility policy, not a native trace.
        let template_only = cell.is_some_and(|cell| cell.xcll_size.is_none());
        let flags = inherit_flags.unwrap_or(if template_only { 0x7ff } else { 0 });
        let mut result = InteriorLighting {
            cell_id,
            xcll_size: cell.and_then(|cell| cell.xcll_size),
            template_id,
            inherit_flags,
            template_only,
            lighting: Lighting::default(),
            sources: BTreeMap::new(),
            missing: Vec::new(),
            ambient_color: None,
            directional_color: None,
            ambient_cube: None,
            light_fade: None,
        };
        macro_rules! select {
            ($field:ident, $bit:expr, $template_value:expr, $dalc:expr) => {{
                let from_template = flags & $bit != 0;
                let value = if from_template {
                    $template_value
                } else {
                    local.$field
                };
                let source = if from_template {
                    match (template_id, template) {
                        (Some(id), Some(_)) if $dalc => LightingSource::TemplateDalc(id),
                        (Some(id), Some(_)) => LightingSource::TemplateData(id),
                        _ => LightingSource::MissingTemplate(template_id),
                    }
                } else if let Some(size) = result.xcll_size.filter(|size| *size < 72) {
                    LightingSource::MalformedCellXcll(size)
                } else if cell.is_some() {
                    LightingSource::CellXcll
                } else {
                    LightingSource::MissingCell
                };
                result.sources.insert(stringify!($field), source);
                if value.is_none() {
                    result.missing.push(stringify!($field));
                }
                result.lighting.$field = value;
            }};
            ($field:ident, $bit:expr) => {
                select!($field, $bit, inherited.$field, false)
            };
        }
        select!(ambient, INHERIT_AMBIENT_AND_DALC);
        // LGTM DATA's embedded ambient block is unused in the serialized format. Use DALC.
        select!(
            directional_ambient,
            INHERIT_AMBIENT_AND_DALC,
            template.and_then(|t| t.directional_ambient),
            true
        );
        select!(directional, INHERIT_DIRECTIONAL_COLOR);
        select!(fog_near_color, INHERIT_FOG_COLORS);
        select!(fog_far_color, INHERIT_FOG_COLORS);
        select!(fog_near, INHERIT_FOG_NEAR);
        select!(fog_far, INHERIT_FOG_FAR);
        // RF-LIGHT-CPU-004: the rotation pair is applied to the sun only when both are present.
        select!(directional_rotation_xy, INHERIT_ROTATION);
        select!(directional_rotation_z, INHERIT_ROTATION);
        // RF-LIGHT-CPU-008 (supersedes RF-LIGHT-CPU-005): the directional-fade getter (XCLL
        // +0x1C) has no runtime reader. Retail only stores the value. The raw selected value is
        // reported. No fade is applied.
        select!(directional_fade, INHERIT_DIRECTIONAL_FADE);
        select!(fog_clip_distance, INHERIT_FOG_CLIP);
        select!(fog_power, INHERIT_FOG_POWER);
        select!(fog_max, INHERIT_FOG_MAX);
        select!(light_fade_begin, INHERIT_LIGHT_FADE);
        select!(light_fade_end, INHERIT_LIGHT_FADE);
        // Reported values derived from the selection. A missing source field stays missing.
        result.ambient_color = result.lighting.ambient.map(unit_rgb);
        result.directional_color = result.lighting.directional.map(unit_rgb);
        result.ambient_cube = result
            .lighting
            .directional_ambient
            .as_ref()
            .map(AmbientCube::from_dalc);
        result.light_fade = result
            .lighting
            .light_fade_begin
            .zip(result.lighting.light_fade_end)
            .map(|(begin, end)| LightFade::new(begin, end));
        result
    }
}

/// One scene selection shared by diagnostics and sky. ActiveSpace owns scene identity.
#[derive(Resource, Debug, Clone, PartialEq, Default, serde::Serialize)]
pub(crate) struct SceneEnvironment {
    pub interior: Option<InteriorLighting>,
    pub cell_flags: Option<u32>,
    pub show_sky: bool,
    pub use_sky_lighting: bool,
    /// Reported only (s3-image-space.md "Cell image space for interiors (XCIM)"). Missing when
    /// the cell flags or the XCIM column are missing. Exterior scenes report `weather_global`.
    pub image_space: Option<ImageSpaceSelection>,
}

impl SceneEnvironment {
    fn select(catalog: &EnvironmentCatalog, space: &ActiveSpace) -> Self {
        let Some(cell_id) = space.interior else {
            return Self {
                show_sky: true,
                use_sky_lighting: true,
                image_space: Some(ImageSpaceSelection {
                    source: ImageSpaceSource::WeatherGlobal,
                    form_id: None,
                }),
                ..default()
            };
        };
        let cell_flags = catalog.cell_flags(cell_id);
        Self {
            interior: Some(catalog.resolve_interior(cell_id)),
            cell_flags,
            show_sky: cell_flags.is_some_and(|flags| flags & SHOW_SKY != 0),
            use_sky_lighting: cell_flags.is_some_and(|flags| flags & USE_SKY_LIGHTING != 0),
            image_space: ImageSpaceSelection::for_interior(
                cell_flags,
                catalog.cell_image_space(cell_id),
            ),
        }
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EnvironmentSet;

pub(crate) struct InteriorEnvironmentPlugin;

impl Plugin for InteriorEnvironmentPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SceneEnvironment>().add_systems(
            PostUpdate,
            (select_environment, apply_interior_sun_direction)
                .chain()
                .in_set(EnvironmentSet)
                .after(crate::door_crossing::DoorCrossingSet)
                .before(TransformSystems::Propagate),
        );
    }
}

/// Runs after queued PostUpdate door switches, before sky and extraction. It records the
/// selection only. It writes no Bevy light: the mapping from the other authored values to the
/// renderer has no Skyrim source yet (#693 to #696). The sun direction is applied separately by
/// [`apply_interior_sun_direction`].
fn select_environment(
    catalog: Res<EnvironmentCatalog>,
    space: Res<ActiveSpace>,
    mut scene: ResMut<SceneEnvironment>,
    mut selected_once: Local<bool>,
) {
    let first_selection = !std::mem::replace(&mut *selected_once, true);
    if first_selection
        || catalog.is_changed()
        || space.is_changed()
        || scene.interior.is_none() && space.interior.is_some()
    {
        let selected = SceneEnvironment::select(&catalog, &space);
        if *scene != selected {
            info!(environment = ?selected, "selected scene environment");
            *scene = selected;
        }
    }
}

/// Applies the one sourced interior light value: the sun direction (RF-LIGHT-CPU-004, medium).
/// It writes the [`WorldSun`] Transform rotation only, and only while the selected interior has
/// both rotation fields. A missing field changes nothing. Colour, illuminance, cascades and
/// `GlobalAmbientLight` are never written. Nothing rewrites the sun rotation each frame in the
/// exterior, so the exterior rotation is saved on the first interior write and restored when
/// the interior no longer selects a rotation.
fn apply_interior_sun_direction(
    scene: Res<SceneEnvironment>,
    mut commands: Commands,
    mut suns: Query<(Entity, &mut Transform, Option<&ExteriorSunRotation>), With<WorldSun>>,
) {
    let wanted = scene.interior.as_ref().and_then(|interior| {
        let direction = interior_sun_direction(
            interior.lighting.directional_rotation_xy?,
            interior.lighting.directional_rotation_z?,
        );
        // The roll about the light axis does not change a directional light. Bevy handles a
        // direction parallel to the up vector.
        Some(Transform::IDENTITY.looking_to(direction, Vec3::Y).rotation)
    });
    for (entity, mut transform, saved) in &mut suns {
        match (wanted, saved) {
            (Some(rotation), saved) => {
                if saved.is_none() {
                    commands
                        .entity(entity)
                        .insert(ExteriorSunRotation(transform.rotation));
                }
                if transform.rotation != rotation {
                    transform.rotation = rotation;
                }
            }
            (None, Some(saved)) => {
                if transform.rotation != saved.0 {
                    transform.rotation = saved.0;
                }
                commands.entity(entity).remove::<ExteriorSunRotation>();
            }
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::light::CascadeShadowConfig;
    use rusqlite::Connection;

    fn fixture() -> EnvironmentCatalog {
        let connection = Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection.execute_batch(
            "INSERT INTO cell_environment(cell_id,data_flags) VALUES(10,1),(11,129),(12,257),(13,385);
             INSERT INTO cell_lighting(cell_id,lighting_template_id,xcll_size,inherit_flags,
                ambient_r,ambient_g,ambient_b,directional_r,directional_g,directional_b,
                directional_rotation_xy,directional_rotation_z,directional_fade)
             VALUES(10,20,92,3,200,201,202,210,211,212,90,30,0.25),
                   (11,20,92,3,200,201,202,210,211,212,90,30,0.25);
             INSERT INTO lighting_templates(id,ambient_r,ambient_g,ambient_b,
                directional_r,directional_g,directional_b,directional_rotation_xy,
                directional_rotation_z,directional_fade)
             VALUES(20,30,40,50,60,70,80,180,60,0.75);
"
        ).unwrap();
        EnvironmentCatalog::from_connection(&connection)
    }

    pub(super) fn app(catalog: EnvironmentCatalog) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(catalog)
            .init_resource::<ActiveSpace>()
            .add_plugins(InteriorEnvironmentPlugin);
        app
    }

    pub(super) fn enter(app: &mut App, cell: Option<u32>) {
        app.world_mut().resource_mut::<ActiveSpace>().interior = cell;
        app.update();
    }

    fn selected(app: &App) -> &InteriorLighting {
        app.world()
            .resource::<SceneEnvironment>()
            .interior
            .as_ref()
            .unwrap()
    }

    #[test]
    fn interior_selection_writes_no_bevy_light() {
        // The sun Transform rotation is now applied (RF-LIGHT-CPU-004) and is asserted by the
        // pack-cell tests. This test covers the light fields that stay unwritten.
        let exterior_rotation = Quat::from_rotation_y(0.4);
        let mut app = app(fixture());
        app.insert_resource(GlobalAmbientLight {
            color: Color::srgb(0.1, 0.2, 0.3),
            brightness: 123.0,
            ..default()
        });
        let sun = app
            .world_mut()
            .spawn((
                DirectionalLight {
                    color: Color::srgb(0.8, 0.7, 0.6),
                    illuminance: 4321.0,
                    ..default()
                },
                CascadeShadowConfig::default(),
                WorldSun,
                Transform::from_rotation(exterior_rotation),
            ))
            .id();
        // `App::update` clears the world change trackers, so detect writes with a system:
        // its change ticks survive across updates. The first two updates settle added state.
        // The probe reads the DirectionalLight component (colour and illuminance live there),
        // the cascade configuration and the ambient resource. It does not read the Transform.
        #[derive(Resource, Default)]
        struct LightWrites(u32);
        app.init_resource::<LightWrites>().add_systems(
            PostUpdate,
            |lights: Query<(Ref<DirectionalLight>, Ref<CascadeShadowConfig>)>,
             ambient: Res<GlobalAmbientLight>,
             mut writes: ResMut<LightWrites>| {
                for (light, cascades) in &lights {
                    writes.0 += (light.is_changed() || cascades.is_changed()) as u32;
                }
                writes.0 += ambient.is_changed() as u32;
            },
        );
        app.update();
        app.update();
        app.world_mut().resource_mut::<LightWrites>().0 = 0;
        // Cell 10 authors non-black ambient and directional colours, a nonzero fade and a
        // rotation, and does not use sky lighting.
        enter(&mut app, Some(10));
        let lighting = &selected(&app).lighting;
        assert_eq!(lighting.ambient, Some([30, 40, 50]));
        assert_eq!(lighting.directional, Some([60, 70, 80]));
        assert_eq!(lighting.directional_fade, Some(0.25));
        assert_eq!(lighting.directional_rotation_xy, Some(90));
        assert!(!app.world().resource::<SceneEnvironment>().use_sky_lighting);

        let entity = app.world().entity(sun);
        let light = entity.get_ref::<DirectionalLight>().unwrap();
        assert_eq!(light.color, Color::srgb(0.8, 0.7, 0.6));
        assert_eq!(light.illuminance, 4321.0);
        let cascades = entity.get_ref::<CascadeShadowConfig>().unwrap();
        assert_eq!(
            format!("{:?}", *cascades),
            format!("{:?}", CascadeShadowConfig::default())
        );
        let ambient = app.world().resource::<GlobalAmbientLight>();
        assert_eq!(ambient.color, Color::srgb(0.1, 0.2, 0.3));
        assert_eq!(ambient.brightness, 123.0);
        assert_eq!(
            app.world().resource::<LightWrites>().0,
            0,
            "the interior selection wrote a Bevy light"
        );
    }

    #[test]
    fn local_inherited_zero_and_missing_values_are_reported_unchanged() {
        let mut catalog = fixture();
        {
            let cell = catalog.cells.get_mut(&10).unwrap();
            cell.inherit_flags = Some(0);
            cell.lighting.ambient = Some([0; 3]);
            cell.lighting.directional = Some([0; 3]);
            cell.lighting.directional_rotation_xy = Some(0);
            cell.lighting.directional_rotation_z = Some(0);
            cell.lighting.directional_fade = Some(0.0);
        }
        let mut app = app(catalog);
        enter(&mut app, Some(10));
        let lighting = &selected(&app).lighting;
        assert_eq!(lighting.ambient, Some([0; 3]));
        assert_eq!(lighting.directional, Some([0; 3]));
        assert_eq!(lighting.directional_fade, Some(0.0));
        assert!(!selected(&app).missing.contains(&"directional_fade"));
        assert_eq!(
            selected(&app).sources["directional"],
            LightingSource::CellXcll
        );
        {
            let mut catalog = app.world_mut().resource_mut::<EnvironmentCatalog>();
            let cell = catalog.cells.get_mut(&10).unwrap();
            cell.inherit_flags = Some(3);
            cell.lighting_template = Some(999);
        }
        app.update();
        assert_eq!(selected(&app).lighting.directional, None);
        assert_eq!(
            selected(&app).sources["directional"],
            LightingSource::MissingTemplate(Some(999))
        );
        assert!(selected(&app).missing.contains(&"directional"));
        enter(&mut app, Some(404));
        assert_eq!(selected(&app).lighting, Lighting::default());
        enter(&mut app, None);
        assert!(
            app.world()
                .resource::<SceneEnvironment>()
                .interior
                .is_none()
        );
    }

    #[test]
    fn template_dalc_uses_separate_subrecord_and_black_is_not_missing() {
        let mut catalog = fixture();
        let template = catalog.templates.get_mut(&20).unwrap();
        template.lighting.directional_ambient = Some(super::super::DirectionalAmbient {
            colors: [[255; 3]; 6],
            ..default()
        });
        template.directional_ambient = Some(super::super::DirectionalAmbient {
            colors: [[0; 3]; 6],
            scale: Some(25.0),
            ..default()
        });
        let mut app = app(catalog);
        enter(&mut app, Some(10));
        let cube = selected(&app).lighting.directional_ambient.unwrap();
        assert_eq!(cube.colors, [[0; 3]; 6]);
        assert_eq!(cube.scale, Some(25.0));
        assert!(!selected(&app).missing.contains(&"directional_ambient"));
        assert_eq!(
            selected(&app).sources["directional_ambient"],
            LightingSource::TemplateDalc(20)
        );
        {
            let mut catalog = app.world_mut().resource_mut::<EnvironmentCatalog>();
            let template = catalog.templates.get_mut(&20).unwrap();
            template.directional_ambient.as_mut().unwrap().colors =
                [[255, 0, 0], [0; 3], [0; 3], [0; 3], [0; 3], [0; 3]];
        }
        app.update();
        assert_eq!(
            selected(&app).lighting.directional_ambient.unwrap().colors,
            [[255, 0, 0], [0; 3], [0; 3], [0; 3], [0; 3], [0; 3]]
        );
    }

    #[test]
    fn every_inheritance_group_and_absent_mask_policy_is_explicit() {
        let mut catalog = fixture();
        let local = Lighting {
            ambient: Some([1; 3]),
            directional: Some([2; 3]),
            fog_near_color: Some([3; 3]),
            fog_far_color: Some([4; 3]),
            fog_near: Some(5.0),
            fog_far: Some(6.0),
            directional_rotation_xy: Some(7),
            directional_rotation_z: Some(8),
            directional_fade: Some(9.0),
            fog_clip_distance: Some(10.0),
            fog_power: Some(11.0),
            fog_max: Some(12.0),
            light_fade_begin: Some(13.0),
            light_fade_end: Some(14.0),
            directional_ambient: None,
        };
        catalog.cells.get_mut(&10).unwrap().lighting = local;
        catalog.cells.get_mut(&10).unwrap().inherit_flags = Some(0);
        assert_eq!(catalog.resolve_interior(10).lighting, local);
        catalog.cells.get_mut(&10).unwrap().inherit_flags = Some(0x7ff);
        let selected = catalog.resolve_interior(10);
        assert_eq!(selected.lighting, catalog.templates[&20].lighting);
        assert!(selected.sources.values().all(|source| matches!(
            source,
            LightingSource::TemplateData(20) | LightingSource::TemplateDalc(20)
        )));
        // Missing template fields remain missing, rather than borrowing a local value.
        assert_eq!(selected.lighting.fog_near, None);
        assert!(selected.missing.contains(&"fog_near"));
        let cell = catalog.cells.get_mut(&10).unwrap();
        cell.xcll_size = Some(72);
        cell.inherit_flags = None;
        assert_eq!(catalog.resolve_interior(10).lighting, local);
        let cell = catalog.cells.get_mut(&10).unwrap();
        cell.xcll_size = None;
        cell.lighting = Lighting::default();
        assert!(catalog.resolve_interior(10).template_only);
        assert_eq!(
            catalog.resolve_interior(10).lighting.directional,
            Some([60, 70, 80])
        );
    }

    #[test]
    fn individual_inheritance_bits_select_independent_scene_fields() {
        let mut catalog = fixture();
        catalog
            .cells
            .get_mut(&10)
            .unwrap()
            .lighting
            .directional_ambient = Some(super::super::DirectionalAmbient {
            colors: [[255, 0, 0]; 6],
            ..default()
        });
        catalog.templates.get_mut(&20).unwrap().directional_ambient =
            Some(super::super::DirectionalAmbient {
                colors: [[0, 255, 0]; 6],
                ..default()
            });
        let mut app = app(catalog);
        for mask in [0, 1, 2, 0x20, 0x40, 0x61, 0x7ff, 0] {
            app.world_mut()
                .resource_mut::<EnvironmentCatalog>()
                .cells
                .get_mut(&10)
                .unwrap()
                .inherit_flags = Some(mask);
            enter(&mut app, Some(10));
            let lighting = &selected(&app).lighting;
            assert_eq!(
                lighting.directional,
                Some(if mask & 2 != 0 {
                    [60, 70, 80]
                } else {
                    [210, 211, 212]
                })
            );
            assert_eq!(
                lighting.directional_fade,
                Some(if mask & 0x40 != 0 { 0.75 } else { 0.25 })
            );
            assert_eq!(
                lighting.directional_ambient.unwrap().colors,
                if mask & 1 != 0 {
                    [[0, 255, 0]; 6]
                } else {
                    [[255, 0, 0]; 6]
                }
            );
            assert_eq!(
                (
                    lighting.directional_rotation_xy,
                    lighting.directional_rotation_z
                ),
                if mask & 0x20 != 0 {
                    (Some(180), Some(60))
                } else {
                    (Some(90), Some(30))
                }
            );
        }
    }

    #[test]
    fn show_sky_and_use_sky_lighting_are_independent_runtime_choices() {
        let mut app = app(fixture());
        for (cell, sky, sky_lighting) in [
            (10, false, false),
            (11, true, false),
            (12, false, true),
            (13, true, true),
        ] {
            enter(&mut app, Some(cell));
            let scene = app.world().resource::<SceneEnvironment>();
            assert_eq!(scene.show_sky, sky);
            assert_eq!(scene.use_sky_lighting, sky_lighting);
        }
    }

    #[test]
    fn old_database_reports_every_authored_field_missing() {
        let catalog = EnvironmentCatalog::from_connection(&Connection::open_in_memory().unwrap());
        let mut app = app(catalog);
        app.update();
        enter(&mut app, Some(10));
        let scene = app.world().resource::<SceneEnvironment>();
        assert!(!scene.show_sky);
        assert_eq!(scene.image_space, None);
        assert_eq!(
            scene.interior.as_ref().unwrap().sources["ambient"],
            LightingSource::MissingCell
        );
        assert_eq!(selected(&app).lighting, Lighting::default());
        enter(&mut app, None);
        let scene = app.world().resource::<SceneEnvironment>();
        assert!(scene.interior.is_none());
        assert!(scene.show_sky && scene.use_sky_lighting);
    }

    // Rows below are copied from `cell_lighting` and `lighting_templates` of the registered pack
    // user-testing-main-b5259d33-clusters, read with read-only sqlite3 (2026-11-14).
    fn cell_row(
        (cell, template): (u32, u32),
        ambient: [u8; 3],
        directional: [u8; 3],
        rotation: (i32, i32),
        faces_and_specular: [[u8; 3]; 7],
        light_fade: (f32, f32),
        inherit: u32,
    ) -> String {
        let colours = faces_and_specular
            .iter()
            .flatten()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "INSERT INTO cell_lighting(cell_id,lighting_template_id,xcll_size,
                ambient_r,ambient_g,ambient_b,directional_r,directional_g,directional_b,
                directional_rotation_xy,directional_rotation_z,directional_fade,
                dalc_x_pos_r,dalc_x_pos_g,dalc_x_pos_b,dalc_x_neg_r,dalc_x_neg_g,dalc_x_neg_b,
                dalc_y_pos_r,dalc_y_pos_g,dalc_y_pos_b,dalc_y_neg_r,dalc_y_neg_g,dalc_y_neg_b,
                dalc_z_pos_r,dalc_z_pos_g,dalc_z_pos_b,dalc_z_neg_r,dalc_z_neg_g,dalc_z_neg_b,
                dalc_specular_r,dalc_specular_g,dalc_specular_b,dalc_scale,
                light_fade_begin,light_fade_end,inherit_flags)
             VALUES({cell},{template},92,{},{},{},{},{},{},{},{},0.0,{colours},1.0,{:?},{:?},{inherit});",
            ambient[0],
            ambient[1],
            ambient[2],
            directional[0],
            directional[1],
            directional[2],
            rotation.0,
            rotation.1,
            light_fade.0,
            light_fade.1,
        )
    }

    fn pack_catalog() -> EnvironmentCatalog {
        let connection = Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        // cell_environment rows (data_flags, image_space_id) of the same pack cells.
        connection
            .execute_batch(
                "INSERT INTO cell_environment(cell_id,data_flags,image_space_id)
                 VALUES(428953,3,191979),(86654,1,665223),(78792,129,367456);",
            )
            .unwrap();
        // Cell 428953: local XCLL, inherit 0. Cell 86654: local XCLL, inherit 0.
        // Cell 78792: LTMP 659862, inherit 159 (0x9f = 0x1|0x2|0x4|0x8|0x10|0x80): ambient,
        // directional colour, fog and the DALC come from the template; rotation and fade stay local.
        for row in [
            cell_row(
                (428953, 201355),
                [66, 58, 39],
                [44, 47, 48],
                (270, 240),
                [
                    [48, 39, 28],
                    [39, 39, 25],
                    [44, 40, 27],
                    [44, 37, 27],
                    [20, 18, 12],
                    [46, 41, 29],
                    [0, 0, 0],
                ],
                (5000.0, 6000.0),
                0,
            ),
            cell_row(
                (86654, 583566),
                [35, 61, 65],
                [44, 99, 105],
                (0, 90),
                [
                    [38, 61, 68],
                    [31, 61, 61],
                    [35, 63, 65],
                    [35, 58, 65],
                    [16, 29, 29],
                    [53, 92, 100],
                    [175, 211, 248],
                ],
                (5000.0, 6000.0),
                0,
            ),
            cell_row(
                (78792, 659862),
                [45, 48, 48],
                [35, 40, 50],
                (0, 30),
                [
                    [49, 48, 50],
                    [40, 48, 45],
                    [45, 50, 48],
                    [45, 45, 48],
                    [21, 22, 21],
                    [68, 73, 74],
                    [31, 52, 46],
                ],
                (0.0, 0.0),
                159,
            ),
        ] {
            connection.execute_batch(&row).unwrap();
        }
        connection
            .execute_batch(
                "INSERT INTO lighting_templates(id,editor_id,data_size,
                    ambient_r,ambient_g,ambient_b,directional_r,directional_g,directional_b,
                    directional_rotation_xy,directional_rotation_z,directional_fade,
                    dalc_x_pos_r,dalc_x_pos_g,dalc_x_pos_b,dalc_x_neg_r,dalc_x_neg_g,dalc_x_neg_b,
                    dalc_y_pos_r,dalc_y_pos_g,dalc_y_pos_b,dalc_y_neg_r,dalc_y_neg_g,dalc_y_neg_b,
                    dalc_z_pos_r,dalc_z_pos_g,dalc_z_pos_b,dalc_z_neg_r,dalc_z_neg_g,dalc_z_neg_b,
                    dalc_specular_r,dalc_specular_g,dalc_specular_b,dalc_scale,
                    light_fade_begin,light_fade_end,
                    subrecord_dalc_x_pos_r,subrecord_dalc_x_pos_g,subrecord_dalc_x_pos_b,
                    subrecord_dalc_x_neg_r,subrecord_dalc_x_neg_g,subrecord_dalc_x_neg_b,
                    subrecord_dalc_y_pos_r,subrecord_dalc_y_pos_g,subrecord_dalc_y_pos_b,
                    subrecord_dalc_y_neg_r,subrecord_dalc_y_neg_g,subrecord_dalc_y_neg_b,
                    subrecord_dalc_z_pos_r,subrecord_dalc_z_pos_g,subrecord_dalc_z_pos_b,
                    subrecord_dalc_z_neg_r,subrecord_dalc_z_neg_g,subrecord_dalc_z_neg_b,
                    subrecord_dalc_specular_r,subrecord_dalc_specular_g,subrecord_dalc_specular_b,
                    subrecord_dalc_scale)
                 VALUES(659862,'FarmLightingTemplate',92,45,48,48,65,70,80,0,45,1.0,
                    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,31,52,46,0.0,4000.0,5000.0,
                    49,48,50,40,48,45,45,50,48,45,45,48,21,22,21,68,73,74,0,0,0,1.0);",
            )
            .unwrap();
        EnvironmentCatalog::from_connection(&connection)
    }

    fn spawn_sun(app: &mut App, exterior: Quat) -> Entity {
        app.world_mut()
            .spawn((
                DirectionalLight::default(),
                WorldSun,
                Transform::from_rotation(exterior),
            ))
            .id()
    }

    fn light_direction(app: &App, sun: Entity) -> Vec3 {
        app.world().get::<Transform>(sun).unwrap().rotation * Vec3::NEG_Z
    }

    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1.0e-6,
                "expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// Expected values are bytes-space literals worked out by hand. `/ 255.0` is the
    /// q5-interior.md "bytes x 1/255" rule, written here independently of the production code.
    struct PackExpectation {
        ambient_bytes: [f32; 3],
        directional_bytes: [f32; 3],
        mean_bytes: [f32; 3],
        half_x_bytes: [f32; 3],
        half_y_bytes: [f32; 3],
        half_z_bytes: [f32; 3],
        specular_bytes: [f32; 3],
        fade_squared: (f32, f32),
        engine_direction: Vec3,
    }

    fn check_pack_cell(cell: u32, expected: PackExpectation) {
        let PackExpectation {
            ambient_bytes,
            directional_bytes,
            mean_bytes,
            half_x_bytes,
            half_y_bytes,
            half_z_bytes,
            specular_bytes,
            fade_squared,
            engine_direction,
        } = expected;
        let unit = |bytes: [f32; 3]| bytes.map(|byte| byte / 255.0);
        let exterior = Quat::from_rotation_y(0.4);
        let mut app = app(pack_catalog());
        let sun = spawn_sun(&mut app, exterior);
        enter(&mut app, Some(cell));
        let interior = selected(&app);
        assert_close(&interior.ambient_color.unwrap(), &unit(ambient_bytes));
        assert_close(
            &interior.directional_color.unwrap(),
            &unit(directional_bytes),
        );
        let cube = interior.ambient_cube.unwrap();
        assert_close(&cube.mean, &unit(mean_bytes));
        assert_close(&cube.half_difference_x, &unit(half_x_bytes));
        assert_close(&cube.half_difference_y, &unit(half_y_bytes));
        assert_close(&cube.half_difference_z, &unit(half_z_bytes));
        assert_close(&cube.specular.unwrap(), &unit(specular_bytes));
        assert_eq!(cube.scale, Some(1.0));
        let fade = interior.light_fade.unwrap();
        assert_eq!(
            (fade.begin_squared, fade.end_squared),
            fade_squared,
            "fade squares"
        );
        // Applied: the sun direction only.
        let direction = light_direction(&app, sun);
        assert!(
            direction.distance(engine_direction) < 1.0e-5,
            "cell {cell}: expected {engine_direction:?}, got {direction:?}"
        );
        // Leaving the interior restores the exterior rotation.
        enter(&mut app, None);
        assert_eq!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            exterior
        );
    }

    #[test]
    fn pack_cell_428953_reports_cube_and_fade_and_turns_the_sun() {
        // Sums per channel: R 48+39+44+44+20+46=241, G 39+39+40+37+18+41=214,
        // B 28+25+27+27+12+29=148. Half-differences: x (39-48, 39-39, 25-28)/2,
        // y (44-44, 37-40, 27-27)/2, z (46-20, 41-18, 29-12)/2.
        // Direction: a=270, b=240. Creation d = (cos a cos b, sin a cos b, -sin b)
        // = (0*(-0.5), (-1)*(-0.5), -(-0.8660254)) = (0, 0.5, 0.8660254).
        // Engine (x, z, -y) = (0, 0.8660254, -0.5). Fade: 5000^2=25000000, 6000^2=36000000.
        check_pack_cell(
            428953,
            PackExpectation {
                ambient_bytes: [66.0, 58.0, 39.0],
                directional_bytes: [44.0, 47.0, 48.0],
                mean_bytes: [241.0 / 6.0, 214.0 / 6.0, 148.0 / 6.0],
                half_x_bytes: [-4.5, 0.0, -1.5],
                half_y_bytes: [0.0, -1.5, 0.0],
                half_z_bytes: [13.0, 11.5, 8.5],
                specular_bytes: [0.0, 0.0, 0.0],
                fade_squared: (25_000_000.0, 36_000_000.0),
                engine_direction: Vec3::new(0.0, 0.866_025_4, -0.5),
            },
        );
    }

    #[test]
    fn pack_cell_86654_reports_cube_and_fade_and_turns_the_sun_straight_down() {
        // Sums: R 38+31+35+35+16+53=208, G 61+61+63+58+29+92=364, B 68+61+65+65+29+100=388.
        // x (31-38, 61-61, 61-68)/2, y (35-35, 58-63, 65-65)/2, z (53-16, 92-29, 100-29)/2.
        // Direction: a=0, b=90. d = (1*0, 0*0, -1) = (0, 0, -1). Engine (x, z, -y) = (0, -1, 0).
        // The direction is parallel to the engine up axis.
        check_pack_cell(
            86654,
            PackExpectation {
                ambient_bytes: [35.0, 61.0, 65.0],
                directional_bytes: [44.0, 99.0, 105.0],
                mean_bytes: [208.0 / 6.0, 364.0 / 6.0, 388.0 / 6.0],
                half_x_bytes: [-3.5, 0.0, -3.5],
                half_y_bytes: [0.0, -2.5, 0.0],
                half_z_bytes: [18.5, 31.5, 35.5],
                specular_bytes: [175.0, 211.0, 248.0],
                fade_squared: (25_000_000.0, 36_000_000.0),
                engine_direction: Vec3::new(0.0, -1.0, 0.0),
            },
        );
    }

    #[test]
    fn pack_cell_78792_reports_template_values_and_local_rotation() {
        // Inherit 159: ambient 45,48,48 and directional 65,70,80 come from LGTM 659862, and so
        // does the separate DALC subrecord (specular 0,0,0, scale 1.0). Rotation (0, 30) and
        // the light fade (0, 0) stay local. Sums: R 49+40+45+45+21+68=268,
        // G 48+48+50+45+22+73=286, B 50+45+48+48+21+74=286. x (40-49, 48-48, 45-50)/2,
        // y (45-45, 45-50, 48-48)/2, z (68-21, 73-22, 74-21)/2.
        // Direction: a=0, b=30. d = (cos30, 0, -sin30) = (0.8660254, 0, -0.5).
        // Engine (x, z, -y) = (0.8660254, -0.5, 0).
        check_pack_cell(
            78792,
            PackExpectation {
                ambient_bytes: [45.0, 48.0, 48.0],
                directional_bytes: [65.0, 70.0, 80.0],
                mean_bytes: [268.0 / 6.0, 286.0 / 6.0, 286.0 / 6.0],
                half_x_bytes: [-4.5, 0.0, -2.5],
                half_y_bytes: [0.0, -2.5, 0.0],
                half_z_bytes: [23.5, 25.5, 26.5],
                specular_bytes: [0.0, 0.0, 0.0],
                fade_squared: (0.0, 0.0),
                engine_direction: Vec3::new(0.866_025_4, -0.5, 0.0),
            },
        );
    }

    #[test]
    fn ambient_at_plus_axis_leans_to_the_odd_record_colour() {
        // s3-image-space.md Q-B: record order, no negation, no clamp. Cell 428953 record colours
        // c0..c5 = (48,39,28) (39,39,25) (44,40,27) (44,37,27) (20,18,12) (46,41,29).
        // Means in bytes: 241/6, 214/6, 148/6.
        // ambient(+X) = mean + (c1 - c0)/2: R 241/6 + (39-48)/2, G 214/6 + (39-39)/2,
        //   B 148/6 + (25-28)/2.
        // ambient(+Y) = mean + (c3 - c2)/2: R 241/6 + (44-44)/2, G 214/6 + (37-40)/2,
        //   B 148/6 + (27-27)/2.
        // ambient(+Z) = mean + (c5 - c4)/2: R 241/6 + (46-20)/2, G 214/6 + (41-18)/2,
        //   B 148/6 + (29-12)/2.
        let mut app = app(pack_catalog());
        enter(&mut app, Some(428953));
        let cube = selected(&app).ambient_cube.unwrap();
        let bytes = |normal: Vec3| cube.ambient_at(normal).map(|value| value * 255.0);
        let close = |actual: [f32; 3], expected: [f32; 3]| {
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!((actual - expected).abs() < 1.0e-4, "{actual} vs {expected}");
            }
        };
        close(bytes(Vec3::ZERO), [241.0 / 6.0, 214.0 / 6.0, 148.0 / 6.0]);
        close(
            bytes(Vec3::X),
            [
                241.0 / 6.0 + (39.0 - 48.0) / 2.0,
                214.0 / 6.0 + (39.0 - 39.0) / 2.0,
                148.0 / 6.0 + (25.0 - 28.0) / 2.0,
            ],
        );
        close(
            bytes(Vec3::Y),
            [
                241.0 / 6.0 + (44.0 - 44.0) / 2.0,
                214.0 / 6.0 + (37.0 - 40.0) / 2.0,
                148.0 / 6.0 + (27.0 - 27.0) / 2.0,
            ],
        );
        close(
            bytes(Vec3::Z),
            [
                241.0 / 6.0 + (46.0 - 20.0) / 2.0,
                214.0 / 6.0 + (41.0 - 18.0) / 2.0,
                148.0 / 6.0 + (29.0 - 12.0) / 2.0,
            ],
        );
    }

    fn image_space(app: &App) -> Option<ImageSpaceSelection> {
        app.world().resource::<SceneEnvironment>().image_space
    }

    #[test]
    fn interior_with_xcim_reports_the_xcim_form() {
        // s3-image-space.md "Cell image space for interiors (XCIM)". Pack rows of
        // cell_environment: cell 428953 data_flags 3, image_space_id 191979 (3 & 0x101 == 1);
        // cell 78792 data_flags 129, image_space_id 367456 (129 & 0x101 == 1).
        let mut app = app(pack_catalog());
        for (cell, form) in [(428953, 191979), (78792, 367456), (86654, 665223)] {
            enter(&mut app, Some(cell));
            assert_eq!(
                image_space(&app),
                Some(ImageSpaceSelection {
                    source: ImageSpaceSource::Xcim,
                    form_id: Some(form),
                })
            );
        }
    }

    #[test]
    fn interior_using_sky_lighting_reports_the_weather_global_block() {
        // Fixture cell 12: data_flags 257 (0x101). 257 & 0x101 == 0x101, not 0x001.
        // Fixture cell 13: data_flags 385 (0x181), 385 & 0x101 == 0x101.
        let mut app = app(fixture());
        for cell in [12, 13] {
            enter(&mut app, Some(cell));
            assert_eq!(
                image_space(&app),
                Some(ImageSpaceSelection {
                    source: ImageSpaceSource::WeatherGlobal,
                    form_id: None,
                })
            );
        }
        // An exterior also reports the weather global block.
        enter(&mut app, None);
        assert_eq!(
            image_space(&app).map(|selection| selection.source),
            Some(ImageSpaceSource::WeatherGlobal)
        );
    }

    #[test]
    fn interior_without_xcim_reports_default_image_space_0x167() {
        // Fixture cells 10 and 11: data_flags 1 and 129, no image_space_id (NULL).
        let mut app = app(fixture());
        for cell in [10, 11] {
            enter(&mut app, Some(cell));
            assert_eq!(
                image_space(&app),
                Some(ImageSpaceSelection {
                    source: ImageSpaceSource::DefaultImageSpace,
                    form_id: Some(0x167),
                })
            );
        }
    }

    #[test]
    fn a_database_without_the_image_space_column_reports_it_missing() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cell_environment(cell_id INTEGER PRIMARY KEY, data_flags INTEGER);
                 INSERT INTO cell_environment VALUES(10,1);",
            )
            .unwrap();
        let catalog = EnvironmentCatalog::from_connection(&connection);
        assert_eq!(catalog.cell_flags(10), Some(1));
        let mut app = app(catalog);
        enter(&mut app, Some(10));
        assert_eq!(image_space(&app), None);
        let value = serde_json::to_value(app.world().resource::<SceneEnvironment>()).unwrap();
        assert!(value["image_space"].is_null());
    }

    #[test]
    fn dalc_scale_reports_the_larger_of_scale_and_one() {
        let report = |scale: Option<f32>| {
            AmbientCube::from_dalc(&DirectionalAmbient {
                colors: [[0; 3]; 6],
                specular: None,
                scale,
            })
            .scale
        };
        assert_eq!(report(Some(0.5)), Some(1.0));
        assert_eq!(report(Some(2.0)), Some(2.0));
        assert_eq!(report(Some(1.0)), Some(1.0));
        // A missing scale stays missing.
        assert_eq!(report(None), None);
    }

    #[test]
    fn point_light_fade_factor_follows_the_squared_distance_ramp() {
        // begin 5000, end 6000 (cells 428953 and 86654): begin^2 = 25000000, end^2 = 36000000.
        let fade = LightFade::new(5000.0, 6000.0);
        assert_eq!((fade.begin_squared, fade.end_squared), (25.0e6, 36.0e6));
        // 24999998 is the f32 below begin^2 (f32 spacing is 2 at this size).
        assert_eq!(point_light_fade_factor(24_999_998.0, &fade), 1.0);
        assert_eq!(point_light_fade_factor(-1.0, &fade), 1.0);
        assert_eq!(point_light_fade_factor(25.0e6, &fade), 1.0);
        // Midpoint (25000000 + 36000000) / 2 = 30500000: 1 - 5500000 / 11000000 = 0.5.
        assert_eq!(point_light_fade_factor(30.5e6, &fade), 0.5);
        assert_eq!(point_light_fade_factor(36.0e6, &fade), 0.0);
        assert_eq!(point_light_fade_factor(40.0e6, &fade), 0.0);
    }

    #[test]
    fn a_missing_rotation_field_leaves_the_sun_unchanged() {
        let exterior = Quat::from_rotation_y(0.4);
        let mut catalog = fixture();
        {
            let cell = catalog.cells.get_mut(&10).unwrap();
            cell.inherit_flags = Some(0);
            cell.lighting.directional_rotation_z = None;
        }
        let mut app = app(catalog);
        let sun = spawn_sun(&mut app, exterior);
        enter(&mut app, Some(10));
        assert!(selected(&app).missing.contains(&"directional_rotation_z"));
        assert_eq!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            exterior
        );
        assert!(app.world().get::<ExteriorSunRotation>(sun).is_none());
    }

    #[test]
    fn leaving_the_interior_restores_the_exterior_sun_rotation() {
        let exterior = Quat::from_rotation_y(0.4);
        let mut app = app(pack_catalog());
        let sun = spawn_sun(&mut app, exterior);
        enter(&mut app, Some(428953));
        assert_ne!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            exterior
        );
        // Moving to a second interior keeps the saved exterior rotation.
        enter(&mut app, Some(86654));
        assert_ne!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            exterior
        );
        enter(&mut app, None);
        assert_eq!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            exterior
        );
        app.update();
        assert!(app.world().get::<ExteriorSunRotation>(sun).is_none());
        // A cell with no lighting row selects no rotation: it also restores the exterior.
        enter(&mut app, Some(428953));
        enter(&mut app, Some(404));
        assert_eq!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            exterior
        );
    }

    #[test]
    fn missing_reported_values_serialize_as_null() {
        let mut app = app(pack_catalog());
        enter(&mut app, Some(404));
        let value = serde_json::to_value(selected(&app)).unwrap();
        for key in [
            "ambient_color",
            "directional_color",
            "ambient_cube",
            "light_fade",
        ] {
            assert!(value[key].is_null(), "{key}: {}", value[key]);
        }
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
    }

    fn record(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
        [
            tag.as_slice(),
            &(payload.len() as u32).to_le_bytes(),
            &0u32.to_le_bytes(),
            &id.to_le_bytes(),
            &[0; 4],
            &44u16.to_le_bytes(),
            &[0; 2],
            payload,
        ]
        .concat()
    }

    fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
        [
            b"GRUP".as_slice(),
            &(24 + payload.len() as u32).to_le_bytes(),
            &label.to_le_bytes(),
            &kind.to_le_bytes(),
            &[0; 8],
            payload,
        ]
        .concat()
    }

    fn header(master: bool) -> Vec<u8> {
        let mut payload = sub(
            b"HEDR",
            &[1.7f32.to_le_bytes(), [0; 4], 0x800u32.to_le_bytes()].concat(),
        );
        if !master {
            payload.extend(sub(b"MAST", b"Base.esm\0"));
            payload.extend(sub(b"DATA", &[0; 8]));
        }
        let mut result = record(b"TES4", 0, &payload);
        result[8..12].copy_from_slice(&u32::from(master).to_le_bytes());
        result
    }

    fn selected(app: &App) -> &InteriorLighting {
        app.world()
            .resource::<SceneEnvironment>()
            .interior
            .as_ref()
            .unwrap()
    }

    #[test]
    fn procedural_plugin_override_reaches_the_scene_environment() {
        // A new cell/template pair, with an unrelated plugin between master and override.
        // Both the identity remap and the override winner go through the actual converter.
        let root = tempfile::tempdir().unwrap();
        let paths = ["Base.esm", "Other.esm", "Override.esp"].map(|name| root.path().join(name));
        let mut data = [0u8; 92];
        data[0..3].copy_from_slice(&[32, 48, 64]);
        data[4..7].copy_from_slice(&[64, 96, 128]);
        data[28..32].copy_from_slice(&0.5f32.to_le_bytes());
        let template_record = |data: &[u8]| {
            group(
                0,
                u32::from_le_bytes(*b"LGTM"),
                &record(
                    b"LGTM",
                    0x800,
                    &[sub(b"EDID", b"NewTemplate\0"), sub(b"DATA", data)].concat(),
                ),
            )
        };
        let mut cell_data = data;
        cell_data[88..92].copy_from_slice(&0x43u32.to_le_bytes());
        let cell = record(
            b"CELL",
            0x801,
            &[
                sub(b"EDID", b"NewInterior\0"),
                sub(b"DATA", &129u16.to_le_bytes()),
                sub(b"LTMP", &0x800u32.to_le_bytes()),
                sub(b"XCLL", &cell_data),
            ]
            .concat(),
        );
        let cell = group(
            0,
            u32::from_le_bytes(*b"CELL"),
            &group(2, 0, &group(3, 0, &cell)),
        );
        std::fs::write(
            &paths[0],
            [header(true), template_record(&data), cell].concat(),
        )
        .unwrap();
        std::fs::write(&paths[1], header(true)).unwrap();
        for (index, color) in [[64, 96, 128], [0, 0, 0], [255, 32, 16]]
            .into_iter()
            .enumerate()
        {
            data[4..7].copy_from_slice(&color);
            // This plugin has one master, but loads at index 2. Its override references index 0.
            std::fs::write(&paths[2], [header(false), template_record(&data)].concat()).unwrap();
            let database = root.path().join(format!("case-{index}.db"));
            converter::esm::EsmParser::convert_plugins(&paths, &database).unwrap();
            let catalog = EnvironmentCatalog::open(&database).unwrap();
            let resolved = catalog.resolve_interior(0x801);
            assert_eq!(resolved.lighting.directional, Some(color));
            assert_eq!(
                resolved.sources["directional"],
                LightingSource::TemplateData(0x800)
            );
            let mut app = super::tests::app(catalog);
            super::tests::enter(&mut app, Some(0x801));
            assert_eq!(selected(&app).lighting.directional, Some(color));
            // The override changes the directional colour only. Authored ambient is unchanged.
            assert_eq!(selected(&app).lighting.ambient, Some([32, 48, 64]));
            assert_eq!(selected(&app).lighting.directional_fade, Some(0.5));
            assert!(app.world().resource::<SceneEnvironment>().show_sky);
            assert!(!app.world().resource::<SceneEnvironment>().use_sky_lighting);
        }
    }

    #[test]
    fn absent_and_malformed_xcll_do_not_select_the_same_template_lighting() {
        use converter::config::RecordReader;

        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("Base.esm");
            let mut data = [0u8; 92];
            data[0..3].copy_from_slice(&[255, 0, 0]);
            data[4..7].copy_from_slice(&[64, 96, 128]);
            data[28..32].copy_from_slice(&0.5f32.to_le_bytes());
            for face in data[40..64].as_chunks_mut::<4>().0 {
                face[..3].copy_from_slice(&[255, 0, 0]);
            }
            let template = group(
                0,
                u32::from_le_bytes(*b"LGTM"),
                &record(b"LGTM", 0x800, &sub(b"DATA", &data)),
            );
            let mut cells = Vec::new();
            for (index, size) in [None, Some(0), Some(71), Some(72), Some(92)]
                .into_iter()
                .enumerate()
            {
                let mut payload = [
                    sub(b"DATA", &1u16.to_le_bytes()),
                    sub(b"LTMP", &0x800u32.to_le_bytes()),
                ]
                .concat();
                if let Some(size) = size {
                    payload.extend(sub(b"XCLL", &data[..size]));
                }
                cells.extend(record(b"CELL", 0x801 + index as u32, &payload));
            }
            cells.extend(record(
                b"CELL",
                0x806,
                &[sub(b"DATA", &[1, 0]), sub(b"XCLL", &data[..71])].concat(),
            ));
            let cells = group(
                0,
                u32::from_le_bytes(*b"CELL"),
                &group(2, 0, &group(3, 0, &cells)),
            );
            std::fs::write(&path, [header(true), template, cells].concat()).unwrap();
            let database = root.path().join("world.db");
            converter::esm::EsmParser::convert_plugins_with_reader(
                &[path],
                &database,
                reader,
                root.path(),
            )
            .unwrap();
            let catalog = EnvironmentCatalog::open(&database).unwrap();
            for (index, size) in [None, Some(0), Some(71), Some(72), Some(92)]
                .into_iter()
                .enumerate()
            {
                let id = 0x801 + index as u32;
                assert_eq!(catalog.cell_lighting(id).unwrap().xcll_size, size);
                assert_eq!(catalog.resolve_interior(id).template_only, size.is_none());
            }
            assert_eq!(catalog.cell_lighting(0x806).unwrap().xcll_size, Some(71));
            let mut app = super::tests::app(catalog);
            for cell in [0x801, 0x802, 0x803, 0x804, 0x805, 0x806, 0x801] {
                super::tests::enter(&mut app, Some(cell));
                let resolved = selected(&app);
                let malformed = matches!(cell, 0x802 | 0x803 | 0x806);
                assert_eq!(
                    resolved.sources["directional"],
                    if cell == 0x801 {
                        LightingSource::TemplateData(0x800)
                    } else if malformed {
                        LightingSource::MalformedCellXcll(if cell == 0x802 { 0 } else { 71 })
                    } else {
                        LightingSource::CellXcll
                    }
                );
                assert_eq!(resolved.missing.contains(&"directional"), malformed);
                assert_eq!(
                    resolved.lighting.directional,
                    (!malformed).then_some([64, 96, 128])
                );
                assert_eq!(
                    resolved.lighting.ambient,
                    (!malformed).then_some([255, 0, 0])
                );
            }
        }
    }

    #[test]
    fn procedural_dalc_overrides_reach_the_scene_environment_unchanged() {
        use converter::config::RecordReader;

        for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
            let root = tempfile::tempdir().unwrap();
            let paths = ["Base.esm", "Override.esp"].map(|name| root.path().join(name));
            let mut data = [0u8; 92];
            data[0..3].copy_from_slice(&[255; 3]);
            data[4..7].copy_from_slice(&[40, 50, 60]);
            data[28..32].copy_from_slice(&0.5f32.to_le_bytes());
            let template = |cube: &[u8]| {
                group(
                    0,
                    u32::from_le_bytes(*b"LGTM"),
                    &record(
                        b"LGTM",
                        0x800,
                        &[sub(b"DATA", &data), sub(b"DALC", cube)].concat(),
                    ),
                )
            };
            let mut local = data;
            local[88..92].copy_from_slice(&0x43u32.to_le_bytes());
            let cell = record(
                b"CELL",
                0x801,
                &[
                    sub(b"DATA", &[1, 0]),
                    sub(b"LTMP", &0x800u32.to_le_bytes()),
                    sub(b"XCLL", &local),
                ]
                .concat(),
            );
            let cells = group(
                0,
                u32::from_le_bytes(*b"CELL"),
                &group(2, 0, &group(3, 0, &cell)),
            );
            std::fs::write(
                &paths[0],
                [header(true), template(&[0; 24]), cells].concat(),
            )
            .unwrap();
            let mut app = super::tests::app(EnvironmentCatalog::default());
            for (index, face) in [[0, 255, 0], [0, 0, 0], [255, 0, 0]]
                .into_iter()
                .enumerate()
            {
                let cube: Vec<_> = (0..6)
                    .flat_map(|_| [face[0], face[1], face[2], 0])
                    .collect();
                std::fs::write(&paths[1], [header(false), template(&cube)].concat()).unwrap();
                let database = root.path().join(format!("{index}.db"));
                converter::esm::EsmParser::convert_plugins_with_reader(
                    &paths,
                    &database,
                    reader,
                    root.path(),
                )
                .unwrap();
                app.insert_resource(EnvironmentCatalog::open(&database).unwrap());
                super::tests::enter(&mut app, Some(0x801));
                let resolved = selected(&app);
                assert_eq!(
                    resolved.sources["directional_ambient"],
                    LightingSource::TemplateDalc(0x800)
                );
                assert_eq!(
                    resolved.lighting.directional_ambient.unwrap().colors,
                    [face; 6]
                );
                assert_eq!(resolved.lighting.directional, Some([40, 50, 60]));
            }
        }
    }

    #[test]
    #[ignore = "opt-in read-only managed-pack check; set MUDCRAB_INTERIOR_TEST_DB"]
    fn managed_farm_cells_reach_the_scene_environment_without_modifying_pack() {
        let path =
            std::env::var_os("MUDCRAB_INTERIOR_TEST_DB").expect("explicit managed database path");
        let catalog = EnvironmentCatalog::open(std::path::Path::new(&path)).unwrap();
        let mut app = super::tests::app(catalog);
        for (cell, elevation) in [(78791, 0), (78792, 30)] {
            super::tests::enter(&mut app, Some(cell));
            let scene = app.world().resource::<SceneEnvironment>();
            let resolved = scene.interior.as_ref().unwrap();
            assert_eq!(resolved.template_id, Some(659862));
            assert_eq!(resolved.lighting.directional, Some([65, 70, 80]));
            assert_eq!(resolved.lighting.directional_rotation_z, Some(elevation));
            assert_eq!(resolved.lighting.directional_fade, Some(0.0));
            assert!(scene.show_sky);
            assert!(!scene.use_sky_lighting);
        }
    }
}
