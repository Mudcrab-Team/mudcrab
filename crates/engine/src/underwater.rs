//! The underwater view: the retail underwater fog, and the test that decides when it applies.
//!
//! What retail does (RF-WATER-031, RF-WATER-033, RF-WATER-052): the underwater look is not a tint.
//! It is the depth-of-field image-space effect's fog program, `out = lerp(scene, C, sat((d - near)
//! / (far - near)))`, where `d` is the view-space depth of the pixel, `C` the water's Shallow Color times the
//! weather's Water Multiplier, and `near` and `far` the water record's underwater fog distances
//! (`DNAM` +0x90 and +0x94). The pass applies when the camera is below the water surface, which retail
//! decides by comparing the SURFACE HEIGHT with the heights of the four corners of the camera's
//! near plane, not with the camera's own height: the share of the near plane that is under the
//! surface picks the mode (above, crossing, below). The image space's tint and saturation fields
//! have no reader in the executable and are not used.
//!
//! What Mudcrab did before: Bevy's distance fog on the world camera, which follows the straight-line
//! distance from the eye and fogs only what a lit material draws, not the sky.
//!
//! What Mudcrab does now, and differently from retail: the fog and the blur are one post-pass
//! (`underwater_pass.rs`) over the view-space depth, and the atmospheric distance fog is switched
//! off while it runs. The pass applies only when the camera is fully below the surface (mode 2),
//! because the crossing mode (mode 1) needs retail's underwater plane mask, which the engine has not
//! got yet (#698).

use crate::{
    sky::SkyPalette,
    underwater_pass::UnderwaterPass,
    world::{
        components::{CELL_SIZE, StreamingCamera, WaterSurface},
        database::AssetCatalog,
        water::FlatWaterBody,
    },
};
use bevy::prelude::*;

/// Where the camera is relative to the water surface (RF-WATER-033: the mode word is 0 above, 1
/// crossing, 2 below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UnderwaterMode {
    #[default]
    Above,
    Crossing,
    Below,
}

/// The fog of the water the camera is in: its colour and the two distances it blends over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct UnderwaterFog {
    pub color: Color,
    pub near: f32,
    pub far: f32,
}

impl UnderwaterFog {
    /// The post-pass settings of this fog for a camera whose near plane is `camera_near`.
    pub(crate) fn pass(&self, camera_near: f32) -> UnderwaterPass {
        UnderwaterPass::new(self.color.to_linear(), self.near, self.far, camera_near)
    }
}

/// What the underwater test found this frame.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct UnderwaterState {
    mode: UnderwaterMode,
    fog: Option<UnderwaterFog>,
}

impl UnderwaterState {
    /// The fog to draw, only while the camera is fully below the surface.
    pub(crate) fn fog(&self) -> Option<UnderwaterFog> {
        (self.mode == UnderwaterMode::Below)
            .then_some(self.fog)
            .flatten()
    }

    /// A state fully below the surface with `fog`, for tests.
    #[cfg(test)]
    pub(crate) fn below(fog: UnderwaterFog) -> Self {
        Self {
            mode: UnderwaterMode::Below,
            fog: Some(fog),
        }
    }

    #[cfg(test)]
    pub(crate) fn mode(&self) -> UnderwaterMode {
        self.mode
    }
}

/// DefaultWater's underwater fog near and far (`DNAM` +0x90, +0x94: -500 and 1600 in the retail
/// records read for this PR), used for a water whose record cannot be read.
pub(crate) const DEFAULT_UNDERWATER_FOG: [f32; 2] = [-500.0, 1600.0];

/// The lowest and highest world height of the four corners of the camera's near plane
/// (RF-WATER-033).
pub(crate) fn near_plane_heights(
    camera: &GlobalTransform,
    near: f32,
    vertical_fov: f32,
    aspect_ratio: f32,
) -> (f32, f32) {
    let half_height = near * (vertical_fov * 0.5).tan();
    let half_width = half_height * aspect_ratio;
    let centre = camera.translation() + camera.forward().as_vec3() * near;
    let right = camera.right().as_vec3();
    let up = camera.up().as_vec3();
    let mut lowest = f32::INFINITY;
    let mut highest = f32::NEG_INFINITY;
    for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let height = (centre + right * (x * half_width) + up * (y * half_height)).y;
        lowest = lowest.min(height);
        highest = highest.max(height);
    }
    (lowest, highest)
}

/// The share of the near plane under the surface: `clamp((H - zmin) / (zmax - zmin), 0, 1)`
/// (RF-WATER-033).
pub(crate) fn underwater_factor(surface_height: f32, lowest: f32, highest: f32) -> f32 {
    ((surface_height - lowest) / (highest - lowest)).clamp(0.0, 1.0)
}

/// The mode word (RF-WATER-033). `in_water` is retail's manager flag "camera is in water". With a
/// flat near plane (`lowest == highest`) only that flag decides; otherwise no flag means above, a
/// factor of 0 above, 1 below and anything between crossing.
pub(crate) fn underwater_mode(
    in_water: bool,
    surface_height: f32,
    lowest: f32,
    highest: f32,
) -> UnderwaterMode {
    if lowest == highest {
        return if in_water {
            UnderwaterMode::Below
        } else {
            UnderwaterMode::Above
        };
    }
    if !in_water {
        return UnderwaterMode::Above;
    }
    let factor = underwater_factor(surface_height, lowest, highest);
    if factor <= 0.0 {
        UnderwaterMode::Above
    } else if factor >= 1.0 {
        UnderwaterMode::Below
    } else {
        UnderwaterMode::Crossing
    }
}

/// The fog colour `C`: the Shallow Color bytes over 255 times the Water Multiplier bytes over 255,
/// read as an sRGB colour (RF-WATER-031, RF-SHADER-007).
pub(crate) fn underwater_fog_color(shallow: [u8; 3], multiplier: [u8; 3]) -> Color {
    let channel =
        |index: usize| f32::from(shallow[index]) / 255.0 * f32::from(multiplier[index]) / 255.0;
    Color::srgb(channel(0), channel(1), channel(2))
}

/// Finds the water the camera is in and what that does to the view.
pub(crate) fn update_underwater_state(
    mut commands: Commands,
    mut state: ResMut<UnderwaterState>,
    camera: Query<
        (Entity, &GlobalTransform, &Projection, Has<UnderwaterPass>),
        With<StreamingCamera>,
    >,
    waters: Query<(&GlobalTransform, &FlatWaterBody), With<WaterSurface>>,
    catalog: Option<Res<AssetCatalog>>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
) {
    let found = camera
        .single()
        .ok()
        .and_then(|(_, transform, projection, _)| {
            let Projection::Perspective(lens) = projection else {
                return None;
            };
            let eye = transform.translation();
            // The highest plane whose cell square holds the eye (the same rule as `water_at_point`).
            let (surface, body) = waters
                .iter()
                .filter(|(plane, _)| {
                    let centre = plane.translation();
                    (eye.x - centre.x).abs() <= CELL_SIZE * 0.5
                        && (eye.z - centre.z).abs() <= CELL_SIZE * 0.5
                })
                .map(|(plane, body)| (plane.translation().y, body))
                .max_by(|a, b| a.0.total_cmp(&b.0))?;
            let (lowest, highest) =
                near_plane_heights(transform, lens.near, lens.fov, lens.aspect_ratio);
            // The camera is "in water" when the eye is under the surface: the engine's stand-in for the
            // physics water callback that sets retail's flag (RF-WATER-034).
            let mode = underwater_mode(eye.y < surface, surface, lowest, highest);
            let colors = catalog
                .as_deref()
                .zip(body.surface().water_type_form_id)
                .and_then(|(catalog, id)| catalog.water_colors(id));
            let shallow = colors.map_or(crate::render::DEFAULT_WATER_SHALLOW_COLOR, |c| c.shallow);
            let [near, far] = colors
                .and_then(|colors| colors.underwater_fog)
                .unwrap_or(DEFAULT_UNDERWATER_FOG);
            Some((
                mode,
                UnderwaterFog {
                    color: underwater_fog_color(
                        shallow,
                        SkyPalette::SKYRIM_CLEAR_DAY_WATER_MULTIPLIER,
                    ),
                    near,
                    far,
                },
            ))
        });
    let wanted = match found {
        Some((mode, fog)) => UnderwaterState {
            mode,
            fog: Some(fog),
        },
        None => UnderwaterState::default(),
    };
    if *state != wanted {
        *state = wanted;
    }
    // The post-pass rides on the camera while it is fully below the surface (and the player has
    // not turned fog off); above it the component is gone and the pass costs nothing.
    let fog_on = settings.as_deref().is_none_or(|settings| settings.fog);
    let Ok((entity, _, projection, has_pass)) = camera.single() else {
        return;
    };
    let near = match projection {
        Projection::Perspective(lens) => lens.near,
        _ => return,
    };
    match state.fog().filter(|_| fog_on) {
        Some(fog) => {
            commands.entity(entity).insert(fog.pass(near));
        }
        None if has_pass => {
            commands.entity(entity).remove::<UnderwaterPass>();
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera at `height`, looking along `-Z` and pitched by `pitch` radians.
    fn camera_at(height: f32, pitch: f32) -> GlobalTransform {
        GlobalTransform::from(
            Transform::from_xyz(0.0, height, 0.0).with_rotation(Quat::from_rotation_x(pitch)),
        )
    }

    #[test]
    fn the_near_plane_heights_are_the_four_corners() {
        // Looking level, the plane is a vertical rectangle: its corners are the eye height plus or
        // minus half the plane's height (near * tan(fov / 2)).
        let fov = 90.0_f32.to_radians();
        let (lowest, highest) = near_plane_heights(&camera_at(100.0, 0.0), 10.0, fov, 2.0);
        assert!((lowest - 90.0).abs() < 1.0e-4 && (highest - 110.0).abs() < 1.0e-4);
        // Looking straight down, the plane is horizontal and every corner has the same height, 10
        // below the eye: the flat-plane case, where only the in-water flag decides.
        let (lowest, highest) =
            near_plane_heights(&camera_at(100.0, -90.0_f32.to_radians()), 10.0, fov, 2.0);
        assert!((lowest - 90.0).abs() < 1.0e-3 && (highest - 90.0).abs() < 1.0e-3);
    }

    #[test]
    fn the_factor_is_the_share_of_the_near_plane_under_the_surface() {
        // RF-WATER-033: clamp((H - zmin) / (zmax - zmin), 0, 1).
        assert_eq!(underwater_factor(90.0, 90.0, 110.0), 0.0);
        assert_eq!(underwater_factor(100.0, 90.0, 110.0), 0.5);
        assert_eq!(underwater_factor(110.0, 90.0, 110.0), 1.0);
        assert_eq!(underwater_factor(500.0, 90.0, 110.0), 1.0);
        assert_eq!(underwater_factor(-500.0, 90.0, 110.0), 0.0);
    }

    #[test]
    fn the_mode_word_follows_the_surface_height_against_the_near_plane() {
        use UnderwaterMode::*;
        // Not in water: above, whatever the heights say.
        assert_eq!(underwater_mode(false, 100.0, 90.0, 110.0), Above);
        // In water and the surface above the whole near plane: below. Below it: above. Between:
        // crossing.
        assert_eq!(underwater_mode(true, 120.0, 90.0, 110.0), Below);
        assert_eq!(underwater_mode(true, 80.0, 90.0, 110.0), Above);
        assert_eq!(underwater_mode(true, 100.0, 90.0, 110.0), Crossing);
        // A flat near plane (looking straight down or up): only the flag decides.
        assert_eq!(underwater_mode(true, 100.0, 95.0, 95.0), Below);
        assert_eq!(underwater_mode(false, 100.0, 95.0, 95.0), Above);
        // The test compares the SURFACE height with the plane's corners, not the camera's height: a
        // camera whose centre is just above a surface that still touches the plane's lower corners
        // is crossing.
        let fov = 90.0_f32.to_radians();
        let (lowest, highest) = near_plane_heights(&camera_at(100.5, 0.0), 10.0, fov, 2.0);
        assert_eq!(underwater_mode(true, 100.0, lowest, highest), Crossing);
    }

    #[test]
    fn only_the_fully_below_mode_draws_the_fog() {
        let fog = UnderwaterFog {
            color: Color::WHITE,
            near: -500.0,
            far: 1600.0,
        };
        let state = |mode| UnderwaterState {
            mode,
            fog: Some(fog),
        };
        assert_eq!(state(UnderwaterMode::Below).fog(), Some(fog));
        assert_eq!(state(UnderwaterMode::Crossing).fog(), None);
        assert_eq!(state(UnderwaterMode::Above).fog(), None);
        assert_eq!(UnderwaterState::default().fog(), None);
    }

    #[test]
    fn the_fog_is_the_retail_linear_term_on_the_shallow_colour() {
        // RF-WATER-031: C = Shallow / 255 * Water Multiplier / 255, near/far DNAM +0x90 / +0x94;
        // DefaultWater (-500, 1600) is 24 % fog at the camera and full colour at 1600 units.
        let fog = UnderwaterFog {
            color: underwater_fog_color(
                [38, 39, 24],
                SkyPalette::SKYRIM_CLEAR_DAY_WATER_MULTIPLIER,
            ),
            near: DEFAULT_UNDERWATER_FOG[0],
            far: DEFAULT_UNDERWATER_FOG[1],
        };
        let expected = Color::srgb(
            38.0 / 255.0 * 175.0 / 255.0,
            39.0 / 255.0 * 237.0 / 255.0,
            24.0 / 255.0 * 214.0 / 255.0,
        );
        assert_eq!(fog.color, expected);
        let pass = fog.pass(0.1);
        let linear = expected.to_linear();
        assert_eq!(
            format!("{pass:?}"),
            format!("{:?}", UnderwaterPass::new(linear, -500.0, 1600.0, 0.1))
        );
        // The record's own distances are used when it has them.
        assert_eq!(DEFAULT_UNDERWATER_FOG, [-500.0, 1600.0]);
    }

    #[test]
    fn the_system_finds_the_water_the_camera_is_in() {
        use crate::world::components::StreamingCamera;
        let mut app = App::new();
        app.init_resource::<UnderwaterState>()
            .add_systems(Update, update_underwater_state);
        // No camera and no water: above.
        app.update();
        assert_eq!(
            app.world().resource::<UnderwaterState>().mode(),
            UnderwaterMode::Above
        );
        // A camera and no water plane: still above.
        app.world_mut().spawn((
            StreamingCamera,
            Projection::Perspective(PerspectiveProjection::default()),
            GlobalTransform::from(Transform::from_xyz(0.0, 50.0, 0.0)),
        ));
        app.update();
        assert_eq!(app.world().resource::<UnderwaterState>().fog(), None);
    }

    #[test]
    fn the_post_pass_rides_on_the_camera_only_while_it_is_fully_below_the_surface() {
        use crate::world::{cache::TerrainSnapshot, database::CellKey};
        let mut app = App::new();
        app.init_resource::<UnderwaterState>()
            .add_systems(Update, update_underwater_state);
        // One exterior plane at height 100, its cell square centred on the origin.
        let terrain = TerrainSnapshot {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![0.0; 33 * 33],
            normals: vec![],
            vertex_colors: vec![],
            layers: vec![],
            water_height: Some(100.0),
            water_type_form_id: None,
        };
        let body = FlatWaterBody::from_terrain(
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 0,
                grid_y: 0,
            },
            &terrain,
        )
        .expect("a body");
        app.world_mut().spawn((
            WaterSurface,
            body,
            GlobalTransform::from(Transform::from_xyz(0.0, 100.0, 0.0)),
        ));
        let lens = PerspectiveProjection::default();
        // Looking straight down keeps the near plane flat, so only the eye's side of the surface
        // decides (RF-WATER-033).
        let down = Quat::from_rotation_x(-90.0_f32.to_radians());
        let camera = app
            .world_mut()
            .spawn((
                StreamingCamera,
                Projection::Perspective(lens.clone()),
                GlobalTransform::from(Transform::from_xyz(0.0, 50.0, 0.0).with_rotation(down)),
            ))
            .id();
        app.update();
        let pass = *app
            .world()
            .get::<UnderwaterPass>(camera)
            .expect("the pass is on a camera under the surface");
        // The record's own fog is not in this app, so DefaultWater's distances stand in, and the
        // camera's near plane feeds the depth rebuild.
        assert_eq!(
            format!("{pass:?}"),
            format!(
                "{:?}",
                UnderwaterFog {
                    color: underwater_fog_color(
                        crate::render::DEFAULT_WATER_SHALLOW_COLOR,
                        SkyPalette::SKYRIM_CLEAR_DAY_WATER_MULTIPLIER
                    ),
                    near: DEFAULT_UNDERWATER_FOG[0],
                    far: DEFAULT_UNDERWATER_FOG[1],
                }
                .pass(lens.near)
            )
        );
        // Above the surface the component is gone.
        app.world_mut()
            .entity_mut(camera)
            .insert(GlobalTransform::from(
                Transform::from_xyz(0.0, 150.0, 0.0).with_rotation(down),
            ));
        app.update();
        assert!(app.world().get::<UnderwaterPass>(camera).is_none());
    }
}
