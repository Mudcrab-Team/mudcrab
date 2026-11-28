//! Authored interior lighting: the retail sky mode rule and the cell key light, ambient and fog.
//!
//! Retail picks a sky mode from the current cell's flags (RF-LIGHT-CPU-010). Only mode 3 (an
//! exterior, or the one interior with flags 0x80 and 0x100) runs the weather sun path. Modes 1
//! and 2 rebuild the one directional light from the cell's lighting (RF-LIGHT-CPU-011,
//! RF-LIGHT-CPU-013): no sun colour, no sun strength, no time-of-day direction and no sun disc.
//! This module applies that rule to the engine's sun entity, `GlobalAmbientLight` and the
//! camera fog, and restores the exterior values on exit.
//! See docs/specs/engine/interior-environment.md for evidence and the open items.
//! Sources: registry rows RF-LIGHT-CPU-001 to RF-LIGHT-CPU-016, RF-LIGHT-GPU-001 to
//! RF-LIGHT-GPU-010, RF-IMGS-001 and RF-WATER-011 (scene fog form). The registry is a private
//! research note and is not in the tree; the spec says where each row matters.

use super::{DirectionalAmbient, EnvironmentCatalog, Lighting, RoomBound};
use crate::room_culling::{InteriorRoot, PointRooms, RoomCullingNear, RoomCullingState};
use crate::streaming::{ActiveSpace, creation_rotation_to_bevy, creation_to_bevy};
use crate::world::components::StreamingCamera;
use bevy::{
    ecs::system::SystemParam,
    pbr::{DirectionalAmbientCube, DistanceFog, FogFalloff},
    prelude::*,
    transform::TransformSystems,
};
use std::collections::BTreeMap;

/// CELL DATA flag bits read by the sky mode selection (RF-LIGHT-CPU-010, `0x1401a3f40`).
const CELL_INTERIOR: u32 = 0x1;
const SHOW_SKY: u32 = 0x80;
const USE_SKY_LIGHTING: u32 = 0x100;

/// The retail sky mode (`Sky+0x1BC`, RF-LIGHT-CPU-010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkyMode {
    /// Mode 1: an interior without Show Sky. Cell lighting only.
    CellLighting,
    /// Mode 2: an interior with Show Sky and without Use Sky Lighting. Retail runs the weather
    /// colour pass, then the cell lighting overwrites it (RF-LIGHT-CPU-013). The sun light is
    /// still the cell's (RF-LIGHT-CPU-011).
    CellLightingWithSky,
    /// Mode 3: an exterior, no cell, or an interior with Show Sky and Use Sky Lighting. Only
    /// `0xF2CAB LabyrinthianMaze` is such an interior in the shipped data (RF-LIGHT-CPU-015).
    #[default]
    WeatherSun,
}

impl SkyMode {
    /// RF-LIGHT-CPU-010: no cell or an exterior gives 3; an interior without 0x80 gives 1; 0x80
    /// without 0x100 gives 2; 0x80 and 0x100 give 3. 0x100 without 0x80 gives 1. The worldspace
    /// template override of mode 3 (WRLD DATA 0x20, Blackreach) is not implemented here.
    pub fn from_cell_flags(flags: Option<u32>) -> Self {
        let Some(flags) = flags.filter(|flags| flags & CELL_INTERIOR != 0) else {
            return Self::WeatherSun;
        };
        match (flags & SHOW_SKY != 0, flags & USE_SKY_LIGHTING != 0) {
            (false, _) => Self::CellLighting,
            (true, false) => Self::CellLightingWithSky,
            (true, true) => Self::WeatherSun,
        }
    }

    /// Modes 1 and 2 take the interior branch of `Sun::Update` (RF-LIGHT-CPU-011).
    pub fn uses_cell_lighting(self) -> bool {
        self != Self::WeatherSun
    }
}

/// RF-IMGS-001 (high for the getter, `0x1402bf340`; s3-image-space.md "Cell image space for
/// interiors (XCIM)"): an interior without the use-sky-lighting flag,
/// `CELL DATA flags & 0x101 == 0x001`, takes its image space from the cell extra `0x56` (XCIM).
/// The same getter returns form 0x167 `DefaultImageSpace` when XCIM is absent. Otherwise the
/// weather block applies. RF-IMGS-001 also names a reference image space override (extra
/// `0x77`, 3 s smoothstep blend, medium). That override is not implemented here.
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
    /// RF-IMGS-001 (s3-image-space.md "Cell image space for interiors (XCIM)"). `xcim` is `None`
    /// when the database has no value for the cell, `Some(None)` when the cell has no XCIM. Missing data
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

// XCLL inheritance bits. A set bit takes the template value when the cell has a template
// (RF-LIGHT-CPU-014, getters `0x1402bbe40`-`0x1402bc790`; extends RF-LIGHT-CPU-001/009).
const INHERIT_AMBIENT_AND_DALC: u32 = 0x1;
const INHERIT_DIRECTIONAL_COLOR: u32 = 0x2;
const INHERIT_FOG_COLORS: u32 = 0x4;
const INHERIT_FOG_NEAR: u32 = 0x8;
const INHERIT_FOG_FAR: u32 = 0x10;
const INHERIT_ROTATION: u32 = 0x20;
const INHERIT_DIRECTIONAL_FADE: u32 = 0x40;
const INHERIT_FOG_CLIP: u32 = 0x80;
const INHERIT_FOG_POWER: u32 = 0x100;
const INHERIT_FOG_MAX: u32 = 0x200;
const INHERIT_LIGHT_FADE: u32 = 0x400;

/// Colour bytes are RGBA8 x 1/255 (q5-interior.md "Inheritance": "Colours are RGBA8 bytes x
/// 1/255", high). This is the only conversion applied. It is a report, not a render value.
const BYTE_TO_UNIT: f32 = 1.0 / 255.0;

fn unit_rgb(bytes: [u8; 3]) -> [f32; 3] {
    bytes.map(|byte| f32::from(byte) * BYTE_TO_UNIT)
}

/// The ambient cube the retail Sky update builds from the six DALC colours (RF-LIGHT-CPU-002,
/// function 0x1414ec3b0, evaluated at 0x1414ec730). RF-LIGHT-GPU-001 (high): this DALC cube is
/// the only ambient input of the retail Lighting shader, `DirectionalAmbient (cb2[11..13]) . (N,1)`
/// per channel with no clamp. Per channel, with c[0..5] the six colours in
/// record order (bytes x 1/255):
/// `mean = (c0+c1+c2+c3+c4+c5)/6`, `half_difference_x = (c1-c0)/2`,
/// `half_difference_y = (c3-c2)/2`, `half_difference_z = (c5-c4)/2`.
/// The specular colour passes through (x 1/255). `scale = max(DALC scale, 1.0)`.
/// Confidence: high. Face mapping (RF-LIGHT-GPU-003, high arithmetic, medium-high world space):
/// record order, no negation, no clamp; a +X normal leans to record colour 1, +Y to colour 3,
/// +Z to colour 5 (xEdit "X-" is record colour 1's face label; xEdit "X+" is light travelling
/// toward +X). Registry row RF-LIGHT-CPU-002 still says "sign unverified"; RF-LIGHT-GPU-003
/// settles it. This value is reported only. No ambient light
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
    /// Face mapping (RF-LIGHT-GPU-003): record order, no negation, no clamp; a +X normal leans to
    /// record colour 1, +Y to colour 3, +Z to colour 5 (xEdit "X+" is light travelling toward
    /// +X). The cube is the only ambient input of the Lighting shader (RF-LIGHT-GPU-001).
    /// `normal` is the world-space normal (RF-LIGHT-GPU-003, medium-high) in Creation axes.
    /// Reported only: nothing applies this to `GlobalAmbientLight` or a shader.
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
/// `Lighting::light_fade_begin` and `light_fade_end`. The distance dimmer uses the pair after
/// [`resolve_light_fade`] replaces zero values. Retail applies the fade only to a light whose
/// BSLight `+0x63` flag is set (RF-LIGHT-CPU-007, see [`point_light_fade_factor`]).
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

/// The light fade begin and end that the distance dimmer uses (NEEDS 20, skyrim_decomp PR 1073).
///
/// `stored` is the pair a room template or a cell carries, or `None` for the exterior. A stored
/// begin of exactly 0.0 is replaced by `start_fade` (`fLightLODStartFade`), and a stored end of
/// exactly 0.0 by `range + start_fade` (`fLightLODRange + fLightLODStartFade`). The exterior, with
/// no override, uses `start_fade` and `start_fade + range`. A stored nonzero begin with a zero end
/// therefore gets `range + start_fade`, which can be below the begin. The dimmer is then a step:
/// 1 below `begin`, 0 at and beyond it (no ramp). What retail does for that pair is unverified;
/// this follows the traced formula. The same replacement is applied to a room template's pair: the
/// answer states it only for the cell initialiser, so that part is unverified.
pub fn resolve_light_fade(stored: Option<(f32, f32)>, start_fade: f32, range: f32) -> LightFade {
    let (begin, end) = stored.unwrap_or((start_fade, range + start_fade));
    LightFade::new(
        if begin == 0.0 { start_fade } else { begin },
        if end == 0.0 { range + start_fade } else { end },
    )
}

/// The retail point-light fade factor (RF-LIGHT-CPU-003, function 0x141575d20) with
/// `q = |camera - light|^2 - r^2` (r: the light term at the same site): 1 if `q < begin^2`;
/// `1 - (q - begin^2) / (end^2 - begin^2)` up to `end^2`; 0 (light off) beyond.
/// Retail applies it only to a light whose per-light fade flag is set. That flag is BSLight
/// `+0x63` (RF-LIGHT-CPU-007, high). It is set from the creation parameter byte 5, which is NOT
/// (LIGH reference has REFR flag 0x00010000, "Never Fades"), and defaults to 1.
/// An `end^2` of exactly 0 gives 1 (NEEDS 20). `crate::lights` applies this to every placed light
/// whose reference does not carry "Never Fades".
pub fn point_light_fade_factor(q: f32, fade: &LightFade) -> f32 {
    if q < fade.begin_squared || fade.end_squared == 0.0 {
        1.0
    } else if q < fade.end_squared {
        1.0 - (q - fade.begin_squared) / (fade.end_squared - fade.begin_squared)
    } else {
        0.0
    }
}

/// The direction the interior key light travels, in engine (Bevy) axes (RF-LIGHT-CPU-012,
/// supersedes RF-LIGHT-CPU-004). Retail converts both angles as unsigned 32-bit integers:
/// `a = (float)(uint32) rotation XY` and `b = (float)(uint32) rotation Z`, in degrees. The light
/// rotation is `A x B` and the light travels along its world rotation column 0, which in Creation
/// axes is `(cos a cos b, -sin a cos b, sin b)`. `creation_to_bevy` `(x, y, z) -> (x, z, -y)` is
/// a proper rotation, so the engine travel is `(cos a cos b, sin b, sin a cos b)`. The world sign
/// assumes an identity parent rotation (RF-LIGHT-CPU-012, medium-high). No shipped cell has a
/// negative angle or one of 360 or more (RF-LIGHT-CPU-015).
pub(crate) fn interior_key_travel(rotation_xy_degrees: i32, rotation_z_degrees: i32) -> Vec3 {
    let (sin_a, cos_a) = (rotation_xy_degrees as u32 as f32).to_radians().sin_cos();
    let (sin_b, cos_b) = (rotation_z_degrees as u32 as f32).to_radians().sin_cos();
    creation_to_bevy(Vec3::new(cos_a * cos_b, -sin_a * cos_b, sin_b))
}

/// The key light illuminance for a cell, in lux. Retail: `DirLightColor = colour x fade (1.0) x
/// SunlightScale` (RF-LIGHT-GPU-006, RF-LIGHT-CPU-011). The engine states one retail light unit
/// as `AMBIENT_ILLUMINANCE` in Bevy ambient units, and a directional term needs the extra `PI`
/// (`crate::lights`). The exterior sun is this same formula with `SUNLIGHT_SCALE`. This is the
/// engine's input scale, not a calibration. Alvor's house (SunlightScale 1.5): 6428.6 lux.
pub(crate) fn interior_key_illuminance(sunlight_scale: f32) -> f32 {
    core::f32::consts::PI * crate::lights::AMBIENT_ILLUMINANCE * sunlight_scale
}

/// The streamed world's sun: the one directional light the interior lighting drives.
#[derive(Component, Debug, Default, Clone, Copy)]
pub(crate) struct WorldSun;

/// The exterior sun values, saved while sky mode 1 or 2 drives the sun. `crate::sun::update_sun`
/// skips a sun with this component, so the clock does not turn the interior key light.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct ExteriorSunState {
    rotation: Quat,
    color: Color,
    illuminance: f32,
    shadow_maps_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum LightingSource {
    CellXcll,
    TemplateData(u32),
    TemplateDalc(u32),
    /// The inherit bit is set, but the template is absent. Retail reads the cell value
    /// (RF-LIGHT-CPU-014: a set bit takes the template value "when the cell has one").
    MissingTemplate(Option<u32>),
    /// The cell has no XCLL. Every retail getter returns 0 (RF-LIGHT-CPU-014).
    NoXcll,
    MissingCell,
}

/// The resolved cell lighting. Values stay optional: authored black/zero never means missing.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct InteriorLighting {
    pub cell_id: u32,
    pub xcll_size: Option<u32>,
    pub template_id: Option<u32>,
    pub inherit_flags: Option<u32>,
    /// The cell has an XCLL block. Only presence is used (all shipped interiors have 92 bytes,
    /// RF-LIGHT-CPU-015).
    pub has_xcll: bool,
    pub lighting: Lighting,
    pub sources: BTreeMap<&'static str, LightingSource>,
    pub missing: Vec<&'static str>,
    /// Authored ambient colour, bytes x 1/255. XCLL ambient is not a Lighting-shader input
    /// (RF-LIGHT-GPU-002), so it is reported only.
    pub ambient_color: Option<[f32; 3]>,
    /// Authored directional colour, bytes x 1/255. Reported; [`InteriorRender`] applies it.
    pub directional_color: Option<[f32; 3]>,
    /// RF-LIGHT-CPU-002, reported. Its mean is applied (see [`InteriorRender::ambient_color`]).
    pub ambient_cube: Option<AmbientCube>,
    /// RF-LIGHT-CPU-001 and RF-LIGHT-CPU-003, reported only.
    pub light_fade: Option<LightFade>,
}

impl EnvironmentCatalog {
    /// Resolve the cell's lighting per field (RF-LIGHT-CPU-014): a set inherit bit takes the
    /// template value when the template exists, else the cell value. The template's DALC comes
    /// from its separate subrecord, not its DATA block. Without an XCLL no bit is read, and the
    /// fields stay missing (retail getters return 0).
    pub(crate) fn resolve_interior(&self, cell_id: u32) -> InteriorLighting {
        let cell = self.cell_lighting(cell_id);
        let template_id = cell.and_then(|cell| cell.lighting_template);
        let template = template_id.and_then(|id| self.lighting_template(id));
        let local = cell.map_or(Lighting::default(), |cell| cell.lighting);
        let inherited = template.map_or(Lighting::default(), |template| template.lighting);
        let inherit_flags = cell.and_then(|cell| cell.inherit_flags);
        let has_xcll = cell.is_some_and(|cell| cell.xcll_size.is_some());
        let flags = if has_xcll {
            inherit_flags.unwrap_or(0)
        } else {
            0
        };
        let mut result = InteriorLighting {
            cell_id,
            xcll_size: cell.and_then(|cell| cell.xcll_size),
            template_id,
            inherit_flags,
            has_xcll,
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
                let inherit = flags & $bit != 0;
                let (value, source) = match (inherit, template_id, template) {
                    (true, Some(id), Some(_)) => (
                        $template_value,
                        if $dalc {
                            LightingSource::TemplateDalc(id)
                        } else {
                            LightingSource::TemplateData(id)
                        },
                    ),
                    (true, _, _) => (local.$field, LightingSource::MissingTemplate(template_id)),
                    (false, _, _) if has_xcll => (local.$field, LightingSource::CellXcll),
                    (false, _, _) if cell.is_some() => (local.$field, LightingSource::NoXcll),
                    (false, _, _) => (local.$field, LightingSource::MissingCell),
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
        // RF-LIGHT-CPU-014: the template DALC is the subrecord at `LGTM+0x80`, not DATA.
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
        select!(directional_rotation_xy, INHERIT_ROTATION);
        select!(directional_rotation_z, INHERIT_ROTATION);
        // RF-LIGHT-CPU-008: the directional fade has no runtime reader. Reported, never applied.
        select!(directional_fade, INHERIT_DIRECTIONAL_FADE);
        select!(fog_clip_distance, INHERIT_FOG_CLIP);
        select!(fog_power, INHERIT_FOG_POWER);
        select!(fog_max, INHERIT_FOG_MAX);
        select!(light_fade_begin, INHERIT_LIGHT_FADE);
        select!(light_fade_end, INHERIT_LIGHT_FADE);
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

/// The fraction of the far distance that retail uses as the fog near distance when the authored
/// near is not usable: near <= 0 or near beyond far (`RF-FOG-001`, constant at `0x141869ddc`,
/// code `0x140413D7B`-`0x140413DB8`; `RF-LIGHT-CPU-023` for the room path `0x140416050`).
///
/// Skyrim authors near 0 for many interiors (84 in the pack's `cell_lighting` own near <= 0 with
/// far > 0, and template-inherited cells come on top), so a near of 0 never means "fog from the
/// camera": retail starts the ramp at 0.17 of the far distance. A near 0, far 5000 cell starts its
/// fog at 850.
const FOG_NEAR_CLAMP_FRACTION: f32 = 0.17;

/// The fog far distance retail uses when the authored far is <= 0 or above it (`RF-FOG-001`,
/// constant `0x141844d48`, 163,840). Only the room blend uses it: a side without a fog ramp stands
/// for it, since fog that starts at 27,853 units is not visible indoors.
const FOG_FAR_CLAMP: f32 = 163_840.0;

/// The linear blend of two colours given as bytes, rounded to a byte. Retail blends the colour
/// triples as floats (`0x140415D50`); a byte step is 1/255 and is not visible.
fn lerp_bytes(from: [u8; 3], to: [u8; 3], w: f32) -> [u8; 3] {
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * w).round() as u8;
    [
        channel(from[0], to[0]),
        channel(from[1], to[1]),
        channel(from[2], to[2]),
    ]
}

/// The cell fog the engine applies in sky modes 1 and 2. Retail scene fog (RF-WATER-011) is
/// `f = min(saturate((d - near) / (far - near))^power, max)` and `colour = lerp(near colour, far
/// colour, f)`, so `max` caps the factor. Bevy's linear fog (`fog.wgsl` `linear_fog`) is
/// `alpha * saturate((d - start) / (end - start))`: alpha multiplies the ramp. With `alpha = max`
/// and `end = near + max * (far - near)` it is `min(saturate((d - near) / (far - near)), max)`,
/// the retail factor for `power = 1`. Bevy has no power term and one colour, so `power` and
/// `far_color` are reported only.
/// The near distance is clamped as retail does ([`FOG_NEAR_CLAMP_FRACTION`]). Retail's far clamp
/// (far <= 0 or above 163,840 becomes 163,840, `RF-FOG-001`) is not applied: a far of 163,840 puts
/// the fog's near at 27,853 units, far past any interior, so "no fog" is the same picture.
/// Clip distance is reported in [`InteriorLighting`] only.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub(crate) struct CellFog {
    pub near_color: [u8; 3],
    pub far_color: [u8; 3],
    pub near: f32,
    pub far: f32,
    pub power: f32,
    pub max: f32,
}

impl CellFog {
    /// A missing field is 0, as the retail getter returns for a cell without XCLL
    /// (RF-LIGHT-CPU-014). Fog with `far <= 0` has no ramp worth drawing (retail's far clamp puts
    /// it at 163,840), and fog with an authored `max <= 0` has a factor of 0 everywhere, so the
    /// engine draws no fog for either. A near that is not usable (<= 0 or beyond far) becomes
    /// [`FOG_NEAR_CLAMP_FRACTION`] of far, as in retail (`RF-FOG-001`), so fog is drawn for those.
    /// A near equal to far is usable but leaves a zero-width ramp: retail draws no fog there
    /// (`RF-LIGHT-CPU-021`, the "no fog" constant block), so this returns `None`.
    ///
    /// A missing `max` with an authored near and far is different: it comes from a short XCLL
    /// (72 to 91 bytes, which ends before the max field) or a short template. What retail does
    /// with a short block is UNVERIFIED (CPU-014 covers a cell without XCLL only). Mudcrab
    /// treats the missing max as no cap (1.0) so that authored fog distances are not lost.
    /// No shipped interior has a short XCLL. Six of the 786 take fog near and far from a short
    /// (72-byte) lighting template without a max, and now get that fog with no cap.
    fn from_lighting(lighting: &Lighting) -> Option<Self> {
        let far = lighting.fog_far.unwrap_or(0.0);
        // retail-default: RF-LIGHT-CPU-014, the getter reads a missing field as 0.
        let authored_near = lighting.fog_near.unwrap_or(0.0);
        let near = if authored_near <= 0.0 || authored_near > far {
            FOG_NEAR_CLAMP_FRACTION * far
        } else {
            authored_near
        };
        let max = lighting.fog_max.unwrap_or(1.0);
        (far > 0.0 && near < far && max > 0.0).then(|| Self {
            near_color: lighting.fog_near_color.unwrap_or([0; 3]),
            far_color: lighting.fog_far_color.unwrap_or([0; 3]),
            near,
            far,
            power: lighting.fog_power.unwrap_or(0.0),
            max,
        })
    }

    /// The fog at weight `w` between `previous` and `current` (`RF-LIGHT-CPU-025`, `0x140416E30`):
    /// both colours, near, far, power and max blend linearly. A side with no fog ramp stands for
    /// retail's clamp of far <= 0 ([`FOG_FAR_CLAMP`], near 0.17 of it, `RF-FOG-001`), which is
    /// fog far beyond any interior, with the other side's colours, power and max; so fog fades in
    /// or out instead of switching. At full weight the result is `current` itself, with no fog if
    /// that is what it has.
    fn blend(previous: Option<Self>, current: Option<Self>, w: f32) -> Option<Self> {
        let (from, to) = match (previous, current) {
            (None, None) => return None,
            (Some(from), Some(to)) => (from, to),
            (Some(from), None) => (from, Self::without_ramp(&from)),
            (None, Some(to)) => (Self::without_ramp(&to), to),
        };
        if w >= 1.0 {
            return current;
        }
        let lerp = |a: f32, b: f32| a + (b - a) * w;
        Some(Self {
            near_color: lerp_bytes(from.near_color, to.near_color, w),
            far_color: lerp_bytes(from.far_color, to.far_color, w),
            near: lerp(from.near, to.near),
            far: lerp(from.far, to.far),
            power: lerp(from.power, to.power),
            max: lerp(from.max, to.max),
        })
    }

    /// The no-ramp side of a blend: retail's clamped far, with `other`'s colours, power and max.
    fn without_ramp(other: &Self) -> Self {
        Self {
            near: FOG_NEAR_CLAMP_FRACTION * FOG_FAR_CLAMP,
            far: FOG_FAR_CLAMP,
            ..*other
        }
    }

    pub(crate) fn distance_fog(&self) -> DistanceFog {
        let [r, g, b] = self.near_color;
        // The saturated ramp never exceeds 1, so a cap above 1 is no cap.
        let cap = self.max.clamp(0.0, 1.0);
        DistanceFog {
            color: Color::srgb_u8(r, g, b).with_alpha(cap),
            // Indoors there is no sun glow in the fog.
            directional_light_color: Color::NONE,
            falloff: FogFalloff::Linear {
                start: self.near,
                end: self.near + cap * (self.far - self.near),
            },
            ..default()
        }
    }
}

/// The values the engine applies in sky modes 1 and 2 (verify-report "The rule", steps 4-6).
/// A missing field applies as 0, as the retail getter returns (RF-LIGHT-CPU-014).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub(crate) struct InteriorRender {
    /// The directional colour, sRGB bytes. Applied as `Color::srgb_u8`, the same conversion as
    /// the exterior sun (`crate::sky::WeatherLighting`).
    pub key_color: [u8; 3],
    pub sunlight_scale: f32,
    /// [`interior_key_illuminance`] of `sunlight_scale`.
    pub key_illuminance: f32,
    /// [`interior_key_travel`] of the rotation pair, engine axes.
    pub key_travel: [f32; 3],
    /// The six DALC colours in record order, sRGB bytes.
    pub ambient_faces: [[u8; 3]; 6],
    pub fog: Option<CellFog>,
    /// The cell's fog fields as stored, for the `skyrim` profile's fog property
    /// ([`FogFields::retail_params`]).
    pub fog_fields: FogFields,
    /// The stored light fade begin and end as the cell or the current room template carries them,
    /// before the zero replacement of [`resolve_light_fade`]. `None` when the cell has no lighting
    /// data: then the distance dimmer is 1 for every light (NEEDS 20, skyrim_decomp PR 1073).
    pub light_fade: Option<(f32, f32)>,
}

impl InteriorRender {
    fn new(lighting: &Lighting, sunlight_scale: f32) -> Self {
        Self {
            key_color: lighting.directional.unwrap_or([0; 3]),
            sunlight_scale,
            key_illuminance: interior_key_illuminance(sunlight_scale),
            key_travel: interior_key_travel(
                lighting.directional_rotation_xy.unwrap_or(0),
                lighting.directional_rotation_z.unwrap_or(0),
            )
            .to_array(),
            ambient_faces: lighting
                .directional_ambient
                .map_or([[0; 3]; 6], |dalc| dalc.colors),
            fog: CellFog::from_lighting(lighting),
            fog_fields: FogFields::from_lighting(lighting),
            light_fade: lighting.light_fade_begin.zip(lighting.light_fade_end),
        }
    }

    /// RF-LIGHT-CPU-013: in modes 1 and 2 a room lighting template has precedence over XCLL for
    /// the colour source (`0x140412df0`, `0x140416fc0`): directional, the DALC cube and the fog
    /// colours. RF-LIGHT-CPU-023: it also supplies the fog near, far, power and max (template
    /// getters `LGTM+0x2C`, `+0x30`, `+0x44`, `+0x6C`, with the same clamps as the cell). The
    /// cell's inherit bits do not apply to a room: a template without a fog ramp gives a room
    /// without fog, even in a cell that has one. Only the key-light rotation
    /// (`Sun::Update`, RF-LIGHT-CPU-011) stays the cell's. Retail blends to the new template
    /// (RF-LIGHT-CPU-024); [`RoomBlend`] does that, over this value and the cell's.
    fn with_room_template(mut self, template: &super::LightingTemplate) -> Self {
        let lighting = &template.lighting;
        self.key_color = lighting.directional.unwrap_or([0; 3]);
        self.ambient_faces = template
            .directional_ambient
            .map_or([[0; 3]; 6], |dalc| dalc.colors);
        self.fog = CellFog::from_lighting(lighting);
        // The `skyrim` fog property takes the template's whole fog too (RF-LIGHT-CPU-023).
        self.fog_fields = FogFields::from_lighting(lighting);
        // The room template's fade pair comes first (NEEDS 20: "current room lighting template,
        // else interior cell fade pair"). A template without the pair leaves the cell's.
        if let Some(pair) = lighting.light_fade_begin.zip(lighting.light_fade_end) {
            self.light_fade = Some(pair);
        }
        self
    }

    /// The blend of `self` (previous) toward `current` by `w`, as retail blends a room change
    /// (`RF-LIGHT-CPU-025`): the directional colour, the six DALC colours (their mean is the
    /// ambient) and the fog, for both profiles' fog. The rotation, the sunlight scale and the key
    /// illuminance are the cell's and are not blended. The light fade pair is not in the blended
    /// list, so it is the current side's. The room image space is not blended: `room_bounds`
    /// carries no image-space data yet.
    fn blended_with(&self, current: &Self, w: f32) -> Self {
        let mut faces = [[0; 3]; 6];
        for (face, (from, to)) in faces
            .iter_mut()
            .zip(self.ambient_faces.iter().zip(&current.ambient_faces))
        {
            *face = lerp_bytes(*from, *to, w);
        }
        Self {
            key_color: lerp_bytes(self.key_color, current.key_color, w),
            ambient_faces: faces,
            fog: CellFog::blend(self.fog, current.fog, w),
            fog_fields: FogFields::blend(&self.fog_fields, &current.fog_fields, w),
            ..*current
        }
    }

    pub(crate) fn key_light_color(&self) -> Color {
        let [r, g, b] = self.key_color;
        Color::srgb_u8(r, g, b)
    }

    /// Bevy lights travel along local -Z.
    pub(crate) fn key_rotation(&self) -> Quat {
        Transform::IDENTITY
            .looking_to(Vec3::from_array(self.key_travel), Vec3::Y)
            .rotation
    }

    /// The `skyrim` key light: the directional colour bytes / 255 (SPEC-1 section 2.3).
    pub(crate) fn retail_key_light_color(&self) -> Color {
        crate::retail_lighting::raw_bytes(self.key_color)
    }

    /// `GlobalAmbientLight.color`: the mean of the six DALC colours, with the same averaging as
    /// the exterior ambient (`WeatherLighting::mean_ambient`). The DALC cube is the only ambient
    /// input of the retail Lighting shader (RF-LIGHT-GPU-001). The per-normal cube term
    /// (RF-LIGHT-CPU-002, RF-LIGHT-GPU-003) is a later shader item.
    pub(crate) fn ambient_color(&self) -> Color {
        crate::sky::WeatherLighting {
            sunlight: Color::BLACK,
            ambient_cube: self.ambient_faces.map(|[r, g, b]| Color::srgb_u8(r, g, b)),
        }
        .mean_ambient()
    }

    /// The whole cell cube for the `skyrim` profile's ambient term (#1010): the six faces, as
    /// [`crate::sky::WeatherLighting::directional_ambient`] reads them (raw bytes divided by 255),
    /// so a surface reads the ambient of its own normal. A room lighting template has already
    /// replaced the faces (`with_room_template`).
    pub(crate) fn directional_ambient(&self) -> DirectionalAmbientCube {
        crate::sky::WeatherLighting {
            sunlight: Color::BLACK,
            ambient_cube: self.ambient_faces.map(|[r, g, b]| Color::srgb_u8(r, g, b)),
        }
        .directional_ambient()
    }
}

/// The interior fog fields as the cell or its template stores them. `None` is a missing field.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub(crate) struct FogFields {
    pub near_color: Option<[u8; 3]>,
    pub far_color: Option<[u8; 3]>,
    pub near: Option<f32>,
    pub far: Option<f32>,
    pub power: Option<f32>,
    pub max: Option<f32>,
}

/// The largest fog far distance retail keeps (SPEC-1 section 8.2).
const RETAIL_MAX_FOG_FAR: f32 = 163_840.0;

/// The share of the far distance that replaces an unusable near distance (SPEC-1 section 8.2).
const RETAIL_FOG_NEAR_SHARE: f32 = 0.17;

impl FogFields {
    fn from_lighting(lighting: &Lighting) -> Self {
        Self {
            near_color: lighting.fog_near_color,
            far_color: lighting.fog_far_color,
            near: lighting.fog_near,
            far: lighting.fog_far,
            power: lighting.fog_power,
            max: lighting.fog_max,
        }
    }

    /// The fields with retail's clamps applied (SPEC-1 section 8.2): colour bytes, a far colour of
    /// zero bytes replaced by the near colour, far and near clamped, power and max 1 when missing.
    ///
    /// STAND-IN(#695): a short `XCLL` or template without the power or max field also takes 1.
    fn resolved(&self) -> ([u8; 3], [u8; 3], f32, f32, f32, f32) {
        let near_color = self.near_color.unwrap_or([0; 3]);
        let far_color = match self.far_color {
            Some(colour) if colour != [0; 3] => colour,
            _ => near_color,
        };
        let far = match self.far {
            Some(far) if far > 0.0 && far <= RETAIL_MAX_FOG_FAR => far,
            _ => RETAIL_MAX_FOG_FAR,
        };
        let near = match self.near {
            Some(near) if near > 0.0 && near <= far => near,
            _ => far * RETAIL_FOG_NEAR_SHARE,
        };
        (
            near_color,
            far_color,
            near,
            far,
            // STAND-IN(#695): a missing power field takes 1.
            self.power.unwrap_or(1.0),
            // STAND-IN(#695): a missing max field takes 1.
            self.max.unwrap_or(1.0),
        )
    }

    /// The room blend of two fog properties (`RF-LIGHT-CPU-025`): both colours, near, far, power
    /// and max blend linearly between the clamped values of each side (RF-LIGHT-CPU-023 applies
    /// the clamps to the template getters). At full weight the result is `current` itself.
    fn blend(previous: &Self, current: &Self, w: f32) -> Self {
        if w >= 1.0 {
            return *current;
        }
        let (a, b) = (previous.resolved(), current.resolved());
        let lerp = |x: f32, y: f32| x + (y - x) * w;
        Self {
            near_color: Some(lerp_bytes(a.0, b.0, w)),
            far_color: Some(lerp_bytes(a.1, b.1, w)),
            near: Some(lerp(a.2, b.2)),
            far: Some(lerp(a.3, b.3)),
            power: Some(lerp(a.4, b.4)),
            max: Some(lerp(a.5, b.5)),
        }
    }

    /// The retail fog property of an interior (SPEC-1 section 8.2): the colour bytes / 255, a far
    /// colour of zero bytes takes the near colour, a far distance at or below 0 or above 163840
    /// becomes 163840, and a near distance at or below 0 or above far becomes `0.17 far`. A cell
    /// without lighting data has power 1 and max 1.
    ///
    /// STAND-IN(#695): a short `XCLL` or template without the power or max field also takes 1.
    pub(crate) fn retail_params(&self) -> crate::retail_fog::RetailFogParams {
        let (near_color, far_color, near, far, power, max) = self.resolved();
        crate::retail_fog::RetailFogParams {
            near,
            far,
            power,
            max,
            near_rgb: unit_rgb(near_color),
            far_rgb: unit_rgb(far_color),
        }
    }
}

/// A room marker box with a lighting template, in engine axes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RoomLighting {
    pub ref_id: u32,
    center: Vec3,
    inverse_rotation: Quat,
    half: Vec3,
    pub render: InteriorRender,
}

impl RoomLighting {
    fn new(bound: &RoomBound, render: InteriorRender) -> Self {
        Self {
            ref_id: bound.ref_id,
            center: creation_to_bevy(Vec3::from_array(bound.position)),
            inverse_rotation: creation_rotation_to_bevy(bound.rotation).inverse(),
            // The box axes map like any Creation vector; only the extent's size matters.
            half: creation_to_bevy(Vec3::from_array(bound.half)).abs(),
            render,
        }
    }

    /// Interiors use absolute coordinates (`crate::door_crossing`), so `position` is the
    /// camera translation.
    fn contains(&self, position: Vec3) -> bool {
        let local = self.inverse_rotation * (position - self.center);
        local.abs().cmple(self.half).all()
    }
}

/// One scene selection. ActiveSpace owns scene identity.
#[derive(Resource, Debug, Clone, PartialEq, Default, serde::Serialize)]
pub(crate) struct SceneEnvironment {
    pub interior: Option<InteriorLighting>,
    pub cell_flags: Option<u32>,
    pub sky_mode: SkyMode,
    pub show_sky: bool,
    pub use_sky_lighting: bool,
    /// Missing when the cell flags or the XCIM column are missing. Exterior scenes report
    /// `weather_global`.
    pub image_space: Option<ImageSpaceSelection>,
    /// What modes 1 and 2 apply when the camera is in no room with a lighting template.
    pub cell_render: Option<InteriorRender>,
    /// The cell's room markers with a resolvable lighting template, in `ref_id` order.
    #[serde(skip)]
    pub rooms: Vec<RoomLighting>,
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
        let interior = catalog.resolve_interior(cell_id);
        let image_space =
            ImageSpaceSelection::for_interior(cell_flags, catalog.cell_image_space(cell_id));
        // A database without the flags still knows the cell is an interior: retail reads bit
        // 0x1 and nothing else, which gives mode 1.
        let sky_mode = SkyMode::from_cell_flags(Some(cell_flags.unwrap_or(CELL_INTERIOR)));
        // An older pack has no authored lighting at all: the built-in lights stay, as before the
        // interior rule. An authored cell without XCLL is different and is lit with zeros.
        let (cell_render, rooms) =
            if sky_mode.uses_cell_lighting() && catalog.has_cell_lighting_table() {
                let render = InteriorRender::new(
                    &interior.lighting,
                    interior_sunlight_scale(catalog, image_space),
                );
                let rooms = catalog
                    .room_bounds(cell_id)
                    .iter()
                    .filter_map(|bound| {
                        let template = catalog.lighting_template(bound.lighting_template)?;
                        Some(RoomLighting::new(
                            bound,
                            render.with_room_template(template),
                        ))
                    })
                    .collect();
                (Some(render), rooms)
            } else {
                (None, Vec::new())
            };
        Self {
            interior: Some(interior),
            cell_flags,
            sky_mode,
            show_sky: cell_flags.is_some_and(|flags| flags & SHOW_SKY != 0),
            use_sky_lighting: cell_flags.is_some_and(|flags| flags & USE_SKY_LIGHTING != 0),
            image_space,
            cell_render,
            rooms,
        }
    }

    /// The templated room whose box holds the point, or `None` for no room (the cell's own
    /// lighting). The lookup for a cell that has no usable room graph; with one, the graph names
    /// the rooms ([`step_room_blend`]). `last` is held as in [`keep_or_first`].
    pub(crate) fn room_at(&self, point: Option<Vec3>, last: Option<u32>) -> Option<u32> {
        let point = point?;
        let holding: Vec<u32> = self
            .rooms
            .iter()
            .filter(|room| room.contains(point))
            .map(|room| room.ref_id)
            .collect();
        keep_or_first(&holding, last)
    }

    /// Whether `ref_id` is a room of this cell with a lighting template.
    fn has_template(&self, ref_id: u32) -> bool {
        self.rooms.iter().any(|room| room.ref_id == ref_id)
    }

    /// What one side of a blend applies: the room's lighting, or the cell's when `room` is `None`
    /// or names no room of this cell. `None` in mode 3.
    fn render_of(&self, room: Option<u32>) -> Option<&InteriorRender> {
        let cell = self.cell_render.as_ref()?;
        Some(
            room.and_then(|id| self.rooms.iter().find(|room| room.ref_id == id))
                .map_or(cell, |room| &room.render),
        )
    }

    /// The current side of the blend, for the values the blend does not cover (the light fade
    /// pair). Without a blend state it is the room the camera is in. `None` in mode 3.
    pub(crate) fn current_render(
        &self,
        blend: Option<&RoomBlend>,
        camera: Option<Vec3>,
    ) -> Option<&InteriorRender> {
        let room = match blend {
            Some(blend) => blend.current,
            None => self.room_at(camera, None),
        };
        self.render_of(room)
    }

    /// The values to apply now, or `None` in mode 3: the current side of the blend, or the blend
    /// of the previous and current sides while a transition runs (RF-LIGHT-CPU-024, `-025`).
    pub(crate) fn blended_render(&self, blend: &RoomBlend) -> Option<InteriorRender> {
        let current = *self.render_of(blend.current)?;
        if blend.weight <= 0.0 {
            return Some(current);
        }
        let previous = *self.render_of(blend.previous)?;
        Some(previous.blended_with(&current, blend.weight))
    }
}

/// The room a point is in, out of the rooms that hold it, in table order.
///
/// RF-LIGHT-CPU-026: retail first re-tests the room it already has, and only when the point has
/// left that room takes the first room in order that holds it. `last` is the room of the previous
/// frame, so a point inside two overlapping rooms stays in the first one it entered.
fn keep_or_first(holding: &[u32], last: Option<u32>) -> Option<u32> {
    last.filter(|id| holding.contains(id))
        .or_else(|| holding.first().copied())
}

/// `fRoomLightingTransitionDuration`: the seconds a switch between room lightings takes
/// (`RF-LIGHT-CPU-024`: record `0x14209E7C0`, default 3.0, one reader `0x14041206C`, no ESM
/// override found). The unit is the frame delta of the renderer's clock, which about 200 retail
/// readers use as seconds per frame (medium confidence), so it is real time here.
pub(crate) const ROOM_LIGHTING_TRANSITION_SECONDS: f32 = 3.0;

/// The weight a started transition begins at (`RF-LIGHT-CPU-024`, `0x3727C5AC`, 1.0e-5). A weight
/// above 0 is what marks a transition as running.
const ROOM_TRANSITION_START_WEIGHT: f32 = 1.0e-5;

/// The retail room-lighting transition (`RF-LIGHT-CPU-024`, `Sky::Update` `0x140411EC0`).
///
/// A side is a room's `ref_id`, or `None` for the cell's own lighting (retail's "default handle").
/// - A new room differing from `current` starts a transition only while none runs: previous takes
///   current, current takes the new room, weight starts at 1.0e-5. A room change during a transition
///   is not started; it is picked up after the transition ends.
/// - While it runs, elapsed time grows by the frame delta, `t = clamp(elapsed / 3.0, 0, 1)` and the
///   weight is `w = (3 - 2t) * t^2`. At `w >= 1` previous returns to the default side, and weight
///   and elapsed return to 0.
/// - A cell change takes the new room at once (`Sky+0x1DC` bit 0x100): previous = current = new,
///   elapsed 0, no transition.
#[derive(Resource, Debug, Clone, PartialEq, Default)]
pub(crate) struct RoomBlend {
    /// The cell the state belongs to. A different cell snaps.
    cell_id: Option<u32>,
    /// The room the camera is in now, with or without a lighting template ([`step_room_blend`]).
    target: Option<u32>,
    previous: Option<u32>,
    current: Option<u32>,
    elapsed: f32,
    weight: f32,
}

impl RoomBlend {
    /// A cell change: no transition, the new room applies at once.
    fn snap(&mut self, cell_id: Option<u32>, room: Option<u32>) {
        *self = Self {
            cell_id,
            target: room,
            previous: room,
            current: room,
            elapsed: 0.0,
            weight: 0.0,
        };
    }

    /// One frame: the camera is in `room`, and `delta` seconds have passed.
    fn step(&mut self, room: Option<u32>, delta: f32) {
        self.target = room;
        if room != self.current && self.weight == 0.0 {
            self.previous = self.current;
            self.current = room;
            self.weight = ROOM_TRANSITION_START_WEIGHT;
            self.elapsed = 0.0;
        }
        if self.weight > 0.0 {
            self.elapsed += delta.max(0.0);
            let t = (self.elapsed / ROOM_LIGHTING_TRANSITION_SECONDS).clamp(0.0, 1.0);
            let w = (3.0 - 2.0 * t) * t * t;
            if w >= 1.0 {
                self.previous = None;
                self.weight = 0.0;
                self.elapsed = 0.0;
            } else {
                // Never below the start weight: the transition stays marked as running.
                self.weight = w.max(ROOM_TRANSITION_START_WEIGHT);
            }
        }
    }
}

/// The XCIM image space's SunlightScale (RF-LIGHT-GPU-006, RF-IMGS-001; Alvor's house: 1.5).
/// An interior that takes the weather image space (flags 0x101 != 0x001, only the maze in the
/// shipped data) or whose image space is missing from the database gets the engine's exterior
/// scale `SUNLIGHT_SCALE`. That fallback is a Mudcrab decision, not a retail trace.
fn interior_sunlight_scale(
    catalog: &EnvironmentCatalog,
    image_space: Option<ImageSpaceSelection>,
) -> f32 {
    image_space
        .and_then(|selection| selection.form_id)
        .and_then(|form| catalog.image_space(form))
        .and_then(|image_space| image_space.sunlight_scale)
        .unwrap_or(crate::lights::SUNLIGHT_SCALE)
}

/// The cell fog `crate::sky` gives every fog camera inside an interior in sky modes 1 and 2.
/// `None` outside those modes or when the cell has no fog ramp.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct InteriorFog(pub Option<CellFog>);

/// The `skyrim` fog property of the interior the camera is in, in sky modes 1 and 2. `None`
/// outside those modes and under `mudcrab`.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct InteriorRetailFog(pub Option<crate::retail_fog::RetailFogParams>);

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EnvironmentSet;

pub(crate) struct InteriorEnvironmentPlugin;

impl Plugin for InteriorEnvironmentPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SceneEnvironment>()
            .init_resource::<InteriorFog>()
            .init_resource::<InteriorRetailFog>()
            .init_resource::<crate::retail_hdr::RetailImageSpace>()
            .init_resource::<RoomBlend>()
            .add_systems(
                PostUpdate,
                (
                    select_environment,
                    crate::retail_hdr::select_retail_image_space,
                    step_room_blend,
                    apply_interior_lighting,
                )
                    .chain()
                    .in_set(EnvironmentSet)
                    .after(crate::door_crossing::DoorCrossingSet)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// Runs after queued PostUpdate door switches, before transform propagation.
fn select_environment(
    catalog: Res<EnvironmentCatalog>,
    space: Res<ActiveSpace>,
    mut scene: ResMut<SceneEnvironment>,
    mut selected_once: Local<bool>,
    mut warned: Local<std::collections::BTreeSet<u32>>,
) {
    let first_selection = !std::mem::replace(&mut *selected_once, true);
    if first_selection
        || catalog.is_changed()
        || space.is_changed()
        || scene.interior.is_none() && space.interior.is_some()
    {
        let selected = SceneEnvironment::select(&catalog, &space);
        // An interior in sky mode 1 or 2 without an XCLL is lit with zeros (RF-LIGHT-CPU-014).
        // That is what retail reads, but it is visible and often a modding mistake: say so once
        // per cell and run.
        if let (Some(cell), Some(interior)) = (selected.cell_render.as_ref(), &selected.interior)
            && !interior.has_xcll
            && warned.insert(interior.cell_id)
        {
            warn!(
                cell_id = interior.cell_id,
                key_color = ?cell.key_color,
                "interior has no XCLL: it is lit with zero colours as retail reads them"
            );
        }
        if *scene != selected {
            info!(environment = ?selected, "selected scene environment");
            *scene = selected;
        }
    }
}

/// The ambient that `apply_interior_lighting` writes: Bevy's flat colour, and the `skyrim`
/// profile's directional cube (#1010).
#[derive(SystemParam)]
struct AmbientTargets<'w, 's> {
    ambient: Option<ResMut<'w, GlobalAmbientLight>>,
    cube: Option<ResMut<'w, DirectionalAmbientCube>>,
    profile: Option<Res<'w, crate::visual_profile::VisualProfile>>,
    /// Whether this system wrote the cube for the cell the player is in.
    cube_indoors: Local<'s, bool>,
    /// The scene's image space, whose `SunlightScale` the `skyrim` key light takes.
    image_space: Option<Res<'w, crate::retail_hdr::RetailImageSpace>>,
    /// The `skyrim` fog property of the interior.
    retail_fog: Option<ResMut<'w, InteriorRetailFog>>,
}

/// Advances the room-lighting blend one frame (`RF-LIGHT-CPU-024`): a cell change takes the new
/// room at once, and a room crossing inside the cell starts or continues a blend.
///
/// The room comes from the cell's portal graph ([`RoomCullingState::rooms_at`]), at retail's room
/// point: the camera plus its near distance along the view direction (RF-ROOM-002). While the
/// graph is read, the cell's own lighting applies and the first answer counts as the cell change.
/// A cell without a usable graph takes the room boxes ([`SceneEnvironment::room_at`]).
fn step_room_blend(
    scene: Res<SceneEnvironment>,
    cameras: Query<
        (&Transform, Option<&RoomCullingNear>),
        (With<StreamingCamera>, Without<WorldSun>),
    >,
    graphs: Option<Res<RoomCullingState>>,
    roots: Query<&GlobalTransform, InteriorRoot>,
    mut blend: ResMut<RoomBlend>,
    time: Option<Res<Time<Real>>>,
) {
    let point = cameras.iter().next().map(|(transform, near)| {
        near.map_or(transform.translation, |near| {
            near.world_point(&GlobalTransform::from(*transform))
        })
    });
    let cell_id = scene.interior.as_ref().map(|interior| interior.cell_id);
    let found = match (cell_id, point, graphs) {
        (Some(cell_id), Some(point), Some(graphs)) => graphs.rooms_at(cell_id, point, &roots),
        _ => PointRooms::Unavailable,
    };
    let (room, side) = match found {
        PointRooms::Pending => {
            // No cell is recorded, so the first answer of the graph applies without a blend.
            let mut unread = RoomBlend::default();
            unread.snap(None, None);
            blend.set_if_neq(unread);
            return;
        }
        PointRooms::Holding(holding) => {
            let room = keep_or_first(&holding, blend.target);
            (room, room.filter(|&id| scene.has_template(id)))
        }
        PointRooms::Unavailable => {
            let room = scene.room_at(point, blend.target);
            (room, room)
        }
    };
    // retail-default: not record data; a run without the real clock has no elapsed time.
    let delta = time.map_or(0.0, |time| time.delta_secs());
    let mut next = blend.clone();
    if next.cell_id == cell_id {
        next.step(side, delta);
    } else {
        next.snap(cell_id, side);
    }
    next.target = room;
    blend.set_if_neq(next);
}

/// Applies sky modes 1 and 2 to the [`WorldSun`], `GlobalAmbientLight` and [`InteriorFog`]
/// (verify-report "The rule", steps 2-8):
/// - Key light: colour `srgb_u8(directional)`, [`interior_key_illuminance`], and the
///   [`interior_key_travel`] direction. The sun colour, strength and clock direction are
///   dropped. The directional fade is never applied (RF-LIGHT-CPU-008). The camera exposure is
///   unchanged (RF-LIGHT-GPU-004 has no exposure term).
/// - Shadows: OFF. Retail interior directional shadowing is UNRESOLVED (RF-LIGHT-GPU-010; the
///   light is wired as the shadow light with a 3000-unit range). This is a Mudcrab interim
///   decision: the exterior cascades would let the roof and floor shadow the room. Their range is
///   the exterior `fShadowDistance`; the interior `fInteriorShadowDistance` is open (#857).
/// - Ambient: the DALC mean, brightness unchanged (`AMBIENT_ILLUMINANCE`).
/// - Fog: the cell fog.
///
/// Mode 3 changes nothing: the exterior sun path (#588) stays. On leaving modes 1 and 2 the
/// saved exterior rotation, colour, illuminance, shadows and ambient return, and
/// `crate::sun::update_sun` drives the sun again. A write by another system while indoors, or in
/// the exit frame (a settings change), replaces the saved value, so the restore returns the
/// current setting.
fn apply_interior_lighting(
    scene: Res<SceneEnvironment>,
    mut suns: Query<
        (
            Entity,
            &mut DirectionalLight,
            &mut Transform,
            Option<&mut ExteriorSunState>,
        ),
        With<WorldSun>,
    >,
    mut fog: ResMut<InteriorFog>,
    blend: Res<RoomBlend>,
    mut saved_ambient: Local<Option<Color>>,
    lights: AmbientTargets,
    mut commands: Commands,
) {
    let wanted = scene.blended_render(&blend);
    let wanted_fog = InteriorFog(wanted.and_then(|render| render.fog));
    if *fog != wanted_fog {
        *fog = wanted_fog;
    }
    let AmbientTargets {
        ambient,
        cube,
        profile,
        mut cube_indoors,
        image_space,
        retail_fog,
    } = lights;
    // The retail light values are the `skyrim` profile only (SPEC-1).
    let skyrim =
        profile.is_some_and(|profile| *profile == crate::visual_profile::VisualProfile::Skyrim);
    if let Some(mut retail_fog) = retail_fog {
        retail_fog.set_if_neq(InteriorRetailFog(
            wanted
                .filter(|_| skyrim)
                .map(|render| render.fog_fields.retail_params()),
        ));
    }
    let sunlight_scale = crate::retail_hdr::scene_block(image_space.as_deref()).sunlight_scale;
    for (entity, mut light, mut transform, saved) in &mut suns {
        match (wanted, saved) {
            (Some(render), saved) => {
                // Another system wrote the sun since this one last ran (`is_changed` does not see
                // this system's own writes): `crate::settings::apply_render_settings` writes the
                // exterior colour, illuminance and shadows on every preference change, indoors
                // too. Those are the new exterior values, so the restore must return them. A
                // change takes all three fields: that writer always writes all three, and a
                // written value equal to the interior one (shadows off) must still count.
                if let Some(mut saved) = saved {
                    if light.is_changed() {
                        // Under `skyrim` the settings write no sun colour or level, so the
                        // light still holds the interior key: only the shadow switch is new.
                        if !skyrim {
                            saved.color = light.color;
                            saved.illuminance = light.illuminance;
                        }
                        saved.shadow_maps_enabled = light.shadow_maps_enabled;
                    }
                    if transform.is_changed() {
                        saved.rotation = transform.rotation;
                    }
                } else {
                    commands.entity(entity).insert(ExteriorSunState {
                        rotation: transform.rotation,
                        color: light.color,
                        illuminance: light.illuminance,
                        shadow_maps_enabled: light.shadow_maps_enabled,
                    });
                }
                let (colour, illuminance) = if skyrim {
                    (
                        render.retail_key_light_color(),
                        crate::retail_lighting::sun_illuminance(sunlight_scale),
                    )
                } else {
                    (render.key_light_color(), render.key_illuminance)
                };
                set_sun(
                    &mut light,
                    &mut transform,
                    colour,
                    illuminance,
                    false,
                    render.key_rotation(),
                );
            }
            (None, Some(mut saved)) => {
                // The exit frame. A preference applied in `Update` just before this frame wrote
                // the exterior values after the last snapshot, and they are the ones to keep.
                if light.is_changed() {
                    if !skyrim {
                        saved.color = light.color;
                        saved.illuminance = light.illuminance;
                    }
                    saved.shadow_maps_enabled = light.shadow_maps_enabled;
                }
                if transform.is_changed() {
                    saved.rotation = transform.rotation;
                }
                let saved = *saved;
                set_sun(
                    &mut light,
                    &mut transform,
                    saved.color,
                    saved.illuminance,
                    saved.shadow_maps_enabled,
                    saved.rotation,
                );
                commands.entity(entity).remove::<ExteriorSunState>();
            }
            (None, None) => {}
        }
    }
    // The directional ambient is the `skyrim` profile only (#1010): `mudcrab` keeps its flat
    // ambient. Outside, `crate::sky::drive_exterior_lighting` writes the exterior cube, in the
    // frame this system hands the ambient back, so leaving the cell only switches it off here.
    if let Some(mut cube) = cube {
        match wanted {
            Some(render) if skyrim => {
                cube.set_if_neq(render.directional_ambient());
                *cube_indoors = true;
            }
            None if *cube_indoors => {
                cube.set_if_neq(DirectionalAmbientCube::default());
                *cube_indoors = false;
            }
            _ => {}
        }
    }
    let Some(mut ambient) = ambient else {
        return;
    };
    match wanted {
        Some(render) => {
            // As for the sun: a write by another system is the new exterior ambient.
            match saved_ambient.as_mut() {
                Some(saved) if ambient.is_changed() => *saved = ambient.color,
                Some(_) => {}
                None => *saved_ambient = Some(ambient.color),
            }
            // Under `skyrim` the flat colour is the raw cube mean, for what Bevy's own
            // shaders draw. The retail surfaces read the cube.
            let color = if skyrim {
                let mean = render.directional_ambient().mean;
                Color::linear_rgb(mean.x, mean.y, mean.z)
            } else {
                render.ambient_color()
            };
            if ambient.color != color {
                ambient.color = color;
            }
        }
        None => {
            // A write since the last run is the new exterior ambient, as for the sun above.
            if let Some(color) = saved_ambient.take()
                && !ambient.is_changed()
                && ambient.color != color
            {
                ambient.color = color;
            }
        }
    }
}

/// Write the sun fields that differ, so an unchanged frame marks nothing changed.
fn set_sun(
    light: &mut Mut<DirectionalLight>,
    transform: &mut Mut<Transform>,
    color: Color,
    illuminance: f32,
    shadow_maps_enabled: bool,
    rotation: Quat,
) {
    if light.color != color
        || light.illuminance != illuminance
        || light.shadow_maps_enabled != shadow_maps_enabled
    {
        light.color = color;
        light.illuminance = illuminance;
        light.shadow_maps_enabled = shadow_maps_enabled;
    }
    if transform.rotation != rotation {
        transform.rotation = rotation;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;
    use rusqlite::Connection;
    use std::time::Duration;

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

    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1.0e-6,
                "expected {expected:?}, got {actual:?}"
            );
        }
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

    // ---- The sky mode rule (verify-report section 3; RF-LIGHT-CPU-010 to RF-LIGHT-CPU-016) ----

    const ALVOR: u32 = 78792;
    const BLEAK_FALLS: u32 = 225758;
    const MAZE: u32 = 0xF2CAB;

    /// Rows copied from the registered pack v010-candidate-2e93bfa3 (read-only sqlite3,
    /// 2026-11-15). Alvor's house: flags 0x81, inherit 0x9F, LTMP 0xA1196 (659862), XCIM
    /// 0x59B60 (367456, SunlightScale 1.5). Bleak Falls Barrow01: flags 0x83, inherit 0, LTMP
    /// 201357, XCIM 191979 (SunlightScale 1.0), first room marker 90193 with template 201356
    /// `BleakFallsBarrowMedium`. The maze: flags 0x181. Fog distances of the maze are made up.
    fn rule_catalog() -> EnvironmentCatalog {
        let connection = Connection::open_in_memory().unwrap();
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
                 INSERT INTO cell_environment(cell_id,data_flags,image_space_id)
                 VALUES(78792,129,367456),(225758,131,191979),(994475,385,665223);
                 INSERT INTO image_spaces(id,sunlight_scale) VALUES(367456,1.5),(191979,1.0);
                 INSERT INTO cell_lighting(cell_id,lighting_template_id,xcll_size,
                    ambient_r,ambient_g,ambient_b,directional_r,directional_g,directional_b,
                    fog_near_r,fog_near_g,fog_near_b,fog_near,fog_far,
                    directional_rotation_xy,directional_rotation_z,directional_fade,
                    fog_clip_distance,fog_power,
                    dalc_x_pos_r,dalc_x_pos_g,dalc_x_pos_b,dalc_x_neg_r,dalc_x_neg_g,dalc_x_neg_b,
                    dalc_y_pos_r,dalc_y_pos_g,dalc_y_pos_b,dalc_y_neg_r,dalc_y_neg_g,dalc_y_neg_b,
                    dalc_z_pos_r,dalc_z_pos_g,dalc_z_pos_b,dalc_z_neg_r,dalc_z_neg_g,dalc_z_neg_b,
                    fog_far_r,fog_far_g,fog_far_b,fog_max,light_fade_begin,light_fade_end,
                    inherit_flags)
                 VALUES(78792,659862,92,45,48,48,35,40,50,31,52,46,0.0,4000.0,0,30,0.0,0.0,1.0,
                    49,48,50,40,48,45,45,50,48,45,45,48,21,22,21,68,73,74,
                    31,52,46,1.0,0.0,0.0,159),
                   (225758,201357,92,66,60,40,44,47,48,75,99,97,0.0,12000.0,270,240,0.0,12000.0,
                    0.7,72,60,41,59,60,38,66,62,40,66,57,40,31,28,18,105,96,65,
                    75,99,97,1.0,0.0,0.0,0),
                   (994475,NULL,92,10,10,10,90,91,92,1,2,3,0.0,1000.0,0,90,0.0,0.0,1.0,
                    1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
                    1,2,3,1.0,0.0,0.0,0);
                 INSERT INTO lighting_templates(id,editor_id,data_size,
                    ambient_r,ambient_g,ambient_b,directional_r,directional_g,directional_b,
                    fog_near_r,fog_near_g,fog_near_b,fog_near,fog_far,
                    directional_rotation_xy,directional_rotation_z,fog_power,fog_max,
                    fog_far_r,fog_far_g,fog_far_b,
                    subrecord_dalc_x_pos_r,subrecord_dalc_x_pos_g,subrecord_dalc_x_pos_b,
                    subrecord_dalc_x_neg_r,subrecord_dalc_x_neg_g,subrecord_dalc_x_neg_b,
                    subrecord_dalc_y_pos_r,subrecord_dalc_y_pos_g,subrecord_dalc_y_pos_b,
                    subrecord_dalc_y_neg_r,subrecord_dalc_y_neg_g,subrecord_dalc_y_neg_b,
                    subrecord_dalc_z_pos_r,subrecord_dalc_z_pos_g,subrecord_dalc_z_pos_b,
                    subrecord_dalc_z_neg_r,subrecord_dalc_z_neg_g,subrecord_dalc_z_neg_b)
                 VALUES(659862,'FarmLightingTemplate',92,45,48,48,65,70,80,31,52,46,100.0,4000.0,
                    0,45,1.0,1.0,31,52,46,
                    49,48,50,40,48,45,45,50,48,45,45,48,21,22,21,68,73,74),
                   (201356,'BleakFallsBarrowMedium',92,0,0,0,68,79,81,7,8,9,0.0,5000.0,
                    0,0,1.0,0.9,7,8,9,
                    11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11);
                 INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,
                    half_x,half_y,half_z,lighting_template_id)
                 VALUES(90193,225758,7936.0,-5015.0757,-1952.0,0,0,0,1408.0,1304.9244,1056.0,
                    201356),
                   (90999,225758,0,0,0,0,0,0,10,10,10,4242);",
            )
            .unwrap();
        EnvironmentCatalog::from_connection(&connection)
    }

    /// The world sun as `crate::app` spawns it: the exterior colour, 12000 lux and shadows on.
    fn spawn_world_sun(app: &mut App) -> Entity {
        app.insert_resource(GlobalAmbientLight {
            color: crate::sky::WeatherLighting::default().mean_ambient(),
            brightness: crate::lights::AMBIENT_ILLUMINANCE,
            ..default()
        });
        app.world_mut()
            .spawn((
                DirectionalLight {
                    color: crate::sky::WeatherLighting::default().sunlight,
                    illuminance: crate::lights::SUN_ILLUMINANCE,
                    shadow_maps_enabled: true,
                    ..default()
                },
                WorldSun,
                crate::sun::SunLight,
                Transform::from_rotation(Quat::from_rotation_x(-0.7)),
            ))
            .id()
    }

    fn sun_light(app: &App, sun: Entity) -> &DirectionalLight {
        app.world().get::<DirectionalLight>(sun).unwrap()
    }

    fn assert_vec(actual: Vec3, expected: Vec3) {
        assert!(
            actual.distance(expected) < 1.0e-5,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn sky_mode_follows_the_cell_flags() {
        use SkyMode::*;
        for (flags, mode) in [
            (None, WeatherSun),
            (Some(0x0), WeatherSun),
            (Some(0x80), WeatherSun),
            (Some(0x1), CellLighting),
            (Some(0x101), CellLighting),
            (Some(0x81), CellLightingWithSky),
            (Some(0x83), CellLightingWithSky),
            (Some(0x181), WeatherSun),
        ] {
            assert_eq!(SkyMode::from_cell_flags(flags), mode, "flags {flags:?}");
        }
        let mut app = app(rule_catalog());
        for (cell, mode) in [
            (ALVOR, CellLightingWithSky),
            (BLEAK_FALLS, CellLightingWithSky),
            (MAZE, WeatherSun),
            (404, CellLighting),
        ] {
            enter(&mut app, Some(cell));
            assert_eq!(app.world().resource::<SceneEnvironment>().sky_mode, mode);
        }
        enter(&mut app, None);
        assert_eq!(
            app.world().resource::<SceneEnvironment>().sky_mode,
            WeatherSun
        );
    }

    #[test]
    fn key_illuminance_is_pi_ambient_times_sunlight_scale() {
        // PI x 1364.2 x 1.5 = 6428.6 lux (Alvor's house); 4285.7 lux at 1.0 (Bleak Falls).
        assert!((interior_key_illuminance(1.5) - 6428.57).abs() < 0.05);
        assert!((interior_key_illuminance(1.0) - 4285.71).abs() < 0.05);
        // The exterior sun is the same formula with SUNLIGHT_SCALE.
        assert!(
            (interior_key_illuminance(crate::lights::SUNLIGHT_SCALE)
                - crate::lights::SUN_ILLUMINANCE)
                .abs()
                < 0.05
        );
    }

    #[test]
    fn key_travel_matches_the_traced_rotation() {
        // Alvor: a=0, b=30 -> Creation (0.866, 0, 0.5) -> engine (0.866, 0.5, 0).
        assert_vec(interior_key_travel(0, 30), Vec3::new(0.866_025_4, 0.5, 0.0));
        // Bleak Falls: a=270, b=240 -> engine (0, -0.866, 0.5): it travels down.
        assert_vec(
            interior_key_travel(270, 240),
            Vec3::new(0.0, -0.866_025_4, 0.5),
        );
        // b=90 travels straight up (383 shipped cells use Z = 90).
        assert_vec(interior_key_travel(0, 90), Vec3::Y);
        // Unsigned conversion: -30 is 4294967266 degrees, not -30.
        let wrapped = interior_key_travel(0, -30);
        assert!(wrapped.distance(interior_key_travel(0, 330)) > 1.0e-3);
    }

    #[test]
    fn alvors_house_gets_the_cell_key_light_ambient_and_fog_not_the_sun() {
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        enter(&mut app, Some(ALVOR));
        app.update();
        let light = sun_light(&app, sun);
        // Colour from the template (inherit 0x2), not the weather sun (190,181,171).
        assert_eq!(light.color, Color::srgb_u8(65, 70, 80));
        assert!((light.illuminance - 6428.57).abs() < 0.05);
        assert!(!light.shadow_maps_enabled);
        let rotation = app.world().get::<Transform>(sun).unwrap().rotation;
        assert_vec(rotation * Vec3::NEG_Z, Vec3::new(0.866_025_4, 0.5, 0.0));
        // Ambient: the template DALC mean (inherit 0x1), brightness unchanged.
        let ambient = app.world().resource::<GlobalAmbientLight>();
        let faces = [
            [49, 48, 50],
            [40, 48, 45],
            [45, 50, 48],
            [45, 45, 48],
            [21, 22, 21],
            [68, 73, 74],
        ];
        let expected = crate::sky::WeatherLighting {
            sunlight: Color::BLACK,
            ambient_cube: faces.map(|[r, g, b]| Color::srgb_u8(r, g, b)),
        }
        .mean_ambient();
        assert_eq!(ambient.color, expected);
        assert_eq!(ambient.brightness, crate::lights::AMBIENT_ILLUMINANCE);
        // The reported byte mean is the report's (0.1752, 0.1869, 0.1869).
        let cube = selected(&app).ambient_cube.unwrap();
        assert_close(
            &cube.mean,
            &[268.0 / 1530.0, 286.0 / 1530.0, 286.0 / 1530.0],
        );
        // Fog: colour (31,52,46) and near 100 inherited (0x4, 0x8), far 4000, max 1.0.
        let fog = app.world().resource::<InteriorFog>().0.unwrap();
        assert_eq!(fog.near_color, [31, 52, 46]);
        assert_eq!((fog.near, fog.far, fog.max), (100.0, 4000.0, 1.0));
        let bevy_fog = fog.distance_fog();
        assert_eq!(bevy_fog.color, Color::srgb_u8(31, 52, 46));
        assert!(matches!(
            bevy_fog.falloff,
            FogFalloff::Linear {
                start: 100.0,
                end: 4000.0
            }
        ));
        // The directional fade (0.0 here) is reported, never applied.
        assert_eq!(selected(&app).lighting.directional_fade, Some(0.0));
    }

    /// Under `skyrim` the cell's whole DALC cube reaches the render resource (#1010). Alvor's
    /// template faces are the ones the flat-ambient test above uses. The cube is the raw bytes
    /// divided by 255 (the object LOD material's rows): the mean of the six and the half
    /// differences of the stored pairs. `mudcrab` keeps the flat ambient: the resource stays off.
    /// Leaving the cell switches it off again.
    #[test]
    fn the_skyrim_profile_gives_the_cell_cube_to_the_ambient_term() {
        use crate::visual_profile::VisualProfile;
        // Raw bytes divided by 255, no sRGB decode (the object LOD material's rows).
        let linear = |faces: [[u8; 3]; 6], face: usize| {
            let [r, g, b] = faces[face];
            Vec3::new(f32::from(r), f32::from(g), f32::from(b)) / 255.0
        };
        let faces = [
            [49, 48, 50],
            [40, 48, 45],
            [45, 50, 48],
            [45, 45, 48],
            [21, 22, 21],
            [68, 73, 74],
        ];
        for profile in [VisualProfile::Skyrim, VisualProfile::Mudcrab] {
            let mut app = app(rule_catalog());
            app.insert_resource(profile)
                .init_resource::<DirectionalAmbientCube>();
            let _sun = spawn_world_sun(&mut app);
            enter(&mut app, Some(ALVOR));
            app.update();
            let cube = *app.world().resource::<DirectionalAmbientCube>();
            if profile == VisualProfile::Mudcrab {
                assert_eq!(cube, DirectionalAmbientCube::default());
                continue;
            }
            assert!(cube.enabled);
            let raw_mean = (0..6).map(|face| linear(faces, face)).sum::<Vec3>() / 6.0;
            assert!(
                (cube.mean - raw_mean).abs().max_element() < 1.0e-6,
                "the cube's mean is the raw bytes divided by 255"
            );
            let expected_x = (linear(faces, 1) - linear(faces, 0)) * 0.5;
            let expected_y = (linear(faces, 3) - linear(faces, 2)) * 0.5;
            let expected_z = (linear(faces, 5) - linear(faces, 4)) * 0.5;
            assert!((cube.slope_x - expected_x).abs().max_element() < 1.0e-6);
            assert!((cube.slope_y - expected_y).abs().max_element() < 1.0e-6);
            assert!((cube.slope_z - expected_z).abs().max_element() < 1.0e-6);
            assert!(
                cube.slope_z.min_element() > 0.01,
                "Alvor's up and down faces differ, so the cell is lit from above"
            );
            // A quiet frame leaves the resource's change tick alone.
            app.update();
            assert!(
                !app.world()
                    .resource_ref::<DirectionalAmbientCube>()
                    .is_changed()
            );
            // Leaving the cell hands the ambient back to the exterior driver: off here.
            enter(&mut app, None);
            assert!(!app.world().resource::<DirectionalAmbientCube>().enabled);
        }
    }

    #[test]
    fn the_clock_does_not_turn_the_interior_key_light() {
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        enter(&mut app, Some(ALVOR));
        app.update();
        let interior = app.world().get::<Transform>(sun).unwrap().rotation;
        // `update_sun` only queries suns without ExteriorSunState.
        let mut query = app
            .world_mut()
            .query_filtered::<Entity, (With<crate::sun::SunLight>, Without<ExteriorSunState>)>();
        assert_eq!(query.iter(app.world()).count(), 0);
        app.update();
        assert_eq!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            interior
        );
    }

    #[test]
    fn leaving_restores_the_exterior_sun_and_ambient() {
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        app.update();
        let exterior_rotation = app.world().get::<Transform>(sun).unwrap().rotation;
        let exterior_ambient = app.world().resource::<GlobalAmbientLight>().color;
        enter(&mut app, Some(ALVOR));
        app.update();
        enter(&mut app, None);
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.color, crate::sky::WeatherLighting::default().sunlight);
        assert_eq!(light.illuminance, crate::lights::SUN_ILLUMINANCE);
        assert!(light.shadow_maps_enabled);
        assert_eq!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            exterior_rotation
        );
        assert!(app.world().get::<ExteriorSunState>(sun).is_none());
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().color,
            exterior_ambient
        );
        assert_eq!(app.world().resource::<InteriorFog>().0, None);
    }

    /// The exterior lighting driver (#653) leaves the interior key light and ambient alone, and
    /// takes the sun and ambient back for the clock's hour in the exit frame.
    #[test]
    fn the_exterior_lighting_driver_yields_indoors_and_resumes_on_exit() {
        use crate::visual_profile::VisualProfile;
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        app.insert_resource(crate::sky::lit_test_source(VisualProfile::Skyrim))
            .insert_resource(VisualProfile::Skyrim)
            .insert_resource(crate::sun::GameClock {
                hour: 12.0,
                timescale: 0.0,
            })
            .add_systems(
                PostUpdate,
                crate::sky::drive_exterior_lighting.after(EnvironmentSet),
            );
        app.update();
        let raw = crate::retail_lighting::raw_bytes;
        assert_eq!(sun_light(&app, sun).color, raw([200, 190, 180]));
        let day_ambient = app.world().resource::<GlobalAmbientLight>().color;
        enter(&mut app, Some(ALVOR));
        app.update();
        let key = sun_light(&app, sun).color;
        let ambient = app.world().resource::<GlobalAmbientLight>().color;
        assert_eq!(key, raw([65, 70, 80]), "the cell key light");
        assert_eq!(
            sun_light(&app, sun).illuminance,
            crate::retail_lighting::sun_illuminance(
                app.world()
                    .resource::<crate::retail_hdr::RetailImageSpace>()
                    .0
                    .sunlight_scale
            )
        );
        // The hour moves indoors: the driver does not touch the room's light.
        app.world_mut().resource_mut::<crate::sun::GameClock>().hour = 0.0;
        app.update();
        assert_eq!(sun_light(&app, sun).color, key);
        assert_eq!(app.world().resource::<GlobalAmbientLight>().color, ambient);
        // Leaving restores a driven exterior in the same frame, for the new hour.
        enter(&mut app, None);
        assert_eq!(sun_light(&app, sun).color, raw([10, 20, 40]));
        assert_ne!(
            app.world().resource::<GlobalAmbientLight>().color,
            day_ambient
        );
        assert!(app.world().get::<ExteriorSunState>(sun).is_none());
    }

    /// Under `skyrim` a settings write indoors (the shadow switch) must not save the interior
    /// key light as the exterior sun.
    #[test]
    fn a_skyrim_shadow_change_indoors_keeps_the_exterior_sun() {
        let mut app = app(rule_catalog());
        app.insert_resource(crate::visual_profile::VisualProfile::Skyrim);
        let sun = spawn_world_sun(&mut app);
        app.update();
        let exterior = *sun_light(&app, sun);
        enter(&mut app, Some(ALVOR));
        app.update();
        assert_ne!(sun_light(&app, sun).color, exterior.color);
        app.world_mut()
            .get_mut::<DirectionalLight>(sun)
            .unwrap()
            .shadow_maps_enabled = false;
        app.update();
        enter(&mut app, None);
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.color, exterior.color);
        assert_eq!(light.illuminance, exterior.illuminance);
        assert!(!light.shadow_maps_enabled);
    }

    #[test]
    fn a_lighting_setting_changed_indoors_is_in_effect_after_the_exit() {
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        enter(&mut app, Some(ALVOR));
        app.update();
        // What `apply_render_settings` does on a preference change: it writes the exterior
        // sun and ambient, here with shadows turned off (the same value the interior applies).
        let new_color = Color::srgb(0.9, 0.8, 0.7);
        let new_ambient = Color::srgb(0.2, 0.3, 0.4);
        {
            let mut light = app.world_mut().get_mut::<DirectionalLight>(sun).unwrap();
            light.color = new_color;
            light.illuminance = 9000.0;
            light.shadow_maps_enabled = false;
        }
        app.world_mut().resource_mut::<GlobalAmbientLight>().color = new_ambient;
        app.update();
        // Indoors the cell key light wins again.
        assert_eq!(sun_light(&app, sun).color, Color::srgb_u8(65, 70, 80));
        enter(&mut app, None);
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.color, new_color);
        assert_eq!(light.illuminance, 9000.0);
        assert!(!light.shadow_maps_enabled);
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().color,
            new_ambient
        );
    }

    /// Bevy 0.19 `fog.wgsl` `linear_fog`: `alpha * (1 - saturate((end - d) / (end - start)))`.
    fn bevy_linear_factor(fog: &DistanceFog, distance: f32) -> f32 {
        let FogFalloff::Linear { start, end } = fog.falloff else {
            panic!("expected linear fog");
        };
        fog.color.alpha() * (1.0 - ((end - distance) / (end - start)).clamp(0.0, 1.0))
    }

    #[test]
    fn fog_max_caps_the_factor_and_does_not_scale_it() {
        let fog = |max: f32| {
            CellFog::from_lighting(&Lighting {
                fog_near: Some(10.0),
                fog_far: Some(110.0),
                fog_power: Some(1.0),
                fog_max: Some(max),
                ..default()
            })
        };
        // RF-WATER-011 with power 1: f = min(saturate((d - 10) / 100), max).
        for max in [0.5, 1.0, 2.0] {
            let bevy = fog(max).unwrap().distance_fog();
            for distance in [0.0, 25.0, 50.0, 75.0, 100.0, 150.0] {
                let retail = ((distance - 10.0) / 100.0_f32).clamp(0.0, 1.0).min(max);
                let actual = bevy_linear_factor(&bevy, distance);
                assert!(
                    (actual - retail).abs() < 1.0e-6,
                    "max {max}, distance {distance}: retail {retail}, Bevy {actual}"
                );
            }
        }
        // Max 0.5: the factor is 0.5 at distance 60 (half way along the ramp), not 0.25.
        let half = fog(0.5).unwrap().distance_fog();
        assert!((bevy_linear_factor(&half, 110.0) - 0.5).abs() < 1.0e-6);
        // Max 0: no fog, and no zero-length ramp.
        assert_eq!(fog(0.0), None);
    }

    #[test]
    fn sun_disabled_indoors_stays_disabled_after_the_exit() {
        // `apply_render_settings` with Sun off writes illuminance 0 and shadows off
        // (`settings.rs`: `illuminance = if settings.sun { .. } else { 0.0 }`,
        // `shadow_maps_enabled = settings.sun && settings.shadows`).
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        enter(&mut app, Some(ALVOR));
        app.update();
        // It also writes the configured sun colour, as it does on every apply.
        let configured = Color::srgb(0.75, 0.71, 0.67);
        {
            let mut light = app.world_mut().get_mut::<DirectionalLight>(sun).unwrap();
            light.color = configured;
            light.illuminance = 0.0;
            light.shadow_maps_enabled = false;
        }
        app.update();
        enter(&mut app, None);
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.illuminance, 0.0);
        assert!(!light.shadow_maps_enabled);
        assert_eq!(light.color, configured);
    }

    /// Production order: the real `apply_render_settings` in `Update` (a settled frame returns
    /// early and writes nothing), the interior systems in `PostUpdate`. The first update applies
    /// the preferences to the sun and the ambient light, so a test starts from a settled exterior.
    fn settings_app() -> (App, Entity) {
        let mut app = app(rule_catalog());
        app.insert_resource(crate::config::EngineConfig::default())
            .insert_resource(crate::settings::RenderingSettings::default())
            .insert_resource(crate::settings::SettingsFile::session_only())
            .insert_resource(crate::color_pipeline::SceneGradeMode::SnapshotSrgb)
            .insert_resource(crate::color_pipeline::SceneVisualPreset::CoolGray)
            .insert_resource(crate::color_pipeline::SceneGradeSource::default())
            .insert_resource(crate::tone_mapping::SceneToneMapSource::default())
            .insert_resource(crate::sky::SkyPalette::default())
            .add_systems(Update, crate::settings::apply_render_settings);
        let sun = spawn_world_sun(&mut app);
        app.world_mut()
            .entity_mut(sun)
            .insert(bevy::light::CascadeShadowConfig::default());
        app.update();
        app.update();
        (app, sun)
    }

    /// Greptile "Lighting settings revert outside" and CodeRabbit "Reapply current preferences
    /// after restoring exterior lighting", with the real settings system: Sun, colour, shadow and
    /// ambient preferences edited while indoors are what the exterior shows after the exit.
    #[test]
    fn real_settings_edited_indoors_are_in_effect_after_the_exit() {
        let (mut app, sun) = settings_app();
        enter(&mut app, Some(ALVOR));
        app.update();
        assert_eq!(sun_light(&app, sun).color, Color::srgb_u8(65, 70, 80));
        {
            let mut settings = app
                .world_mut()
                .resource_mut::<crate::settings::RenderingSettings>();
            settings.sun = false;
            settings.sun_color = [0.9, 0.8, 0.7];
            settings.ambient_color = [0.2, 0.3, 0.4];
        }
        app.update();
        // Indoors the cell key light still wins.
        assert_eq!(sun_light(&app, sun).color, Color::srgb_u8(65, 70, 80));
        assert!(sun_light(&app, sun).illuminance > 0.0);
        for _ in 0..3 {
            app.update();
        }
        enter(&mut app, None);
        for _ in 0..3 {
            app.update();
        }
        let light = sun_light(&app, sun);
        assert_eq!(light.illuminance, 0.0, "Sun is off in the preferences");
        assert!(!light.shadow_maps_enabled);
        assert_eq!(light.color, Color::srgb(0.9, 0.8, 0.7));
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().color,
            Color::srgb(0.2, 0.3, 0.4)
        );
        // Turning Sun back on outside is applied at once.
        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .sun = true;
        app.update();
        assert!(sun_light(&app, sun).illuminance > 0.0);
    }

    /// The preferences change in the same frame as the exit: `apply_render_settings` (`Update`)
    /// writes the exterior values before the interior systems (`PostUpdate`) see the exit, so the
    /// restore must not replace them with the snapshot taken before that write.
    #[test]
    fn a_setting_edited_in_the_exit_frame_is_not_overwritten_by_the_restore() {
        let (mut app, sun) = settings_app();
        enter(&mut app, Some(ALVOR));
        app.update();
        {
            let mut settings = app
                .world_mut()
                .resource_mut::<crate::settings::RenderingSettings>();
            settings.sun = false;
            settings.sun_color = [0.9, 0.8, 0.7];
            settings.ambient_color = [0.2, 0.3, 0.4];
        }
        app.world_mut().resource_mut::<ActiveSpace>().interior = None;
        app.update();
        for _ in 0..3 {
            app.update();
        }
        let light = sun_light(&app, sun);
        assert_eq!(light.illuminance, 0.0, "Sun is off in the preferences");
        assert!(!light.shadow_maps_enabled);
        assert_eq!(light.color, Color::srgb(0.9, 0.8, 0.7));
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().color,
            Color::srgb(0.2, 0.3, 0.4)
        );
    }

    /// Greptile "Older packs lose interior lighting". A pack converted before the environment
    /// tables has no `cell_lighting`, `cell_environment` or `room_bounds`. The catalog then knows
    /// nothing about the cell, which is not the same as an authored cell without XCLL, so the
    /// built-in sun, ambient and fog stay as they were before this PR.
    #[test]
    fn an_older_pack_without_the_environment_tables_keeps_the_built_in_lights() {
        let connection = Connection::open_in_memory().unwrap();
        let mut app = app(EnvironmentCatalog::from_connection(&connection));
        let sun = spawn_world_sun(&mut app);
        let ambient = app.world().resource::<GlobalAmbientLight>().color;
        let rotation = app.world().get::<Transform>(sun).unwrap().rotation;
        enter(&mut app, Some(ALVOR));
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.color, crate::sky::WeatherLighting::default().sunlight);
        assert_eq!(light.illuminance, crate::lights::SUN_ILLUMINANCE);
        assert!(light.shadow_maps_enabled);
        assert_eq!(
            app.world().get::<Transform>(sun).unwrap().rotation,
            rotation
        );
        assert!(app.world().get::<ExteriorSunState>(sun).is_none());
        assert_eq!(app.world().resource::<GlobalAmbientLight>().color, ambient);
        assert_eq!(app.world().resource::<InteriorFog>().0, None);
        assert!(
            app.world()
                .resource::<SceneEnvironment>()
                .cell_render
                .is_none()
        );
    }

    /// The table prepares but its rows cannot be read (a wrong-shape value): the catalog knows
    /// nothing about any cell, exactly as for an older pack, so the built-in lights stay.
    #[test]
    fn a_cell_lighting_table_whose_rows_cannot_be_read_keeps_the_built_in_lights() {
        let connection = Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "INSERT INTO cell_environment(cell_id,data_flags) VALUES(78792,129);
                 INSERT INTO cell_lighting(cell_id,xcll_size,ambient_r) VALUES(78792,92,'not a number');",
            )
            .unwrap();
        let catalog = EnvironmentCatalog::from_connection(&connection);
        assert!(!catalog.has_cell_lighting_table());
        let mut app = app(catalog);
        let sun = spawn_world_sun(&mut app);
        enter(&mut app, Some(ALVOR));
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.color, crate::sky::WeatherLighting::default().sunlight);
        assert_eq!(light.illuminance, crate::lights::SUN_ILLUMINANCE);
        assert!(light.shadow_maps_enabled);
        assert_eq!(app.world().resource::<InteriorFog>().0, None);
        assert!(
            app.world()
                .resource::<SceneEnvironment>()
                .cell_render
                .is_none()
        );
    }

    /// A short XCLL (72 bytes) ends before the fog max, so `fog_max` and the far colour are NULL
    /// while near and far are authored. What retail does with a short block is unverified, so the
    /// engine keeps the authored distances and applies no cap (see `CellFog::from_lighting`).
    #[test]
    fn a_72_byte_xcll_keeps_its_fog_with_no_cap() {
        let connection = Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "INSERT INTO cell_environment(cell_id,data_flags) VALUES(66,1);
                 INSERT INTO cell_lighting(cell_id,xcll_size,fog_near_r,fog_near_g,fog_near_b,
                    fog_near,fog_far)
                 VALUES(66,72,31,52,46,100.0,4000.0);",
            )
            .unwrap();
        let mut app = app(EnvironmentCatalog::from_connection(&connection));
        spawn_world_sun(&mut app);
        enter(&mut app, Some(66));
        app.update();
        let fog = app.world().resource::<InteriorFog>().0.unwrap();
        assert_eq!(
            (fog.near_color, fog.near, fog.far),
            ([31, 52, 46], 100.0, 4000.0)
        );
        assert_eq!(fog.max, 1.0);
        let bevy_fog = fog.distance_fog();
        assert!((bevy_linear_factor(&bevy_fog, 4000.0) - 1.0).abs() < 1.0e-6);
        // A full-length block that authors a max of 0 still draws no fog.
        assert_eq!(
            CellFog::from_lighting(&Lighting {
                fog_near: Some(100.0),
                fog_far: Some(4000.0),
                fog_max: Some(0.0),
                ..default()
            }),
            None
        );
    }

    /// The same for a short lighting template (72 bytes, no fog max): six shipped interiors
    /// inherit their fog distances from one (inherit bits 0x4, 0x8, 0x10 and 0x200).
    #[test]
    fn a_short_lighting_template_keeps_the_inherited_fog_with_no_cap() {
        let connection = Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "INSERT INTO cell_environment(cell_id,data_flags) VALUES(77,1);
                 INSERT INTO cell_lighting(cell_id,lighting_template_id,xcll_size,inherit_flags)
                 VALUES(77,88,92,540);
                 INSERT INTO lighting_templates(id,fog_near_r,fog_near_g,fog_near_b,fog_near,fog_far)
                 VALUES(88,1,2,3,128.0,6000.0);",
            )
            .unwrap();
        let mut app = app(EnvironmentCatalog::from_connection(&connection));
        spawn_world_sun(&mut app);
        enter(&mut app, Some(77));
        app.update();
        let fog = app.world().resource::<InteriorFog>().0.unwrap();
        assert_eq!(
            (fog.near_color, fog.near, fog.far, fog.max),
            ([1, 2, 3], 128.0, 6000.0, 1.0)
        );
    }

    /// The other case: the pack has the tables and the cell has no XCLL and no LTMP, so it has no
    /// `cell_lighting` row. Every retail getter returns 0 for it (RF-LIGHT-CPU-014), so the cell
    /// is lit with colour 0 and the built-in lights do not show through.
    #[test]
    fn an_authored_interior_without_xcll_is_lit_with_zeros() {
        let connection = Connection::open_in_memory().unwrap();
        converter::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch("INSERT INTO cell_environment(cell_id,data_flags) VALUES(55,1);")
            .unwrap();
        let mut app = app(EnvironmentCatalog::from_connection(&connection));
        let sun = spawn_world_sun(&mut app);
        enter(&mut app, Some(55));
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.color, Color::srgb_u8(0, 0, 0));
        assert!(!light.shadow_maps_enabled);
        assert!(
            app.world()
                .resource::<SceneEnvironment>()
                .cell_render
                .is_some()
        );
    }

    #[test]
    fn mode_3_keeps_the_exterior_sun_path() {
        // The maze (0x80 + 0x100) and the exterior keep the sun colour, strength and shadows.
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        let rotation = app.world().get::<Transform>(sun).unwrap().rotation;
        for cell in [Some(MAZE), None] {
            enter(&mut app, cell);
            app.update();
            let light = sun_light(&app, sun);
            assert_eq!(light.color, crate::sky::WeatherLighting::default().sunlight);
            assert_eq!(light.illuminance, crate::lights::SUN_ILLUMINANCE);
            assert!(light.shadow_maps_enabled);
            assert_eq!(
                app.world().get::<Transform>(sun).unwrap().rotation,
                rotation
            );
            assert!(app.world().get::<ExteriorSunState>(sun).is_none());
            assert_eq!(app.world().resource::<InteriorFog>().0, None);
            assert!(
                app.world()
                    .resource::<SceneEnvironment>()
                    .cell_render
                    .is_none()
            );
        }
    }

    #[test]
    fn inheritance_takes_the_template_per_field_and_the_cell_when_it_is_missing() {
        let mut catalog = rule_catalog();
        // Alvor: 0x9F takes ambient/DALC, directional, fog colours, fog near/far and clip from
        // the template. Rotation (0x20) and fog power (0x100) stay the cell's.
        let alvor = catalog.resolve_interior(ALVOR);
        assert_eq!(alvor.lighting.directional, Some([65, 70, 80]));
        assert_eq!(alvor.lighting.fog_near, Some(100.0));
        assert_eq!(alvor.lighting.directional_rotation_z, Some(30));
        assert_eq!(
            alvor.sources["directional_ambient"],
            LightingSource::TemplateDalc(659862)
        );
        assert_eq!(
            alvor.sources["directional_rotation_z"],
            LightingSource::CellXcll
        );
        // A set bit with a missing template reads the cell value.
        catalog.cells.get_mut(&ALVOR).unwrap().lighting_template = Some(999);
        let missing = catalog.resolve_interior(ALVOR);
        assert_eq!(missing.lighting.directional, Some([35, 40, 50]));
        assert_eq!(
            missing.sources["directional"],
            LightingSource::MissingTemplate(Some(999))
        );
        // No XCLL: no bit is read and every field is missing, which applies as 0.
        let cell = catalog.cells.get_mut(&ALVOR).unwrap();
        cell.lighting_template = Some(659862);
        cell.xcll_size = None;
        cell.lighting = Lighting::default();
        let no_xcll = catalog.resolve_interior(ALVOR);
        assert!(!no_xcll.has_xcll);
        assert_eq!(no_xcll.lighting, Lighting::default());
        assert_eq!(no_xcll.sources["directional"], LightingSource::NoXcll);
        let render = InteriorRender::new(&no_xcll.lighting, 1.5);
        assert_eq!(render.key_color, [0; 3]);
        assert_eq!(render.ambient_faces, [[0; 3]; 6]);
        assert_eq!(render.fog, None);
    }

    /// The light fade pair travels to the render values (NEEDS 20): the cell's stored pair, with no
    /// pair for a cell without lighting data, and a room template's pair first.
    #[test]
    fn the_render_values_carry_the_cell_and_room_light_fade_pair() {
        let catalog = rule_catalog();
        let alvor = catalog.resolve_interior(ALVOR);
        // The shipped Alvor cell stores 0/0: the dimmer later reads it as the INI pair.
        let render = InteriorRender::new(&alvor.lighting, 1.5);
        assert_eq!(render.light_fade, Some((0.0, 0.0)));
        let no_lighting = InteriorRender::new(&Lighting::default(), 1.5);
        assert_eq!(no_lighting.light_fade, None);
        let with = |begin, end| crate::world::environment::LightingTemplate {
            lighting: Lighting {
                light_fade_begin: begin,
                light_fade_end: end,
                ..Lighting::default()
            },
            ..crate::world::environment::LightingTemplate::default()
        };
        // A template with both values replaces the cell's; one value, or none, leaves it.
        let room = render.with_room_template(&with(Some(800.0), Some(1600.0)));
        assert_eq!(room.light_fade, Some((800.0, 1600.0)));
        assert_eq!(
            render
                .with_room_template(&with(Some(800.0), None))
                .light_fade,
            Some((0.0, 0.0))
        );
        assert_eq!(
            no_lighting.with_room_template(&with(None, None)).light_fade,
            None
        );
    }

    #[test]
    fn a_room_template_has_precedence_over_the_cell_colours() {
        let mut app = app(rule_catalog());
        // One update is longer than the transition, so a crossing completes in the frame it
        // happens in; the blend itself is tested below.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(4)));
        let sun = spawn_world_sun(&mut app);
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, Transform::from_xyz(0.0, 5000.0, 0.0)))
            .id();
        enter(&mut app, Some(BLEAK_FALLS));
        app.update();
        // Marker 4242 has no template row, so only room 90193 resolves.
        assert_eq!(app.world().resource::<SceneEnvironment>().rooms.len(), 1);
        // Outside every room: the cell's own XCLL (inherit 0).
        let light = sun_light(&app, sun);
        assert_eq!(light.color, Color::srgb_u8(44, 47, 48));
        assert!((light.illuminance - 4285.71).abs() < 0.05);
        let travel = Vec3::new(0.0, -0.866_025_4, 0.5);
        assert_vec(
            app.world().get::<Transform>(sun).unwrap().rotation * Vec3::NEG_Z,
            travel,
        );
        let fog = app.world().resource::<InteriorFog>().0.unwrap();
        assert_eq!((fog.near_color, fog.far), ([75, 99, 97], 12000.0));
        // The cell authors near 0: retail starts its fog at 0.17 of far (RF-FOG-001).
        assert_eq!(fog.near, 0.17 * 12000.0);
        // Inside room 90193 (Creation centre (7936, -5015, -1952)): the room template colours.
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = creation_to_bevy(Vec3::new(7936.0, -5015.0, -1900.0));
        app.update();
        let light = sun_light(&app, sun);
        assert_eq!(light.color, Color::srgb_u8(68, 79, 81));
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().color,
            InteriorRender {
                ambient_faces: [[11; 3]; 6],
                ..app
                    .world()
                    .resource::<SceneEnvironment>()
                    .cell_render
                    .unwrap()
            }
            .ambient_color()
        );
        // The room template supplies the whole fog (RF-LIGHT-CPU-023): colour, near (0 clamped to
        // 0.17 of far), far and max. The cell's 12000 and max 1.0 do not apply.
        let fog = app.world().resource::<InteriorFog>().0.unwrap();
        assert_eq!(
            (fog.near_color, fog.near, fog.far, fog.max),
            ([7, 8, 9], 0.17 * 5000.0, 5000.0, 0.9)
        );
        // The rotation stays the cell's.
        assert_vec(
            app.world().get::<Transform>(sun).unwrap().rotation * Vec3::NEG_Z,
            travel,
        );
        // Out of the room again: back to the cell.
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = Vec3::ZERO;
        app.update();
        assert_eq!(sun_light(&app, sun).color, Color::srgb_u8(44, 47, 48));
    }

    /// Retail's fog near clamp (`RF-FOG-001`): near <= 0 or beyond far becomes 0.17 of far; a
    /// usable near is kept; no far, or a max of 0, is no fog.
    #[test]
    fn the_fog_near_clamp_follows_retail() {
        let fog = |near: Option<f32>, far: Option<f32>| {
            CellFog::from_lighting(&Lighting {
                fog_near: near,
                fog_far: far,
                fog_max: Some(1.0),
                ..default()
            })
        };
        assert_eq!(FOG_NEAR_CLAMP_FRACTION, 0.17, "RF-FOG-001");
        // The pack's example: near 0, far 5000 starts at 850.
        let zero_near = fog(Some(0.0), Some(5000.0)).unwrap();
        assert_eq!((zero_near.near, zero_near.far), (850.0, 5000.0));
        // A missing near is 0 and is clamped the same way, a negative near too.
        assert_eq!(fog(None, Some(5000.0)).unwrap().near, 850.0);
        assert_eq!(fog(Some(-5.0), Some(5000.0)).unwrap().near, 850.0);
        // A near beyond far is clamped; the ramp is no longer empty.
        assert_eq!(fog(Some(6000.0), Some(5000.0)).unwrap().near, 850.0);
        // A usable near is kept.
        assert_eq!(fog(Some(100.0), Some(5000.0)).unwrap().near, 100.0);
        // Near equal to far is a zero-width ramp, which retail draws as no fog
        // (RF-LIGHT-CPU-021); Bevy's linear fog would divide by zero.
        assert_eq!(fog(Some(5000.0), Some(5000.0)), None);
        // No far: nothing to ramp to.
        assert_eq!(fog(Some(100.0), None), None);
        assert_eq!(fog(Some(0.0), Some(0.0)), None);
        // The Bevy ramp starts at the clamped near: no fog there, a full ramp at far.
        let bevy = zero_near.distance_fog();
        assert_eq!(bevy_linear_factor(&bevy, 850.0), 0.0);
        assert!((bevy_linear_factor(&bevy, 5000.0) - 1.0).abs() < 1.0e-6);
    }

    /// A room template supplies fog near, far, power and max, not only the colours
    /// (`RF-LIGHT-CPU-023`), and the cell's inherit bits do not apply to a room: a template
    /// with no fog ramp makes a room without fog in a cell that has one.
    #[test]
    fn a_room_template_supplies_the_whole_fog() {
        let cell = InteriorRender::new(
            &Lighting {
                fog_near: Some(300.0),
                fog_far: Some(9000.0),
                fog_power: Some(2.0),
                fog_max: Some(0.5),
                fog_near_color: Some([1, 1, 1]),
                ..default()
            },
            1.0,
        );
        assert_eq!(cell.fog.unwrap().far, 9000.0);
        let template = |lighting: Lighting| super::super::LightingTemplate {
            lighting,
            ..default()
        };
        let room = cell.with_room_template(&template(Lighting {
            fog_near: Some(0.0),
            fog_far: Some(4000.0),
            fog_power: Some(1.0),
            fog_max: Some(0.75),
            fog_near_color: Some([7, 8, 9]),
            fog_far_color: Some([4, 5, 6]),
            ..default()
        }));
        let fog = room.fog.unwrap();
        assert_eq!(fog.near, 0.17 * 4000.0, "near 0 is clamped like the cell's");
        assert_eq!((fog.far, fog.power, fog.max), (4000.0, 1.0, 0.75));
        assert_eq!((fog.near_color, fog.far_color), ([7, 8, 9], [4, 5, 6]));
        // A short template (no max) keeps retail's constructor value, 1.0.
        let short = cell.with_room_template(&template(Lighting {
            fog_near: Some(100.0),
            fog_far: Some(2000.0),
            ..default()
        }));
        assert_eq!(short.fog.unwrap().max, 1.0);
        // No fog ramp in the template: no room fog, whatever the cell has. Near equal to far is
        // no ramp either (RF-LIGHT-CPU-021).
        assert_eq!(cell.with_room_template(&template(default())).fog, None);
        let equal = template(Lighting {
            fog_near: Some(2000.0),
            fog_far: Some(2000.0),
            ..default()
        });
        assert_eq!(cell.with_room_template(&equal).fog, None);
        // The rotation stays the cell's.
        assert_eq!(room.key_travel, cell.key_travel);
    }

    /// The retail curve (`RF-LIGHT-CPU-024`): `w = (3 - 2t) t^2` with `t = elapsed / 3.0 s`, a
    /// weight of 1.0e-5 when the transition starts, and the end at `t = 1` returning to the default
    /// side with weight 0.
    #[test]
    fn the_room_blend_follows_the_retail_smoothstep_over_three_seconds() {
        assert_eq!(ROOM_LIGHTING_TRANSITION_SECONDS, 3.0, "RF-LIGHT-CPU-024");
        let mut blend = RoomBlend::default();
        blend.snap(Some(7), None);
        assert_eq!(
            (blend.previous, blend.current, blend.weight),
            (None, None, 0.0)
        );
        // A frame of zero length still starts the transition, at the 1e-5 weight.
        blend.step(Some(5), 0.0);
        assert_eq!((blend.previous, blend.current), (None, Some(5)));
        assert_eq!(blend.weight, ROOM_TRANSITION_START_WEIGHT);
        // 0.75 s steps: t = 0.25, 0.5, 0.75.
        for (t, expected) in [(0.25_f32, 0.156_25), (0.5, 0.5), (0.75, 0.843_75)] {
            blend.step(Some(5), 0.75);
            assert!(
                (blend.weight - expected).abs() < 1.0e-5,
                "t = {t}: {}",
                blend.weight
            );
            assert_eq!(blend.current, Some(5));
        }
        // t = 1: done. Previous returns to the default side, and the weight to 0.
        blend.step(Some(5), 0.75);
        assert_eq!(
            (blend.previous, blend.current, blend.weight),
            (None, Some(5), 0.0)
        );
        assert_eq!(blend.elapsed, 0.0);
    }

    /// A room change while a transition runs is not started; it is picked up after the transition
    /// ends (`RF-LIGHT-CPU-024`).
    #[test]
    fn a_room_change_during_a_transition_waits_for_it_to_end() {
        let mut blend = RoomBlend::default();
        blend.snap(Some(7), None);
        blend.step(Some(1), 1.0);
        assert_eq!(blend.current, Some(1));
        blend.step(Some(2), 1.0);
        assert_eq!((blend.previous, blend.current), (None, Some(1)));
        assert_eq!(blend.target, Some(2));
        blend.step(Some(2), 1.0);
        // The first transition ends; the second has not started yet.
        assert_eq!((blend.current, blend.weight), (Some(1), 0.0));
        blend.step(Some(2), 1.0);
        assert_eq!((blend.previous, blend.current), (Some(1), Some(2)));
        assert!(blend.weight > 0.0);
    }

    /// A new cell takes its lighting at once: no transition, whatever was running.
    #[test]
    fn a_cell_change_snaps_the_room_blend() {
        let mut blend = RoomBlend::default();
        blend.snap(Some(7), Some(1));
        blend.step(Some(2), 1.0);
        assert!(blend.weight > 0.0);
        blend.snap(Some(8), None);
        assert_eq!(
            blend,
            RoomBlend {
                cell_id: Some(8),
                ..default()
            }
        );
    }

    /// The blend covers the directional colour, the six DALC colours, both fog colours and
    /// near, far, power and max (`RF-LIGHT-CPU-025`), and leaves the rotation, the sunlight scale
    /// and the key illuminance to the cell.
    #[test]
    fn the_blend_covers_every_listed_field_and_keeps_the_cell_rotation() {
        let side = |value: u8, fog: Option<CellFog>| InteriorRender {
            key_color: [value; 3],
            sunlight_scale: 1.5,
            key_illuminance: 100.0,
            key_travel: [0.0, 1.0, 0.0],
            ambient_faces: [[value; 3]; 6],
            fog,
            fog_fields: FogFields::default(),
            light_fade: None,
        };
        let fog = |value: u8, near: f32, far: f32, power: f32, max: f32| CellFog {
            near_color: [value; 3],
            far_color: [value + 1; 3],
            near,
            far,
            power,
            max,
        };
        let previous = side(0, Some(fog(0, 100.0, 1000.0, 1.0, 0.5)));
        let current = side(100, Some(fog(100, 300.0, 3000.0, 3.0, 1.0)));
        let half = previous.blended_with(&current, 0.5);
        assert_eq!(half.key_color, [50; 3]);
        assert_eq!(half.ambient_faces, [[50; 3]; 6]);
        let fog = half.fog.unwrap();
        assert_eq!((fog.near_color, fog.far_color), ([50; 3], [51; 3]));
        assert_eq!(
            (fog.near, fog.far, fog.power, fog.max),
            (200.0, 2000.0, 2.0, 0.75)
        );
        assert_eq!(half.key_travel, current.key_travel);
        assert_eq!((half.sunlight_scale, half.key_illuminance), (1.5, 100.0));
        // The ends are the sides themselves.
        assert_eq!(previous.blended_with(&current, 1.0), current);
        // A side with no fog ramp stands for retail's far clamp: fog fades in over the blend, and
        // a blend that ends on no fog ends with none.
        let none = side(100, None);
        let fading = previous.blended_with(&none, 0.5).fog.unwrap();
        assert!(
            fading.far > 80_000.0 && fading.far < FOG_FAR_CLAMP,
            "{}",
            fading.far
        );
        assert_eq!(previous.blended_with(&none, 1.0).fog, None);
        let arriving = none.blended_with(&current, 0.5).fog.unwrap();
        assert_eq!(arriving.far, 0.5 * (FOG_FAR_CLAMP + 3000.0));
        assert_eq!(FOG_FAR_CLAMP, 163_840.0, "RF-FOG-001");
        assert_eq!(none.blended_with(&none, 0.5).fog, None);
    }

    /// The `skyrim` fog property takes the room template's whole fog (RF-LIGHT-CPU-023) and blends
    /// between the clamped values of both sides (RF-LIGHT-CPU-025, SPEC-1 section 8.2).
    #[test]
    fn the_retail_fog_property_follows_the_room_template_and_its_blend() {
        let cell = InteriorRender::new(
            &Lighting {
                fog_near: Some(300.0),
                fog_far: Some(9000.0),
                fog_power: Some(2.0),
                fog_max: Some(0.5),
                fog_near_color: Some([10, 20, 30]),
                ..default()
            },
            1.0,
        );
        let room = cell.with_room_template(&super::super::LightingTemplate {
            lighting: Lighting {
                fog_near: Some(0.0),
                fog_far: Some(4000.0),
                fog_near_color: Some([110, 120, 130]),
                fog_far_color: Some([1, 2, 3]),
                ..default()
            },
            ..default()
        });
        let params = room.fog_fields.retail_params();
        // Near 0 takes 0.17 of far; the missing power and max take 1 (STAND-IN #695).
        assert_eq!(
            (params.near, params.far, params.power, params.max),
            (0.17 * 4000.0, 4000.0, 1.0, 1.0)
        );
        let half = cell.blended_with(&room, 0.5).fog_fields.retail_params();
        assert_eq!(half.near, 0.5 * (300.0 + 680.0));
        assert_eq!(half.far, 0.5 * (9000.0 + 4000.0));
        assert_eq!((half.power, half.max), (1.5, 0.75));
        // The cell's far colour is zero bytes, so it is the near colour on that side.
        assert_eq!(half.near_rgb, unit_rgb([60, 70, 80]));
        assert_eq!(half.far_rgb, unit_rgb([6, 11, 17]));
        assert_eq!(cell.blended_with(&room, 1.0).fog_fields, room.fog_fields);
    }

    /// The light fade pair is not blended: the current side of the blend gives it.
    #[test]
    fn the_current_render_is_the_current_side_of_the_blend() {
        let render = |value: u8| InteriorRender {
            key_color: [value; 3],
            ..InteriorRender::new(&Lighting::default(), 1.0)
        };
        let room = RoomLighting::new(
            &RoomBound {
                ref_id: 4,
                position: [0.0; 3],
                rotation: [0.0; 3],
                half: [100.0; 3],
                lighting_template: 1,
            },
            render(9),
        );
        let scene = SceneEnvironment {
            cell_render: Some(render(1)),
            rooms: vec![room],
            ..default()
        };
        let mut blend = RoomBlend::default();
        blend.snap(Some(7), None);
        blend.step(Some(4), 0.5);
        assert_eq!(
            scene.current_render(Some(&blend), None).unwrap().key_color,
            [9; 3]
        );
        assert_eq!(
            scene
                .current_render(None, Some(Vec3::ZERO))
                .unwrap()
                .key_color,
            [9; 3]
        );
        assert_eq!(
            scene
                .current_render(None, Some(Vec3::splat(1000.0)))
                .unwrap()
                .key_color,
            [1; 3]
        );
    }

    /// The point stays in the room it is in while it is still inside, and only otherwise takes the
    /// first room in order that holds it (`RF-LIGHT-CPU-026`).
    #[test]
    fn the_room_choice_is_sticky_while_the_point_stays_inside() {
        let render = InteriorRender::new(&Lighting::default(), 1.0);
        let room = |ref_id: u32, x: f32| {
            RoomLighting::new(
                &RoomBound {
                    ref_id,
                    position: [x, 0.0, 0.0],
                    rotation: [0.0; 3],
                    half: [100.0; 3],
                    lighting_template: 1,
                },
                render,
            )
        };
        // Rooms 1 and 2 overlap between x = 50 and x = 100 (Creation x is engine x).
        let scene = SceneEnvironment {
            cell_render: Some(render),
            rooms: vec![room(1, 0.0), room(2, 150.0)],
            ..default()
        };
        let at = |x: f32| Some(Vec3::new(x, 0.0, 0.0));
        assert_eq!(scene.room_at(at(0.0), None), Some(1));
        assert_eq!(scene.room_at(at(75.0), None), Some(1), "first in order");
        assert_eq!(scene.room_at(at(75.0), Some(2)), Some(2), "sticky");
        assert_eq!(scene.room_at(at(150.0), Some(1)), Some(2), "left room 1");
        assert_eq!(scene.room_at(at(1000.0), Some(2)), None, "in no room");
        assert_eq!(scene.room_at(None, Some(2)), None, "no camera");
    }

    /// End to end in the plugin: a door crossing applies the cell at once, a room crossing starts a
    /// blend that is not at the room's lighting until three seconds have passed, and a cell change
    /// during it applies the new cell with no blend.
    #[test]
    fn a_room_crossing_blends_over_three_seconds_and_a_cell_change_does_not() {
        let mut app = app(rule_catalog());
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            0.5,
        )));
        let sun = spawn_world_sun(&mut app);
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, Transform::from_xyz(0.0, 5000.0, 0.0)))
            .id();
        enter(&mut app, Some(BLEAK_FALLS));
        for _ in 0..3 {
            app.update();
        }
        let cell_color = Color::srgb_u8(44, 47, 48);
        let room_color = Color::srgb_u8(68, 79, 81);
        assert_eq!(sun_light(&app, sun).color, cell_color, "the cell, at once");
        // Into room 90193.
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = creation_to_bevy(Vec3::new(7936.0, -5015.0, -1900.0));
        app.update();
        // One 0.5 s frame: t = 1/6, w = 0.074, so red has moved about 2 of the 24 from 44 to 68.
        let start = sun_light(&app, sun).color.to_srgba().red * 255.0;
        assert!(
            (44.0..50.0).contains(&start),
            "the first frame is still near the cell's lighting: {start}"
        );
        // 0.5 s per update: after six more the transition is at t = 1.
        let mut weights = Vec::new();
        for _ in 0..6 {
            app.update();
            let weight = app.world().resource::<RoomBlend>().weight;
            if weight == 0.0 {
                break;
            }
            weights.push(weight);
            assert_ne!(
                sun_light(&app, sun).color,
                room_color,
                "not at the room's colour before the end"
            );
        }
        assert!(
            weights.len() >= 4 && weights.windows(2).all(|pair| pair[0] < pair[1]),
            "{weights:?}"
        );
        app.update();
        assert_eq!(
            sun_light(&app, sun).color,
            room_color,
            "the room, at the end"
        );
        // Back out starts a new blend toward the cell; a door crossing to another cell during it
        // takes that cell at once.
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = Vec3::ZERO;
        app.update();
        app.update();
        assert!(app.world().resource::<RoomBlend>().weight > 0.0);
        enter(&mut app, Some(ALVOR));
        assert_eq!(app.world().resource::<RoomBlend>().weight, 0.0);
        assert_eq!(sun_light(&app, sun).color, Color::srgb_u8(65, 70, 80));
    }

    /// With a room graph, the graph names the rooms, at the camera plus its near distance
    /// (RF-ROOM-002). Until the graph is read the cell's lighting applies, and the first answer
    /// is no crossing. A room without a lighting template holds the camera with the cell's
    /// lighting.
    #[test]
    fn the_room_graph_names_the_room_at_the_near_distance() {
        use crate::room_culling::{Room, RoomGraph};
        use crate::world::components::{CellRef, StreamedCellRoot};

        const UNTEMPLATED: u32 = 5;
        let center = Vec3::new(7936.0, -5015.0, -1900.0);
        let room = |ref_id| Room::from_creation(ref_id, center.to_array(), [0.0; 3], [100.0; 3]);
        let graph = |rooms| RoomGraph { rooms, ..default() };
        let mut app = app(rule_catalog());
        app.insert_resource(RoomCullingState::unread());
        let sun = spawn_world_sun(&mut app);
        // 10 units outside the box's face, looking in: the near point is 5 units inside.
        let outside_face = creation_to_bevy(center) + Vec3::Z * 110.0;
        let camera = app
            .world_mut()
            .spawn((
                StreamingCamera,
                RoomCullingNear::SKYRIM,
                Transform::from_translation(outside_face),
            ))
            .id();
        let root = app
            .world_mut()
            .spawn((
                StreamedCellRoot,
                CellRef(BLEAK_FALLS),
                GlobalTransform::IDENTITY,
            ))
            .id();
        enter(&mut app, Some(BLEAK_FALLS));
        app.update();
        let cell_color = Color::srgb_u8(44, 47, 48);
        let room_color = Color::srgb_u8(68, 79, 81);
        assert_eq!(
            sun_light(&app, sun).color,
            cell_color,
            "the graph is unread"
        );

        app.world_mut()
            .resource_mut::<RoomCullingState>()
            .insert_graph(BLEAK_FALLS, root, graph(vec![room(90_193)]));
        app.update();
        assert_eq!(sun_light(&app, sun).color, room_color, "at once");
        assert_eq!(app.world().resource::<RoomBlend>().weight, 0.0);

        // Turned away at the same eye, the near point is outside: a crossing, so a blend.
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .rotate_y(std::f32::consts::PI);
        app.update();
        let blend = app.world().resource::<RoomBlend>();
        assert_eq!((blend.target, blend.current), (None, None));
        assert!(blend.weight > 0.0);

        // The first room in table order holds the camera, also without a template.
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .rotation = Quat::IDENTITY;
        enter(&mut app, Some(ALVOR));
        app.world_mut()
            .resource_mut::<RoomCullingState>()
            .insert_graph(
                BLEAK_FALLS,
                root,
                graph(vec![room(UNTEMPLATED), room(90_193)]),
            );
        enter(&mut app, Some(BLEAK_FALLS));
        app.update();
        let blend = app.world().resource::<RoomBlend>();
        assert_eq!(
            (blend.target, blend.current),
            (Some(UNTEMPLATED), None),
            "the cell's lighting"
        );
        assert_eq!(sun_light(&app, sun).color, cell_color);
    }

    /// Retail takes the sky mode from the cell the player is in (RF-LIGHT-CPU-010), and an
    /// interior's coordinates are local to that cell. A camera in the exterior at a point that
    /// lies inside an interior's room box, or beside the Barrow's exterior door (grid -1,-11),
    /// is still in an exterior cell: only a door crossing (`ActiveSpace`) selects the interior.
    #[test]
    fn the_camera_position_never_selects_an_interior_but_a_crossing_does() {
        const BARROW_DOOR_CREATION: Vec3 = Vec3::new(-2966.176, -43_330.668, 6141.133);
        let mut app = app(rule_catalog());
        let sun = spawn_world_sun(&mut app);
        let exterior_rotation = app.world().get::<Transform>(sun).unwrap().rotation;
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, Transform::default()))
            .id();
        for creation in [
            // Inside room 90193 of BleakFallsBarrow01, as an exterior position.
            Vec3::new(7936.0, -5015.0, -1900.0),
            // Beside the exterior load door into BleakFallsBarrow01.
            BARROW_DOOR_CREATION,
            Vec3::ZERO,
        ] {
            app.world_mut()
                .get_mut::<Transform>(camera)
                .unwrap()
                .translation = creation_to_bevy(creation);
            for _ in 0..3 {
                app.update();
            }
            let scene = app.world().resource::<SceneEnvironment>();
            assert_eq!(scene.interior, None, "camera at {creation:?}");
            assert_eq!(
                scene.sky_mode,
                SkyMode::WeatherSun,
                "camera at {creation:?}"
            );
            assert!(scene.cell_render.is_none() && scene.rooms.is_empty());
            assert_eq!(app.world().resource::<InteriorFog>().0, None);
            let light = sun_light(&app, sun);
            assert_eq!(light.illuminance, crate::lights::SUN_ILLUMINANCE);
            assert!(light.shadow_maps_enabled);
            assert_eq!(
                app.world().get::<Transform>(sun).unwrap().rotation,
                exterior_rotation
            );
        }
        // The door crossing writes ActiveSpace; the same camera then gets the interior.
        enter(&mut app, Some(BLEAK_FALLS));
        assert_eq!(selected(&app).cell_id, BLEAK_FALLS);
        assert_eq!(
            app.world().resource::<SceneEnvironment>().sky_mode,
            SkyMode::CellLightingWithSky
        );
        assert!(app.world().resource::<InteriorFog>().0.is_some());
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
