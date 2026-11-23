//! The near-land seam rules use G01 and G02 from the reviewed terrain seam findings.
//! https://github.com/Mudcrab-Team/skyrim_decomp/pull/947

use super::{
    ActiveSpace, MergedTerrainCoverage, StreamingMetrics, StreamingWorld, TerrainCoverage,
    TerrainPatch,
    lod::{LodStreaming, TerrainLodOverlay},
};
use crate::{
    config::EngineConfig,
    render::TerrainMaterial,
    sun::GameClock,
    swap_bridge::SwapWaiting,
    terrain_lod_noise::LandNoiseImage,
    terrain_merge::material::MergedTerrainMaterial,
    visual_profile::VisualProfile,
    world::{components::CELL_SIZE, database::CellKey},
};
use bevy::{prelude::*, render::render_resource::ShaderType};

#[derive(ShaderType, Reflect, Debug, Default, Clone, PartialEq)]
pub(crate) struct NearTerrainBlendSettings {
    pub centre: Vec4,
    pub atlas: Vec4,
}

// #947 G02 traces the initialized gate. stand-in(#939): Use that initial
// value when the live gate and draw keys are unknown. Missing bindings disable the blend.
const INITIAL_GATE: f32 = 1.0;

// #947 G01 covers the settled grid and the temporary transition mask.
fn uses_distant_blend(offset: IVec2, transitioning: bool) -> bool {
    let from_centre = offset.abs().element_sum();
    offset.abs().max_element() <= 2 && (from_centre > 1 || transitioning && from_centre == 1)
}

const SECONDS_PER_DAY: f64 = 24.0 * 3600.0;

#[derive(Default)]
pub(super) struct GridCentre {
    worldspace: Option<u32>,
    from: Vec2,
    to: Vec2,
    started: f64,
}

impl GridCentre {
    fn fraction(&self, seconds: f64) -> f64 {
        // The game clock wraps at midnight, so measure elapsed time modulo one day. A settled
        // transition sets `from` to `to` in `update`, so a later wrap cannot restart it.
        let elapsed = (seconds - self.started).rem_euclid(SECONDS_PER_DAY);
        // stand-in(#939): Use the traced initial interpolation scale of one.
        (elapsed * f64::from(0.2f32)).clamp(0.0, 1.0)
    }

    fn at(&self, seconds: f64) -> Vec2 {
        let fraction = self.fraction(seconds);
        self.from
            .as_dvec2()
            .lerp(self.to.as_dvec2(), fraction)
            .as_vec2()
    }

    fn update(&mut self, worldspace: u32, cell: IVec2, clock: Option<&GameClock>) -> Vec2 {
        let target = cell.as_vec2() + Vec2::splat(0.5);
        // stand-in(#939): The capture clock is frozen. Start each fixed pose at its settled centre.
        let Some(clock) = clock.filter(|clock| clock.timescale != 0.0) else {
            self.worldspace = None;
            return target;
        };
        let seconds = clock.hour * 3600.0;
        if self.worldspace != Some(worldspace) {
            self.worldspace = Some(worldspace);
            self.from = target;
            self.to = target;
            self.started = seconds;
        } else if self.to != target {
            self.from = self.at(seconds);
            self.to = target;
            self.started = seconds;
        }
        let centre = self.at(seconds);
        if self.fraction(seconds) >= 1.0 {
            self.from = self.to;
        }
        centre
    }
}

/// Each merged transition cell owns its settings. Inner cells can share the base material.
#[derive(Component)]
pub(super) struct OwnBlendMaterial;

fn record_missing_blend_bindings(
    metrics: &mut StreamingMetrics,
    noise_missing: bool,
    atlas_missing: bool,
) {
    if noise_missing {
        if metrics.near_terrain_blend_missing_noise == 0 {
            warn!("Near terrain blend requested without a loaded land noise image");
        }
        metrics.near_terrain_blend_missing_noise += 1;
    }
    if atlas_missing {
        if metrics.near_terrain_blend_missing_atlas == 0 {
            warn!("Near terrain blend requested without a ready terrain LOD atlas");
        }
        metrics.near_terrain_blend_missing_atlas += 1;
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn update_near_terrain_blend(
    mut commands: Commands,
    config: Res<EngineConfig>,
    space: Res<ActiveSpace>,
    streaming: Res<StreamingWorld>,
    mut metrics: ResMut<StreamingMetrics>,
    lod: Res<LodStreaming>,
    clock: Option<Res<GameClock>>,
    asset_server: Res<AssetServer>,
    land_noise: Option<Res<LandNoiseImage>>,
    overlays: Query<&TerrainLodOverlay>,
    quadrants: Query<
        (&TerrainCoverage, &ChildOf, &MeshMaterial3d<TerrainMaterial>),
        (With<TerrainPatch>, Without<SwapWaiting>),
    >,
    mut merged: Query<
        (
            Entity,
            &MergedTerrainCoverage,
            &ChildOf,
            &mut MeshMaterial3d<MergedTerrainMaterial>,
            Has<OwnBlendMaterial>,
        ),
        Without<SwapWaiting>,
    >,
    mut quadrant_materials: ResMut<Assets<TerrainMaterial>>,
    merged_materials: Option<ResMut<Assets<MergedTerrainMaterial>>>,
    mut interpolation: Local<GridCentre>,
) {
    if config.visual_profile != VisualProfile::Skyrim {
        return;
    }
    let worldspace = space.exterior_worldspace(config.worldspace_id);
    let centre = streaming
        .planned_center
        .filter(|_| space.interior.is_none());
    let position = centre.map(|cell| interpolation.update(worldspace, cell, clock.as_deref()));
    if centre.is_none() {
        interpolation.worldspace = None;
    }
    // stand-in(#939): The transition ends with interpolation. Mudcrab has no retail pending pointer.
    let transitioning = interpolation.from != interpolation.to
        && clock.as_ref().is_some_and(|clock| {
            clock.timescale != 0.0 && interpolation.fraction(clock.hour * 3600.0) < 1.0
        });
    let noise = land_noise
        .as_ref()
        .and_then(|image| image.loaded(&asset_server));
    let mut cells = bevy::platform::collections::HashMap::new();
    let mut select = |grid: IVec2, parent: Entity| {
        cells
            .entry((grid, parent))
            .or_insert_with(|| {
                let cell = centre?;
                // #947 G01 covers the technique mask for a 5×5 exterior grid.
                if config.stream_radius != 2
                    || !uses_distant_blend(grid - cell, transitioning)
                    || streaming.resident_root(CellKey::Exterior {
                        worldspace_id: worldspace,
                        grid_x: grid.x,
                        grid_y: grid.y,
                    }) != Some(parent)
                {
                    return None;
                }
                let overlay = lod.near_terrain_overlay(
                    &overlays,
                    worldspace,
                    cell,
                    grid,
                    &config.terrain_lod,
                );
                record_missing_blend_bindings(&mut metrics, noise.is_none(), overlay.is_none());
                let (image, base, width) = overlay?;
                let noise = noise?;
                let local = (position? - grid.as_vec2()) * CELL_SIZE;
                // #947 G02 gives the quadrant offset as (cell - base + half quadrant) / width.
                // Both meshes already use cell-wide UVs. Their high quadrants include the half
                // cell in the vertex UV, so it is added once by the shader before flipping V.
                let offset = (grid - base).as_vec2() / width;
                let settings = NearTerrainBlendSettings {
                    centre: Vec4::new(local.x, local.y, 0.0, 0.0),
                    atlas: Vec4::new(offset.x, offset.y, width.recip(), INITIAL_GATE),
                };
                // stand-in(#939): Reuse the converted atlas view and sampler until the live slot 13 binding is traced.
                Some((settings, image.clone(), noise.clone()))
            })
            .as_ref()
            .cloned()
    };
    for (coverage, parent, handle) in &quadrants {
        let blend = select(coverage.grid, parent.parent());
        if let Some(material) = quadrant_materials.get(&handle.0)
            && material.extension.blend_differs(blend.as_ref())
            && let Some(mut material) = quadrant_materials.get_mut(&handle.0)
        {
            material.extension.set_blend(blend);
        }
    }
    let Some(mut materials) = merged_materials else {
        return;
    };
    for (entity, coverage, parent, mut handle, owned) in &mut merged {
        let blend = select(coverage.grid, parent.parent());
        let Some(material) = materials.get(&handle.0) else {
            continue;
        };
        if !material.extension.blend_differs(blend.as_ref()) {
            continue;
        }
        if owned {
            if let Some(mut material) = materials.get_mut(&handle.0) {
                material.extension.set_blend(blend);
            }
        } else {
            let mut material = material.clone();
            material.extension.set_blend(blend);
            handle.0 = materials.add(material);
            commands.entity(entity).insert(OwnBlendMaterial);
        }
    }
}

pub(crate) type NearTerrainBlend = (NearTerrainBlendSettings, Handle<Image>, Handle<Image>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_cross_uses_technique_eight_in_the_settled_grid() {
        let mut count = 0;
        for y in -2i32..=2 {
            for x in -2i32..=2 {
                let technique_19 = uses_distant_blend(IVec2::new(x, y), false);
                assert_eq!(
                    technique_19,
                    x.abs() == 2 || y.abs() == 2 || (x != 0 && y != 0)
                );
                count += usize::from(technique_19);
            }
        }
        assert_eq!(count, 20);
        assert!(uses_distant_blend(IVec2::X, true));
        assert!(!uses_distant_blend(IVec2::ZERO, true));
        assert!(!uses_distant_blend(IVec2::new(3, 0), false));
    }

    #[test]
    fn near_terrain_blend_counts_missing_bindings_independently() {
        let mut metrics = StreamingMetrics::default();
        record_missing_blend_bindings(&mut metrics, false, false);
        assert_eq!(metrics.near_terrain_blend_missing_noise, 0);
        assert_eq!(metrics.near_terrain_blend_missing_atlas, 0);

        record_missing_blend_bindings(&mut metrics, true, false);
        assert_eq!(metrics.near_terrain_blend_missing_noise, 1);
        assert_eq!(metrics.near_terrain_blend_missing_atlas, 0);

        record_missing_blend_bindings(&mut metrics, true, true);
        assert_eq!(metrics.near_terrain_blend_missing_noise, 2);
        assert_eq!(metrics.near_terrain_blend_missing_atlas, 1);
    }

    #[test]
    fn grid_centre_interpolates_in_game_seconds() {
        let mut state = GridCentre::default();
        let mut clock = GameClock {
            hour: 0.0,
            timescale: 1.0,
        };
        assert_eq!(state.update(0, IVec2::ZERO, Some(&clock)), Vec2::splat(0.5));
        assert_eq!(state.update(0, IVec2::X, Some(&clock)), Vec2::splat(0.5));
        clock.hour = 2.5 / 3600.0;
        assert_eq!(state.update(0, IVec2::X, Some(&clock)), Vec2::new(1.0, 0.5));
        clock.hour = 5.0 / 3600.0;
        assert_eq!(state.update(0, IVec2::X, Some(&clock)), Vec2::new(1.5, 0.5));
    }

    #[test]
    fn grid_centre_interpolation_continues_across_midnight() {
        let mut state = GridCentre::default();
        let mut clock = GameClock {
            hour: 24.0 - 1.0 / 3600.0,
            timescale: 1.0,
        };
        state.update(0, IVec2::ZERO, Some(&clock));
        state.update(0, IVec2::X, Some(&clock));
        clock.hour = 1.5 / 3600.0;
        assert_eq!(state.update(0, IVec2::X, Some(&clock)), Vec2::new(1.0, 0.5));
        clock.hour = 4.0 / 3600.0;
        assert_eq!(state.update(0, IVec2::X, Some(&clock)), Vec2::new(1.5, 0.5));
        // A full day later the settled centre must not restart the transition.
        clock.hour = 24.0 - 0.5 / 3600.0;
        assert_eq!(state.update(0, IVec2::X, Some(&clock)), Vec2::new(1.5, 0.5));
    }
}
